use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, Notify, RwLock};

const STATE_SCHEMA: u32 = 1;
const TOKEN_HEADER: &str = "x-willdeep-token";
const SERVER_VERSION_HEADER: &str = "x-willdeep-version";
const REQUEST_ID_HEADER: &str = "x-willdeep-request-id";
const LOCK_STALE_AFTER_SECONDS: u64 = 10;
const LOCK_HEARTBEAT_SECONDS: u64 = 2;
const LOCK_RECOVERY_ATTEMPTS: usize = 120;

mod agent_control;
mod agent_store;
mod control_api;
pub(crate) mod diff_review;
mod event_stream;
mod headless;
mod herdr;
mod local_transport;
mod session_store;
mod tool_store;
pub(crate) mod tui_bridge;
mod workspace_store;
mod worktree_maintenance;
mod worktree_review;

use agent_control::AgentCommandStore;
pub(crate) use agent_control::{AgentCommandWatcher, start_agent_command_watcher};
use agent_store::{AgentStore, RuntimeAgentStatus};
use event_stream::EventLog;
pub(crate) use headless::{HeadlessRuntimeRequest, HeadlessRuntimeStatus, execute_headless_turn};
use local_transport::LocalTransportState;
pub(crate) use tui_bridge::{
    RemoteGate, RemoteRuntimeEvent, RuntimeSnapshot, answer_remote_question, cancel_remote_task,
    delete_remote_session, ensure_runtime_session, export_remote_session, fork_remote_session,
    instruct_remote_agent, remote_session_states, rename_remote_session, resolve_remote_approval,
    retry_remote_agent, runtime_event_head, runtime_events, runtime_snapshot,
    search_remote_sessions, set_remote_session_archived, start_runtime_event_follower,
    stop_remote_agent, stop_remote_turn, submit_runtime_turn,
};
pub(crate) use workspace_store::WorkspaceAccess;
pub(crate) use workspace_store::{
    RuntimeWorkspace, activate_remote_workspace, ensure_remote_workspace, remote_workspaces,
};
pub(crate) use worktree_review::{WorktreeReview, remote_merge, remote_review};

struct RuntimeEventSink {
    task_id: uuid::Uuid,
    session_id: Option<uuid::Uuid>,
    turn_id: Option<uuid::Uuid>,
    root_agent_id: uuid::Uuid,
    home: PathBuf,
    workspace: PathBuf,
    events: Arc<EventLog>,
    agents: Arc<AgentStore>,
    tools: Arc<tool_store::ToolStore>,
    diff_baselines: AsyncMutex<HashMap<String, (PathBuf, diff_review::DiffCapture)>>,
    child_workspaces: AsyncMutex<HashMap<uuid::Uuid, PathBuf>>,
}

impl RuntimeEventSink {
    async fn observe_diff(&self, event: &willdeep_core::AgentEvent) {
        use willdeep_core::AgentEvent;
        if let AgentEvent::SubagentStarted { id, workspace, .. } = event {
            self.child_workspaces
                .lock()
                .await
                .insert(*id, workspace.clone());
            return;
        }
        let requested = match event {
            AgentEvent::ToolRequested(call) => Some((
                format!("root:{}", call.id),
                self.root_agent_id,
                call.name.clone(),
            )),
            AgentEvent::SubagentToolRequested { id, name } => {
                Some((format!("child:{id}"), *id, name.clone()))
            }
            _ => None,
        };
        if let Some((key, agent_id, tool)) = requested {
            if !tool_may_modify_workspace(&tool) {
                return;
            }
            let workspace = self.agent_workspace(agent_id).await;
            if let Ok(capture) = diff_review::capture(&workspace) {
                self.diff_baselines
                    .lock()
                    .await
                    .insert(key, (workspace, capture));
            }
            return;
        }
        let completed = match event {
            AgentEvent::ToolCompleted { call, .. } => Some((
                format!("root:{}", call.id),
                self.root_agent_id,
                call.name.clone(),
            )),
            AgentEvent::SubagentToolCompleted { id, name, .. } => {
                Some((format!("child:{id}"), *id, name.clone()))
            }
            _ => None,
        };
        let Some((key, agent_id, tool)) = completed else {
            return;
        };
        if !tool_may_modify_workspace(&tool) {
            return;
        }
        let Some((workspace, before)) = self.diff_baselines.lock().await.remove(&key) else {
            return;
        };
        if let Err(error) = diff_review::record_tool_attribution(
            &self.home,
            before,
            &workspace,
            diff_review::AttributionContext {
                session_id: self.session_id,
                turn_id: self.turn_id,
                task_id: self.task_id,
                agent_id,
                tool,
            },
        )
        .await
        {
            eprintln!(
                "record Diff attribution for task {}: {error:#}",
                self.task_id
            );
        }
    }

    async fn agent_workspace(&self, agent_id: uuid::Uuid) -> PathBuf {
        if agent_id == self.root_agent_id {
            return self.workspace.clone();
        }
        self.child_workspaces
            .lock()
            .await
            .get(&agent_id)
            .cloned()
            .unwrap_or_else(|| self.workspace.clone())
    }
}

fn tool_may_modify_workspace(name: &str) -> bool {
    matches!(
        name,
        "create_file" | "edit_file" | "run_command" | "create_worktree" | "computer_use"
    ) || name.starts_with("mcp__")
}

#[async_trait]
impl willdeep_core::EventSink for RuntimeEventSink {
    async fn emit(&self, event: willdeep_core::AgentEvent) {
        if let Err(error) = tool_store::observe(
            &self.tools,
            self.session_id,
            self.turn_id,
            self.task_id,
            self.root_agent_id,
            &event,
        ) {
            eprintln!("record Tool activity for task {}: {error:#}", self.task_id);
        }
        self.observe_diff(&event).await;
        let line = crate::agent_event_json(event).to_string();
        if self
            .agents
            .apply_harness_event(self.task_id, &line)
            .is_err()
        {
            return;
        }
        let _ = self
            .events
            .append("task.output", format!("task_id={} {line}", self.task_id));
    }
}

