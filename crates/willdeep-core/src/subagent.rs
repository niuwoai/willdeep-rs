use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentInstructionInbox, EventSink,
    SubagentLifecycleStatus,
};
use crate::background::{
    BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskStatus, TaskResult,
};
use crate::provider::Provider;
use crate::subagent_worktree::{
    PreparedSubagentWorkspace, SubagentWorktreeManager, SubagentWorktreePolicy,
    worktree_result_note,
};
use crate::tools::{ApprovalMode, ToolError, ToolRegistry};

#[derive(Clone)]
pub struct SubagentProfile {
    pub id: String,
    pub purpose: String,
    pub provider: Arc<dyn Provider>,
    pub tool_names: Vec<String>,
    pub capability_prompt: String,
    pub max_turns: usize,
    pub context_window: u64,
    pub token_budget: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub max_consecutive_failures: usize,
    pub requires_write_target: bool,
    pub worktree: SubagentWorktreePolicy,
}

#[derive(Clone)]
pub struct SubagentCatalog {
    workspace: PathBuf,
    profiles: BTreeMap<String, SubagentProfile>,
    background: Arc<BackgroundTaskRegistry>,
    sink: Arc<dyn EventSink>,
    failures: Arc<Mutex<BTreeMap<String, usize>>>,
    worktrees: Option<SubagentWorktreeManager>,
}

struct SubagentNoopSink;

#[async_trait::async_trait]
impl EventSink for SubagentNoopSink {
    async fn emit(&self, _event: AgentEvent) {}
}

struct ChildEventSink {
    id: uuid::Uuid,
    parent: Arc<dyn EventSink>,
}

