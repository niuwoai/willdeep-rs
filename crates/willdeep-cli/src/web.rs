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
use tokio::sync::{Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use willdeep_core::{MessageAttachment, Role, Session, SessionStore, SkillCatalog};

const MAX_PROMPT_CHARS: usize = 100_000;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

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
    archived: bool,
    active: bool,
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
struct WebAgentPromptAction {
    workspace: String,
    message: String,
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
    status: &'static str,
    current_turn: u64,
    current_tool: Option<String>,
    total_tokens: Option<u64>,
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
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/chat/stream", post(chat_stream))
        .route("/api/sessions", get(sessions))
        .route(
            "/api/sessions/{id}",
            get(session_detail).delete(delete_session),
        )
        .route("/api/sessions/{id}/rename", post(rename_session))
        .route("/api/sessions/{id}/fork", post(fork_session))
        .route("/api/sessions/{id}/archive", post(archive_session))
        .route("/api/sessions/{id}/unarchive", post(unarchive_session))
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
        .route(
            "/api/runtime/agents/{id}/prompt",
            post(prompt_runtime_agent),
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
        agents: snapshot
            .agents
            .into_iter()
            .map(|agent| WebRuntimeAgent {
                id: agent.id,
                parent_id: agent.parent_id,
                label: agent.label,
                background: agent.background,
                profile: agent.profile,
                status: runtime_status_name(agent.status),
                current_turn: agent.current_turn,
                current_tool: agent.current_tool,
                total_tokens: agent.total_tokens,
            })
            .collect(),
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
    Json(action): Json<WebWorkspaceAction>,
) -> Result<StatusCode, WebError> {
    authorize_runtime_agent(&state, &action.workspace, id).await?;
    crate::daemon::retry_remote_agent(&state.home, id)
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
        .map(|session| (session.id, (session.archived, session.active)))
        .collect::<HashMap<_, _>>();
    let values = SessionStore::new(&state.home)
        .list()
        .map_err(|error| WebError::internal(error.to_string()))?
        .into_iter()
        .filter(|session| allowed.contains(&session.workspace))
        .map(|session| {
            let (archived, active) = runtime_states
                .get(&session.id)
                .copied()
                .unwrap_or((false, false));
            SessionSummary {
                id: session.id.to_string(),
                title: session.title,
                workspace: session.workspace.display().to_string(),
                updated_at: session.updated_at,
                archived,
                active,
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
        }),
    )
    .await;
    relay_runtime_turn(
        &state,
        &workspace,
        event_head,
        remote_session.id,
        turn.id,
        Language::parse(input.language.as_deref()).unwrap_or(state_language(&state)),
        tx,
    )
    .await
}

async fn relay_runtime_turn(
    state: &WebState,
    workspace: &std::path::Path,
    mut cursor: u64,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
    language: Language,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), WebError> {
    let mut task_id = None;
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
                    send_event(tx, client).await;
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
                    send_event(
                        tx,
                        serde_json::json!({
                            "type":"completed",
                            "text":text,
                            "session_id":session_id,
                            "turn_id":turn_id,
                        }),
                    )
                    .await;
                } else {
                    send_event(
                        tx,
                        serde_json::json!({
                            "type":"error",
                            "message":event.kind,
                            "session_id":session_id,
                            "turn_id":turn_id,
                        }),
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
    Ok(registered_web_workspaces(state)
        .await?
        .iter()
        .any(|workspace| workspace.root == requested))
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
            status: runtime_status_name(willdeep_core::RuntimeStatus::Working),
            current_turn: 2,
            current_tool: Some("read_file".to_owned()),
            total_tokens: Some(100),
        })
        .unwrap();
        assert_eq!(value["status"], "working");
        assert!(value.get("workspace").is_none());
        assert!(value.get("report").is_none());
        assert!(value.get("error").is_none());
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
            tools: Vec::new(),
            artifacts: Vec::new(),
        };
        assert!(snapshot_has_approval(&snapshot, approval_id));
        assert!(snapshot_has_question(&snapshot, question_id));
        assert!(!snapshot_has_approval(&snapshot, uuid::Uuid::new_v4()));
        assert!(!snapshot_has_agent(&snapshot, uuid::Uuid::new_v4()));
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
            serde_json::from_value::<WebApprovalAction>(serde_json::json!({
                "workspace": "/allowed",
                "decision": "allow_once",
                "always_allow": true
            }))
            .is_err()
        );
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