#[derive(Clone, Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the local Runtime Daemon in the background.
    Start,
    /// Show Runtime Daemon health and endpoint information.
    Status,
    /// Show negotiated Runtime protocol objects, operations, transports and limits.
    Capabilities,
    /// Gracefully stop the local Runtime Daemon.
    Stop,
    /// Print Runtime Daemon logs.
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
    },
    /// Submit a non-interactive Harness task to the persistent Runtime.
    Submit {
        /// Workspace root available to the task.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Provider profile from config.toml.
        #[arg(long)]
        profile: Option<String>,
        /// Optional model override for the selected Provider profile.
        #[arg(long)]
        model: Option<String>,
        /// TOML configuration path inherited by the task.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Prompt sent through private stdin rather than process arguments.
        #[arg(value_name = "PROMPT", num_args = 1.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// List tasks owned by the Runtime.
    Tasks,
    /// Show one Runtime-owned task.
    Task { id: uuid::Uuid },
    /// List structured agents owned by the Runtime.
    Agents,
    /// Show one Runtime-owned agent.
    Agent { id: uuid::Uuid },
    /// Preview an exact Diff and conflict check for a child Agent worktree.
    AgentWorktreeReview { id: uuid::Uuid },
    /// Apply an exact reviewed child Agent patch to its root Workspace.
    MergeAgentWorktree {
        id: uuid::Uuid,
        #[arg(long)]
        review: String,
        #[arg(long)]
        yes: bool,
    },
    /// Audit managed, missing, reviewable, merged and unknown child Worktrees.
    WorktreesAudit,
    /// Move an exact clean or merged Worktree into recoverable quarantine.
    QuarantineAgentWorktree {
        id: uuid::Uuid,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        yes: bool,
    },
    /// Create a persistent interactive Runtime Session.
    CreateSession {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        title: Option<String>,
    },
    /// List persistent interactive Runtime Sessions.
    Sessions,
    /// List registered Runtime Workspaces.
    Workspaces,
    /// Register or update a Runtime Workspace and its independent settings.
    RegisterWorkspace {
        #[arg(value_name = "PATH")]
        root: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value_t = workspace_store::WorkspaceAccess::WorkspaceWrite)]
        access: workspace_store::WorkspaceAccess,
        #[arg(long)]
        provider_profile: Option<String>,
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long = "mcp-server")]
        mcp_servers: Vec<String>,
    },
    /// Make one registered Workspace the default for new clients.
    ActivateWorkspace { id: uuid::Uuid },
    /// Remove a Workspace registration without deleting files or Sessions.
    RemoveWorkspace {
        id: uuid::Uuid,
        #[arg(long)]
        yes: bool,
    },
    /// Show one persistent Runtime Session.
    Session { id: uuid::Uuid },
    /// Search persistent Runtime Sessions by title or message text.
    SearchSessions {
        #[arg(value_name = "QUERY", num_args = 0.., trailing_var_arg = true)]
        query: Vec<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        updated_after: Option<u64>,
        #[arg(long)]
        updated_before: Option<u64>,
    },
    /// Rename a persistent Runtime Session.
    RenameSession {
        id: uuid::Uuid,
        #[arg(value_name = "TITLE", num_args = 1.., trailing_var_arg = true)]
        title: Vec<String>,
    },
    /// Fork a stable Session snapshot without copying its Turn history.
    ForkSession {
        id: uuid::Uuid,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, value_name = "TURN_ID")]
        through_turn: Option<uuid::Uuid>,
        #[arg(long, value_name = "PROFILE")]
        provider_profile: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Archive an inactive Runtime Session.
    ArchiveSession { id: uuid::Uuid },
    /// Restore an archived Runtime Session to Idle.
    UnarchiveSession { id: uuid::Uuid },
    /// Export a Runtime Session as a credential-free JSON snapshot.
    ExportSession {
        id: uuid::Uuid,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Permanently delete an inactive Runtime Session and its local history.
    DeleteSession {
        id: uuid::Uuid,
        #[arg(long)]
        yes: bool,
    },
    /// Queue a user Turn in a persistent Runtime Session.
    SubmitTurn {
        session_id: uuid::Uuid,
        #[arg(long)]
        request_id: Option<uuid::Uuid>,
        #[arg(value_name = "PROMPT", num_args = 1.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// List Turns in a Runtime Session.
    Turns { session_id: uuid::Uuid },
    /// Show one Runtime Turn.
    Turn { id: uuid::Uuid },
    /// Stop a queued or running Runtime Turn.
    StopTurn { id: uuid::Uuid },
    /// Request cancellation of a running background child Agent.
    StopAgent { id: uuid::Uuid },
    /// Retry a terminal background child Agent.
    RetryAgent { id: uuid::Uuid },
    /// Add instructions to a running background child Agent.
    InstructAgent {
        id: uuid::Uuid,
        #[arg(value_name = "INSTRUCTION", num_args = 1.., trailing_var_arg = true)]
        instruction: Vec<String>,
    },
    /// Capture a structured Diff snapshot for a Runtime Workspace.
    DiffSnapshot {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Show one file from an exact Diff snapshot.
    DiffFile {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        path: String,
        #[arg(long, value_enum, default_value_t = diff_review::DiffArea::Combined)]
        area: diff_review::DiffArea,
    },
    /// Save a decision for one file in an exact Diff snapshot.
    DiffReview {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        path: String,
        #[arg(long, value_enum)]
        decision: diff_review::ReviewDecision,
        #[arg(long)]
        note: Option<String>,
    },
    /// List test verifications bound to an exact Diff snapshot.
    DiffVerifications {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
    },
    /// List Turn, Task, Agent, and Tool attribution for an exact Diff snapshot.
    DiffAttributions {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
    },
    /// Preview an exact commit, tag, and push target without mutating Git.
    DiffCommitPreview {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        message: String,
        #[arg(long, default_value = "origin")]
        remote: String,
        #[arg(long)]
        tag: Option<String>,
    },
    /// Safely revert one file from an exact Diff snapshot.
    DiffRevert {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        path: String,
        #[arg(long, value_enum, default_value_t = diff_review::DiffArea::Combined)]
        area: diff_review::DiffArea,
    },
    /// Request cancellation of a Runtime-owned task.
    Cancel { id: uuid::Uuid },
    /// List approvals and questions currently blocking Runtime tasks.
    Pending,
    /// Resolve a pending Runtime approval.
    Resolve {
        id: uuid::Uuid,
        #[arg(value_enum)]
        decision: ApprovalArg,
    },
    /// Answer a pending Runtime question.
    Answer {
        id: uuid::Uuid,
        #[arg(value_name = "ANSWER", num_args = 1.., trailing_var_arg = true)]
        answer: Vec<String>,
    },
    /// Internal foreground server entry used by `daemon start`.
    #[command(hide = true)]
    Run,
}

#[derive(Clone, Debug, Subcommand)]
pub enum SessionAction {
    /// List persistent Runtime Sessions.
    List,
    /// Show one Session and its active Turn.
    Get { id: uuid::Uuid },
    /// List Turns owned by one Session.
    Turns { id: uuid::Uuid },
    /// Stop the Session's active or queued Turn.
    Stop { id: uuid::Uuid },
}

pub async fn handle_session(action: SessionAction) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    match action {
        SessionAction::List => session_store::list_sessions_cli(&home).await,
        SessionAction::Get { id } => session_store::show_session_cli(&home, id).await,
        SessionAction::Turns { id } => session_store::list_turns_cli(&home, id).await,
        SessionAction::Stop { id } => session_store::stop_session_cli(&home, id).await,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DaemonState {
    schema: u32,
    version: String,
    pid: u32,
    address: SocketAddr,
    token: String,
    started_at: u64,
    #[serde(default)]
    local_transport: Option<LocalTransportState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DaemonLock {
    token: String,
    created_at: u64,
}

#[derive(Clone)]
struct ServerState {
    home: PathBuf,
    token: String,
    started_at: u64,
    shutdown: Arc<Notify>,
    events: Arc<EventLog>,
    tasks: Arc<TaskManager>,
    agents: Arc<AgentStore>,
    agent_commands: Arc<AgentCommandStore>,
    sessions: Arc<session_store::RuntimeSessionStore>,
    workspaces: Arc<workspace_store::WorkspaceStore>,
    diff_review_lock: Arc<tokio::sync::Mutex<()>>,
    idempotency: Arc<control_api::IdempotencyStore>,
    local_transport: Option<LocalTransportState>,
    tools: Arc<tool_store::ToolStore>,
}

type RuntimeEvent = willdeep_runtime_protocol::RuntimeEvent;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuntimeTaskStatus {
    Queued,
    Running,
    Cancelling,
    WaitingApproval,
    WaitingAnswer,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeTask {
    pub(crate) id: uuid::Uuid,
    #[serde(default)]
    pub(crate) session_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub(crate) turn_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub(crate) agent_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub(crate) event_start_sequence: u64,
    status: RuntimeTaskStatus,
    workspace: PathBuf,
    profile: Option<String>,
    pid: Option<u32>,
    created_at: u64,
    started_at: Option<u64>,
    completed_at: Option<u64>,
    exit_code: Option<i32>,
    #[serde(default)]
    failure_domain: Option<willdeep_runtime_protocol::FailureDomain>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SubmitTask {
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) attachments: Vec<willdeep_core::MessageAttachment>,
    pub(crate) workspace: PathBuf,
    #[serde(skip)]
    pub(crate) workspace_access: Option<WorkspaceAccess>,
    #[serde(skip)]
    pub(crate) workspace_skills: Option<Vec<String>>,
    #[serde(skip)]
    pub(crate) workspace_mcp_servers: Option<Vec<String>>,
    pub(crate) profile: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    pub(crate) config: Option<PathBuf>,
    #[serde(default)]
    pub(crate) session_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub(crate) turn_id: Option<uuid::Uuid>,
}

#[derive(Clone)]
pub(crate) struct RuntimeSubmitOptions {
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub config: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RuntimeConnection {
    url: String,
    token: String,
    task_id: uuid::Uuid,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ApprovalArg {
    AllowOnce,
    Deny,
    AlwaysAllow,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InteractionKind {
    Approval {
        description: String,
        always_allow_available: bool,
    },
    Question {
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum InteractionResolution {
    AllowOnce,
    Deny,
    AlwaysAllow,
    Answer(Option<String>),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum InteractionStatus {
    Pending,
    Resolved,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeInteraction {
    id: uuid::Uuid,
    task_id: uuid::Uuid,
    kind: InteractionKind,
    status: InteractionStatus,
    resolution: Option<InteractionResolution>,
    created_at: u64,
    resolved_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CreateInteraction {
    kind: InteractionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ResolveInteraction {
    resolution: InteractionResolution,
}

struct TaskManager {
    path: PathBuf,
    home: PathBuf,
    events: Arc<EventLog>,
    agents: Arc<AgentStore>,
    tools: Arc<tool_store::ToolStore>,
    sessions: Arc<session_store::RuntimeSessionStore>,
    workspaces: Arc<workspace_store::WorkspaceStore>,
    runtime_url: String,
    runtime_token: String,
    tasks: RwLock<HashMap<uuid::Uuid, RuntimeTask>>,
    persistence: AsyncMutex<()>,
    cancellations: Mutex<HashMap<uuid::Uuid, Arc<Notify>>>,
    interactions_path: PathBuf,
    interactions: RwLock<HashMap<uuid::Uuid, RuntimeInteraction>>,
    interaction_waiters:
        Mutex<HashMap<uuid::Uuid, tokio::sync::oneshot::Sender<InteractionResolution>>>,
    turn_scheduler: tokio::sync::mpsc::UnboundedSender<uuid::Uuid>,
    herdr: Option<herdr::HerdrReporter>,
}

pub async fn handle(action: DaemonAction) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    match action {
        DaemonAction::Start => start(&home, true).await,
        DaemonAction::Status => status(&home).await,
        DaemonAction::Capabilities => capabilities_cli(&home).await,
        DaemonAction::Stop => stop(&home).await,
        DaemonAction::Logs { lines, follow } => logs(&home, lines, follow).await,
        DaemonAction::Submit {
            workspace,
            profile,
            model,
            config,
            prompt,
        } => submit(&home, workspace, profile, model, config, prompt).await,
        DaemonAction::Tasks => list_tasks(&home).await,
        DaemonAction::Task { id } => show_task(&home, id).await,
        DaemonAction::Agents => list_agents(&home).await,
        DaemonAction::Agent { id } => show_agent(&home, id).await,
        DaemonAction::AgentWorktreeReview { id } => worktree_review::review_cli(&home, id).await,
        DaemonAction::MergeAgentWorktree { id, review, yes } => {
            worktree_review::merge_cli(&home, id, review, yes).await
        }
        DaemonAction::WorktreesAudit => worktree_maintenance::audit_cli(&home).await,
        DaemonAction::QuarantineAgentWorktree { id, snapshot, yes } => {
            worktree_maintenance::quarantine_cli(&home, id, snapshot, yes).await
        }
        DaemonAction::CreateSession {
            workspace,
            profile,
            model,
            config,
            title,
        } => {
            session_store::create_session_cli(&home, workspace, profile, model, config, title).await
        }
        DaemonAction::Sessions => session_store::list_sessions_cli(&home).await,
        DaemonAction::Workspaces => workspace_store::list_cli(&home).await,
        DaemonAction::RegisterWorkspace {
            root,
            name,
            access,
            provider_profile,
            skills,
            mcp_servers,
        } => {
            workspace_store::register_cli(
                &home,
                root,
                name,
                access,
                provider_profile,
                skills,
                mcp_servers,
            )
            .await
        }
        DaemonAction::ActivateWorkspace { id } => workspace_store::activate_cli(&home, id).await,
        DaemonAction::RemoveWorkspace { id, yes } => {
            workspace_store::remove_cli(&home, id, yes).await
        }
        DaemonAction::Session { id } => session_store::show_session_cli(&home, id).await,
        DaemonAction::SearchSessions {
            query,
            workspace,
            status,
            profile,
            model,
            updated_after,
            updated_before,
        } => {
            session_store::search_sessions_cli(
                &home,
                query,
                workspace,
                status,
                profile,
                model,
                updated_after,
                updated_before,
            )
            .await
        }
        DaemonAction::RenameSession { id, title } => {
            session_store::rename_session_cli(&home, id, title).await
        }
        DaemonAction::ForkSession {
            id,
            title,
            through_turn,
            provider_profile,
            model,
        } => {
            session_store::fork_session_cli(&home, id, title, through_turn, provider_profile, model)
                .await
        }
        DaemonAction::ArchiveSession { id } => {
            session_store::archive_session_cli(&home, id, false).await
        }
        DaemonAction::UnarchiveSession { id } => {
            session_store::archive_session_cli(&home, id, true).await
        }
        DaemonAction::ExportSession { id, output } => {
            session_store::export_session_cli(&home, id, output).await
        }
        DaemonAction::DeleteSession { id, yes } => {
            session_store::delete_session_cli(&home, id, yes).await
        }
        DaemonAction::SubmitTurn {
            session_id,
            request_id,
            prompt,
        } => session_store::submit_turn_cli(&home, session_id, request_id, prompt).await,
        DaemonAction::Turns { session_id } => {
            session_store::list_turns_cli(&home, session_id).await
        }
        DaemonAction::Turn { id } => session_store::show_turn_cli(&home, id).await,
        DaemonAction::StopTurn { id } => session_store::stop_turn_cli(&home, id).await,
        DaemonAction::StopAgent { id } => {
            agent_control::control_agent(&home, id, agent_control::AgentCommandKind::Stop).await
        }
        DaemonAction::RetryAgent { id } => {
            agent_control::control_agent(&home, id, agent_control::AgentCommandKind::Retry).await
        }
        DaemonAction::InstructAgent { id, instruction } => {
            agent_control::instruct_agent(&home, id, instruction.join(" ")).await
        }
        DaemonAction::DiffSnapshot { workspace } => {
            diff_review::snapshot_cli(&home, workspace).await
        }
        DaemonAction::DiffFile {
            workspace,
            snapshot,
            path,
            area,
        } => diff_review::content_cli(&home, workspace, snapshot, path, area).await,
        DaemonAction::DiffReview {
            workspace,
            snapshot,
            path,
            decision,
            note,
        } => diff_review::review_cli(&home, workspace, snapshot, path, decision, note).await,
        DaemonAction::DiffVerifications {
            workspace,
            snapshot,
        } => diff_review::verifications_cli(&home, workspace, snapshot).await,
        DaemonAction::DiffAttributions {
            workspace,
            snapshot,
        } => diff_review::attributions_cli(&home, workspace, snapshot).await,
        DaemonAction::DiffCommitPreview {
            workspace,
            snapshot,
            message,
            remote,
            tag,
        } => {
            diff_review::commit_preview_cli(&home, workspace, snapshot, message, remote, tag).await
        }
        DaemonAction::DiffRevert {
            workspace,
            snapshot,
            path,
            area,
        } => diff_review::revert_cli(&home, workspace, snapshot, path, area).await,
        DaemonAction::Cancel { id } => cancel_task(&home, id).await,
        DaemonAction::Pending => list_pending(&home).await,
        DaemonAction::Resolve { id, decision } => resolve_pending(&home, id, decision).await,
        DaemonAction::Answer { id, answer } => answer_pending(&home, id, answer).await,
        DaemonAction::Run => run(&home).await,
    }
}

pub async fn attach(after: u64) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    let paths = DaemonPaths::new(&home);
    let state = load_state(&paths.state).context("Runtime Daemon is not running")?;
    probe(&state)
        .await
        .context("Runtime Daemon is unavailable")?;
    println!(
        "Attached to WillDeep Runtime at {} after event {}. Ctrl+C detaches without stopping it.",
        state.address, after
    );
    let mut cursor = after;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("Detached at event {cursor}; Runtime continues running.");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                for event in fetch_events(&state, cursor).await? {
                    println!("{}\t{}\t{}", event.sequence, event.kind, event.message);
                    cursor = cursor.max(event.sequence);
                }
            }
        }
    }
}

pub async fn detach() -> Result<()> {
    let home = crate::config::willdeep_home()?;
    let state =
        load_state(&DaemonPaths::new(&home).state).context("Runtime Daemon is not running")?;
    let health = probe(&state)
        .await
        .context("Runtime Daemon is unavailable")?;
    println!(
        "Client detached; Runtime pid {} continues running (uptime {}s).",
        health.pid, health.uptime_seconds
    );
    Ok(())
}

pub async fn api(
    operation: String,
    params_file: Option<PathBuf>,
    request_id: Option<uuid::Uuid>,
    ndjson: bool,
) -> Result<()> {
    if !willdeep_runtime_protocol::SUPPORTED_OPERATIONS.contains(&operation.as_str())
        && !matches!(operation.as_str(), "agent.prompt" | "agent.wait")
    {
        bail!("unknown Runtime operation: {operation}");
    }
    let params = match params_file.as_deref() {
        None => serde_json::json!({}),
        Some(path) if path == Path::new("-") => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            serde_json::from_str(&input).context("parse API params from stdin")?
        }
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)
            .with_context(|| format!("parse API params file as JSON: {}", path.display()))?,
    };
    if !params.is_object() {
        bail!("API params must be a JSON object");
    }
    let home = crate::config::willdeep_home()?;
    let state = ensure_running(&home).await?;
    if operation == "event.stream" {
        if !ndjson {
            bail!("event.stream is an NDJSON stream; pass --ndjson");
        }
        return stream_api_events(
            &state,
            &params,
            request_id.unwrap_or_else(uuid::Uuid::new_v4),
        )
        .await;
    }
    let response: willdeep_runtime_protocol::ApiResponse<serde_json::Value> =
        runtime_client(&state)?
            .call(operation, &params, request_id)
            .await?;
    let failed = matches!(
        response,
        willdeep_runtime_protocol::ApiResponse::Error { .. }
    );
    let value = serde_json::to_value(response)?;
    if ndjson {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    if failed {
        bail!("Runtime API operation failed; inspect the error envelope above");
    }
    Ok(())
}

async fn stream_api_events(
    state: &DaemonState,
    params: &serde_json::Value,
    request_id: uuid::Uuid,
) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StreamParams {
        #[serde(default)]
        after: u64,
        #[serde(default = "default_event_limit")]
        limit: usize,
    }

    let params: StreamParams = serde_json::from_value(params.clone())
        .context("event.stream params must contain only after and limit")?;
    let mut events = runtime_client(state)?
        .stream_events(params.after, params.limit, Some(request_id))
        .await?;
    let mut stdout = std::io::stdout().lock();
    while let Some(event) = events.next::<RuntimeEvent>().await? {
        serde_json::to_writer(&mut stdout, &event)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

async fn submit(
    home: &Path,
    workspace: Option<PathBuf>,
    profile: Option<String>,
    model: Option<String>,
    config: Option<PathBuf>,
    prompt: Vec<String>,
) -> Result<()> {
    let prompt = prompt.join(" ");
    let workspace = workspace_store::resolve_cli_root(home, workspace).await?;
    let task = submit_runtime_prompt(
        home,
        &RuntimeSubmitOptions {
            workspace,
            profile,
            model,
            config,
        },
        prompt,
        Vec::new(),
    )
    .await?;
    println!(
        "submitted\tid={}\tstatus={:?}\tworkspace={}",
        task.id,
        task.status,
        task.workspace.display()
    );
    Ok(())
}

pub(crate) async fn submit_runtime_prompt(
    home: &Path,
    options: &RuntimeSubmitOptions,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
) -> Result<RuntimeTask> {
    if prompt.trim().is_empty() && attachments.is_empty() {
        bail!("Runtime task prompt and attachments must not both be empty");
    }
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&SubmitTask {
            prompt,
            attachments,
            workspace: options.workspace.canonicalize()?,
            workspace_access: None,
            workspace_skills: None,
            workspace_mcp_servers: None,
            profile: options.profile.clone(),
            model: options.model.clone(),
            config: options.config.clone(),
            session_id: None,
            turn_id: None,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected task submission: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
}

async fn list_tasks(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let tasks: Vec<RuntimeTask> = authorized_get(&state, "/v1/tasks").await?;
    for task in tasks {
        print_task(&task);
    }
    Ok(())
}

async fn show_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let task: RuntimeTask = authorized_get(&state, &format!("/v1/tasks/{id}")).await?;
    print_task(&task);
    Ok(())
}

async fn list_agents(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let agents: Vec<agent_store::RuntimeAgent> = authorized_get(&state, "/v1/agents").await?;
    for agent in agents {
        print_agent(&agent);
    }
    Ok(())
}

async fn show_agent(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let agent: agent_store::RuntimeAgent =
        authorized_get(&state, &format!("/v1/agents/{id}")).await?;
    print_agent(&agent);
    println!(
        "policy\tmax_turns={}\ttoken_budget={}\ttimeout_seconds={}",
        agent
            .max_turns
            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        agent
            .token_budget
            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        agent
            .timeout_seconds
            .map_or_else(|| "-".to_owned(), |value| value.to_string())
    );
    if let Some(report) = &agent.report {
        println!("report\n{report}");
    }
    Ok(())
}

async fn cancel_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected cancellation: {}", response.text().await?);
    }
    let task: RuntimeTask = response.json().await?;
    print_task(&task);
    Ok(())
}

async fn ensure_running(home: &Path) -> Result<DaemonState> {
    let path = DaemonPaths::new(home).state;
    if let Ok(state) = load_state(&path)
        && probe(&state).await.is_ok()
    {
        return Ok(state);
    }
    start(home, false).await?;
    load_state(&path)
}

async fn authorized_get<T: serde::de::DeserializeOwned>(
    state: &DaemonState,
    path: &str,
) -> Result<T> {
    let response = client()
        .get(format!("http://{}{}", state.address, path))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime request failed: {}", response.text().await?);
    }
    Ok(response.json().await?)
}

fn print_task(task: &RuntimeTask) {
    println!(
        "{}\t{:?}\tpid={}\texit={}\t{}",
        task.id,
        task.status,
        task.pid
            .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
        task.exit_code
            .map_or_else(|| "-".to_owned(), |code| code.to_string()),
        task.workspace.display()
    );
}

fn print_agent(agent: &agent_store::RuntimeAgent) {
    println!(
        "{}\t{:?}\ttask={}\tparent={}\tprofile={}\tlabel={}\tmode={}\tturn={}/{}\ttool={}\ttokens={}/{}\ttimeout={}\t{}",
        agent.id,
        agent.status,
        agent.task_id,
        agent
            .parent_id
            .map_or_else(|| "-".to_owned(), |id| id.to_string()),
        agent.profile.as_deref().unwrap_or("-"),
        agent.label.as_deref().unwrap_or("-"),
        if agent.background {
            "background"
        } else {
            "foreground"
        },
        agent.current_turn,
        agent
            .max_turns
            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        agent.current_tool.as_deref().unwrap_or("-"),
        agent
            .total_tokens
            .map_or_else(|| "-".to_owned(), |tokens| tokens.to_string()),
        agent
            .token_budget
            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        agent
            .timeout_seconds
            .map_or_else(|| "-".to_owned(), |value| value.to_string()),
        agent.workspace.display()
    );
}

async fn list_pending(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let interactions: Vec<RuntimeInteraction> = authorized_get(&state, "/v1/interactions").await?;
    for interaction in interactions {
        match &interaction.kind {
            InteractionKind::Approval { description, .. } => println!(
                "{}\tapproval\ttask={}\t{}",
                interaction.id, interaction.task_id, description
            ),
            InteractionKind::Question { question, .. } => println!(
                "{}\tquestion\ttask={}\t{}",
                interaction.id, interaction.task_id, question
            ),
        }
    }
    Ok(())
}

async fn resolve_pending(home: &Path, id: uuid::Uuid, decision: ApprovalArg) -> Result<()> {
    let resolution = match decision {
        ApprovalArg::AllowOnce => InteractionResolution::AllowOnce,
        ApprovalArg::Deny => InteractionResolution::Deny,
        ApprovalArg::AlwaysAllow => InteractionResolution::AlwaysAllow,
    };
    resolve_interaction(home, id, resolution).await
}

async fn answer_pending(home: &Path, id: uuid::Uuid, answer: Vec<String>) -> Result<()> {
    let answer = answer.join(" ");
    if answer.trim().is_empty() {
        bail!("answer must not be empty");
    }
    resolve_interaction(home, id, InteractionResolution::Answer(Some(answer))).await
}

async fn resolve_interaction(
    home: &Path,
    id: uuid::Uuid,
    resolution: InteractionResolution,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/interactions/{id}/resolve",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(&ResolveInteraction { resolution })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected interaction resolution: {}",
            response.text().await?
        );
    }
    let interaction: RuntimeInteraction = response.json().await?;
    println!(
        "resolved\tid={}\ttask={}\tstatus={:?}",
        interaction.id, interaction.task_id, interaction.status
    );
    Ok(())
}

struct RuntimeApprover {
    url: String,
    token: String,
    task_id: uuid::Uuid,
    client: reqwest::Client,
}

pub fn runtime_approver(
    connection: Option<&RuntimeConnection>,
) -> Result<Option<Arc<dyn willdeep_core::Approver>>> {
    let Some(connection) = connection else {
        return Ok(None);
    };
    Ok(Some(Arc::new(RuntimeApprover {
        url: connection.url.clone(),
        token: connection.token.clone(),
        task_id: connection.task_id,
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()?,
    })))
}

impl RuntimeApprover {
    async fn interact(&self, kind: InteractionKind) -> Result<InteractionResolution> {
        let response = self
            .client
            .post(format!(
                "{}/v1/tasks/{}/interactions",
                self.url, self.task_id
            ))
            .header(TOKEN_HEADER, &self.token)
            .json(&CreateInteraction { kind })
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Runtime interaction failed: HTTP {}", response.status());
        }
        Ok(response.json().await?)
    }
}

#[async_trait]
impl willdeep_core::Approver for RuntimeApprover {
    async fn approve(
        &self,
        description: &str,
        always_allow_available: bool,
    ) -> willdeep_core::ApprovalDecision {
        match self
            .interact(InteractionKind::Approval {
                description: description.to_owned(),
                always_allow_available,
            })
            .await
        {
            Ok(InteractionResolution::AllowOnce) => willdeep_core::ApprovalDecision::AllowOnce,
            Ok(InteractionResolution::AlwaysAllow) if always_allow_available => {
                willdeep_core::ApprovalDecision::AlwaysAllow
            }
            _ => willdeep_core::ApprovalDecision::Deny,
        }
    }

    async fn ask_user(&self, request: willdeep_core::UserQuestion) -> Option<String> {
        match self
            .interact(InteractionKind::Question {
                question: request.question,
                options: request.options,
                multi_select: request.multi_select,
            })
            .await
        {
            Ok(InteractionResolution::Answer(answer)) => answer,
            _ => None,
        }
    }
}

async fn start(home: &Path, announce: bool) -> Result<()> {
    let paths = DaemonPaths::new(home);
    std::fs::create_dir_all(&paths.directory)?;
    if let Ok(state) = load_state(&paths.state)
        && probe(&state).await.is_ok()
    {
        if announce {
            println!(
                "WillDeep Runtime Daemon is already running (pid {}).",
                state.pid
            );
        }
        return Ok(());
    }
    let lock = match acquire_daemon_lock(&paths.lock) {
        Ok(lock) => lock,
        Err(mut error) => {
            let mut recovered_lock = None;
            for _ in 0..LOCK_RECOVERY_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Ok(state) = load_state(&paths.state)
                    && probe(&state).await.is_ok()
                {
                    if announce {
                        println!(
                            "WillDeep Runtime Daemon is already running (pid {}).",
                            state.pid
                        );
                    }
                    return Ok(());
                }
                match acquire_daemon_lock(&paths.lock) {
                    Ok(lock) => {
                        recovered_lock = Some(lock);
                        break;
                    }
                    Err(next_error) => error = next_error,
                }
            }
            recovered_lock
                .ok_or(error)
                .context("another Runtime Daemon owns a live lease or did not finish starting")?
        }
    };
    remove_stale_state(&paths.state)?;
    let mut lock_cleanup = OwnedLockCleanup::new(paths.lock.clone(), lock.token.clone());
    let stdout = append_log(&paths.log)?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe().context("resolve current WillDeep executable")?;
    let mut command = Command::new(executable);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env("WILLDEEP_DAEMON_LOCK_TOKEN", &lock.token);
    configure_detached(&mut command);
    let child = command.spawn().context("start Runtime Daemon")?;

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(state) = load_state(&paths.state)
            && probe(&state).await.is_ok()
        {
            if announce {
                println!(
                    "WillDeep Runtime Daemon started (pid {}, {}).",
                    state.pid, state.address
                );
            }
            lock_cleanup.disarm();
            return Ok(());
        }
    }
    bail!(
        "Runtime Daemon process {} did not become healthy; inspect {}",
        child.id(),
        paths.log.display()
    )
}

async fn status(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) => state,
        Err(_) => {
            println!("WillDeep Runtime Daemon is stopped.");
            return Ok(());
        }
    };
    match probe(&state).await {
        Ok(health) => {
            println!(
                "running\tpid={}\taddress={}\tversion={}\tuptime={}s",
                state.pid, state.address, health.version, health.uptime_seconds
            );
            Ok(())
        }
        Err(error) => bail!("Runtime Daemon state is stale (pid {}): {error}", state.pid),
    }
}

