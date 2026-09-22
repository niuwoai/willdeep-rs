use super::*;
use crate::background::{BackgroundTaskRegistry, BackgroundTaskStatus};
use crate::provider::{Provider, ProviderError};
use crate::subagent::{SubagentCatalog, TaskPacket, TaskVerifier, builtin_profiles};
use crate::types::{Completion, ToolCall, ToolDefinition};
use crate::{Message, Role};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct RecoveryProvider {
    calls: AtomicUsize,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    resumed: Mutex<Vec<Message>>,
}

struct RecoveryParent {
    id: uuid::Uuid,
    calls: AtomicUsize,
    cached: bool,
}

#[async_trait::async_trait]
impl Provider for RecoveryParent {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        assert!(tools.iter().any(|tool| tool.name == "resume_agent"));
        let call = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Some(ToolCall {
                id: "list-recovery".into(),
                name: "list_agent_recoveries".into(),
                arguments: "{}".into(),
            }),
            1 => {
                let list: serde_json::Value =
                    serde_json::from_str(&messages.last().unwrap().content).unwrap();
                assert_eq!(list["agents"][0]["agent_id"], self.id.to_string());
                assert_eq!(list["agents"][0]["completed_report_available"], self.cached);
                Some(ToolCall {
                    id: "resume-original".into(),
                    name: "resume_agent".into(),
                    arguments: serde_json::json!({"agent_id":self.id}).to_string(),
                })
            }
            _ => {
                if self.cached {
                    assert!(
                        messages
                            .last()
                            .unwrap()
                            .content
                            .contains("tools and verifier were not rerun")
                    );
                }
                None
            }
        };
        Ok(Completion {
            reasoning: None,
            content: "recovered".into(),
            tool_calls: call.into_iter().collect(),
            finish_reason: Some("stop".into()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn foreground_recovery_tools_resume_once_then_return_cached_verified_report() {
    let root = std::env::temp_dir().join(format!("foreground-recovery-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("repo");
    let home = root.join("workers");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    git(&workspace, &["init", "--quiet"]);
    git(
        &workspace,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let parent = uuid::Uuid::new_v4();
    let (started, observed) = tokio::sync::oneshot::channel();
    let provider = Arc::new(RecoveryProvider {
        calls: AtomicUsize::new(0),
        started: Mutex::new(Some(started)),
        resumed: Mutex::new(Vec::new()),
    });
    let make = || {
        SubagentCatalog::new(
            &workspace,
            builtin_profiles(provider.clone()),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_state_home(&home)
        .with_parent_session(parent)
        .with_worktree_root(root.join("worktrees"))
        .with_safety_judge(Arc::new(super::super::test_support::AllowingJudge))
    };
    let original = make();
    let verifier = "ruby -e 'File.open(%q(verified), %q(a)) { |file| file.write(%q(x)) }'";
    let mut execution = Box::pin(original.run(
        SpawnAgentArgs {
            profile: Some("implementer".into()),
            prompt: "Create new.rs once".into(),
            task: Some(TaskPacket {
                goal: "create one file".into(),
                write_files: vec!["new.rs".into()],
                verifier: Some(TaskVerifier {
                    command: verifier.into(),
                    expected_exit_code: Some(0),
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Some(BTreeSet::from([workspace.join("new.rs")])),
    ));
    tokio::select! {
        result = &mut execution => panic!("child unexpectedly finished: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(10), observed) => result.unwrap().unwrap(),
    }
    drop(execution);
    drop(original);
    let list = list_foreground(&home, parent, None).unwrap();
    let id = list["agents"][0]["agent_id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    let record = load(&home, id).unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(record.prepared.workspace.join("new.rs")).unwrap(),
        "once"
    );
    assert!(!record.prepared.workspace.join("verified").exists());
    for cached in [false, true] {
        let agent = crate::Agent::new(
            Arc::new(RecoveryParent {
                id,
                calls: AtomicUsize::new(0),
                cached,
            }),
            crate::ToolRegistry::new(&workspace, crate::ApprovalMode::WorkspaceAccess).unwrap(),
            crate::AgentConfig {
                max_turns: 4,
                system_prompt: String::new(),
                context_window: 32000,
                token_budget: None,
            },
        )
        .with_subagents(Arc::new(make()));
        let outcome = agent
            .run("Recover the original foreground child")
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, crate::AgentStopReason::Finished);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            std::fs::read_to_string(record.prepared.workspace.join("verified")).unwrap(),
            "x",
            "a cached report must not repeat the verifier"
        );
    }
    assert!(
        list_foreground(&home, uuid::Uuid::new_v4(), None).unwrap()["agents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        make()
            .with_parent_session(uuid::Uuid::new_v4())
            .resume_foreground_agent(id)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(&record.prepared.workspace).unwrap();
    assert!(
        make()
            .resume_foreground_agent(id)
            .await
            .unwrap()
            .contains("tools and verifier were not rerun")
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[async_trait::async_trait]
impl Provider for RecoveryProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(Completion {
                reasoning: None,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "write-once".into(),
                    name: "create_file".into(),
                    arguments: r#"{"path":"new.rs","content":"once"}"#.into(),
                }],
                finish_reason: Some("tool_calls".into()),
                usage: None,
            }),
            1 => {
                if let Some(started) = self.started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                std::future::pending().await
            }
            _ => {
                *self.resumed.lock().unwrap() = messages.to_vec();
                Ok(Completion {
                    reasoning: None,
                    content: "finished".into(),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".into()),
                    usage: None,
                })
            }
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn recovery_inventory_beyond_4096_paginates_within_parent_and_marks_reports() {
    let root = std::env::temp_dir().join(format!("recovery-pages-{}", uuid::Uuid::new_v4()));
    let parent = uuid::Uuid::new_v4();
    for index in (1..=4114).rev() {
        let id = uuid::Uuid::from_u128(index);
        save(
            &root,
            &DispatchRecord {
                version: 1,
                id,
                parent_session: Some(parent),
                args: SpawnAgentArgs {
                    prompt: "original task".into(),
                    profile: Some("scout".into()),
                    ..Default::default()
                },
                approved_targets: None,
                approved_command: None,
                model: None,
                prepared: PreparedSubagentWorkspace {
                    workspace: root.clone(),
                    root_workspace: root.clone(),
                    branch: None,
                    dedicated: false,
                },
            },
        )
        .unwrap();
    }
    save_report(&root, uuid::Uuid::from_u128(1), "verified report").unwrap();
    let first = list_foreground(&root, parent, None).unwrap();
    assert_eq!(first["agents"].as_array().unwrap().len(), 16);
    assert_eq!(first["agents"][0]["completed_report_available"], true);
    let cursor = first["next_after_id"].as_str().unwrap().parse().unwrap();
    let second = list_foreground(&root, parent, Some(cursor)).unwrap();
    assert_eq!(second["agents"].as_array().unwrap().len(), 16);
    assert_eq!(
        second["agents"][0]["agent_id"],
        uuid::Uuid::from_u128(17).to_string()
    );
    let beyond = list_foreground(&root, parent, Some(uuid::Uuid::from_u128(4096))).unwrap();
    assert_eq!(beyond["agents"].as_array().unwrap().len(), 16);
    assert_eq!(
        beyond["agents"][0]["agent_id"],
        uuid::Uuid::from_u128(4097).to_string()
    );
    let cursor = beyond["next_after_id"].as_str().unwrap().parse().unwrap();
    let last = list_foreground(&root, parent, Some(cursor)).unwrap();
    assert_eq!(last["agents"].as_array().unwrap().len(), 2);
    assert_eq!(
        last["agents"][1]["agent_id"],
        uuid::Uuid::from_u128(4114).to_string()
    );
    assert!(last["next_after_id"].is_null());
    let empty = list_foreground(&root, parent, Some(uuid::Uuid::from_u128(4114))).unwrap();
    assert!(empty["agents"].as_array().unwrap().is_empty());
    assert!(empty["next_after_id"].is_null());
    assert!(
        list_foreground(&root, uuid::Uuid::new_v4(), None).unwrap()["agents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(save_report(&root, uuid::Uuid::from_u128(1), "replacement report").is_err());
    assert_eq!(
        load_report(&root, uuid::Uuid::from_u128(1))
            .unwrap()
            .as_deref(),
        Some("verified report")
    );
    std::fs::remove_dir_all(root).unwrap();
}

async fn settled(registry: &BackgroundTaskRegistry, id: uuid::Uuid) -> BackgroundTaskStatus {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(task) = registry.snapshots().into_iter().find(|task| {
                task.agent_id == Some(id) && task.status != BackgroundTaskStatus::Running
            }) {
                return task.status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn recreated_catalog_resumes_original_worktree_and_completed_write() {
    let root = std::env::temp_dir().join(format!("dispatch-recovery-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("repo");
    let home = root.join("workers");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    git(&workspace, &["init", "--quiet"]);
    git(
        &workspace,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let parent = uuid::Uuid::new_v4();
    let (started, observed) = tokio::sync::oneshot::channel();
    let provider = Arc::new(RecoveryProvider {
        calls: AtomicUsize::new(0),
        started: Mutex::new(Some(started)),
        resumed: Mutex::new(Vec::new()),
    });
    let make = |registry| {
        SubagentCatalog::new(&workspace, builtin_profiles(provider.clone()), registry)
            .with_state_home(&home)
            .with_parent_session(parent)
            .with_worktree_root(root.join("worktrees"))
    };
    let original_registry = Arc::new(BackgroundTaskRegistry::default());
    let original = make(original_registry.clone());
    original
        .run(
            SpawnAgentArgs {
                profile: Some("implementer".into()),
                prompt: "Create new.rs once and verify".into(),
                run_in_background: Some(true),
                task: Some(TaskPacket {
                    goal: "create new.rs".into(),
                    write_files: vec!["new.rs".into()],
                    constraints: vec!["Do not replay completed writes".into()],
                    verifier: Some(TaskVerifier {
                        command: "true".into(),
                        expected_exit_code: Some(0),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(BTreeSet::from([workspace.join("new.rs")])),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), observed)
        .await
        .unwrap()
        .unwrap();
    let id = original_registry.snapshots()[0].agent_id.unwrap();
    let record = load(&home, id).unwrap().unwrap();
    assert!(record.prepared.dedicated);
    let packet = record.args.task.as_ref().unwrap();
    assert_eq!(packet.write_files, vec!["new.rs"]);
    assert_eq!(packet.constraints, vec!["Do not replay completed writes"]);
    assert_eq!(packet.verifier.as_ref().unwrap().command, "true");
    assert_eq!(
        std::fs::read_to_string(record.prepared.workspace.join("new.rs")).unwrap(),
        "once"
    );
    assert!(!workspace.join("new.rs").exists());
    assert!(original_registry.kill_agent(id));
    assert_eq!(
        settled(&original_registry, id).await,
        BackgroundTaskStatus::Killed
    );
    drop(original);
    drop(original_registry);
    let fresh_registry = Arc::new(BackgroundTaskRegistry::default());
    let fresh = make(fresh_registry.clone());
    assert!(
        fresh
            .retry_background_agent(id, None)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        settled(&fresh_registry, id).await,
        BackgroundTaskStatus::Completed
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    {
        let messages = provider.resumed.lock().unwrap();
        assert_eq!(
            messages
                .iter()
                .flat_map(|message| &message.tool_calls)
                .filter(|call| call.id == "write-once")
                .count(),
            1
        );
        assert!(messages.iter().any(|message| message.role == Role::Tool
            && message.tool_call_id.as_deref() == Some("write-once")));
    }
    assert_eq!(
        load(&home, id).unwrap().unwrap().prepared.workspace,
        record.prepared.workspace
    );
    let wrong_parent =
        make(Arc::new(BackgroundTaskRegistry::default())).with_parent_session(uuid::Uuid::new_v4());
    assert!(wrong_parent.retry_background_agent(id, None).await.is_err());
    let mut changed = record.clone();
    changed.args.prompt = "replacement task".into();
    assert!(save(&home, &changed).is_err());
    assert_eq!(
        load(&home, id).unwrap().unwrap().args.prompt,
        record.args.prompt
    );
    let mut wrong_branch = record.prepared.clone();
    wrong_branch.branch = Some("different-branch".into());
    assert!(validate_workspace(&wrong_branch, &workspace).await.is_err());
    assert!(validate_workspace(&record.prepared, &root).await.is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(home.join("dispatches").join(format!("{id}.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
