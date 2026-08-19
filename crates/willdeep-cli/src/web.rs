use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::i18n::Language;
use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use willdeep_core::{MessageAttachment, Role, Session, SessionStore, SkillCatalog};

const MAX_PROMPT_CHARS: usize = 100_000;
const MAX_AGENT_LABEL_CHARS: usize = 128;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

pub(crate) fn embedded_assets_available() -> bool {
    WebAssets::get("index.html").is_some()
}

pub struct WebConfig {
    pub listen: SocketAddr,
    pub config_path: PathBuf,
    pub profile: Option<String>,
    pub workspaces: Vec<PathBuf>,
    pub home: PathBuf,
    pub language: Language,
}

struct WebState {
    config_path: PathBuf,
    profile: Option<String>,
    workspaces: Vec<PathBuf>,
    home: PathBuf,
    language: Language,
    harness_slots: Arc<Semaphore>,
    settings_writable: bool,
    settings_write_lock: Mutex<()>,
}

#[derive(Clone, Deserialize)]
struct ChatRequest {
    prompt: String,
    session_id: Option<String>,
    workspace: Option<String>,
    language: Option<String>,
    #[serde(default)]
    attachments: Vec<MessageAttachment>,
}

#[derive(Serialize)]
struct SessionSummary {
    id: String,
    title: String,
    workspace: String,
    updated_at: u64,
    pinned_at: Option<u64>,
    archived: bool,
    active: bool,
    active_turn_id: Option<uuid::Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeStreamQuery {
    #[serde(default)]
    after: Option<u64>,
    language: Option<String>,
}

#[derive(Deserialize)]
struct RenameSessionRequest {
    title: String,
}

#[derive(Default, Deserialize)]
struct ForkSessionRequest {
    title: Option<String>,
    #[serde(default)]
    through_turn_id: Option<uuid::Uuid>,
    #[serde(default)]
    provider_profile: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct DeleteSessionRequest {
    confirmation: uuid::Uuid,
}

#[derive(Serialize)]
struct ForkSessionResponse {
    id: String,
}

#[derive(Serialize)]
struct SessionDetail {
    id: String,
    messages: Vec<SessionMessage>,
}

#[derive(Serialize)]
struct SessionMessage {
    role: &'static str,
    content: String,
    attachment_count: usize,
}

#[derive(Serialize)]
struct WorkspaceSummary {
    id: String,
    path: String,
    name: String,
    active: bool,
    access: &'static str,
}

#[derive(Deserialize)]
struct RuntimeActivityQuery {
    workspace: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebWorkspaceAction {
    workspace: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebAgentRetryAction {
    workspace: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebAgentPromptAction {
    workspace: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebAgentSpawnAction {
    workspace: String,
    session_id: uuid::Uuid,
    profile: String,
    prompt: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WebApprovalDecision {
    AllowOnce,
    Deny,
    AlwaysAllow,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebApprovalAction {
    workspace: String,
    decision: WebApprovalDecision,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebQuestionAction {
    workspace: String,
    answer: Option<String>,
}

#[derive(Serialize)]
struct RuntimeActivitySummary {
    tools: Vec<willdeep_runtime_protocol::RuntimeTool>,
    artifacts: Vec<willdeep_runtime_protocol::RuntimeArtifact>,
    agents: Vec<WebRuntimeAgent>,
    tasks: Vec<WebRuntimeTask>,
    gates: Vec<WebRuntimeGate>,
    attention_count: usize,
}

#[derive(Serialize)]
struct WebRuntimeAgent {
    id: uuid::Uuid,
    parent_id: Option<uuid::Uuid>,
    label: Option<String>,
    background: bool,
    profile: Option<String>,
    model: Option<String>,
    status: &'static str,
    current_turn: u64,
    current_tool: Option<String>,
    total_tokens: Option<u64>,
    elapsed_seconds: u64,
    finished_seconds_ago: Option<u64>,
    worktree_branch: Option<String>,
    dedicated_worktree: bool,
}

#[derive(Serialize)]
struct WebRuntimeTask {
    id: uuid::Uuid,
    session_id: Option<uuid::Uuid>,
    turn_id: Option<uuid::Uuid>,
    agent_id: Option<uuid::Uuid>,
    status: &'static str,
    profile: Option<String>,
    elapsed_seconds: u64,
    exit_code: Option<i32>,
    failure_domain: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WebRuntimeGate {
    Approval {
        id: uuid::Uuid,
        task_id: uuid::Uuid,
        description: String,
        always_allow_available: bool,
    },
    Question {
        id: uuid::Uuid,
        task_id: uuid::Uuid,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
}

#[derive(Deserialize)]
struct ComposerQuery {
    workspace: Option<String>,
}

#[derive(Serialize)]
struct ComposerData {
    commands: Vec<&'static str>,
    skills: Vec<ComposerSkill>,
}

#[derive(Serialize)]
struct ComposerSkill {
    identifier: String,
    name: String,
    description: String,
}

pub async fn serve(config: WebConfig) -> Result<()> {
    for workspace in &config.workspaces {
        crate::daemon::ensure_remote_workspace(&config.home, workspace).await?;
    }
    let state = Arc::new(WebState {
        config_path: config.config_path,
        profile: config.profile,
        workspaces: config.workspaces,
        home: config.home,
        language: config.language,
        harness_slots: Arc::new(Semaphore::new(2)),
        settings_writable: config.listen.ip().is_loopback(),
        settings_write_lock: Mutex::new(()),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/chat/stream", post(chat_stream))
        .route("/api/sessions", get(sessions))
        .route(
            "/api/sessions/{id}",
            get(session_detail).delete(delete_session),
        )
        .route("/api/sessions/{id}/stream", get(resume_session_stream))
        .route("/api/sessions/{id}/rename", post(rename_session))
        .route("/api/sessions/{id}/fork", post(fork_session))
        .route("/api/sessions/{id}/archive", post(archive_session))
        .route("/api/sessions/{id}/unarchive", post(unarchive_session))
        .route("/api/sessions/{id}/pin", post(pin_session))
        .route("/api/sessions/{id}/unpin", post(unpin_session))
        .route("/api/sessions/{id}/export", get(export_session))
        .route("/api/turns/{id}/stop", post(stop_turn))
        .route("/api/workspaces", get(workspaces))
        .route("/api/runtime/activity", get(runtime_activity))
        .route(
            "/api/runtime/approvals/{id}/resolve",
            post(resolve_runtime_approval),
        )
        .route(
            "/api/runtime/questions/{id}/answer",
            post(answer_runtime_question),
        )
        .route("/api/runtime/agents/{id}/stop", post(stop_runtime_agent))
        .route("/api/runtime/agents/{id}/retry", post(retry_runtime_agent))
        .route("/api/runtime/agents/spawn", post(spawn_runtime_agent))
        .route(
            "/api/runtime/agents/{id}/prompt",
            post(prompt_runtime_agent),
        )
        .route(
            "/api/settings/model-routing",
            get(model_routing_settings).put(update_model_routing_settings),
        )
        .route("/api/composer", get(composer))
        .route("/", get(index))
        .route("/{*path}", get(asset))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(server_version_header))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("bind Web server at {}", config.listen))?;
    println!("WillDeep Web: http://{}", config.listen);
    for workspace in &state.workspaces {
        println!("Workspace: {}", workspace.display());
    }
    if !config.listen.ip().is_loopback() {
        eprintln!(
            "warning: Web mode has no application authentication; place it behind nginx/VPN and HTTPS"
        );
    }
    axum::serve(listener, app).await.context("run Web server")?;
    Ok(())
}

async fn composer(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ComposerQuery>,
) -> Result<Json<ComposerData>, WebError> {
    let workspace = select_workspace(&state, query.workspace.as_deref()).await?;
    let catalog = SkillCatalog::discover(&workspace.root, &[]).allow_only(&workspace.skills);
    let skills = catalog
        .list()
        .iter()
        .filter(|skill| !skill.identifier.starts_with("auto-"))
        .map(|skill| ComposerSkill {
            identifier: skill.identifier.clone(),
            name: skill.name.clone(),
            description: safe_skill_description(&skill.description),
        })
        .collect();
    Ok(Json(ComposerData {
        commands: vec!["/help", "/goal", "/compress", "/skills", "/clear"],
        skills,
    }))
}

async fn model_routing_settings(
    State(state): State<Arc<WebState>>,
) -> Result<Json<crate::model_routing::ModelRoutingSettings>, WebError> {
    crate::model_routing::load(&state.config_path, state.profile.as_deref())
        .map(Json)
        .map_err(WebError::from_anyhow)
}

async fn update_model_routing_settings(
    State(state): State<Arc<WebState>>,
    Json(update): Json<crate::model_routing::ModelRoutingUpdate>,
) -> Result<Json<crate::model_routing::ModelRoutingSettings>, WebError> {
    if !state.settings_writable {
        return Err(WebError::forbidden(
            "model routing settings can only be changed from a loopback Web server",
        ));
    }
    let _guard = state.settings_write_lock.lock().await;
    crate::model_routing::save(&state.config_path, state.profile.as_deref(), &update)
        .map(Json)
        .map_err(|error| {
            if error
                .to_string()
                .contains(crate::model_routing::STALE_CONFIG_MESSAGE)
            {
                WebError::conflict(error.to_string())
            } else {
                WebError::bad_request(error.to_string())
            }
        })
}

fn safe_skill_description(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if [
        "password", "passwd", "api_key", "api-key", "token=", "secret",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[sensitive description hidden]".to_owned();
    }
    truncate(value, 320)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok","version":willdeep_core::VERSION}))
}

async fn server_version_header(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    if let Ok(value) = header::HeaderValue::from_str(willdeep_core::VERSION) {
        response.headers_mut().insert("x-app-version", value);
    }
    response
}

async fn workspaces(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<WorkspaceSummary>>, WebError> {
    let values = registered_web_workspaces(&state)
        .await?
        .into_iter()
        .map(|workspace| WorkspaceSummary {
            id: workspace.id.to_string(),
            path: workspace.root.display().to_string(),
            name: workspace.name,
            active: workspace.active,
            access: match workspace.access {
                crate::daemon::WorkspaceAccess::ReadOnly => "read_only",
                crate::daemon::WorkspaceAccess::Smart => "smart",
                crate::daemon::WorkspaceAccess::WorkspaceWrite => "workspace_write",
            },
        })
        .collect();
    Ok(Json(values))
}

async fn runtime_activity(
    State(state): State<Arc<WebState>>,
    Query(query): Query<RuntimeActivityQuery>,
) -> Result<Json<RuntimeActivitySummary>, WebError> {
    let workspace = select_workspace(&state, Some(&query.workspace)).await?;
    let snapshot = crate::daemon::runtime_snapshot(&state.home, &workspace.root)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(Json(RuntimeActivitySummary {
        tools: snapshot.tools,
        artifacts: snapshot.artifacts,
        agents: snapshot.agents.into_iter().map(web_runtime_agent).collect(),
        tasks: snapshot.tasks.into_iter().map(web_runtime_task).collect(),
        gates: snapshot
            .gates
            .into_iter()
            .map(|gate| match gate {
                crate::daemon::RemoteGate::Approval {
                    id,
                    task_id,
                    description,
                    always_allow_available,
                } => WebRuntimeGate::Approval {
                    id,
                    task_id,
                    description,
                    always_allow_available,
                },
                crate::daemon::RemoteGate::Question {
                    id,
                    task_id,
                    question,
                    options,
                    multi_select,
                } => WebRuntimeGate::Question {
                    id,
                    task_id,
                    question,
                    options,
                    multi_select,
                },
            })
            .collect(),
        attention_count: snapshot.attention.len(),
    }))
}

fn web_runtime_agent(agent: crate::daemon::tui_bridge::RemoteAgent) -> WebRuntimeAgent {
    let elapsed_seconds = agent_elapsed_seconds(&agent);
    let finished_seconds_ago = agent
        .completed_at
        .map(|completed_at| now_seconds().saturating_sub(completed_at));
    WebRuntimeAgent {
        id: agent.id,
        parent_id: agent.parent_id,
        label: agent.label,
        background: agent.background,
        profile: agent.profile,
        model: agent.model,
        status: runtime_status_name(agent.status),
        current_turn: agent.current_turn,
        current_tool: agent.current_tool,
        total_tokens: agent.total_tokens,
        elapsed_seconds,
        finished_seconds_ago,
        worktree_branch: agent.worktree_branch,
        dedicated_worktree: agent.dedicated_worktree,
    }
}

fn web_runtime_task(task: crate::daemon::tui_bridge::RemoteTask) -> WebRuntimeTask {
    let started_at = task.started_at.unwrap_or(task.created_at);
    let completed_at = task.completed_at.unwrap_or_else(now_seconds);
    WebRuntimeTask {
        id: task.id,
        session_id: task.session_id,
        turn_id: task.turn_id,
        agent_id: task.agent_id,
        status: task_status_name(task.status),
        profile: task.profile,
        elapsed_seconds: completed_at.saturating_sub(started_at),
        exit_code: task.exit_code,
        failure_domain: task.failure_domain.map(failure_domain_name),
    }
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn agent_elapsed_seconds(agent: &crate::daemon::tui_bridge::RemoteAgent) -> u64 {
    let end = agent.completed_at.unwrap_or_else(now_seconds);
    end.saturating_sub(agent.created_at)
}

fn task_status_name(status: willdeep_runtime_protocol::TaskStatus) -> &'static str {
    match status {
        willdeep_runtime_protocol::TaskStatus::Queued => "queued",
        willdeep_runtime_protocol::TaskStatus::Running => "running",
        willdeep_runtime_protocol::TaskStatus::Cancelling => "cancelling",
        willdeep_runtime_protocol::TaskStatus::WaitingApproval => "waiting_approval",
        willdeep_runtime_protocol::TaskStatus::WaitingAnswer => "waiting_answer",
        willdeep_runtime_protocol::TaskStatus::Completed => "completed",
        willdeep_runtime_protocol::TaskStatus::Failed => "failed",
        willdeep_runtime_protocol::TaskStatus::Cancelled => "cancelled",
        willdeep_runtime_protocol::TaskStatus::Interrupted => "interrupted",
    }
}

fn failure_domain_name(domain: willdeep_runtime_protocol::FailureDomain) -> &'static str {
    match domain {
        willdeep_runtime_protocol::FailureDomain::Provider => "provider",
        willdeep_runtime_protocol::FailureDomain::Policy => "policy",
        willdeep_runtime_protocol::FailureDomain::Tool => "tool",
        willdeep_runtime_protocol::FailureDomain::Harness => "harness",
        willdeep_runtime_protocol::FailureDomain::Internal => "internal",
        willdeep_runtime_protocol::FailureDomain::Unknown => "unknown",
    }
}

fn runtime_status_name(status: willdeep_core::RuntimeStatus) -> &'static str {
    match status {
        willdeep_core::RuntimeStatus::Idle => "idle",
        willdeep_core::RuntimeStatus::Working => "working",
        willdeep_core::RuntimeStatus::Blocked => "blocked",
        willdeep_core::RuntimeStatus::WaitingApproval => "waiting_approval",
        willdeep_core::RuntimeStatus::WaitingAnswer => "waiting_answer",
        willdeep_core::RuntimeStatus::Failed => "failed",
        willdeep_core::RuntimeStatus::Done => "done",
        willdeep_core::RuntimeStatus::Cancelled => "cancelled",
        willdeep_core::RuntimeStatus::Unknown => "unknown",
    }
}

async fn resolve_runtime_approval(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(action): Json<WebApprovalAction>,
) -> Result<StatusCode, WebError> {
    let snapshot = authorized_runtime_snapshot(&state, &action.workspace).await?;
    let allowed = snapshot_has_approval(&snapshot, id);
    if !allowed {
        return Err(WebError::not_found(
            "Runtime approval not found in this workspace",
        ));
    }
    let decision = match action.decision {
        WebApprovalDecision::AllowOnce => willdeep_core::ApprovalDecision::AllowOnce,
        WebApprovalDecision::Deny => willdeep_core::ApprovalDecision::Deny,
        WebApprovalDecision::AlwaysAllow => willdeep_core::ApprovalDecision::AlwaysAllow,
    };
    crate::daemon::resolve_remote_approval(&state.home, id, decision)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn answer_runtime_question(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(action): Json<WebQuestionAction>,
) -> Result<StatusCode, WebError> {
    let snapshot = authorized_runtime_snapshot(&state, &action.workspace).await?;
    let allowed = snapshot_has_question(&snapshot, id);
    if !allowed {
        return Err(WebError::not_found(
            "Runtime question not found in this workspace",
        ));
    }
    crate::daemon::answer_remote_question(&state.home, id, action.answer)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stop_runtime_agent(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(action): Json<WebWorkspaceAction>,
) -> Result<StatusCode, WebError> {
    authorize_runtime_agent(&state, &action.workspace, id).await?;
    crate::daemon::stop_remote_agent(&state.home, id)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn retry_runtime_agent(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(action): Json<WebAgentRetryAction>,
) -> Result<StatusCode, WebError> {
    authorize_runtime_agent(&state, &action.workspace, id).await?;
    let model = action.model.map(|model| model.trim().to_owned());
    if model
        .as_deref()
        .is_some_and(|model| model.is_empty() || model.len() > 256)
    {
        return Err(WebError::bad_request(
            "model must contain 1 to 256 bytes when provided",
        ));
    }
    crate::daemon::retry_remote_agent_with_model(&state.home, id, model)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn prompt_runtime_agent(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(action): Json<WebAgentPromptAction>,
) -> Result<StatusCode, WebError> {
    authorize_runtime_agent(&state, &action.workspace, id).await?;
    crate::daemon::instruct_remote_agent(&state.home, id, action.message)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn spawn_runtime_agent(
    State(state): State<Arc<WebState>>,
    Json(action): Json<WebAgentSpawnAction>,
) -> Result<(StatusCode, Json<WebRuntimeAgent>), WebError> {
    let workspace = select_workspace(&state, Some(&action.workspace)).await?;
    let session = SessionStore::new(&state.home)
        .load(action.session_id)
        .map_err(|_| WebError::not_found("Session not found"))?;
    if session.workspace != workspace.root {
        return Err(WebError::not_found(
            "Session not found in the selected workspace",
        ));
    }
    let active = crate::daemon::remote_session_states(&state.home)
        .await
        .map_err(WebError::from_anyhow)?
        .into_iter()
        .any(|candidate| candidate.id == session.id && candidate.active && !candidate.archived);
    if !active {
        return Err(WebError::conflict(
            "A read-only child Agent requires an active parent session",
        ));
    }
    let (profile, prompt, label) = validate_agent_spawn(action)?;
    let agent = crate::daemon::spawn_remote_agent(&state.home, session.id, prompt, profile, label)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok((StatusCode::ACCEPTED, Json(web_runtime_agent(agent))))
}

fn validate_agent_spawn(
    action: WebAgentSpawnAction,
) -> Result<(String, String, Option<String>), WebError> {
    let profile = action.profile.trim().to_owned();
    if !matches!(
        profile.as_str(),
        "scout" | "reader" | "log_inspector" | "git_detective"
    ) {
        return Err(WebError::bad_request(
            "profile must be one of scout, reader, log_inspector, or git_detective; deep requires a runtime escalation ticket",
        ));
    }
    let prompt = action.prompt.trim().to_owned();
    let prompt_chars = prompt.chars().count();
    if prompt_chars == 0 || prompt_chars > MAX_PROMPT_CHARS {
        return Err(WebError::bad_request(format!(
            "prompt must contain 1 to {MAX_PROMPT_CHARS} characters"
        )));
    }
    let label = action
        .label
        .map(|label| label.trim().to_owned())
        .filter(|label| !label.is_empty());
    if label
        .as_deref()
        .is_some_and(|label| label.chars().count() > MAX_AGENT_LABEL_CHARS)
    {
        return Err(WebError::bad_request(format!(
            "label must contain at most {MAX_AGENT_LABEL_CHARS} characters"
        )));
    }
    Ok((profile, prompt, label))
}

async fn authorized_runtime_snapshot(
    state: &WebState,
    workspace: &str,
) -> Result<crate::daemon::RuntimeSnapshot, WebError> {
    let workspace = select_workspace(state, Some(workspace)).await?;
    crate::daemon::runtime_snapshot(&state.home, &workspace.root)
        .await
        .map_err(WebError::from_anyhow)
}

async fn authorize_runtime_agent(
    state: &WebState,
    workspace: &str,
    id: uuid::Uuid,
) -> Result<(), WebError> {
    let snapshot = authorized_runtime_snapshot(state, workspace).await?;
    if snapshot_has_agent(&snapshot, id) {
        Ok(())
    } else {
        Err(WebError::not_found(
            "Runtime Agent not found in this workspace",
        ))
    }
}

/// Turn actions carry no Workspace in the request, so the Session that owns the
/// Turn is resolved first and its Workspace checked against the server
/// allowlist. Everything unknown or out of scope collapses into the same 404 so
/// a caller cannot probe for Turn ids in other Workspaces.
async fn authorize_runtime_turn(state: &WebState, id: uuid::Uuid) -> Result<(), WebError> {
    let not_found = || WebError::not_found("Runtime Turn not found in this workspace");
    let session_id = crate::daemon::remote_turn_session(&state.home, id)
        .await
        .map_err(WebError::from_anyhow)?
        .ok_or_else(not_found)?;
    let session = SessionStore::new(&state.home)
        .load(session_id)
        .map_err(|_| not_found())?;
    if workspace_allowed(state, &session.workspace).await? {
        Ok(())
    } else {
        Err(not_found())
    }
}

fn snapshot_has_approval(snapshot: &crate::daemon::RuntimeSnapshot, id: uuid::Uuid) -> bool {
    snapshot.gates.iter().any(
        |gate| matches!(gate, crate::daemon::RemoteGate::Approval { id: value, .. } if *value == id),
    )
}

fn snapshot_has_question(snapshot: &crate::daemon::RuntimeSnapshot, id: uuid::Uuid) -> bool {
    snapshot.gates.iter().any(
        |gate| matches!(gate, crate::daemon::RemoteGate::Question { id: value, .. } if *value == id),
    )
}

fn snapshot_has_agent(snapshot: &crate::daemon::RuntimeSnapshot, id: uuid::Uuid) -> bool {
    snapshot.agents.iter().any(|agent| agent.id == id)
}

async fn sessions(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<SessionSummary>>, WebError> {
    let allowed = registered_web_workspaces(&state)
        .await?
        .into_iter()
        .map(|workspace| workspace.root)
        .collect::<Vec<_>>();
    let runtime_states = crate::daemon::remote_session_states(&state.home)
        .await
        .map_err(WebError::from_anyhow)?
        .into_iter()
        .map(|session| {
            (
                session.id,
                (session.archived, session.active, session.active_turn_id),
            )
        })
        .collect::<HashMap<_, _>>();
    // 会话目录可能有上百个文件、几十 MB；解析走 blocking 线程池，别堵 tokio worker。
    let home = state.home.clone();
    let digests = tokio::task::spawn_blocking(move || SessionStore::new(&home).digests())
        .await
        .map_err(|error| WebError::internal(error.to_string()))?;
    let values = digests
        .into_iter()
        .filter(|session| allowed.contains(&session.workspace))
        .filter(|session| {
            session.has_user_input
                || runtime_states
                    .get(&session.id)
                    .is_some_and(|(_, active, active_turn_id)| *active || active_turn_id.is_some())
        })
        .map(|session| {
            let (archived, active, active_turn_id) = runtime_states
                .get(&session.id)
                .copied()
                .unwrap_or((false, false, None));
            SessionSummary {
                id: session.id.to_string(),
                title: session.title,
                workspace: session.workspace.display().to_string(),
                updated_at: session.updated_at,
                pinned_at: session.pinned_at,
                archived,
                active,
                active_turn_id,
            }
        })
        .collect();
    Ok(Json(values))
}

async fn session_detail(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<SessionDetail>, WebError> {
    let session = SessionStore::new(&state.home)
        .load(id)
        .map_err(|_| WebError::bad_request("session was not found"))?;
    if !workspace_allowed(&state, &session.workspace).await? {
        return Err(WebError::bad_request(
            "session workspace is not in the server allowlist",
        ));
    }
    let messages = session
        .messages
        .into_iter()
        .filter_map(|message| {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant if !message.content.trim().is_empty() => "assistant",
                _ => return None,
            };
            Some(SessionMessage {
                role,
                content: message.content,
                attachment_count: message.attachments.len(),
            })
        })
        .collect();
    Ok(Json(SessionDetail {
        id: session.id.to_string(),
        messages,
    }))
}

async fn rename_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<RenameSessionRequest>,
) -> Result<StatusCode, WebError> {
    ensure_web_runtime_session(&state, id).await?;
    crate::daemon::rename_remote_session(&state.home, id, request.title)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn fork_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<ForkSessionRequest>,
) -> Result<(StatusCode, Json<ForkSessionResponse>), WebError> {
    ensure_web_runtime_session(&state, id).await?;
    let fork_id = crate::daemon::fork_remote_session(
        &state.home,
        id,
        request.title,
        request.through_turn_id,
        request.provider_profile,
        request.model,
    )
    .await
    .map_err(WebError::from_anyhow)?;
    Ok((
        StatusCode::CREATED,
        Json(ForkSessionResponse {
            id: fork_id.to_string(),
        }),
    ))
}

async fn archive_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, WebError> {
    ensure_web_runtime_session(&state, id).await?;
    crate::daemon::set_remote_session_archived(&state.home, id, true)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unarchive_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, WebError> {
    ensure_web_runtime_session(&state, id).await?;
    crate::daemon::set_remote_session_archived(&state.home, id, false)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pin_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, WebError> {
    set_session_pinned(&state, id, true).await
}

async fn unpin_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, WebError> {
    set_session_pinned(&state, id, false).await
}

// 置顶是本地会话元数据（与 Xedit 的 pinnedAt 同语义），不需要把会话拉入 Runtime。
async fn set_session_pinned(
    state: &WebState,
    id: uuid::Uuid,
    pinned: bool,
) -> Result<StatusCode, WebError> {
    let store = SessionStore::new(&state.home);
    let session = store
        .load(id)
        .map_err(|_| WebError::bad_request("session was not found"))?;
    if !workspace_allowed(state, &session.workspace).await? {
        return Err(WebError::bad_request(
            "session workspace is not in the server allowlist",
        ));
    }
    store
        .set_pinned(id, pinned)
        .map_err(|error| WebError::internal(error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<DeleteSessionRequest>,
) -> Result<StatusCode, WebError> {
    if request.confirmation != id {
        return Err(WebError::bad_request(
            "session deletion confirmation does not match target",
        ));
    }
    ensure_web_runtime_session(&state, id).await?;
    crate::daemon::delete_remote_session(&state.home, id)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn export_session(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, WebError> {
    ensure_web_runtime_session(&state, id).await?;
    crate::daemon::export_remote_session(&state.home, id)
        .await
        .map(Json)
        .map_err(WebError::from_anyhow)
}

async fn ensure_web_runtime_session(state: &WebState, id: uuid::Uuid) -> Result<(), WebError> {
    let session = SessionStore::new(&state.home)
        .load(id)
        .map_err(|_| WebError::bad_request("session was not found"))?;
    if !workspace_allowed(state, &session.workspace).await? {
        return Err(WebError::bad_request(
            "session workspace is not in the server allowlist",
        ));
    }
    crate::daemon::ensure_runtime_session(
        &state.home,
        session.id,
        &session.workspace,
        session.profile,
        None,
        session.title,
    )
    .await
    .map_err(WebError::from_anyhow)?;
    Ok(())
}

async fn stop_turn(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, WebError> {
    authorize_runtime_turn(&state, id).await?;
    crate::daemon::stop_remote_turn(&state.home, id)
        .await
        .map_err(WebError::from_anyhow)?;
    Ok(StatusCode::ACCEPTED)
}

async fn chat_stream(
    State(state): State<Arc<WebState>>,
    Json(input): Json<ChatRequest>,
) -> Result<impl IntoResponse, WebError> {
    let workspace = validate_chat(&state, &input).await?;
    let profile = workspace
        .provider_profile
        .clone()
        .or_else(|| state.profile.clone());
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run_runtime_turn(state, input, workspace.root, profile, tx));
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("working"),
    ))
}

async fn resume_session_stream(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<ResumeStreamQuery>,
) -> Result<impl IntoResponse, WebError> {
    let session = SessionStore::new(&state.home)
        .load(id)
        .map_err(|_| WebError::bad_request("session was not found"))?;
    if !workspace_allowed(&state, &session.workspace).await? {
        return Err(WebError::bad_request(
            "session workspace is not in the server allowlist",
        ));
    }
    let event_head = crate::daemon::runtime_event_head(&state.home)
        .await
        .map_err(WebError::from_anyhow)?;
    let language = query
        .language
        .as_deref()
        .map(|value| Language::parse(Some(value)))
        .transpose()
        .map_err(|error| WebError::bad_request(error.to_string()))?
        .unwrap_or_else(|| state_language(&state));
    let (tx, rx) = mpsc::channel(64);
    let stream_state = state.clone();
    if let Some(active) = crate::daemon::remote_active_turn(&state.home, id)
        .await
        .map_err(WebError::from_anyhow)?
    {
        let cursor = bounded_resume_cursor(query.after, active.replay_after, event_head);
        tokio::spawn(async move {
            send_event(
                &tx,
                serde_json::json!({
                    "type":"resumed",
                    "session_id":active.session_id,
                    "turn_id":active.turn_id,
                    "cursor":cursor,
                }),
            )
            .await;
            if relay_runtime_turn(
                &stream_state,
                &session.workspace,
                RuntimeTurnRelay {
                    cursor,
                    session_id: active.session_id,
                    turn_id: active.turn_id,
                    task_id: active.task_id,
                    language,
                },
                &tx,
            )
            .await
            .is_err()
            {
                send_event(
                    &tx,
                    serde_json::json!({"type":"error","message":"Runtime stream unavailable"}),
                )
                .await;
            }
        });
    } else {
        let turn = crate::daemon::remote_latest_turn(&state.home, id)
            .await
            .map_err(WebError::from_anyhow)?
            .ok_or_else(|| WebError::conflict("session has no Turn to resume"))?;
        let cursor = query.after.unwrap_or(event_head).min(event_head);
        let final_text = latest_assistant_text(&state.home, id).unwrap_or_default();
        tokio::spawn(async move {
            send_event_at(
                &tx,
                serde_json::json!({
                    "type":"resumed",
                    "session_id":id,
                    "turn_id":turn.id,
                    "cursor":cursor,
                }),
                cursor,
            )
            .await;
            let event = match turn.status {
                willdeep_runtime_protocol::TurnStatus::Completed => serde_json::json!({
                    "type":"completed",
                    "text":final_text,
                    "session_id":id,
                    "turn_id":turn.id,
                }),
                status => serde_json::json!({
                    "type":"error",
                    "message":format!("turn.{status:?}").to_ascii_lowercase(),
                    "session_id":id,
                    "turn_id":turn.id,
                }),
            };
            send_event_at(&tx, event, event_head).await;
        });
    }
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("working"),
    ))
}

fn bounded_resume_cursor(requested: Option<u64>, replay_after: u64, event_head: u64) -> u64 {
    requested
        .unwrap_or(replay_after)
        .max(replay_after)
        .min(event_head.max(replay_after))
}

async fn validate_chat(
    state: &WebState,
    input: &ChatRequest,
) -> Result<crate::daemon::RuntimeWorkspace, WebError> {
    let prompt = input.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(WebError::bad_request(
            "prompt must contain 1 to 100000 characters",
        ));
    }
    Language::parse(input.language.as_deref())
        .map_err(|error| WebError::bad_request(error.to_string()))?;
    validate_attachments(&input.attachments)?;
    let workspace = select_workspace(state, input.workspace.as_deref()).await?;
    if let Some(raw_id) = input.session_id.as_deref() {
        let id = uuid::Uuid::parse_str(raw_id)
            .map_err(|_| WebError::bad_request("session_id must be a UUID"))?;
        let session = SessionStore::new(&state.home)
            .load(id)
            .map_err(|_| WebError::bad_request("session_id was not found"))?;
        if session.workspace != workspace.root {
            return Err(WebError::bad_request(
                "session does not belong to the selected workspace",
            ));
        }
    }
    Ok(workspace)
}

async fn run_runtime_turn(
    state: Arc<WebState>,
    input: ChatRequest,
    workspace: PathBuf,
    profile: Option<String>,
    tx: mpsc::Sender<Result<Event, Infallible>>,
) {
    let result = run_runtime_turn_inner(state, input, workspace, profile, &tx).await;
    if let Err(error) = result {
        send_event(
            &tx,
            serde_json::json!({"type":"error","message":error.message}),
        )
        .await;
    }
}

async fn run_runtime_turn_inner(
    state: Arc<WebState>,
    input: ChatRequest,
    workspace: PathBuf,
    profile: Option<String>,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), WebError> {
    let _slot = state
        .harness_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| WebError::internal(error.to_string()))?;
    let store = SessionStore::new(&state.home);
    let mut session = if let Some(raw_id) = input.session_id.as_deref() {
        let id = uuid::Uuid::parse_str(raw_id)
            .map_err(|_| WebError::bad_request("session_id must be a UUID"))?;
        store
            .load(id)
            .map_err(|_| WebError::bad_request("session_id was not found"))?
    } else {
        Session::new(workspace.clone(), profile.clone(), input.prompt.trim())
    };
    let event_head = crate::daemon::runtime_event_head(&state.home)
        .await
        .map_err(WebError::from_anyhow)?;
    let needs_adoption_save = !session.runtime_managed;
    if session.config.is_none() {
        session.config = Some(state.config_path.clone());
    }
    session.runtime_managed = true;
    if session.runtime_event_cursor == 0 {
        session.runtime_event_cursor = event_head;
    }
    if needs_adoption_save {
        store
            .save(&mut session)
            .map_err(|error| WebError::internal(error.to_string()))?;
    }
    let remote_session = crate::daemon::ensure_runtime_session(
        &state.home,
        session.id,
        &workspace,
        session.profile.clone().or(profile),
        None,
        session.title.clone(),
    )
    .await
    .map_err(WebError::from_anyhow)?;
    let turn = crate::daemon::submit_runtime_turn(
        &state.home,
        remote_session.id,
        input.prompt.trim().to_owned(),
        input.attachments,
    )
    .await
    .map_err(WebError::from_anyhow)?;
    send_event(
        tx,
        serde_json::json!({
            "type":"submitted",
            "session_id":remote_session.id,
            "turn_id":turn.id,
            "root_agent_id":remote_session.root_agent_id,
            "cursor":event_head,
        }),
    )
    .await;
    relay_runtime_turn(
        &state,
        &workspace,
        RuntimeTurnRelay {
            cursor: event_head,
            session_id: remote_session.id,
            turn_id: turn.id,
            task_id: None,
            language: Language::parse(input.language.as_deref()).unwrap_or(state_language(&state)),
        },
        tx,
    )
    .await
}

struct RuntimeTurnRelay {
    cursor: u64,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
    task_id: Option<uuid::Uuid>,
    language: Language,
}

async fn relay_runtime_turn(
    state: &WebState,
    workspace: &std::path::Path,
    relay: RuntimeTurnRelay,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), WebError> {
    let RuntimeTurnRelay {
        mut cursor,
        session_id,
        turn_id,
        mut task_id,
        language,
    } = relay;
    let mut final_text = None;
    loop {
        if tx.is_closed() {
            return Ok(());
        }
        let events = crate::daemon::runtime_events(&state.home, cursor, workspace)
            .await
            .map_err(WebError::from_anyhow)?;
        for event in events {
            cursor = cursor.max(event.sequence);
            if event.kind == "turn.started"
                && event_uuid(&event.message, "turn_id") == Some(turn_id)
            {
                task_id = event_uuid(&event.message, "task_id");
            }
            if event.kind == "task.output"
                && task_id.is_some()
                && event_uuid(&event.message, "task_id") == task_id
                && let Some(value) = runtime_output_payload(&event.message)
            {
                if value.get("type").and_then(|value| value.as_str()) == Some("completed") {
                    final_text = value
                        .get("text")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned);
                } else if let Some(client) = client_event(value, language) {
                    send_event_at(tx, client, event.sequence).await;
                }
            }
            if matches!(
                event.kind.as_str(),
                "turn.completed" | "turn.cancelled" | "turn.interrupted" | "turn.failed"
            ) && event_uuid(&event.message, "turn_id") == Some(turn_id)
            {
                if event.kind == "turn.completed" {
                    let text = final_text
                        .or_else(|| latest_assistant_text(&state.home, session_id))
                        .unwrap_or_default();
                    send_event_at(
                        tx,
                        serde_json::json!({
                            "type":"completed",
                            "text":text,
                            "session_id":session_id,
                            "turn_id":turn_id,
                        }),
                        event.sequence,
                    )
                    .await;
                } else {
                    send_event_at(
                        tx,
                        serde_json::json!({
                            "type":"error",
                            "message":event.kind,
                            "session_id":session_id,
                            "turn_id":turn_id,
                        }),
                        event.sequence,
                    )
                    .await;
                }
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn runtime_output_payload(message: &str) -> Option<serde_json::Value> {
    let (_, payload) = message.split_once(' ')?;
    serde_json::from_str(payload).ok()
}

fn event_uuid(message: &str, key: &str) -> Option<uuid::Uuid> {
    message.split_whitespace().find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key)
            .then(|| uuid::Uuid::parse_str(value).ok())
            .flatten()
    })
}

fn latest_assistant_text(home: &std::path::Path, session_id: uuid::Uuid) -> Option<String> {
    SessionStore::new(home)
        .load(session_id)
        .ok()?
        .messages
        .into_iter()
        .rev()
        .find(|message| message.role == Role::Assistant && !message.content.trim().is_empty())
        .map(|message| message.content)
}

fn state_language(state: &WebState) -> Language {
    state.language
}

fn validate_attachments(attachments: &[MessageAttachment]) -> Result<(), WebError> {
    if attachments.len() > 12 {
        return Err(WebError::bad_request("at most 12 attachments are allowed"));
    }
    for attachment in attachments {
        match attachment {
            MessageAttachment::Text { content, .. } if content.chars().count() > 200_000 => {
                return Err(WebError::bad_request("pasted text attachment is too large"));
            }
            MessageAttachment::Image {
                media_type,
                data,
                width,
                height,
                ..
            } => {
                if !matches!(
                    media_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                ) || data.len() > 20_000_000
                    || *width == 0
                    || *height == 0
                {
                    return Err(WebError::bad_request(
                        "image attachment is unsupported or too large",
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn client_event(value: serde_json::Value, language: Language) -> Option<serde_json::Value> {
    let kind = value.get("type")?.as_str()?;
    let label = match kind {
        "turn_started" => format!(
            "{} {}",
            language.text("正在思考 · 第", "Thinking · turn", "思考中 · ターン"),
            value.get("turn").and_then(|v| v.as_u64()).unwrap_or(1)
        ),
        "tool_requested" => format!(
            "{} {}",
            language.text("正在使用", "Using", "使用中"),
            value.get("name").and_then(|v| v.as_str()).unwrap_or("tool")
        ),
        "tool_completed" => format!(
            "{} {}",
            if value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                language.text("执行失败", "Failed", "実行失敗")
            } else {
                language.text("已完成", "Finished", "完了")
            },
            value.get("name").and_then(|v| v.as_str()).unwrap_or("tool")
        ),
        "compression_started" => language
            .text(
                "正在压缩上下文",
                "Compressing context",
                "コンテキストを圧縮中",
            )
            .to_owned(),
        "compression_completed" => language
            .text(
                "上下文压缩完成",
                "Context compressed",
                "コンテキストを圧縮しました",
            )
            .to_owned(),
        "usage" => language
            .text("正在整理结果", "Preparing result", "結果を整理中")
            .to_owned(),
        "completed" => return Some(value),
        "assistant_text" => {
            let thought = value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            return Some(serde_json::json!({"type":"thought","text":truncate(thought,240)}));
        }
        _ => kind.to_owned(),
    };
    Some(serde_json::json!({
        "type":kind,
        "label":label,
        "id":value.get("id").and_then(|item| item.as_str()),
        "name":value.get("name").and_then(|item| item.as_str()),
        "is_error":value.get("is_error").and_then(|item| item.as_bool()),
    }))
}

async fn send_event(tx: &mpsc::Sender<Result<Event, Infallible>>, value: serde_json::Value) {
    if let Ok(event) = Event::default().json_data(value) {
        let _ = tx.send(Ok(event)).await;
    }
}

async fn send_event_at(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    mut value: serde_json::Value,
    cursor: u64,
) {
    if let Some(object) = value.as_object_mut() {
        object.insert("cursor".to_owned(), serde_json::Value::from(cursor));
    }
    if let Ok(event) = Event::default().id(cursor.to_string()).json_data(value) {
        let _ = tx.send(Ok(event)).await;
    }
}

async fn registered_web_workspaces(
    state: &WebState,
) -> Result<Vec<crate::daemon::RuntimeWorkspace>, WebError> {
    Ok(crate::daemon::remote_workspaces(&state.home)
        .await
        .map_err(WebError::from_anyhow)?
        .into_iter()
        .filter(|workspace| state.workspaces.contains(&workspace.root))
        .collect())
}

async fn workspace_allowed(
    state: &WebState,
    requested: &std::path::Path,
) -> Result<bool, WebError> {
    Ok(workspace_in_allowlist(
        &registered_web_workspaces(state).await?,
        requested,
    ))
}

fn workspace_in_allowlist(
    allowed: &[crate::daemon::RuntimeWorkspace],
    requested: &std::path::Path,
) -> bool {
    allowed.iter().any(|workspace| workspace.root == requested)
}

async fn select_workspace(
    state: &WebState,
    requested: Option<&str>,
) -> Result<crate::daemon::RuntimeWorkspace, WebError> {
    select_registered_workspace(registered_web_workspaces(state).await?, requested)
}

fn select_registered_workspace(
    allowed: Vec<crate::daemon::RuntimeWorkspace>,
    requested: Option<&str>,
) -> Result<crate::daemon::RuntimeWorkspace, WebError> {
    match requested {
        Some(value) => allowed
            .into_iter()
            .find(|workspace| workspace.root.to_string_lossy() == value)
            .ok_or_else(|| WebError::bad_request("workspace is not in the server allowlist")),
        None => allowed
            .iter()
            .find(|workspace| workspace.active)
            .cloned()
            .or_else(|| allowed.into_iter().next())
            .ok_or_else(|| WebError::internal("server has no workspace")),
    }
}

async fn index() -> Response {
    embedded("index.html")
}
async fn asset(Path(path): Path<String>) -> Response {
    embedded(&path)
}
fn embedded(path: &str) -> Response {
    let Some(file) = WebAssets::get(path).or_else(|| WebAssets::get("index.html")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    Response::builder().header(header::CONTENT_TYPE, content_type).header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:")
        .body(Body::from(file.data.into_owned())).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[derive(Debug)]
struct WebError {
    status: StatusCode,
    message: String,
}
impl WebError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
    fn from_anyhow(error: anyhow::Error) -> Self {
        Self::internal(error.to_string())
    }
}
impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({"error":self.message}))).into_response()
    }
}
fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(path: &str, active: bool) -> crate::daemon::RuntimeWorkspace {
        crate::daemon::RuntimeWorkspace {
            schema: 1,
            id: uuid::Uuid::new_v4(),
            name: path.to_owned(),
            root: PathBuf::from(path),
            access: crate::daemon::WorkspaceAccess::Smart,
            provider_profile: None,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            created_at: 1,
            updated_at: 1,
            active,
        }
    }

    #[test]
    fn workspace_selection_rejects_unknown_paths() {
        let allowed = vec![workspace("/workspace/a", false)];
        assert!(select_registered_workspace(allowed.clone(), Some("/workspace/b")).is_err());
        assert_eq!(
            select_registered_workspace(allowed, None).unwrap().root,
            PathBuf::from("/workspace/a")
        );
    }

    #[test]
    fn workspace_selection_prefers_the_active_allowed_registration() {
        let allowed = vec![
            workspace("/workspace/a", false),
            workspace("/workspace/b", true),
        ];
        assert_eq!(
            select_registered_workspace(allowed, None).unwrap().root,
            PathBuf::from("/workspace/b")
        );
    }
    // 「空会话不进 Web 历史」「只有附件也算用户输入」这两条语义，
    // 现在由 willdeep_core::session 的 digest 测试覆盖。
    #[test]
    fn embedded_frontend_exists() {
        assert!(WebAssets::get("index.html").is_some());
    }
    #[test]
    fn tool_events_are_redacted_for_sse() {
        let event=client_event(serde_json::json!({"type":"tool_completed","name":"read_file","output":"secret","is_error":false}),Language::En).unwrap();
        assert!(event.get("output").is_none());
    }
    #[test]
    fn thought_events_are_compact_and_tool_arguments_stay_private() {
        let event = client_event(
            serde_json::json!({"type":"assistant_text","text":"x".repeat(500)}),
            Language::En,
        )
        .unwrap();
        assert_eq!(event["type"], "thought");
        assert!(event["text"].as_str().unwrap().chars().count() <= 240);
    }
    #[test]
    fn validates_web_attachments() {
        let valid = vec![MessageAttachment::Image {
            name: "shot.png".into(),
            media_type: "image/png".into(),
            data: "AAAA".into(),
            width: 1,
            height: 1,
        }];
        assert!(validate_attachments(&valid).is_ok());
        let invalid = vec![MessageAttachment::Image {
            name: "shot.svg".into(),
            media_type: "image/svg+xml".into(),
            data: "AAAA".into(),
            width: 1,
            height: 1,
        }];
        assert!(validate_attachments(&invalid).is_err());
    }
    #[test]
    fn skill_descriptions_hide_sensitive_metadata() {
        assert_eq!(
            safe_skill_description("password: example"),
            "[sensitive description hidden]"
        );
        assert_eq!(
            safe_skill_description("Review Rust code"),
            "Review Rust code"
        );
        assert!(safe_skill_description(&"x".repeat(500)).chars().count() <= 320);
    }

    #[test]
    fn web_runtime_agent_summary_excludes_workspace_report_and_internal_errors() {
        let value = serde_json::to_value(WebRuntimeAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: None,
            label: Some("reader".to_owned()),
            background: true,
            profile: Some("reader".to_owned()),
            model: Some("reader-model".to_owned()),
            status: runtime_status_name(willdeep_core::RuntimeStatus::Working),
            current_turn: 2,
            current_tool: Some("read_file".to_owned()),
            total_tokens: Some(100),
            elapsed_seconds: 12,
            finished_seconds_ago: None,
            worktree_branch: None,
            dedicated_worktree: false,
        })
        .unwrap();
        assert_eq!(value["status"], "working");
        assert!(value["finished_seconds_ago"].is_null());
        assert!(value.get("workspace").is_none());
        assert!(value.get("report").is_none());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn web_runtime_agent_reports_run_duration_and_time_since_completion() {
        let completed_at = now_seconds() - 120;
        let value =
            serde_json::to_value(web_runtime_agent(crate::daemon::tui_bridge::RemoteAgent {
                id: uuid::Uuid::new_v4(),
                parent_id: None,
                label: Some("root".to_owned()),
                background: false,
                profile: None,
                model: None,
                status: willdeep_core::RuntimeStatus::Done,
                current_turn: 0,
                current_tool: None,
                total_tokens: None,
                max_turns: None,
                token_budget: None,
                timeout_seconds: None,
                report: None,
                workspace: std::path::PathBuf::from("/tmp"),
                worktree_branch: None,
                dedicated_worktree: false,
                created_at: completed_at - 30,
                completed_at: Some(completed_at),
            }))
            .expect("serialize public agent summary");
        assert_eq!(value["elapsed_seconds"], 30);
        assert!(value["finished_seconds_ago"].as_u64().unwrap_or_default() >= 120);
    }

    #[test]
    fn web_runtime_task_summary_keeps_public_state_without_private_payloads() {
        let task_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let value = serde_json::to_value(web_runtime_task(crate::daemon::tui_bridge::RemoteTask {
            id: task_id,
            session_id: Some(session_id),
            turn_id: None,
            agent_id: Some(agent_id),
            status: willdeep_runtime_protocol::TaskStatus::Failed,
            profile: Some("reader".to_owned()),
            created_at: 10,
            started_at: Some(20),
            completed_at: Some(32),
            exit_code: Some(17),
            failure_domain: Some(willdeep_runtime_protocol::FailureDomain::Tool),
        }))
        .expect("serialize public task summary");
        assert_eq!(value["id"], task_id.to_string());
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["agent_id"], agent_id.to_string());
        assert_eq!(value["status"], "failed");
        assert_eq!(value["elapsed_seconds"], 12);
        assert_eq!(value["exit_code"], 17);
        assert_eq!(value["failure_domain"], "tool");
        for private in [
            "workspace",
            "prompt",
            "command",
            "arguments",
            "output",
            "error",
            "report",
            "path",
            "config",
            "model",
            "pid",
        ] {
            assert!(
                value.get(private).is_none(),
                "private field {private} leaked"
            );
        }
    }

    #[test]
    fn web_structured_logs_keep_relations_without_private_tool_payloads() {
        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let tool = serde_json::to_value(willdeep_runtime_protocol::RuntimeTool {
            id: uuid::Uuid::new_v4(),
            session_id: Some(uuid::Uuid::new_v4()),
            turn_id: Some(uuid::Uuid::new_v4()),
            task_id,
            agent_id,
            name: "read_file".to_owned(),
            status: willdeep_runtime_protocol::ToolStatus::Completed,
            started_at_ms: 10,
            completed_at_ms: Some(20),
        })
        .expect("serialize public tool");
        assert_eq!(tool["agent_id"], agent_id.to_string());
        assert_eq!(tool["task_id"], task_id.to_string());
        for private in [
            "arguments",
            "params",
            "output",
            "error",
            "workspace",
            "path",
        ] {
            assert!(
                tool.get(private).is_none(),
                "private field {private} leaked"
            );
        }

        let artifact = serde_json::to_value(willdeep_runtime_protocol::RuntimeArtifact {
            id: uuid::Uuid::new_v4(),
            kind: willdeep_runtime_protocol::ArtifactKind::WorkspaceChange,
            session_id: None,
            turn_id: None,
            task_id,
            agent_id,
            title: "Workspace changes".to_owned(),
            source_id: "snapshot-id".to_owned(),
            item_count: 3,
            created_at: 30,
        })
        .expect("serialize public artifact");
        assert_eq!(artifact["agent_id"], agent_id.to_string());
        assert_eq!(artifact["task_id"], task_id.to_string());
        for private in ["workspace", "path", "report", "error"] {
            assert!(
                artifact.get(private).is_none(),
                "private field {private} leaked"
            );
        }
    }

    #[test]
    fn runtime_actions_require_the_target_in_the_selected_workspace_snapshot() {
        let approval_id = uuid::Uuid::new_v4();
        let question_id = uuid::Uuid::new_v4();
        let snapshot = crate::daemon::RuntimeSnapshot {
            attention: Vec::new(),
            gates: vec![
                crate::daemon::RemoteGate::Approval {
                    id: approval_id,
                    task_id: uuid::Uuid::new_v4(),
                    description: "approval".to_owned(),
                    always_allow_available: true,
                },
                crate::daemon::RemoteGate::Question {
                    id: question_id,
                    task_id: uuid::Uuid::new_v4(),
                    question: "question".to_owned(),
                    options: vec!["A".to_owned()],
                    multi_select: false,
                },
            ],
            agents: Vec::new(),
            tasks: Vec::new(),
            tools: Vec::new(),
            artifacts: Vec::new(),
            runtime_version: None,
        };
        assert!(snapshot_has_approval(&snapshot, approval_id));
        assert!(snapshot_has_question(&snapshot, question_id));
        assert!(!snapshot_has_approval(&snapshot, uuid::Uuid::new_v4()));
        assert!(!snapshot_has_agent(&snapshot, uuid::Uuid::new_v4()));
    }

    #[test]
    fn turn_actions_require_the_owning_session_workspace_in_the_allowlist() {
        let registered = |root: &str| crate::daemon::RuntimeWorkspace {
            schema: 1,
            id: uuid::Uuid::new_v4(),
            name: "workspace".to_owned(),
            root: std::path::PathBuf::from(root),
            access: crate::daemon::WorkspaceAccess::WorkspaceWrite,
            provider_profile: None,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            created_at: 0,
            updated_at: 0,
            active: true,
        };
        let allowed = vec![registered("/allowed"), registered("/also-allowed")];

        assert!(workspace_in_allowlist(
            &allowed,
            std::path::Path::new("/allowed")
        ));
        assert!(workspace_in_allowlist(
            &allowed,
            std::path::Path::new("/also-allowed")
        ));
        // A Turn whose Session lives outside the allowlist must not be
        // stoppable, even with a valid Turn id.
        assert!(!workspace_in_allowlist(
            &allowed,
            std::path::Path::new("/other-workspace")
        ));
        assert!(!workspace_in_allowlist(
            &[],
            std::path::Path::new("/allowed")
        ));
    }

    #[test]
    fn resume_cursor_stays_inside_the_active_turn_event_window() {
        assert_eq!(bounded_resume_cursor(None, 10, 20), 10);
        assert_eq!(bounded_resume_cursor(Some(5), 10, 20), 10);
        assert_eq!(bounded_resume_cursor(Some(15), 10, 20), 15);
        assert_eq!(bounded_resume_cursor(Some(99), 10, 20), 20);
        assert_eq!(bounded_resume_cursor(Some(1), 30, 20), 30);
    }

    #[test]
    fn resume_stream_query_rejects_client_selected_runtime_scope() {
        assert!(
            serde_json::from_value::<ResumeStreamQuery>(serde_json::json!({
                "after": 12,
                "language": "zh-CN"
            }))
            .is_ok()
        );
        for private_scope in ["workspace", "turn_id", "task_id", "agent_id"] {
            assert!(
                serde_json::from_value::<ResumeStreamQuery>(serde_json::json!({
                    "after": 12,
                    (private_scope): uuid::Uuid::new_v4()
                }))
                .is_err(),
                "resume query accepted client scope {private_scope}"
            );
        }
    }

    #[test]
    fn runtime_action_bodies_reject_client_supplied_extra_scope() {
        assert!(
            serde_json::from_value::<WebWorkspaceAction>(serde_json::json!({
                "workspace": "/allowed",
                "agent_id": uuid::Uuid::new_v4()
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<WebAgentRetryAction>(serde_json::json!({
                "workspace": "/allowed",
                "model": "qwen3-max",
                "agent_id": uuid::Uuid::new_v4()
            }))
            .is_err()
        );
        let retry = serde_json::from_value::<WebAgentRetryAction>(serde_json::json!({
            "workspace": "/allowed",
            "model": "qwen3-max"
        }))
        .expect("valid retry action");
        assert_eq!(retry.model.as_deref(), Some("qwen3-max"));
        assert!(
            serde_json::from_value::<WebApprovalAction>(serde_json::json!({
                "workspace": "/allowed",
                "decision": "allow_once",
                "always_allow": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<WebAgentSpawnAction>(serde_json::json!({
                "workspace": "/allowed",
                "session_id": uuid::Uuid::new_v4(),
                "profile": "scout",
                "prompt": "inspect the repository",
                "workspace_root": "/attacker-selected"
            }))
            .is_err()
        );
    }

    #[test]
    fn web_agent_spawn_accepts_only_bounded_read_only_profiles() {
        let session_id = uuid::Uuid::new_v4();
        let valid = serde_json::from_value::<WebAgentSpawnAction>(serde_json::json!({
            "workspace": "/allowed",
            "session_id": session_id,
            "profile": "reader",
            "prompt": "  read the architecture docs  ",
            "label": "  architecture reader  "
        }))
        .expect("valid spawn body");
        let (profile, prompt, label) = validate_agent_spawn(valid).expect("valid spawn action");
        assert_eq!(profile, "reader");
        assert_eq!(prompt, "read the architecture docs");
        assert_eq!(label.as_deref(), Some("architecture reader"));

        for profile in ["deep", "editor", "writer", "shell"] {
            let action = WebAgentSpawnAction {
                workspace: "/allowed".to_owned(),
                session_id,
                profile: profile.to_owned(),
                prompt: "do work".to_owned(),
                label: None,
            };
            assert!(validate_agent_spawn(action).is_err());
        }
        let empty_prompt = WebAgentSpawnAction {
            workspace: "/allowed".to_owned(),
            session_id,
            profile: "reader".to_owned(),
            prompt: "   ".to_owned(),
            label: None,
        };
        assert!(validate_agent_spawn(empty_prompt).is_err());
        let long_label = WebAgentSpawnAction {
            workspace: "/allowed".to_owned(),
            session_id,
            profile: "scout".to_owned(),
            prompt: "inspect".to_owned(),
            label: Some("x".repeat(MAX_AGENT_LABEL_CHARS + 1)),
        };
        assert!(validate_agent_spawn(long_label).is_err());
    }

    #[test]
    fn runtime_event_metadata_and_payload_are_parsed_without_exposing_prefixes() {
        let task_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let message = format!(
            "session_id={} turn_id={turn_id} task_id={task_id}",
            uuid::Uuid::new_v4()
        );
        assert_eq!(event_uuid(&message, "turn_id"), Some(turn_id));
        assert_eq!(event_uuid(&message, "task_id"), Some(task_id));

        let output = format!(
            "task_id={task_id} {}",
            serde_json::json!({"type":"assistant_text","text":"hello"})
        );
        let payload = runtime_output_payload(&output).unwrap();
        assert_eq!(payload["text"], "hello");
    }
}