async fn capabilities_cli(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?.capabilities(None).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn runtime_client(state: &DaemonState) -> Result<willdeep_runtime_client::RuntimeClient> {
    #[cfg(unix)]
    if let Some(LocalTransportState::UnixSocket { path }) = &state.local_transport {
        return Ok(willdeep_runtime_client::RuntimeClient::new_unix_socket(
            path,
            state.token.clone(),
        )?);
    }
    #[cfg(windows)]
    if let Some(LocalTransportState::WindowsNamedPipe { name }) = &state.local_transport {
        return Ok(
            willdeep_runtime_client::RuntimeClient::new_windows_named_pipe(
                name,
                state.token.clone(),
            )?,
        );
    }
    Ok(willdeep_runtime_client::RuntimeClient::new(
        format!("http://{}", state.address),
        state.token.clone(),
    )?)
}

async fn stop(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) => state,
        Err(_) => {
            println!("WillDeep Runtime Daemon is already stopped.");
            return Ok(());
        }
    };
    runtime_client(&state)?
        .post_empty("/v1/shutdown")
        .await
        .context("contact Runtime Daemon")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !paths.state.exists() {
            println!("WillDeep Runtime Daemon stopped.");
            return Ok(());
        }
    }
    bail!("Runtime Daemon acknowledged shutdown but did not exit")
}