#[async_trait::async_trait]
impl EventSink for ChildEventSink {
    async fn emit(&self, event: AgentEvent) {
        let event = match event {
            AgentEvent::TurnStarted { turn } => {
                Some(AgentEvent::SubagentTurnStarted { id: self.id, turn })
            }
            AgentEvent::ToolRequested(call) => Some(AgentEvent::SubagentToolRequested {
                id: self.id,
                name: call.name,
            }),
            AgentEvent::ToolCompleted { call, is_error, .. } => {
                Some(AgentEvent::SubagentToolCompleted {
                    id: self.id,
                    name: call.name,
                    is_error,
                })
            }
            AgentEvent::Usage(usage) => Some(AgentEvent::SubagentUsage { id: self.id, usage }),
            _ => None,
        };
        if let Some(event) = event {
            self.parent.emit(event).await;
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SpawnAgentArgs {
    pub prompt: String,
    pub label: Option<String>,
    pub profile: Option<String>,
    pub run_in_background: Option<bool>,
    pub target_file: Option<String>,
}

impl SubagentCatalog {
    pub fn new(
        workspace: impl AsRef<Path>,
        profiles: Vec<SubagentProfile>,
        background: Arc<BackgroundTaskRegistry>,
    ) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
            profiles: profiles
                .into_iter()
                .map(|profile| (profile.id.clone(), profile))
                .collect(),
            background,
            sink: Arc::new(SubagentNoopSink),
            failures: Arc::new(Mutex::new(BTreeMap::new())),
            worktrees: None,
        }
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn with_worktree_root(mut self, root: impl AsRef<Path>) -> Self {
        self.worktrees = Some(SubagentWorktreeManager::new(root.as_ref().to_path_buf()));
        self
    }

    pub fn description(&self) -> String {
        self.profiles
            .values()
            .map(|profile| format!("`{}` — {}", profile.id, profile.purpose))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn needs_write_approval(&self, id: Option<&str>) -> bool {
        self.profile(id)
            .is_some_and(|profile| profile.requires_write_target)
    }

    fn profile(&self, id: Option<&str>) -> Option<&SubagentProfile> {
        let id = id.unwrap_or("deep").trim().to_ascii_lowercase();
        self.profiles.get(&id).or_else(|| self.profiles.get("deep"))
    }

    pub(crate) async fn run(
        &self,
        args: SpawnAgentArgs,
        approved_target: Option<PathBuf>,
    ) -> Result<String, AgentError> {
        self.run_with_id(uuid::Uuid::new_v4(), args, approved_target)
            .await
    }

    pub async fn spawn_external_read_only(
        &self,
        id: uuid::Uuid,
        prompt: String,
        label: Option<String>,
        profile: Option<String>,
    ) -> Result<(), AgentError> {
        let profile_id = profile
            .as_deref()
            .unwrap_or("deep")
            .trim()
            .to_ascii_lowercase();
        let selected = self.profiles.get(&profile_id).ok_or_else(|| {
            AgentError::Subagent(format!("subagent profile not found: {profile_id}"))
        })?;
        const READ_ONLY_TOOLS: &[&str] = &[
            "search_files",
            "grep_files",
            "list_directory",
            "read_file",
            "git_status",
            "git_diff",
            "git_log",
            "git_blame",
        ];
        if selected.requires_write_target
            || selected
                .tool_names
                .iter()
                .any(|name| !READ_ONLY_TOOLS.contains(&name.as_str()))
        {
            return Err(AgentError::Subagent(format!(
                "profile {profile_id} is not eligible for external read-only spawn"
            )));
        }
        self.run_with_id(
            id,
            SpawnAgentArgs {
                prompt,
                label,
                profile: Some(profile_id),
                run_in_background: Some(true),
                target_file: None,
            },
            None,
        )
        .await?;
        Ok(())
    }

    async fn run_with_id(
        &self,
        agent_id: uuid::Uuid,
        args: SpawnAgentArgs,
        approved_target: Option<PathBuf>,
    ) -> Result<String, AgentError> {
        let profile = self
            .profile(args.profile.as_deref())
            .ok_or_else(|| AgentError::Subagent("no subagent profiles configured".to_owned()))?
            .clone();
        let failure_count = self
            .failures
            .lock()
            .map_err(|_| {
                AgentError::Subagent("subagent failure tracker is unavailable".to_owned())
            })?
            .get(&profile.id)
            .copied()
            .unwrap_or(0);
        if failure_count >= profile.max_consecutive_failures {
            return Err(AgentError::Subagent(format!(
                "profile {} circuit is open after {} consecutive failures",
                profile.id, failure_count
            )));
        }
        if profile.requires_write_target && approved_target.is_none() {
            return Err(AgentError::Subagent(
                "editor profile requires an approved target_file".to_owned(),
            ));
        }
        let label = args
            .label
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| args.prompt.chars().take(48).collect());
        let prompt = args.prompt;
        let profile_id = profile.id.clone();
        let background = args.run_in_background.unwrap_or(false);
        if background {
            let running = self
                .background
                .snapshots()
                .into_iter()
                .filter(|task| {
                    task.kind == BackgroundTaskKind::Subagent
                        && task.status == BackgroundTaskStatus::Running
                })
                .count();
            if running >= 3 {
                return Err(AgentError::Subagent(
                    "at most 3 background subagents may run concurrently".to_owned(),
                ));
            }
        }
        let prepared = self.prepare_workspace(agent_id, profile.worktree).await?;
        let approved_target = remap_approved_target(approved_target, &prepared)?;
        let workspace = prepared.workspace.clone();
        if background {
            let runner_sink = self.sink.clone();
            let failures = self.failures.clone();
            let lifecycle_sink = self.sink.clone();
            let lifecycle_background = self.background.clone();
            let lifecycle_profile = profile_id.clone();
            let lifecycle_label = label.clone();
            let lifecycle_max_turns = profile.max_turns;
            let lifecycle_token_budget = profile.token_budget;
            let lifecycle_timeout_seconds = profile.timeout_seconds;
            let lifecycle_workspace = prepared.workspace.clone();
            let lifecycle_root_workspace = prepared.root_workspace.clone();
            let lifecycle_worktree_branch = prepared.branch.clone();
            let lifecycle_dedicated_worktree = prepared.dedicated;
            let instruction_inbox = Arc::new(AgentInstructionInbox::default());
            let runner_instruction_inbox = instruction_inbox.clone();
            let runner_prepared = prepared.clone();
            let id = self.background.start_retriable_with_lifecycle(
                agent_id,
                BackgroundTaskKind::Subagent,
                format!("{label} · {profile_id}"),
                instruction_inbox,
                move || {
                    let workspace = workspace.clone();
                    let profile = profile.clone();
                    let profile_id = profile.id.clone();
                    let prompt = prompt.clone();
                    let approved_target = approved_target.clone();
                    let sink = runner_sink.clone();
                    let failures = failures.clone();
                    let instruction_inbox = runner_instruction_inbox.clone();
                    let prepared = runner_prepared.clone();
                    async move {
                        let result = run_profile(
                            workspace,
                            profile,
                            prompt,
                            approved_target,
                            sink.clone(),
                            agent_id,
                            Some(instruction_inbox),
                        )
                        .await;
                        let result = attach_worktree_report(result, &prepared).await;
                        record_profile_result(&failures, &profile_id, &result);
                        subagent_task_result(result)
                    }
                },
                move |snapshot| {
                    let sink = lifecycle_sink.clone();
                    let background = lifecycle_background.clone();
                    let profile = lifecycle_profile.clone();
                    let label = lifecycle_label.clone();
                    let workspace = lifecycle_workspace.clone();
                    let root_workspace = lifecycle_root_workspace.clone();
                    let worktree_branch = lifecycle_worktree_branch.clone();
                    async move {
                        if snapshot.status == BackgroundTaskStatus::Running {
                            sink.emit(AgentEvent::SubagentStarted {
                                id: agent_id,
                                profile,
                                label,
                                background: true,
                                max_turns: lifecycle_max_turns,
                                token_budget: lifecycle_token_budget,
                                timeout_seconds: lifecycle_timeout_seconds,
                                workspace,
                                root_workspace,
                                worktree_branch,
                                dedicated_worktree: lifecycle_dedicated_worktree,
                            })
                            .await;
                        } else {
                            sink.emit(AgentEvent::SubagentCompleted {
                                id: agent_id,
                                status: background_lifecycle_status(&snapshot.status),
                                report: background.output(&snapshot.id, 200),
                            })
                            .await;
                        }
                    }
                },
            );
            Ok(format!(
                "Subagent started: agent_id={agent_id}, background_task={id}. Its report will be delivered automatically to the main harness."
            ))
        } else {
            self.sink
                .emit(AgentEvent::SubagentStarted {
                    id: agent_id,
                    profile: profile_id.clone(),
                    label,
                    background: false,
                    max_turns: profile.max_turns,
                    token_budget: profile.token_budget,
                    timeout_seconds: profile.timeout_seconds,
                    workspace: prepared.workspace.clone(),
                    root_workspace: prepared.root_workspace.clone(),
                    worktree_branch: prepared.branch.clone(),
                    dedicated_worktree: prepared.dedicated,
                })
                .await;
            let result = run_profile(
                workspace,
                profile,
                prompt,
                approved_target,
                self.sink.clone(),
                agent_id,
                None,
            )
            .await;
            let result = attach_worktree_report(result, &prepared).await;
            record_profile_result(&self.failures, &profile_id, &result);
            self.sink
                .emit(AgentEvent::SubagentCompleted {
                    id: agent_id,
                    status: subagent_lifecycle_status(&result),
                    report: result
                        .as_ref()
                        .ok()
                        .cloned()
                        .or_else(|| result.as_ref().err().map(ToString::to_string))
                        .map(bounded_report),
                })
                .await;
            result
        }
    }

    async fn prepare_workspace(
        &self,
        agent_id: uuid::Uuid,
        policy: SubagentWorktreePolicy,
    ) -> Result<PreparedSubagentWorkspace, AgentError> {
        if policy == SubagentWorktreePolicy::Shared {
            return Ok(PreparedSubagentWorkspace {
                workspace: self.workspace.clone(),
                root_workspace: self.workspace.clone(),
                branch: None,
                dedicated: false,
            });
        }
        let manager = self.worktrees.as_ref().ok_or_else(|| {
            AgentError::Subagent("dedicated subagent worktree root is not configured".to_owned())
        })?;
        manager.prepare(&self.workspace, agent_id, policy).await
    }
}

async fn attach_worktree_report(
    result: Result<String, AgentError>,
    prepared: &PreparedSubagentWorkspace,
) -> Result<String, AgentError> {
    let report = result?;
    let Some(note) = worktree_result_note(prepared).await? else {
        return Ok(report);
    };
    Ok(format!("{report}\n\n{note}"))
}

fn remap_approved_target(
    target: Option<PathBuf>,
    prepared: &PreparedSubagentWorkspace,
) -> Result<Option<PathBuf>, AgentError> {
    let Some(target) = target else {
        return Ok(None);
    };
    if !prepared.dedicated {
        return Ok(Some(target));
    }
    let relative = target.strip_prefix(&prepared.root_workspace).map_err(|_| {
        AgentError::Subagent(format!(
            "approved editor target {} is outside root workspace {}",
            target.display(),
            prepared.root_workspace.display()
        ))
    })?;
    Ok(Some(prepared.workspace.join(relative)))
}

fn background_lifecycle_status(status: &BackgroundTaskStatus) -> SubagentLifecycleStatus {
    match status {
        BackgroundTaskStatus::Completed => SubagentLifecycleStatus::Completed,
        BackgroundTaskStatus::Blocked => SubagentLifecycleStatus::Blocked,
        BackgroundTaskStatus::Killed => SubagentLifecycleStatus::Cancelled,
        BackgroundTaskStatus::Running
        | BackgroundTaskStatus::Failed
        | BackgroundTaskStatus::TimedOut
        | BackgroundTaskStatus::LaunchFailed => SubagentLifecycleStatus::Failed,
    }
}

fn subagent_lifecycle_status(result: &Result<String, AgentError>) -> SubagentLifecycleStatus {
    match result {
        Ok(_) => SubagentLifecycleStatus::Completed,
        Err(AgentError::Tool(ToolError::ApprovalDenied(_))) => SubagentLifecycleStatus::Blocked,
        Err(_) => SubagentLifecycleStatus::Failed,
    }
}

fn subagent_task_result(result: Result<String, AgentError>) -> TaskResult {
    match result {
        Ok(report) => TaskResult {
            status: BackgroundTaskStatus::Completed,
            exit_code: Some(0),
            output: report,
        },
        Err(AgentError::Tool(ToolError::ApprovalDenied(message))) => TaskResult {
            status: BackgroundTaskStatus::Blocked,
            exit_code: None,
            output: message,
        },
        Err(error) => TaskResult {
            status: BackgroundTaskStatus::Failed,
            exit_code: Some(1),
            output: error.to_string(),
        },
    }
}

async fn run_profile(
    workspace: PathBuf,
    profile: SubagentProfile,
    prompt: String,
    approved_target: Option<PathBuf>,
    lifecycle_sink: Arc<dyn EventSink>,
    agent_id: uuid::Uuid,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
) -> Result<String, AgentError> {
    let approval = if approved_target.is_some() {
        ApprovalMode::WorkspaceAccess
    } else {
        ApprovalMode::Strict
    };
    let tools = ToolRegistry::new(&workspace, approval)?
        .with_allowed_tools(profile.tool_names.clone())
        .with_write_target(approved_target);
    let system_prompt = format!(
        "You are a WillDeep subagent working in {}. You do not see the parent conversation, cannot ask the user, and cannot spawn another agent. Your final response is the report returned to the parent.\n\n{}",
        workspace.display(),
        profile.capability_prompt
    );
    let timeout_seconds = profile.timeout_seconds;
    let mut agent = Agent::new(
        profile.provider,
        tools,
        AgentConfig {
            max_turns: profile.max_turns,
            system_prompt,
            context_window: profile.context_window,
            token_budget: profile.token_budget,
        },
    )
    .with_event_sink(Arc::new(ChildEventSink {
        id: agent_id,
        parent: lifecycle_sink,
    }));
    if let Some(inbox) = instruction_inbox {
        agent = agent.with_instruction_inbox(inbox);
    }
    let run = Box::pin(agent.run(prompt));
    let outcome = if let Some(seconds) = timeout_seconds {
        tokio::time::timeout(Duration::from_secs(seconds), run)
            .await
            .map_err(|_| {
                AgentError::Subagent(format!("subagent timed out after {seconds} seconds"))
            })??
    } else {
        run.await?
    };
    Ok(outcome.final_text)
}

fn record_profile_result(
    failures: &Mutex<BTreeMap<String, usize>>,
    profile: &str,
    result: &Result<String, AgentError>,
) {
    let Ok(mut failures) = failures.lock() else {
        return;
    };
    if result.is_ok() {
        failures.remove(profile);
    } else {
        let count = failures.entry(profile.to_owned()).or_default();
        *count = count.saturating_add(1);
    }
}

fn bounded_report(value: String) -> String {
    const MAX_REPORT_BYTES: usize = 64 * 1024;
    if value.len() <= MAX_REPORT_BYTES {
        return value;
    }
    let mut boundary = MAX_REPORT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[report truncated]", &value[..boundary])
}

pub fn builtin_profiles(
    parent: Arc<dyn Provider>,
    cheap: Arc<dyn Provider>,
    context_window: u64,
) -> Vec<SubagentProfile> {
    vec![
        profile(
            cheap.clone(),
            context_window,
            ProfileSpec {
                id: "scout",
                purpose: "Locate files, symbols and call sites quickly; no shell or writes.",
                tools: &["search_files", "grep_files", "list_directory", "read_file"],
                prompt: "Your trade is LOCATION. Report exact paths and line numbers; do not redesign.",
                max_turns: 8,
                requires_write_target: false,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap.clone(),
            context_window,
            ProfileSpec {
                id: "reader",
                purpose: "Read and summarize long files or documentation; no shell or writes.",
                tools: &["read_file", "list_directory", "search_files"],
                prompt: "Your trade is READING. Answer with specific evidence and say which parts you read.",
                max_turns: 8,
                requires_write_target: false,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            parent,
            context_window,
            ProfileSpec {
                id: "deep",
                purpose: "Complex investigation across files and repository state.",
                tools: &[
                    "search_files",
                    "grep_files",
                    "read_file",
                    "list_directory",
                    "git_status",
                ],
                prompt: "Your trade is INVESTIGATION. Follow evidence across files and state what you could not confirm.",
                max_turns: 12,
                requires_write_target: false,
                worktree: SubagentWorktreePolicy::Shared,
            },
        ),
        profile(
            cheap,
            context_window,
            ProfileSpec {
                id: "editor",
                purpose: "Edit exactly one separately approved target_file.",
                tools: &["read_file", "edit_file"],
                prompt: "Your trade is EDITING EXACTLY ONE FILE. Read it first, make a minimal exact edit, and touch no other path.",
                max_turns: 6,
                requires_write_target: true,
                worktree: SubagentWorktreePolicy::Dedicated,
            },
        ),
    ]
}

struct ProfileSpec<'a> {
    id: &'a str,
    purpose: &'a str,
    tools: &'a [&'a str],
    prompt: &'a str,
    max_turns: usize,
    requires_write_target: bool,
    worktree: SubagentWorktreePolicy,
}

fn profile(
    provider: Arc<dyn Provider>,
    context_window: u64,
    spec: ProfileSpec<'_>,
) -> SubagentProfile {
    SubagentProfile {
        id: spec.id.to_owned(),
        purpose: spec.purpose.to_owned(),
        provider,
        tool_names: spec.tools.iter().map(|value| (*value).to_owned()).collect(),
        capability_prompt: spec.prompt.to_owned(),
        max_turns: spec.max_turns,
        context_window,
        token_budget: None,
        timeout_seconds: Some(300),
        max_consecutive_failures: 3,
        requires_write_target: spec.requires_write_target,
        worktree: spec.worktree,
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::provider::ProviderError;
    use crate::types::{Completion, Message, ToolDefinition};

    struct ReportProvider;

    #[derive(Default)]
    struct CaptureSink(std::sync::Mutex<Vec<AgentEvent>>);

    #[async_trait]
    impl EventSink for CaptureSink {
        async fn emit(&self, event: AgentEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
    #[async_trait]
    impl Provider for ReportProvider {
        async fn complete(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            assert!(messages[0].content.contains("cannot spawn another agent"));
            assert!(tools.iter().all(|tool| tool.name != "spawn_agent"));
            Ok(Completion {
                content: "subagent report".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: None,
            })
        }
    }

    fn fixture() -> (SubagentCatalog, PathBuf) {
        let root = std::env::temp_dir().join(format!("willdeep-subagent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        let provider: Arc<dyn Provider> = Arc::new(ReportProvider);
        let background = Arc::new(BackgroundTaskRegistry::default());
        (
            SubagentCatalog::new(
                &root,
                builtin_profiles(provider.clone(), provider, 128_000),
                background,
            ),
            root,
        )
    }

    #[test]
    fn approval_denial_marks_background_subagent_blocked() {
        let result = subagent_task_result(Err(AgentError::Tool(ToolError::ApprovalDenied(
            "write access needed".to_owned(),
        ))));
        assert_eq!(result.status, BackgroundTaskStatus::Blocked);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "write access needed");
    }

    #[tokio::test]
    async fn foreground_profile_runs_in_isolated_non_nested_agent() {
        let (catalog, root) = fixture();
        let sink = Arc::new(CaptureSink::default());
        let catalog = catalog.with_event_sink(sink.clone());
        let report = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "inspect".to_owned(),
                    label: None,
                    profile: Some("scout".to_owned()),
                    run_in_background: Some(false),
                    target_file: None,
                },
                None,
            )
            .await
            .expect("run");
        assert_eq!(report, "subagent report");
        let events = sink.0.lock().unwrap();
        let started = events.iter().find_map(|event| match event {
            AgentEvent::SubagentStarted {
                id,
                profile,
                background,
                ..
            } => Some((*id, profile.as_str(), *background)),
            _ => None,
        });
        let completed = events.iter().find_map(|event| match event {
            AgentEvent::SubagentCompleted { id, status, .. } => Some((*id, *status)),
            _ => None,
        });
        assert_eq!(
            started.map(|(_, profile, background)| (profile, background)),
            Some(("scout", false))
        );
        assert_eq!(
            completed,
            started.map(|(id, _, _)| (id, SubagentLifecycleStatus::Completed))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentTurnStarted { id, turn: 1 }
                if Some(*id) == started.map(|(id, _, _)| id)
        )));
        drop(events);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn editor_requires_preapproved_target() {
        let (catalog, root) = fixture();
        let result = catalog
            .run(
                SpawnAgentArgs {
                    prompt: "edit".to_owned(),
                    label: None,
                    profile: Some("editor".to_owned()),
                    run_in_background: Some(false),
                    target_file: Some("file.txt".to_owned()),
                },
                None,
            )
            .await;
        assert!(matches!(result, Err(AgentError::Subagent(_))));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn external_spawn_is_background_only_and_rejects_writing_profiles() {
        let (catalog, root) = fixture();
        let sink = Arc::new(CaptureSink::default());
        let catalog = catalog.with_event_sink(sink.clone());
        assert!(
            catalog
                .spawn_external_read_only(
                    uuid::Uuid::new_v4(),
                    "edit".to_owned(),
                    None,
                    Some("editor".to_owned()),
                )
                .await
                .is_err()
        );
        assert!(
            catalog
                .spawn_external_read_only(
                    uuid::Uuid::new_v4(),
                    "inspect".to_owned(),
                    None,
                    Some("missing".to_owned()),
                )
                .await
                .is_err()
        );

        let id = uuid::Uuid::new_v4();
        catalog
            .spawn_external_read_only(
                id,
                "inspect".to_owned(),
                Some("external scout".to_owned()),
                Some("scout".to_owned()),
            )
            .await
            .expect("spawn external read-only agent");
        for _ in 0..50 {
            if sink.0.lock().unwrap().iter().any(|event| {
                matches!(event, AgentEvent::SubagentCompleted { id: event_id, .. } if *event_id == id)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = sink.0.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentStarted {
                id: event_id,
                profile,
                background: true,
                ..
            } if *event_id == id && profile == "scout"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SubagentCompleted { id: event_id, .. } if *event_id == id
        )));
        drop(events);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn approved_editor_target_maps_to_the_same_relative_worktree_path() {
        let root = PathBuf::from("/workspace/project");
        let prepared = PreparedSubagentWorkspace {
            workspace: PathBuf::from("/managed/agent"),
            root_workspace: root.clone(),
            branch: Some("willdeep/agent-test".to_owned()),
            dedicated: true,
        };
        let mapped = remap_approved_target(Some(root.join("src/lib.rs")), &prepared)
            .expect("map target")
            .expect("target");
        assert_eq!(mapped, PathBuf::from("/managed/agent/src/lib.rs"));
        assert!(remap_approved_target(Some(PathBuf::from("/elsewhere/file")), &prepared).is_err());
    }
}
