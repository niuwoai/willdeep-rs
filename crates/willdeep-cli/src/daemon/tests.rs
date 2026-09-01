use super::*;
use willdeep_core::EventSink;

fn test_agent_store(root: &Path) -> Arc<AgentStore> {
    Arc::new(AgentStore::open(root.join("agents.json")).unwrap())
}

fn test_runtime_session_store(root: &Path) -> Arc<session_store::RuntimeSessionStore> {
    Arc::new(
        session_store::RuntimeSessionStore::open(root.join("runtime-sessions.json"), root).unwrap(),
    )
}

fn test_turn_scheduler() -> tokio::sync::mpsc::UnboundedSender<uuid::Uuid> {
    tokio::sync::mpsc::unbounded_channel().0
}

fn initialize_git_workspace(root: &Path) {
    let status = Command::new("git")
        .args(["init"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn state_round_trips_without_exposing_token_in_logs() {
    let root = std::env::temp_dir().join(format!("willdeep-daemon-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.json");
    let state = DaemonState {
        schema: STATE_SCHEMA,
        version: "1.2.3".to_owned(),
        pid: 42,
        address: "127.0.0.1:9847".parse().unwrap(),
        token: "private-token".to_owned(),
        started_at: 10,
        local_transport: Some(LocalTransportState::UnixSocket {
            path: root.join("control.sock"),
        }),
    };
    write_state(&path, &state).unwrap();
    assert_eq!(load_state(&path).unwrap(), state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_state_without_local_transport_remains_readable() {
    let state: DaemonState = serde_json::from_value(serde_json::json!({
        "schema": STATE_SCHEMA,
        "version": "0.20.0",
        "pid": 42,
        "address": "127.0.0.1:9847",
        "token": "private-token",
        "started_at": 10
    }))
    .unwrap();
    assert_eq!(state.local_transport, None);
}

#[test]
fn completed_runtime_tasks_leave_recent_attention_after_five_minutes() {
    let task = |completed_at| willdeep_runtime_protocol::RuntimeTask {
        id: uuid::Uuid::new_v4(),
        session_id: None,
        turn_id: None,
        agent_id: None,
        event_start_sequence: 0,
        status: willdeep_runtime_protocol::TaskStatus::Completed,
        workspace: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        profile: None,
        prompt_excerpt: None,
        created_at: 1,
        started_at: Some(10),
        completed_at: Some(completed_at),
        exit_code: Some(0),
        failure_domain: None,
    };
    assert!(tui_bridge::runtime_task_visible(&task(700), 1_000));
    assert!(!tui_bridge::runtime_task_visible(&task(699), 1_000));
}

/// 摘要进公共 DTO 前的三道处理各自都要能单独兜住：凭据打码（用户会把
/// token 粘进提示词）、空白压平、按字符截断（多字节中间切一刀是 panic）。
#[test]
fn task_prompt_excerpts_are_redacted_collapsed_and_char_truncated() {
    assert_eq!(task_prompt_excerpt("   \n\t "), None);
    assert_eq!(
        task_prompt_excerpt("deploy   with\ntoken=abc123secret").as_deref(),
        Some("deploy with token=[REDACTED]")
    );

    let exact = "a".repeat(TASK_PROMPT_EXCERPT_MAX_CHARS);
    assert_eq!(task_prompt_excerpt(&exact).as_deref(), Some(exact.as_str()));

    let long = "汉".repeat(TASK_PROMPT_EXCERPT_MAX_CHARS + 10);
    let excerpt = task_prompt_excerpt(&long).expect("non-empty prompt yields an excerpt");
    assert_eq!(excerpt.chars().count(), TASK_PROMPT_EXCERPT_MAX_CHARS + 1);
    assert!(excerpt.ends_with('…'));
}

#[test]
fn submitted_workspace_policy_cannot_be_supplied_by_client_json() {
    let request: SubmitTask = serde_json::from_value(serde_json::json!({
        "prompt": "inspect",
        "attachments": [],
        "workspace": std::env::temp_dir(),
        "workspace_access": "workspace_write",
        "workspace_skills": ["untrusted"],
        "workspace_mcp_servers": ["untrusted"],
        "profile": null,
        "model": null,
        "config": null,
        "session_id": null,
        "turn_id": null
    }))
    .unwrap();
    assert_eq!(request.workspace_access, None);
    assert_eq!(request.workspace_skills, None);
    assert_eq!(request.workspace_mcp_servers, None);
}

#[tokio::test]
async fn runtime_sink_attributes_child_agent_file_changes_without_chat_metadata() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-runtime-attribution-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    initialize_git_workspace(&workspace);
    let task_id = uuid::Uuid::new_v4();
    let session_id = uuid::Uuid::new_v4();
    let turn_id = uuid::Uuid::new_v4();
    let root_agent_id = uuid::Uuid::new_v4();
    let child_agent_id = uuid::Uuid::new_v4();
    let child_workspace = root.join("child-workspace");
    std::fs::create_dir_all(&child_workspace).unwrap();
    initialize_git_workspace(&child_workspace);
    let tools = Arc::new(tool_store::ToolStore::open(root.join("runtime/tools.json")).unwrap());
    let sink = RuntimeEventSink {
        task_id,
        session_id: Some(session_id),
        turn_id: Some(turn_id),
        root_agent_id,
        home: root.clone(),
        workspace: workspace.clone(),
        events: Arc::new(EventLog::open(root.join("events.ndjson")).unwrap()),
        agents: test_agent_store(&root),
        tools: tools.clone(),
        diff_baselines: AsyncMutex::new(HashMap::new()),
        child_workspaces: AsyncMutex::new(HashMap::new()),
    };

    sink.emit(willdeep_core::AgentEvent::SubagentStarted {
        id: child_agent_id,
        profile: "editor".to_owned(),
        model: Some("editor-model".to_owned()),
        label: "isolated edit".to_owned(),
        background: true,
        max_turns: 6,
        token_budget: None,
        timeout_seconds: Some(300),
        workspace: child_workspace.clone(),
        root_workspace: workspace.clone(),
        worktree_branch: Some("willdeep/agent-test".to_owned()),
        dedicated_worktree: true,
    })
    .await;
    sink.emit(willdeep_core::AgentEvent::SubagentToolRequested {
        id: child_agent_id,
        name: "edit_file".to_owned(),
    })
    .await;
    std::fs::write(child_workspace.join("child.txt"), "child change\n").unwrap();
    sink.emit(willdeep_core::AgentEvent::SubagentToolCompleted {
        id: child_agent_id,
        name: "edit_file".to_owned(),
        is_error: false,
    })
    .await;
    let root_call = willdeep_core::ToolCall {
        id: "root-write".to_owned(),
        name: "create_file".to_owned(),
        arguments: "{}".to_owned(),
    };
    sink.emit(willdeep_core::AgentEvent::ToolRequested(root_call.clone()))
        .await;
    std::fs::write(workspace.join("root.txt"), "root change\n").unwrap();
    sink.emit(willdeep_core::AgentEvent::ToolCompleted {
        call: root_call,
        output: "created".to_owned(),
        is_error: false,
    })
    .await;
    sink.emit(willdeep_core::AgentEvent::BackgroundShellStarted {
        id: "job_exact".to_owned(),
    })
    .await;
    sink.emit(willdeep_core::AgentEvent::BackgroundShellCompleted {
        id: "job_exact".to_owned(),
        status: willdeep_core::BackgroundTaskStatus::Completed,
        exit_code: Some(0),
        elapsed_millis: 125,
        output_bytes: 42,
    })
    .await;

    let records: Vec<diff_review::DiffAttributionRecord> = serde_json::from_slice(
        &std::fs::read(root.join("runtime/diff-attributions.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].agent_id, child_agent_id);
    assert_eq!(records[0].session_id, Some(session_id));
    assert_eq!(records[0].turn_id, Some(turn_id));
    assert_eq!(records[0].paths, vec!["child.txt"]);
    assert_eq!(records[1].agent_id, root_agent_id);
    assert_eq!(records[1].paths, vec!["root.txt"]);
    let tool_records = tools
        .list(willdeep_runtime_protocol::ListToolsParams {
            task_id: Some(task_id),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(tool_records.len(), 3);
    assert!(tool_records.iter().all(|tool| {
        tool.status == willdeep_runtime_protocol::ToolStatus::Completed
            && tool.session_id == Some(session_id)
            && tool.turn_id == Some(turn_id)
            && tool.task_id == task_id
    }));
    let background = tool_records
        .iter()
        .find(|tool| tool.name == "background_shell:job_exact")
        .expect("persisted background Shell resource");
    assert_eq!(background.agent_id, root_agent_id);
    let persisted_tools: Vec<willdeep_runtime_protocol::RuntimeTool> =
        serde_json::from_slice(&std::fs::read(root.join("runtime/tools.json")).unwrap()).unwrap();
    assert!(persisted_tools.iter().any(|tool| {
        tool.name == "background_shell:job_exact"
            && tool.status == willdeep_runtime_protocol::ToolStatus::Completed
            && tool.session_id == Some(session_id)
            && tool.turn_id == Some(turn_id)
            && tool.task_id == task_id
            && tool.agent_id == root_agent_id
    }));
    let events = std::fs::read_to_string(root.join("events.ndjson")).unwrap();
    assert!(events.contains("background_shell_started"));
    assert!(events.contains("background_shell_completed"));
    assert!(!events.contains("private command"));
    assert!(!events.contains("private output"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn shutdown_signal_is_observed_by_late_and_parallel_listeners() {
    let (shutdown, receiver) = watch::channel(false);
    shutdown.send(true).unwrap();
    let first = receiver.clone();
    let second = receiver;

    tokio::time::timeout(Duration::from_millis(100), async move {
        tokio::join!(wait_for_shutdown(first), wait_for_shutdown(second));
    })
    .await
    .expect("all Runtime listeners must observe an already-issued shutdown");
}

#[tokio::test]
async fn authorization_requires_exact_local_token() {
    let root = std::env::temp_dir().join(format!("willdeep-daemon-auth-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let state = Arc::new(ServerState {
        home: root.clone(),
        token: "expected".to_owned(),
        started_at: 0,
        shutdown: watch::channel(false).0,
        events: events.clone(),
        tasks: Arc::new(
            TaskManager::open(TaskManagerOptions {
                path: root.join("tasks.json"),
                interactions_path: root.join("interactions.json"),
                home: root.clone(),
                events,
                agents: agents.clone(),
                sessions: test_runtime_session_store(&root),
                turn_scheduler: test_turn_scheduler(),
                runtime_url: "http://127.0.0.1:1".to_owned(),
                runtime_token: "test-token".to_owned(),
            })
            .unwrap(),
        ),
        agents,
        agent_commands: Arc::new(
            AgentCommandStore::open(root.join("agent-commands.json")).unwrap(),
        ),
        sessions: Arc::new(
            session_store::RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root)
                .unwrap(),
        ),
        workspaces: Arc::new(
            workspace_store::WorkspaceStore::open(root.join("workspaces.json")).unwrap(),
        ),
        diff_review_lock: Arc::new(tokio::sync::Mutex::new(())),
        idempotency: Arc::new(control_api::IdempotencyStore::default()),
        local_transport: Some(LocalTransportState::UnixSocket {
            path: root.join("control.sock"),
        }),
        tools: Arc::new(tool_store::ToolStore::open(root.join("tools.json")).unwrap()),
        work_gate: Arc::new(RwLock::new(false)),
    });
    assert!(
        runtime_capabilities(&state)
            .transports
            .contains(&willdeep_runtime_protocol::TransportKind::UnixSocket)
    );
    assert_eq!(
        authorize(&state, &HeaderMap::new()),
        Err(StatusCode::UNAUTHORIZED)
    );
    let mut headers = HeaderMap::new();
    headers.insert(TOKEN_HEADER, HeaderValue::from_static("expected"));
    assert_eq!(authorize(&state, &headers), Ok(()));
    assert_eq!(
        authorize_internal(&state, &headers),
        Err(StatusCode::NOT_FOUND)
    );
    headers.insert(internal_transport::HEADER, HeaderValue::from_static("1"));
    assert_eq!(authorize_internal(&state, &headers), Ok(()));
    assert_eq!(
        capabilities_handler(State(state.clone()), HeaderMap::new())
            .await
            .unwrap_err(),
        StatusCode::UNAUTHORIZED
    );
    let capabilities = capabilities_handler(State(state.clone()), headers.clone())
        .await
        .unwrap();
    assert_eq!(capabilities.status(), StatusCode::OK);
    assert_eq!(
        worktree_review::review_handler(
            State(state.clone()),
            HeaderMap::new(),
            AxumPath(uuid::Uuid::new_v4()),
        )
        .await
        .unwrap_err(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        worktree_maintenance::audit_handler(State(state), HeaderMap::new())
            .await
            .unwrap_err(),
        StatusCode::UNAUTHORIZED
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn event_log_assigns_sequences_and_resumes_after_cursor() {
    let root = std::env::temp_dir().join(format!("willdeep-events-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("events.ndjson");
    let log = EventLog::open(path.clone()).unwrap();
    assert_eq!(log.append("first", "one").unwrap().sequence, 1);
    assert_eq!(log.append("second", "two").unwrap().sequence, 2);
    assert_eq!(log.read_after(1, 10).unwrap()[0].kind, "second");
    drop(log);

    let reopened = EventLog::open(path).unwrap();
    assert_eq!(reopened.append("third", "three").unwrap().sequence, 3);
    assert_eq!(reopened.read_after(0, 2).unwrap().len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_store_persists_structured_harness_lifecycle() {
    let root = std::env::temp_dir().join(format!("willdeep-agent-store-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("agents.json");
    let task_id = uuid::Uuid::new_v4();
    let store = AgentStore::open(path.clone()).unwrap();
    let agent = store
        .ensure_root(
            task_id,
            root.clone(),
            Some("editor".to_owned()),
            Some("root-model".to_owned()),
            RuntimeAgentStatus::Queued,
        )
        .unwrap();
    store
        .apply_harness_event(task_id, r#"{"type":"turn_started","turn":2}"#)
        .unwrap();
    store
        .apply_harness_event(task_id, r#"{"type":"tool_requested","name":"read_file"}"#)
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            r#"{"type":"usage","input_tokens":10,"output_tokens":4,"total_tokens":14}"#,
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            r#"{"type":"usage","input_tokens":5,"output_tokens":1,"total_tokens":6}"#,
        )
        .unwrap();
    let child_id = uuid::Uuid::new_v4();
    let child_event = |kind: &str, extra: serde_json::Value| {
        let mut value = serde_json::json!({"type": kind, "id": child_id});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value.to_string()
    };
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_started",
                serde_json::json!({
                    "profile": "scout",
                    "model": "scout-model",
                    "label": "inspect files",
                    "background": true,
                    "workspace": root.join("child-worktree"),
                    "root_workspace": root,
                    "worktree_branch": "willdeep/agent-test",
                    "dedicated_worktree": true
                }),
            ),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event("subagent_turn_started", serde_json::json!({"turn": 2})),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_usage",
                serde_json::json!({
                    "input_tokens": 7,
                    "output_tokens": 2,
                    "total_tokens": 9
                }),
            ),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_verdict",
                serde_json::json!({
                    "repo_commit": "0123456789abcdef0123456789abcdef01234567",
                    "verifier_command": "cargo test -p willdeep-core",
                    "verifier_passed": true,
                    "attempts": 2
                }),
            ),
        )
        .unwrap();
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_completed",
                serde_json::json!({"status": "completed"}),
            ),
        )
        .unwrap();
    let completed_child = store.get(child_id).unwrap().unwrap();
    assert_eq!(completed_child.status, RuntimeAgentStatus::Completed);
    // What the run proved has to survive to the persisted record: the verdict
    // is the only thing that separates a verified pass from a confident report.
    assert_eq!(completed_child.verifier_passed, Some(true));
    assert_eq!(completed_child.attempts, Some(2));
    assert_eq!(
        completed_child.verifier_command.as_deref(),
        Some("cargo test -p willdeep-core")
    );
    assert_eq!(
        completed_child.repo_commit.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(completed_child.model.as_deref(), Some("scout-model"));
    assert_eq!(completed_child.current_turn, 2);
    assert_eq!(completed_child.total_tokens, Some(9));
    assert!(completed_child.dedicated_worktree);
    assert_eq!(
        completed_child.worktree_branch.as_deref(),
        Some("willdeep/agent-test")
    );

    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_started",
                serde_json::json!({
                    "profile": "scout",
                    "label": "inspect files retry",
                    "background": true
                }),
            ),
        )
        .unwrap();
    let retried_child = store.get(child_id).unwrap().unwrap();
    assert_eq!(retried_child.status, RuntimeAgentStatus::Running);
    assert_eq!(retried_child.current_turn, 0);
    assert_eq!(retried_child.total_tokens, Some(9));
    // A rerun starts unproved. Carrying the previous verdict forward would
    // let a retry inherit a pass it has not earned yet.
    assert_eq!(retried_child.verifier_passed, None);
    assert_eq!(retried_child.attempts, None);
    assert_eq!(retried_child.repo_commit, None);
    store
        .apply_harness_event(
            task_id,
            &child_event(
                "subagent_completed",
                serde_json::json!({"status": "cancelled"}),
            ),
        )
        .unwrap();
    store
        .set_status_for_task(task_id, RuntimeAgentStatus::Completed, None)
        .unwrap();
    let next_task_id = uuid::Uuid::new_v4();
    let continued_root = store
        .ensure_session_root(
            agent.id,
            next_task_id,
            root.clone(),
            Some("mock".to_owned()),
            Some("continued-model".to_owned()),
            RuntimeAgentStatus::Queued,
        )
        .unwrap();
    assert_eq!(continued_root.input_tokens, Some(15));
    assert_eq!(continued_root.output_tokens, Some(5));
    assert_eq!(continued_root.total_tokens, Some(20));
    assert_eq!(continued_root.model.as_deref(), Some("continued-model"));
    store
        .set_status_for_task(next_task_id, RuntimeAgentStatus::Completed, None)
        .unwrap();
    drop(store);

    let restored = AgentStore::open(path)
        .unwrap()
        .get(agent.id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, RuntimeAgentStatus::Completed);
    assert_eq!(restored.current_turn, 0);
    assert_eq!(restored.current_tool, None);
    assert_eq!(restored.input_tokens, Some(15));
    assert_eq!(restored.output_tokens, Some(5));
    assert_eq!(restored.total_tokens, Some(20));
    assert!(restored.completed_at.is_some());
    let child = AgentStore::open(root.join("agents.json"))
        .unwrap()
        .get(child_id)
        .unwrap()
        .unwrap();
    assert_eq!(child.parent_id, Some(agent.id));
    assert_eq!(child.profile.as_deref(), Some("scout"));
    assert!(child.background);
    assert_eq!(child.status, RuntimeAgentStatus::Cancelled);
    assert_eq!(child.current_turn, 0);
    assert_eq!(child.total_tokens, Some(9));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn restart_recovery_closes_child_agent_tool_and_command_resources_once() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-execution-resource-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    let child_worktree = root.join("child-worktree");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&child_worktree).unwrap();
    let task_id = uuid::Uuid::new_v4();
    let child_id = uuid::Uuid::new_v4();
    let reserved_spawn_id = uuid::Uuid::new_v4();
    let agents_path = root.join("agents.json");
    let commands_path = root.join("agent-commands.json");
    let tools_path = root.join("tools.json");

    let agents = AgentStore::open(agents_path.clone()).unwrap();
    let root_agent = agents
        .ensure_root(
            task_id,
            workspace.clone(),
            None,
            None,
            RuntimeAgentStatus::Running,
        )
        .unwrap();
    agents
        .apply_harness_event(
            task_id,
            &serde_json::json!({
                "type": "subagent_started",
                "id": child_id,
                "profile": "editor",
                "background": true,
                "workspace": child_worktree.clone(),
                "root_workspace": workspace.clone(),
                "worktree_branch": "willdeep/recovery-test",
                "dedicated_worktree": true
            })
            .to_string(),
        )
        .unwrap();
    agents
        .reserve_external_child(
            reserved_spawn_id,
            root_agent.id,
            task_id,
            "scout".to_owned(),
            Some("pending scout".to_owned()),
        )
        .unwrap();
    drop(agents);

    let commands = AgentCommandStore::open(commands_path.clone()).unwrap();
    commands
        .enqueue(
            task_id,
            child_id,
            agent_control::AgentCommandKind::Stop,
            None,
            None,
        )
        .unwrap();
    commands
        .enqueue_spawn(
            task_id,
            reserved_spawn_id,
            "inspect the workspace".to_owned(),
            Some("scout".to_owned()),
            Some("pending scout".to_owned()),
        )
        .unwrap();
    drop(commands);

    let tools = tool_store::ToolStore::open(tools_path.clone()).unwrap();
    let tool = tools
        .start(tool_store::StartTool {
            session_id: None,
            turn_id: None,
            task_id,
            agent_id: child_id,
            correlation: format!("child:{child_id}"),
            name: "edit_file".to_owned(),
        })
        .unwrap();
    let background_tool = tools
        .start(tool_store::StartTool {
            session_id: None,
            turn_id: None,
            task_id,
            agent_id: root_agent.id,
            correlation: "background:job_recover".to_owned(),
            name: "background_shell:job_recover".to_owned(),
        })
        .unwrap();
    drop(tools);

    let agents = AgentStore::open(agents_path).unwrap();
    let commands = AgentCommandStore::open(commands_path).unwrap();
    let tools = tool_store::ToolStore::open(tools_path).unwrap();
    let events = EventLog::open(root.join("events.ndjson")).unwrap();
    report_execution_resource_recovery(&events, &agents, &commands, &tools).unwrap();

    let child = agents.get(child_id).unwrap().unwrap();
    assert_eq!(child.status, RuntimeAgentStatus::Interrupted);
    assert_eq!(
        child.worktree_branch.as_deref(),
        Some("willdeep/recovery-test")
    );
    assert!(child.dedicated_worktree);
    assert_eq!(child.workspace, child_worktree);
    let rejected_spawn = agents.get(reserved_spawn_id).unwrap().unwrap();
    assert_eq!(rejected_spawn.status, RuntimeAgentStatus::Failed);
    assert!(
        rejected_spawn
            .error
            .as_deref()
            .is_some_and(|error| error.contains("before external Agent spawn was applied"))
    );
    assert_eq!(
        tools.get(tool.id).unwrap().unwrap().status,
        willdeep_runtime_protocol::ToolStatus::Interrupted
    );
    assert_eq!(
        tools.get(background_tool.id).unwrap().unwrap().status,
        willdeep_runtime_protocol::ToolStatus::Interrupted
    );

    let recovered_events = events.read_after(0, 20).unwrap();
    assert_eq!(
        recovered_events
            .iter()
            .filter(|event| event.kind == "agent.command_rejected")
            .count(),
        2
    );
    assert_eq!(
        recovered_events
            .iter()
            .filter(|event| event.kind == "agent.spawn_rejected")
            .count(),
        1
    );
    assert_eq!(
        recovered_events
            .iter()
            .filter(|event| event.kind == "agent.interrupted")
            .count(),
        1
    );
    assert_eq!(
        recovered_events
            .iter()
            .filter(|event| event.kind == "tool.interrupted")
            .count(),
        2
    );
    assert!(recovered_events.iter().any(|event| {
        event.kind == "tool.interrupted"
            && event
                .message
                .contains(&format!("tool_id={}", background_tool.id))
            && event
                .message
                .contains(&format!("agent_id={}", root_agent.id))
    }));
    assert!(recovered_events.iter().all(|event| {
        !event.message.contains(root.to_string_lossy().as_ref())
            && !event.message.contains("inspect the workspace")
    }));

    report_execution_resource_recovery(&events, &agents, &commands, &tools).unwrap();
    assert_eq!(
        events.read_after(0, 20).unwrap().len(),
        recovered_events.len()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_store_marks_active_tasks_interrupted_after_restart() {
    let root = std::env::temp_dir().join(format!("willdeep-tasks-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tasks.json");
    let id = uuid::Uuid::new_v4();
    let task = RuntimeTask {
        id,
        session_id: None,
        turn_id: None,
        agent_id: None,
        event_start_sequence: 0,
        status: RuntimeTaskStatus::Running,
        workspace: root.clone(),
        profile: None,
        model: None,
        prompt_excerpt: None,
        pid: Some(10),
        created_at: 1,
        started_at: Some(2),
        completed_at: None,
        exit_code: None,
        failure_domain: None,
        error: None,
    };
    persist_tasks(&path, &HashMap::from([(id, task)])).unwrap();
    let agents = test_agent_store(&root);
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path,
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: events.clone(),
        agents,
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let recovered = manager.tasks.blocking_read();
    assert_eq!(recovered[&id].status, RuntimeTaskStatus::Interrupted);
    assert_eq!(recovered[&id].pid, None);
    assert!(recovered[&id].completed_at.is_some());
    drop(recovered);
    let recovered_events = events.read_after(0, 10).unwrap();
    assert_eq!(recovered_events.len(), 1);
    assert_eq!(recovered_events[0].kind, "task.interrupted");
    assert!(
        recovered_events[0]
            .message
            .contains(&format!("task_id={id}"))
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_recovery_survives_dangling_session_reference() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-dangling-session-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tasks.json");
    let id = uuid::Uuid::new_v4();
    let task = RuntimeTask {
        id,
        session_id: Some(uuid::Uuid::new_v4()),
        turn_id: None,
        agent_id: None,
        event_start_sequence: 0,
        status: RuntimeTaskStatus::Running,
        workspace: root.clone(),
        profile: None,
        model: None,
        prompt_excerpt: None,
        pid: Some(10),
        created_at: 1,
        started_at: Some(2),
        completed_at: None,
        exit_code: None,
        failure_domain: None,
        error: None,
    };
    persist_tasks(&path, &HashMap::from([(id, task)])).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path,
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events,
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let recovered = manager.tasks.blocking_read();
    assert_eq!(recovered[&id].status, RuntimeTaskStatus::Interrupted);
    assert!(recovered[&id].agent_id.is_some());
    drop(recovered);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_recovery_interrupts_waiting_task_and_cancels_its_interaction() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-waiting-task-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let task_id = uuid::Uuid::new_v4();
    let interaction_id = uuid::Uuid::new_v4();
    let tasks_path = root.join("tasks.json");
    let interactions_path = root.join("interactions.json");
    persist_tasks(
        &tasks_path,
        &HashMap::from([(
            task_id,
            RuntimeTask {
                id: task_id,
                session_id: None,
                turn_id: None,
                agent_id: None,
                event_start_sequence: 0,
                status: RuntimeTaskStatus::WaitingApproval,
                workspace: root.clone(),
                profile: None,
                model: None,
                prompt_excerpt: None,
                pid: None,
                created_at: 1,
                started_at: Some(2),
                completed_at: None,
                exit_code: None,
                failure_domain: None,
                error: None,
            },
        )]),
    )
    .unwrap();
    persist_interactions(
        &interactions_path,
        &HashMap::from([(
            interaction_id,
            RuntimeInteraction {
                id: interaction_id,
                task_id,
                kind: InteractionKind::Approval {
                    description: "test approval".to_owned(),
                    always_allow_available: true,
                },
                status: InteractionStatus::Pending,
                resolution: None,
                created_at: 3,
                resolved_at: None,
            },
        )]),
    )
    .unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path: tasks_path,
        interactions_path: interactions_path.clone(),
        home: root.clone(),
        events: events.clone(),
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();

    assert_eq!(
        manager.tasks.blocking_read()[&task_id].status,
        RuntimeTaskStatus::Interrupted
    );
    let interactions = load_interactions(&interactions_path).unwrap();
    assert_eq!(
        interactions[&interaction_id].status,
        InteractionStatus::Cancelled
    );
    let kinds = events
        .read_after(0, 10)
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["task.interrupted", "task.interaction_cancelled"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn task_recovery_preserves_the_session_root_agent_id() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-session-task-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions = test_runtime_session_store(&root);
    let session = sessions
        .create(session_store::CreateRuntimeSession {
            id: None,
            workspace: workspace.clone(),
            profile: None,
            model: None,
            config: None,
            title: None,
        })
        .unwrap();
    let task_id = uuid::Uuid::new_v4();
    let turn_id = uuid::Uuid::new_v4();
    let path = root.join("tasks.json");
    let task = RuntimeTask {
        id: task_id,
        session_id: Some(session.id),
        turn_id: Some(turn_id),
        agent_id: Some(session.root_agent_id),
        event_start_sequence: 0,
        status: RuntimeTaskStatus::Running,
        workspace,
        profile: None,
        model: Some("restored-model".to_owned()),
        prompt_excerpt: None,
        pid: Some(10),
        created_at: 1,
        started_at: Some(2),
        completed_at: None,
        exit_code: None,
        failure_domain: None,
        error: None,
    };
    persist_tasks(&path, &HashMap::from([(task_id, task)])).unwrap();
    let agents = test_agent_store(&root);
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let manager = TaskManager::open(TaskManagerOptions {
        path,
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: events.clone(),
        agents: agents.clone(),
        sessions,
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();

    assert_eq!(
        manager.tasks.blocking_read()[&task_id].agent_id,
        Some(session.root_agent_id)
    );
    assert_eq!(agents.list().unwrap().len(), 1);
    assert_eq!(agents.list().unwrap()[0].id, session.root_agent_id);
    assert_eq!(
        agents.list().unwrap()[0].model.as_deref(),
        Some("restored-model")
    );
    assert_eq!(
        events
            .read_after(0, 10)
            .unwrap()
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec!["task.interrupted", "turn.interrupted"]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_lock_is_exclusive_and_owned_cleanup_is_safe() {
    let root = std::env::temp_dir().join(format!("willdeep-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    let first = acquire_daemon_lock(&path).unwrap();
    assert!(acquire_daemon_lock(&path).is_err());
    remove_owned_lock(&path, "not-the-owner");
    assert!(path.exists());
    remove_owned_lock(&path, &first.token);
    assert!(acquire_daemon_lock(&path).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_lock_recovers_after_stale_lease() {
    let root = std::env::temp_dir().join(format!("willdeep-stale-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    write_json_atomic(
        &path,
        &DaemonLock {
            token: "stale".to_owned(),
            created_at: 0,
        },
    )
    .unwrap();
    assert_ne!(acquire_daemon_lock(&path).unwrap().token, "stale");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_lock_heartbeat_refreshes_only_the_owner_lease() {
    let root =
        std::env::temp_dir().join(format!("willdeep-lock-heartbeat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("daemon.lock");
    let lock = acquire_daemon_lock(&path).unwrap();
    assert!(refresh_daemon_lock(&path, "not-the-owner").is_err());
    refresh_daemon_lock(&path, &lock.token).unwrap();
    let refreshed = load_daemon_lock(&path).unwrap();
    assert_eq!(refreshed.token, lock.token);
    assert!(refreshed.created_at >= lock.created_at);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn concurrent_task_updates_persist_a_complete_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-concurrent-tasks-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("tasks.json");
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let manager = Arc::new(
        TaskManager::open(TaskManagerOptions {
            path: path.clone(),
            interactions_path: root.join("interactions.json"),
            home: root.clone(),
            events,
            agents,
            sessions: test_runtime_session_store(&root),
            turn_scheduler: test_turn_scheduler(),
            runtime_url: "http://127.0.0.1:1".to_owned(),
            runtime_token: "test-token".to_owned(),
        })
        .unwrap(),
    );
    let mut updates = Vec::new();
    for index in 0..20 {
        let manager = manager.clone();
        let workspace = root.clone();
        updates.push(tokio::spawn(async move {
            let id = uuid::Uuid::new_v4();
            manager
                .insert_and_persist(RuntimeTask {
                    id,
                    session_id: None,
                    turn_id: None,
                    agent_id: None,
                    event_start_sequence: 0,
                    status: RuntimeTaskStatus::Completed,
                    workspace,
                    profile: None,
                    model: None,
                    prompt_excerpt: None,
                    pid: None,
                    created_at: index,
                    started_at: Some(index),
                    completed_at: Some(index),
                    exit_code: Some(0),
                    failure_domain: None,
                    error: None,
                })
                .await
                .unwrap();
        }));
    }
    for update in updates {
        update.await.unwrap();
    }
    assert_eq!(load_tasks(&path).unwrap().len(), 20);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn drain_wait_ignores_stale_cancellation_for_terminal_task() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-drain-stale-cancellation-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let manager = TaskManager::open(TaskManagerOptions {
        path: root.join("tasks.json"),
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: Arc::new(EventLog::open(root.join("events.ndjson")).unwrap()),
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let task_id = uuid::Uuid::new_v4();
    manager
        .insert_and_persist(RuntimeTask {
            id: task_id,
            session_id: None,
            turn_id: None,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Completed,
            workspace: root.clone(),
            profile: None,
            model: None,
            prompt_excerpt: None,
            pid: None,
            created_at: 1,
            started_at: Some(1),
            completed_at: Some(2),
            exit_code: Some(0),
            failure_domain: None,
            error: None,
        })
        .await
        .unwrap();
    manager
        .cancellations
        .lock()
        .unwrap()
        .insert(task_id, Arc::new(Notify::new()));

    tokio::time::timeout(Duration::from_millis(100), manager.wait_until_idle())
        .await
        .expect("terminal tasks must not keep Runtime draining");

    std::fs::remove_dir_all(root).unwrap();
}

/// 一个等审批的任务曾经能把 `daemon upgrade` 永久钉死：drain 等的是「未终态」，
/// 而等人的任务只有等到人来才会离开那个状态。真实后果是 300 秒超时之后
/// Runtime 停在 draining，既不接新活也升不了级。
#[tokio::test]
async fn drain_does_not_wait_on_tasks_that_are_waiting_on_a_human() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-drain-awaiting-human-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let manager = TaskManager::open(TaskManagerOptions {
        path: root.join("tasks.json"),
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: Arc::new(EventLog::open(root.join("events.ndjson")).unwrap()),
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let mut waiting_ids = Vec::new();
    for status in [
        RuntimeTaskStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer,
    ] {
        let task_id = uuid::Uuid::new_v4();
        waiting_ids.push(task_id);
        manager
            .insert_and_persist(RuntimeTask {
                id: task_id,
                session_id: None,
                turn_id: None,
                agent_id: None,
                event_start_sequence: 0,
                status,
                workspace: root.clone(),
                profile: None,
                model: None,
                prompt_excerpt: None,
                pid: None,
                created_at: 1,
                started_at: Some(1),
                completed_at: None,
                exit_code: None,
                failure_domain: None,
                error: None,
            })
            .await
            .unwrap();
    }

    tokio::time::timeout(Duration::from_millis(200), manager.wait_until_idle())
        .await
        .expect("tasks waiting on a human must not pin the Runtime in draining");

    // 不拦 drain，但必须点得出来——交接会把它们带走，静默丢弃是不能接受的。
    waiting_ids.sort();
    assert_eq!(manager.tasks_awaiting_a_human().await, waiting_ids);

    std::fs::remove_dir_all(root).unwrap();
}

/// 反过来，真正在跑的活还是要等——drain 的本意是别把它们腰斩。
#[tokio::test]
async fn drain_still_waits_for_work_that_finishes_on_its_own() {
    let root =
        std::env::temp_dir().join(format!("willdeep-drain-running-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let manager = TaskManager::open(TaskManagerOptions {
        path: root.join("tasks.json"),
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events: Arc::new(EventLog::open(root.join("events.ndjson")).unwrap()),
        agents: test_agent_store(&root),
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    manager
        .insert_and_persist(RuntimeTask {
            id: uuid::Uuid::new_v4(),
            session_id: None,
            turn_id: None,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Running,
            workspace: root.clone(),
            profile: None,
            model: None,
            prompt_excerpt: None,
            pid: None,
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            exit_code: None,
            failure_domain: None,
            error: None,
        })
        .await
        .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(200), manager.wait_until_idle())
            .await
            .is_err(),
        "a running task must still hold the drain open"
    );
    assert!(manager.tasks_awaiting_a_human().await.is_empty());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn drain_blocking_statuses_exclude_everything_that_waits_on_a_human() {
    for status in [
        RuntimeTaskStatus::Queued,
        RuntimeTaskStatus::Running,
        RuntimeTaskStatus::Cancelling,
    ] {
        assert!(runtime_task_status_blocks_drain(status));
    }
    // 未终态但不拦 drain——这一条正是这次修复的分界线。
    for status in [
        RuntimeTaskStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer,
    ] {
        assert!(runtime_task_status_is_active(status));
        assert!(!runtime_task_status_blocks_drain(status));
    }
    for status in [
        RuntimeTaskStatus::Completed,
        RuntimeTaskStatus::Failed,
        RuntimeTaskStatus::Cancelled,
        RuntimeTaskStatus::Interrupted,
    ] {
        assert!(!runtime_task_status_blocks_drain(status));
    }
}

#[test]
fn active_runtime_task_statuses_are_explicit() {
    for status in [
        RuntimeTaskStatus::Queued,
        RuntimeTaskStatus::Running,
        RuntimeTaskStatus::Cancelling,
        RuntimeTaskStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer,
    ] {
        assert!(runtime_task_status_is_active(status));
    }
    for status in [
        RuntimeTaskStatus::Completed,
        RuntimeTaskStatus::Failed,
        RuntimeTaskStatus::Cancelled,
        RuntimeTaskStatus::Interrupted,
    ] {
        assert!(!runtime_task_status_is_active(status));
    }
}

#[tokio::test]
async fn pending_approval_blocks_until_a_valid_resolution_arrives() {
    let root = std::env::temp_dir().join(format!("willdeep-interaction-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
    let agents = test_agent_store(&root);
    let manager = TaskManager::open(TaskManagerOptions {
        path: root.join("tasks.json"),
        interactions_path: root.join("interactions.json"),
        home: root.clone(),
        events,
        agents,
        sessions: test_runtime_session_store(&root),
        turn_scheduler: test_turn_scheduler(),
        runtime_url: "http://127.0.0.1:1".to_owned(),
        runtime_token: "test-token".to_owned(),
    })
    .unwrap();
    let task_id = uuid::Uuid::new_v4();
    manager
        .insert_and_persist(RuntimeTask {
            id: task_id,
            session_id: None,
            turn_id: None,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Running,
            workspace: root.clone(),
            profile: None,
            model: None,
            prompt_excerpt: None,
            pid: Some(42),
            created_at: 1,
            started_at: Some(1),
            completed_at: None,
            exit_code: None,
            failure_domain: None,
            error: None,
        })
        .await
        .unwrap();
    let receiver = manager
        .create_interaction(
            task_id,
            InteractionKind::Approval {
                description: "run tests".to_owned(),
                always_allow_available: true,
            },
        )
        .await
        .unwrap();
    let interaction = manager.pending_interactions().await.remove(0);
    assert_eq!(
        manager.get(task_id).await.unwrap().status,
        RuntimeTaskStatus::WaitingApproval
    );
    assert!(
        manager
            .resolve_interaction(
                interaction.id,
                InteractionResolution::Answer(Some("wrong kind".to_owned()))
            )
            .await
            .is_err()
    );
    manager
        .resolve_interaction(interaction.id, InteractionResolution::AlwaysAllow)
        .await
        .unwrap();
    assert_eq!(receiver.await.unwrap(), InteractionResolution::AlwaysAllow);
    assert_eq!(
        manager.get(task_id).await.unwrap().status,
        RuntimeTaskStatus::Running
    );
    assert!(manager.pending_interactions().await.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