async fn logs(home: &Path, lines: usize, follow: bool) -> Result<()> {
    let path = DaemonPaths::new(home).log;
    let mut file = File::open(&path)
        .with_context(|| format!("open Runtime Daemon log at {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let tail = content
        .lines()
        .rev()
        .take(lines.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if !tail.is_empty() {
        println!("{tail}");
    }
    if !follow {
        return Ok(());
    }
    let mut position = file.seek(SeekFrom::End(0))?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                let length = file.metadata()?.len();
                if length < position {
                    position = 0;
                }
                if length > position {
                    file.seek(SeekFrom::Start(position))?;
                    let mut chunk = String::new();
                    file.read_to_string(&mut chunk)?;
                    print!("{chunk}");
                    position = length;
                }
            }
        }
    }
}

async fn run(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    std::fs::create_dir_all(&paths.directory)?;
    let lock_token = std::env::var("WILLDEEP_DAEMON_LOCK_TOKEN")
        .context("daemon run is an internal command requiring an acquired lock")?;
    let lock = load_daemon_lock(&paths.lock)?;
    if lock.token != lock_token {
        bail!("Runtime Daemon lock ownership changed before startup");
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind Runtime Daemon control endpoint")?;
    let address = listener.local_addr()?;
    let started_at = now();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let (local_listener, local_transport) = local_transport::bind(&paths.local_socket, &token)?;
    let state = DaemonState {
        schema: STATE_SCHEMA,
        version: willdeep_core::VERSION.to_owned(),
        pid: std::process::id(),
        address,
        token,
        started_at,
        local_transport: Some(local_transport),
    };
    write_state(&paths.state, &state)?;
    let cleanup = StateCleanup {
        path: paths.state.clone(),
        token: state.token.clone(),
        lock_path: paths.lock.clone(),
        lock_token,
        local_socket: paths.local_socket.clone(),
    };
    let lock_heartbeat = spawn_lock_heartbeat(paths.lock.clone(), cleanup.lock_token.clone());
    let shutdown_signal = Arc::new(Notify::new());
    let events = Arc::new(EventLog::open(paths.events.clone())?);
    let agents = Arc::new(AgentStore::open(paths.agents.clone())?);
    let agent_commands = Arc::new(AgentCommandStore::open(paths.agent_commands.clone())?);
    let sessions = Arc::new(session_store::RuntimeSessionStore::open(
        paths.runtime_sessions.clone(),
        home,
    )?);
    let (turn_scheduler, mut scheduled_sessions) = tokio::sync::mpsc::unbounded_channel();
    let tasks = Arc::new(TaskManager::open(TaskManagerOptions {
        path: paths.tasks.clone(),
        interactions_path: paths.interactions.clone(),
        home: home.to_path_buf(),
        events: events.clone(),
        agents: agents.clone(),
        sessions: sessions.clone(),
        turn_scheduler,
        runtime_url: format!("http://{address}"),
        runtime_token: state.token.clone(),
    })?);
    tasks.report_herdr_state().await;
    let server_state = Arc::new(ServerState {
        home: home.to_path_buf(),
        token: state.token.clone(),
        started_at,
        shutdown: shutdown_signal.clone(),
        events: events.clone(),
        tasks: tasks.clone(),
        agents: agents.clone(),
        agent_commands: agent_commands.clone(),
        sessions: sessions.clone(),
        workspaces: tasks.workspaces.clone(),
        diff_review_lock: Arc::new(tokio::sync::Mutex::new(())),
        idempotency: Arc::new(control_api::IdempotencyStore::open(
            paths.idempotency.clone(),
        )?),
        local_transport: state.local_transport.clone(),
        tools: tasks.tools.clone(),
    });
    let scheduler_state = server_state.clone();
    tokio::spawn(async move {
        while let Some(session_id) = scheduled_sessions.recv().await {
            let claimed = scheduler_state.sessions.claim_next(session_id);
            let Ok(Some(claimed)) = claimed else {
                continue;
            };
            let turn_id = claimed.metadata.id;
            match scheduler_state.tasks.submit(claimed.request).await {
                Ok(task) => {
                    if task.status == RuntimeTaskStatus::Cancelled {
                        let _ = scheduler_state.tasks.schedule_session(session_id);
                        continue;
                    }
                    let _ = scheduler_state.events.append(
                        "turn.started",
                        format!(
                            "session_id={session_id} turn_id={turn_id} task_id={}",
                            task.id
                        ),
                    );
                }
                Err(error) => {
                    let _ = scheduler_state.sessions.complete_claim_failure(
                        turn_id,
                        format!("start queued Turn task: {error:#}"),
                    );
                    let _ = scheduler_state.tasks.schedule_session(session_id);
                }
            }
        }
    });
    for session_id in sessions.schedulable_sessions()? {
        tasks.schedule_session(session_id)?;
    }
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/capabilities", get(capabilities_handler))
        .route("/v1/api", post(control_api::handler))
        .route("/v1/events", get(events_handler))
        .route(
            "/v1/events/stream",
            get(event_stream::events_stream_handler),
        )
        .route(
            "/v1/events/stream.ndjson",
            get(event_stream::events_ndjson_handler),
        )
        .route("/v1/agents", get(agents_handler))
        .route("/v1/agents/{id}", get(agent_handler))
        .route(
            "/v1/agents/{id}/worktree-review",
            get(worktree_review::review_handler),
        )
        .route(
            "/v1/agents/{id}/worktree-merge",
            post(worktree_review::merge_handler),
        )
        .route(
            "/v1/worktrees/audit",
            get(worktree_maintenance::audit_handler),
        )
        .route(
            "/v1/agents/{id}/worktree-quarantine",
            post(worktree_maintenance::quarantine_handler),
        )
        .route(
            "/v1/workspaces",
            get(workspace_store::list_handler).post(workspace_store::register_handler),
        )
        .route(
            "/v1/workspaces/ensure",
            post(workspace_store::ensure_handler),
        )
        .route(
            "/v1/workspaces/{id}",
            get(workspace_store::get_handler).delete(workspace_store::remove_handler),
        )
        .route(
            "/v1/workspaces/{id}/activate",
            post(workspace_store::activate_handler),
        )
        .route(
            "/v1/agents/{id}/stop",
            post(agent_control::stop_agent_handler),
        )
        .route(
            "/v1/agents/{id}/retry",
            post(agent_control::retry_agent_handler),
        )
        .route(
            "/v1/agents/{id}/instructions",
            post(agent_control::instruct_agent_handler),
        )
        .route("/v1/diffs", get(diff_review::snapshot_handler))
        .route("/v1/diffs/{id}/content", get(diff_review::content_handler))
        .route(
            "/v1/diffs/{id}/reviews",
            get(diff_review::reviews_handler).post(diff_review::review_handler),
        )
        .route(
            "/v1/diffs/{id}/verifications",
            get(diff_review::verifications_handler).post(diff_review::verification_handler),
        )
        .route(
            "/v1/diffs/{id}/attributions",
            get(diff_review::attributions_handler),
        )
        .route(
            "/v1/diffs/{id}/commit-preview",
            get(diff_review::commit_preview_handler),
        )
        .route("/v1/diffs/{id}/revert", post(diff_review::revert_handler))
        .route("/v1/tasks", get(tasks_handler).post(submit_task_handler))
        .route(
            "/v1/sessions",
            get(session_store::sessions_handler).post(session_store::create_session_handler),
        )
        .route(
            "/v1/sessions/search",
            get(session_store::search_sessions_handler),
        )
        .route(
            "/v1/sessions/{id}",
            get(session_store::session_handler).delete(session_store::delete_session_handler),
        )
        .route(
            "/v1/sessions/{id}/rename",
            post(session_store::rename_session_handler),
        )
        .route(
            "/v1/sessions/{id}/fork",
            post(session_store::fork_session_handler),
        )
        .route(
            "/v1/sessions/{id}/archive",
            post(session_store::archive_session_handler),
        )
        .route(
            "/v1/sessions/{id}/unarchive",
            post(session_store::unarchive_session_handler),
        )
        .route(
            "/v1/sessions/{id}/export",
            get(session_store::export_session_handler),
        )
        .route(
            "/v1/sessions/{id}/turns",
            get(session_store::turns_handler).post(session_store::create_turn_handler),
        )
        .route("/v1/turns/{id}", get(session_store::turn_handler))
        .route(
            "/v1/turns/{id}/stop",
            post(session_store::stop_turn_handler),
        )
        .route("/v1/tasks/{id}", get(task_handler))
        .route("/v1/tasks/{id}/stop", post(stop_task_handler))
        .route(
            "/v1/tasks/{id}/agent-commands",
            get(agent_control::agent_commands_handler),
        )
        .route(
            "/v1/tasks/{task_id}/agent-commands/{command_id}/resolve",
            post(agent_control::resolve_agent_command_handler),
        )
        .route(
            "/v1/tasks/{id}/interactions",
            post(create_interaction_handler),
        )
        .route("/v1/interactions", get(interactions_handler))
        .route(
            "/v1/interactions/{id}/resolve",
            post(resolve_interaction_handler),
        )
        .route("/v1/shutdown", post(shutdown_handler))
        .layer(middleware::from_fn(server_version_header))
        .with_state(server_state);
    events.append(
        "daemon.started",
        format!("pid={} address={address}", std::process::id()),
    )?;
    eprintln!(
        "WillDeep Runtime Daemon {} listening on {} and local transport (pid {})",
        willdeep_core::VERSION,
        address,
        std::process::id()
    );
    let tcp_shutdown = shutdown_signal.clone();
    let local_shutdown = shutdown_signal.clone();
    let tcp_server = axum::serve(listener, app.clone())
        .with_graceful_shutdown(async move { tcp_shutdown.notified().await });
    let local_server = axum::serve(local_listener, app)
        .with_graceful_shutdown(async move { local_shutdown.notified().await });
    tokio::try_join!(tcp_server, local_server).context("run Runtime Daemon control servers")?;
    tasks.cancel_all().await;
    events.append("daemon.stopped", format!("pid={}", std::process::id()))?;
    lock_heartbeat.abort();
    drop(cleanup);
    eprintln!("WillDeep Runtime Daemon stopped");
    Ok(())
}

async fn health(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let mut response = Json(Health {
        status: "ok".to_owned(),
        version: willdeep_core::VERSION.to_owned(),
        pid: std::process::id(),
        uptime_seconds: now().saturating_sub(state.started_at),
        event_sequence: state.events.latest_sequence(),
    })
    .into_response();
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    Ok(response)
}

async fn capabilities_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<uuid::Uuid>().ok());
    Ok(Json(willdeep_runtime_protocol::ApiResponse::ok(
        runtime_capabilities(&state),
        willdeep_core::VERSION,
        request_id,
    ))
    .into_response())
}

fn runtime_capabilities(state: &ServerState) -> willdeep_runtime_protocol::RuntimeCapabilities {
    let mut capabilities =
        willdeep_runtime_protocol::RuntimeCapabilities::current(willdeep_core::VERSION);
    match state.local_transport {
        Some(LocalTransportState::UnixSocket { .. }) => capabilities
            .transports
            .push(willdeep_runtime_protocol::TransportKind::UnixSocket),
        Some(LocalTransportState::WindowsNamedPipe { .. }) => capabilities
            .transports
            .push(willdeep_runtime_protocol::TransportKind::WindowsNamedPipe),
        None => {}
    }
    capabilities
}

async fn server_version_header(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    response
}

async fn shutdown_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let tasks = state.tasks.clone();
    let shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        tasks.cancel_all().await;
        shutdown.notify_waiters();
    });
    Ok(StatusCode::ACCEPTED.into_response())
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

fn default_event_limit() -> usize {
    200
}

async fn events_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let events = state
        .events
        .read_after(query.after, query.limit.clamp(1, 1_000))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut response = Json(events).into_response();
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    Ok(response)
}

async fn tasks_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(state.tasks.list().await).into_response())
}

async fn agents_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let agents = state
        .agents
        .list()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(agents).into_response())
}

async fn agent_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .agents
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .tasks
        .get(id)
        .await
        .map(|task| Json(task).into_response())
        .ok_or(StatusCode::NOT_FOUND)
}

async fn submit_task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(mut request): Json<SubmitTask>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = state
        .workspaces
        .ensure_registered(&request.workspace)
        .map_err(|error| {
            eprintln!("register submitted Runtime Workspace: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    request.workspace = workspace.root;
    if request.profile.is_none() {
        request.profile = workspace.provider_profile;
    }
    let task = state.tasks.submit(request).await.map_err(|error| {
        eprintln!("Runtime task submission failed: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

async fn stop_task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .tasks
        .cancel(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn interactions_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(state.tasks.pending_interactions().await).into_response())
}

async fn create_interaction_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(task_id): AxumPath<uuid::Uuid>,
    Json(request): Json<CreateInteraction>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let receiver = state
        .tasks
        .create_interaction(task_id, request.kind)
        .await
        .map_err(|error| {
            eprintln!("create Runtime interaction: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    receiver
        .await
        .map(Json)
        .map(IntoResponse::into_response)
        .map_err(|_| StatusCode::GONE)
}

async fn resolve_interaction_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<ResolveInteraction>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .tasks
        .resolve_interaction(id, request.resolution)
        .await
        .map_err(|error| {
            eprintln!("resolve Runtime interaction: {error:#}");
            StatusCode::BAD_REQUEST
        })?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

fn authorize(state: &ServerState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(state.token.as_str())
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Health {
    status: String,
    version: String,
    pid: u32,
    uptime_seconds: u64,
    #[serde(default)]
    event_sequence: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RuntimeDiagnostic {
    pub status: &'static str,
    pub version: Option<String>,
    pub uptime_seconds: Option<u64>,
    pub transport: Option<&'static str>,
}

pub(crate) async fn diagnostic(home: &Path) -> RuntimeDiagnostic {
    let path = DaemonPaths::new(home).state;
    let Ok(state) = load_state(&path) else {
        return RuntimeDiagnostic {
            status: "stopped",
            version: None,
            uptime_seconds: None,
            transport: None,
        };
    };
    let transport = match &state.local_transport {
        Some(LocalTransportState::UnixSocket { .. }) => "unix_socket",
        Some(LocalTransportState::WindowsNamedPipe { .. }) => "windows_named_pipe",
        None => "loopback_http",
    };
    match probe(&state).await {
        Ok(health) => RuntimeDiagnostic {
            status: "running",
            version: Some(health.version),
            uptime_seconds: Some(health.uptime_seconds),
            transport: Some(transport),
        },
        Err(_) => RuntimeDiagnostic {
            status: "stale",
            version: None,
            uptime_seconds: None,
            transport: Some(transport),
        },
    }
}

async fn probe(state: &DaemonState) -> Result<Health> {
    if state.schema != STATE_SCHEMA {
        bail!("unsupported state schema {}", state.schema);
    }
    Ok(runtime_client(state)?.get_json("/v1/health").await?)
}

async fn fetch_events(state: &DaemonState, after: u64) -> Result<Vec<RuntimeEvent>> {
    Ok(runtime_client(state)?
        .get_json(&format!("/v1/events?after={after}&limit=200"))
        .await?)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
        .expect("daemon HTTP client")
}

struct DaemonPaths {
    directory: PathBuf,
    state: PathBuf,
    log: PathBuf,
    events: PathBuf,
    tasks: PathBuf,
    agents: PathBuf,
    agent_commands: PathBuf,
    idempotency: PathBuf,
    runtime_sessions: PathBuf,
    lock: PathBuf,
    interactions: PathBuf,
    local_socket: PathBuf,
}

impl DaemonPaths {
    fn new(home: &Path) -> Self {
        let directory = home.join("runtime");
        Self {
            state: directory.join("daemon.json"),
            log: directory.join("daemon.log"),
            events: directory.join("events.ndjson"),
            tasks: directory.join("tasks.json"),
            agents: directory.join("agents.json"),
            agent_commands: directory.join("agent-commands.json"),
            idempotency: directory.join("idempotency.json"),
            runtime_sessions: directory.join("sessions.json"),
            lock: directory.join("daemon.lock"),
            interactions: directory.join("interactions.json"),
            local_socket: directory.join("control.sock"),
            directory,
        }
    }
}

struct TaskManagerOptions {
    path: PathBuf,
    interactions_path: PathBuf,
    home: PathBuf,
    events: Arc<EventLog>,
    agents: Arc<AgentStore>,
    sessions: Arc<session_store::RuntimeSessionStore>,
    turn_scheduler: tokio::sync::mpsc::UnboundedSender<uuid::Uuid>,
    runtime_url: String,
    runtime_token: String,
}

impl TaskManager {
    fn open(options: TaskManagerOptions) -> Result<Self> {
        let TaskManagerOptions {
            path,
            interactions_path,
            home,
            events,
            agents,
            sessions,
            turn_scheduler,
            runtime_url,
            runtime_token,
        } = options;
        let mut tasks = load_tasks(&path)?;
        let mut interactions = load_interactions(&interactions_path)?;
        let mut recovered = false;
        let mut recovered_tasks = Vec::new();
        for task in tasks.values_mut() {
            if matches!(
                task.status,
                RuntimeTaskStatus::Queued
                    | RuntimeTaskStatus::Running
                    | RuntimeTaskStatus::Cancelling
                    | RuntimeTaskStatus::WaitingApproval
                    | RuntimeTaskStatus::WaitingAnswer
            ) {
                task.status = RuntimeTaskStatus::Interrupted;
                task.pid = None;
                task.completed_at = Some(now());
                task.error = Some("Runtime restarted while task was active".to_owned());
                recovered = true;
                recovered_tasks.push(task.clone());
            }
            let agent = if let Some(session_id) = task.session_id {
                let session = sessions
                    .get(session_id)?
                    .with_context(|| format!("Runtime Session {session_id} for recovered task"))?;
                agents.ensure_session_root(
                    session.root_agent_id,
                    task.id,
                    task.workspace.clone(),
                    task.profile.clone(),
                    agent_status(task.status),
                )?
            } else {
                agents.ensure_root(
                    task.id,
                    task.workspace.clone(),
                    task.profile.clone(),
                    agent_status(task.status),
                )?
            };
            if task.agent_id != Some(agent.id) {
                task.agent_id = Some(agent.id);
                recovered = true;
            }
        }
        if recovered {
            persist_tasks(&path, &tasks)?;
        }
        let mut interactions_recovered = false;
        let mut recovered_interactions = Vec::new();
        for interaction in interactions.values_mut() {
            if interaction.status == InteractionStatus::Pending {
                interaction.status = InteractionStatus::Cancelled;
                interaction.resolution = Some(match &interaction.kind {
                    InteractionKind::Approval { .. } => InteractionResolution::Deny,
                    InteractionKind::Question { .. } => InteractionResolution::Answer(None),
                });
                interaction.resolved_at = Some(now());
                interactions_recovered = true;
                recovered_interactions.push((interaction.id, interaction.task_id));
            }
        }
        if interactions_recovered {
            persist_interactions(&interactions_path, &interactions)?;
        }
        for task in &recovered_tasks {
            let error = task.error.as_deref().unwrap_or_default();
            events.append(
                "task.interrupted",
                format!(
                    "task_id={} session_id={} turn_id={} exit_code=none error={error}",
                    task.id,
                    task.session_id
                        .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                    task.turn_id
                        .map_or_else(|| "none".to_owned(), |id| id.to_string())
                ),
            )?;
            if let (Some(session_id), Some(turn_id)) = (task.session_id, task.turn_id) {
                events.append(
                    "turn.interrupted",
                    format!(
                        "session_id={session_id} turn_id={turn_id} task_id={} exit_code=none error={error}",
                        task.id
                    ),
                )?;
            }
        }
        for (interaction_id, task_id) in recovered_interactions {
            events.append(
                "task.interaction_cancelled",
                format!(
                    "task_id={task_id} interaction_id={interaction_id} reason=runtime_restarted"
                ),
            )?;
        }
        let workspaces = Arc::new(workspace_store::WorkspaceStore::open(
            home.join("runtime/workspaces.json"),
        )?);
        let tools = Arc::new(tool_store::ToolStore::open(
            home.join("runtime/tools.json"),
        )?);
        Ok(Self {
            path,
            home,
            events,
            agents,
            tools,
            sessions,
            workspaces,
            runtime_url,
            runtime_token,
            tasks: RwLock::new(tasks),
            persistence: AsyncMutex::new(()),
            cancellations: Mutex::new(HashMap::new()),
            interactions_path,
            interactions: RwLock::new(interactions),
            interaction_waiters: Mutex::new(HashMap::new()),
            turn_scheduler,
            herdr: herdr::HerdrReporter::detect(),
        })
    }

    async fn list(&self) -> Vec<RuntimeTask> {
        let mut tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.created_at));
        tasks
    }

    fn schedule_session(&self, session_id: uuid::Uuid) -> Result<()> {
        self.turn_scheduler
            .send(session_id)
            .map_err(|_| anyhow::anyhow!("Runtime Turn scheduler stopped"))
    }

    async fn get(&self, id: uuid::Uuid) -> Option<RuntimeTask> {
        self.tasks.read().await.get(&id).cloned()
    }

    async fn pending_interactions(&self) -> Vec<RuntimeInteraction> {
        let mut interactions = self
            .interactions
            .read()
            .await
            .values()
            .filter(|item| item.status == InteractionStatus::Pending)
            .cloned()
            .collect::<Vec<_>>();
        interactions.sort_by_key(|item| item.created_at);
        interactions
    }

    async fn create_interaction(
        &self,
        task_id: uuid::Uuid,
        kind: InteractionKind,
    ) -> Result<tokio::sync::oneshot::Receiver<InteractionResolution>> {
        let status = match &kind {
            InteractionKind::Approval { .. } => RuntimeTaskStatus::WaitingApproval,
            InteractionKind::Question { .. } => RuntimeTaskStatus::WaitingAnswer,
        };
        let interaction = RuntimeInteraction {
            id: uuid::Uuid::new_v4(),
            task_id,
            kind,
            status: InteractionStatus::Pending,
            resolution: None,
            created_at: now(),
            resolved_at: None,
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let _persistence = self.persistence.lock().await;
        let task_snapshot = {
            let mut tasks = self.tasks.write().await;
            let task = tasks.get_mut(&task_id).context("Runtime task not found")?;
            if !matches!(task.status, RuntimeTaskStatus::Running) {
                bail!("Runtime task is not running");
            }
            task.status = status;
            tasks.clone()
        };
        let interaction_snapshot = {
            let mut interactions = self.interactions.write().await;
            interactions.insert(interaction.id, interaction.clone());
            interactions.clone()
        };
        persist_tasks(&self.path, &task_snapshot)?;
        persist_interactions(&self.interactions_path, &interaction_snapshot)?;
        self.agents
            .set_status_for_task(interaction.task_id, agent_status(status), None)?;
        self.sessions.set_task_waiting(
            interaction.task_id,
            match status {
                RuntimeTaskStatus::WaitingApproval => {
                    session_store::RuntimeTurnStatus::WaitingApproval
                }
                RuntimeTaskStatus::WaitingAnswer => session_store::RuntimeTurnStatus::WaitingAnswer,
                _ => session_store::RuntimeTurnStatus::Running,
            },
        )?;
        self.interaction_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime interaction waiter lock poisoned"))?
            .insert(interaction.id, sender);
        self.events.append(
            match &interaction.kind {
                InteractionKind::Approval { .. } => "task.waiting_approval",
                InteractionKind::Question { .. } => "task.waiting_answer",
            },
            format!(
                "task_id={} interaction_id={}",
                interaction.task_id, interaction.id
            ),
        )?;
        self.report_herdr_state().await;
        Ok(receiver)
    }

    async fn resolve_interaction(
        &self,
        id: uuid::Uuid,
        resolution: InteractionResolution,
    ) -> Result<Option<RuntimeInteraction>> {
        let _persistence = self.persistence.lock().await;
        let (interaction, interaction_snapshot) = {
            let mut interactions = self.interactions.write().await;
            let Some(interaction) = interactions.get_mut(&id) else {
                return Ok(None);
            };
            if interaction.status != InteractionStatus::Pending {
                bail!("Runtime interaction is no longer pending");
            }
            validate_resolution(&interaction.kind, &resolution)?;
            interaction.status = InteractionStatus::Resolved;
            interaction.resolution = Some(resolution.clone());
            interaction.resolved_at = Some(now());
            (interaction.clone(), interactions.clone())
        };
        let task_snapshot = {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(&interaction.task_id)
                && matches!(
                    task.status,
                    RuntimeTaskStatus::WaitingApproval | RuntimeTaskStatus::WaitingAnswer
                )
            {
                task.status = RuntimeTaskStatus::Running;
            }
            tasks.clone()
        };
        persist_interactions(&self.interactions_path, &interaction_snapshot)?;
        persist_tasks(&self.path, &task_snapshot)?;
        self.agents
            .set_status_for_task(interaction.task_id, RuntimeAgentStatus::Running, None)?;
        self.sessions.set_task_waiting(
            interaction.task_id,
            session_store::RuntimeTurnStatus::Running,
        )?;
        let sender = self
            .interaction_waiters
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime interaction waiter lock poisoned"))?
            .remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(resolution);
        }
        self.events.append(
            "task.interaction_resolved",
            format!(
                "task_id={} interaction_id={}",
                interaction.task_id, interaction.id
            ),
        )?;
        self.report_herdr_state().await;
        Ok(Some(interaction))
    }

    async fn submit(self: &Arc<Self>, mut request: SubmitTask) -> Result<RuntimeTask> {
        if request.prompt.trim().is_empty() && request.attachments.is_empty() {
            bail!("task prompt and attachments must not both be empty");
        }
        let workspace = self.workspaces.ensure_registered(&request.workspace)?;
        request.workspace = workspace.root;
        request.workspace_access = Some(workspace.access);
        request.workspace_skills = Some(workspace.skills);
        request.workspace_mcp_servers = Some(workspace.mcp_servers);
        if request.profile.is_none() {
            request.profile = workspace.provider_profile;
        }
        if let Some(config) = request.config.as_mut() {
            *config = config
                .canonicalize()
                .with_context(|| format!("invalid config: {}", config.display()))?;
        }

        let id = uuid::Uuid::new_v4();
        let mut task = RuntimeTask {
            id,
            session_id: request.session_id,
            turn_id: request.turn_id,
            agent_id: None,
            event_start_sequence: 0,
            status: RuntimeTaskStatus::Queued,
            workspace: request.workspace.clone(),
            profile: request.profile.clone(),
            pid: None,
            created_at: now(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            failure_domain: None,
            error: None,
        };
        let agent = if let Some(session_id) = request.session_id {
            let session = self
                .sessions
                .get(session_id)?
                .context("Runtime Session not found")?;
            self.agents.ensure_session_root(
                session.root_agent_id,
                id,
                request.workspace.clone(),
                request.profile.clone(),
                RuntimeAgentStatus::Queued,
            )?
        } else {
            self.agents.ensure_root(
                id,
                request.workspace.clone(),
                request.profile.clone(),
                RuntimeAgentStatus::Queued,
            )?
        };
        task.agent_id = Some(agent.id);
        self.events.append(
            "agent.created",
            format!("agent_id={} task_id={id} parent_id=none", agent.id),
        )?;
        self.insert_and_persist(task.clone()).await?;
        if let Some(turn_id) = task.turn_id
            && !self.sessions.bind_task(turn_id, id)?
        {
            task.status = RuntimeTaskStatus::Cancelled;
            task.completed_at = Some(now());
            self.insert_and_persist(task.clone()).await?;
            self.agents.set_status_for_task(
                id,
                RuntimeAgentStatus::Cancelled,
                Some("Turn was cancelled before task startup".to_owned()),
            )?;
            self.events.append(
                "task.cancelled",
                format!("task_id={id} session_id={} turn_id={turn_id} exit_code=none error=cancelled before startup", task.session_id.map_or_else(|| "none".to_owned(), |value| value.to_string())),
            )?;
            return Ok(task);
        }
        task.event_start_sequence = self
            .events
            .append("task.queued", format!("task_id={id}"))?
            .sequence;
        self.insert_and_persist(task.clone()).await?;

        task.status = RuntimeTaskStatus::Running;
        task.pid = None;
        task.started_at = Some(now());
        self.insert_and_persist(task.clone()).await?;
        self.agents
            .set_status_for_task(id, RuntimeAgentStatus::Running, None)?;
        self.events.append(
            "agent.running",
            format!("agent_id={} task_id={id}", agent.id),
        )?;
        self.events
            .append("task.started", format!("task_id={id} mode=in_process"))?;
        let cancellation = Arc::new(Notify::new());
        self.cancellations
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime task cancellation lock poisoned"))?
            .insert(id, cancellation.clone());

        let manager = self.clone();
        let home = self.home.clone();
        let connection = RuntimeConnection {
            url: self.runtime_url.clone(),
            token: self.runtime_token.clone(),
            task_id: id,
        };
        let sink: Arc<dyn willdeep_core::EventSink> = Arc::new(RuntimeEventSink {
            task_id: id,
            session_id: task.session_id,
            turn_id: task.turn_id,
            root_agent_id: agent.id,
            home: self.home.clone(),
            workspace: request.workspace.clone(),
            events: self.events.clone(),
            agents: self.agents.clone(),
            tools: self.tools.clone(),
            diff_baselines: AsyncMutex::new(HashMap::new()),
            child_workspaces: AsyncMutex::new(HashMap::new()),
        });
        tokio::spawn(async move {
            let execution = crate::harness::execute_runtime(&home, request, connection, sink);
            let (result, cancelled) = tokio::select! {
                result = execution => (Some(result), false),
                _ = cancellation.notified() => {
                    (None, true)
                }
            };
            if let Ok(mut cancellations) = manager.cancellations.lock() {
                cancellations.remove(&id);
            }
            let (final_status, error, failure_domain) = match result {
                _ if cancelled => (RuntimeTaskStatus::Cancelled, None, None),
                Some(Ok(outcome)) => {
                    let completed = serde_json::json!({
                        "type":"completed",
                        "turns":outcome.turns,
                        "text":outcome.final_text,
                        "session_id":outcome.session_id,
                    });
                    let _ = manager
                        .events
                        .append("task.output", format!("task_id={id} {completed}"));
                    (RuntimeTaskStatus::Completed, None, None)
                }
                Some(Err(error)) => (
                    RuntimeTaskStatus::Failed,
                    Some(format!("{error:#}")),
                    Some(crate::runtime_failure_domain(&error)),
                ),
                None => (RuntimeTaskStatus::Cancelled, None, None),
            };
            if let Err(error) = manager
                .finish(id, final_status, None, error, failure_domain)
                .await
            {
                eprintln!("persist Runtime task {id} completion: {error:#}");
            }
        });
        Ok(task)
    }

    async fn cancel(&self, id: uuid::Uuid) -> Result<Option<RuntimeTask>> {
        let cancellation = self
            .cancellations
            .lock()
            .ok()
            .and_then(|items| items.get(&id).cloned());
        if let Some(cancellation) = cancellation {
            let _persistence = self.persistence.lock().await;
            let task = {
                let mut tasks = self.tasks.write().await;
                let Some(task) = tasks.get_mut(&id) else {
                    return Ok(None);
                };
                task.status = RuntimeTaskStatus::Cancelling;
                let task = task.clone();
                persist_tasks(&self.path, &tasks)?;
                task
            };
            cancellation.notify_one();
            self.events
                .append("task.cancellation_requested", format!("task_id={id}"))?;
            drop(_persistence);
            self.report_herdr_state().await;
            return Ok(Some(task));
        }
        Ok(self.get(id).await)
    }

    async fn cancel_all(&self) {
        let cancellations = self
            .cancellations
            .lock()
            .map(|items| items.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for cancellation in cancellations {
            cancellation.notify_one();
        }
        for _ in 0..50 {
            if self
                .cancellations
                .lock()
                .is_ok_and(|items| items.is_empty())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn insert_and_persist(&self, task: RuntimeTask) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task.id, task);
            tasks.clone()
        };
        persist_tasks(&self.path, &snapshot)?;
        drop(_persistence);
        self.report_herdr_state().await;
        Ok(())
    }

    async fn finish(
        self: &Arc<Self>,
        id: uuid::Uuid,
        status: RuntimeTaskStatus,
        exit_code: Option<i32>,
        error: Option<String>,
        failure_domain: Option<willdeep_runtime_protocol::FailureDomain>,
    ) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let (finished_task, snapshot) = {
            let mut tasks = self.tasks.write().await;
            let task = tasks.get_mut(&id).context("Runtime task disappeared")?;
            task.status = status;
            task.completed_at = Some(now());
            task.exit_code = exit_code;
            task.failure_domain = failure_domain;
            task.error = error.clone();
            (task.clone(), tasks.clone())
        };
        persist_tasks(&self.path, &snapshot)?;
        self.agents
            .set_status_for_task(id, agent_status(status), error.clone())?;
        self.events.append(
            match status {
                RuntimeTaskStatus::Completed => "task.completed",
                RuntimeTaskStatus::Cancelled => "task.cancelled",
                RuntimeTaskStatus::Interrupted => "task.interrupted",
                _ => "task.failed",
            },
            format!(
                "task_id={id} session_id={} turn_id={} exit_code={} failure_domain={} error={}",
                finished_task
                    .session_id
                    .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                finished_task
                    .turn_id
                    .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                failure_domain.map_or("none", |domain| match domain {
                    willdeep_runtime_protocol::FailureDomain::Provider => "provider",
                    willdeep_runtime_protocol::FailureDomain::Policy => "policy",
                    willdeep_runtime_protocol::FailureDomain::Tool => "tool",
                    willdeep_runtime_protocol::FailureDomain::Harness => "harness",
                    willdeep_runtime_protocol::FailureDomain::Internal => "internal",
                    willdeep_runtime_protocol::FailureDomain::Unknown => "unknown",
                }),
                error.clone().unwrap_or_default()
            ),
        )?;
        let runtime_session = self.sessions.complete_task(id, status, error.clone())?;
        if let (Some(session_id), Some(turn_id)) = (finished_task.session_id, finished_task.turn_id)
        {
            self.events.append(
                match status {
                    RuntimeTaskStatus::Completed => "turn.completed",
                    RuntimeTaskStatus::Cancelled => "turn.cancelled",
                    RuntimeTaskStatus::Interrupted => "turn.interrupted",
                    _ => "turn.failed",
                },
                format!(
                    "session_id={session_id} turn_id={turn_id} task_id={id} exit_code={} error={}",
                    exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                    error.clone().unwrap_or_default()
                ),
            )?;
        }
        drop(_persistence);
        self.cancel_task_interactions(id).await?;
        if let Some(session_id) = runtime_session {
            self.schedule_session(session_id)?;
        }
        self.report_herdr_state().await;
        Ok(())
    }

    async fn report_herdr_state(&self) {
        let Some(reporter) = &self.herdr else {
            return;
        };
        let statuses = self
            .tasks
            .read()
            .await
            .values()
            .map(|task| task.status)
            .collect::<Vec<_>>();
        reporter.report(statuses.into_iter());
    }

    async fn cancel_task_interactions(&self, task_id: uuid::Uuid) -> Result<()> {
        let pending = self
            .interactions
            .read()
            .await
            .values()
            .filter(|item| item.task_id == task_id && item.status == InteractionStatus::Pending)
            .map(|item| {
                let resolution = match &item.kind {
                    InteractionKind::Approval { .. } => InteractionResolution::Deny,
                    InteractionKind::Question { .. } => InteractionResolution::Answer(None),
                };
                (item.id, resolution)
            })
            .collect::<Vec<_>>();
        for (id, resolution) in pending {
            let _ = self.resolve_interaction(id, resolution).await;
        }
        Ok(())
    }
}

fn validate_resolution(kind: &InteractionKind, resolution: &InteractionResolution) -> Result<()> {
    match (kind, resolution) {
        (InteractionKind::Approval { .. }, InteractionResolution::AllowOnce)
        | (InteractionKind::Approval { .. }, InteractionResolution::Deny)
        | (InteractionKind::Question { .. }, InteractionResolution::Answer(_)) => Ok(()),
        (
            InteractionKind::Approval {
                always_allow_available: true,
                ..
            },
            InteractionResolution::AlwaysAllow,
        ) => Ok(()),
        _ => bail!("resolution does not match the pending interaction"),
    }
}

fn agent_status(status: RuntimeTaskStatus) -> RuntimeAgentStatus {
    match status {
        RuntimeTaskStatus::Queued => RuntimeAgentStatus::Queued,
        RuntimeTaskStatus::Running | RuntimeTaskStatus::Cancelling => RuntimeAgentStatus::Running,
        RuntimeTaskStatus::WaitingApproval => RuntimeAgentStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer => RuntimeAgentStatus::WaitingAnswer,
        RuntimeTaskStatus::Completed => RuntimeAgentStatus::Completed,
        RuntimeTaskStatus::Failed => RuntimeAgentStatus::Failed,
        RuntimeTaskStatus::Cancelled => RuntimeAgentStatus::Cancelled,
        RuntimeTaskStatus::Interrupted => RuntimeAgentStatus::Interrupted,
    }
}

fn load_tasks(path: &Path) -> Result<HashMap<uuid::Uuid, RuntimeTask>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let tasks: Vec<RuntimeTask> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(tasks.into_iter().map(|task| (task.id, task)).collect())
}

fn persist_tasks(path: &Path, tasks: &HashMap<uuid::Uuid, RuntimeTask>) -> Result<()> {
    let mut tasks = tasks.values().cloned().collect::<Vec<_>>();
    tasks.sort_by_key(|task| task.created_at);
    write_json_atomic(path, &tasks)
}

fn load_interactions(path: &Path) -> Result<HashMap<uuid::Uuid, RuntimeInteraction>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let interactions: Vec<RuntimeInteraction> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(interactions
        .into_iter()
        .map(|interaction| (interaction.id, interaction))
        .collect())
}

fn persist_interactions(
    path: &Path,
    interactions: &HashMap<uuid::Uuid, RuntimeInteraction>,
) -> Result<()> {
    let mut interactions = interactions.values().cloned().collect::<Vec<_>>();
    interactions.sort_by_key(|interaction| interaction.created_at);
    write_json_atomic(path, &interactions)
}

fn read_events(path: &Path, after: u64, limit: usize) -> Result<Vec<RuntimeEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: RuntimeEvent = serde_json::from_str(&line)
            .with_context(|| format!("parse Runtime event from {}", path.display()))?;
        if event.sequence > after {
            events.push(event);
            if events.len() >= limit {
                break;
            }
        }
    }
    Ok(events)
}

struct StateCleanup {
    path: PathBuf,
    token: String,
    lock_path: PathBuf,
    lock_token: String,
    local_socket: PathBuf,
}

impl Drop for StateCleanup {
    fn drop(&mut self) {
        if load_state(&self.path).is_ok_and(|state| state.token == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
        remove_owned_lock(&self.lock_path, &self.lock_token);
        local_transport::remove_if_owned(&self.local_socket);
    }
}

struct OwnedLockCleanup {
    path: PathBuf,
    token: String,
    armed: bool,
}

impl OwnedLockCleanup {
    fn new(path: PathBuf, token: String) -> Self {
        Self {
            path,
            token,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedLockCleanup {
    fn drop(&mut self) {
        if self.armed {
            remove_owned_lock(&self.path, &self.token);
        }
    }
}

fn load_state(path: &Path) -> Result<DaemonState> {
    let data = std::fs::read(path)
        .with_context(|| format!("read Runtime Daemon state at {}", path.display()))?;
    serde_json::from_slice(&data).context("parse Runtime Daemon state")
}

fn write_state(path: &Path, state: &DaemonState) -> Result<()> {
    write_json_atomic(path, state)
}

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let data = serde_json::to_vec_pretty(value)?;
    write_private(&temporary, &data)?;
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn acquire_daemon_lock(path: &Path) -> Result<DaemonLock> {
    if let Ok(existing) = load_daemon_lock(path) {
        if now().saturating_sub(existing.created_at) > LOCK_STALE_AFTER_SECONDS {
            remove_owned_lock(path, &existing.token);
        } else {
            bail!("Runtime Daemon lock already exists");
        }
    }
    let lock = DaemonLock {
        token: uuid::Uuid::new_v4().simple().to_string(),
        created_at: now(),
    };
    let data = serde_json::to_vec_pretty(&lock)?;
    write_private(path, &data).context("acquire Runtime Daemon single-instance lock")?;
    Ok(lock)
}

fn spawn_lock_heartbeat(path: PathBuf, token: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(LOCK_HEARTBEAT_SECONDS));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = refresh_daemon_lock(&path, &token) {
                eprintln!("refresh Runtime Daemon lease: {error:#}");
                break;
            }
        }
    })
}

fn refresh_daemon_lock(path: &Path, token: &str) -> Result<()> {
    let mut lock = load_daemon_lock(path)?;
    if lock.token != token {
        bail!("Runtime Daemon lease ownership changed");
    }
    lock.created_at = now();
    write_json_atomic(path, &lock)
}

fn load_daemon_lock(path: &Path) -> Result<DaemonLock> {
    serde_json::from_slice(&std::fs::read(path)?).context("parse Runtime Daemon lock")
}

fn remove_owned_lock(path: &Path, token: &str) {
    if load_daemon_lock(path).is_ok_and(|lock| lock.token == token) {
        let _ = std::fs::remove_file(path);
    }
}

fn append_log(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    private_options(&mut options);
    options
        .open(path)
        .with_context(|| format!("open Runtime Daemon log at {}", path.display()))
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    private_options(&mut options);
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn private_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_options(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_detached(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

fn remove_stale_state(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale Runtime Daemon state at {}", path.display()))?;
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests;
