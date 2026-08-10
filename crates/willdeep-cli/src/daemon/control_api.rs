use super::*;
use willdeep_runtime_protocol::{ApiRequest, ApiResponse, ErrorCode, RuntimeCapabilities};

const IDEMPOTENCY_CACHE_LIMIT: usize = 1_024;

#[derive(Default)]
pub(super) struct IdempotencyStore {
    state: AsyncMutex<IdempotencyState>,
}

#[derive(Default)]
struct IdempotencyState {
    responses: HashMap<uuid::Uuid, CachedResponse>,
    order: std::collections::VecDeque<uuid::Uuid>,
}

#[derive(Clone)]
struct CachedResponse {
    fingerprint: u64,
    status: StatusCode,
    body: ApiResponse<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    id: uuid::Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventListParams {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentPromptParams {
    id: uuid::Uuid,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentWaitParams {
    id: uuid::Uuid,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveApprovalParams {
    id: uuid::Uuid,
    decision: ApprovalDecisionParam,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApprovalDecisionParam {
    AllowOnce,
    Deny,
    AlwaysAllow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerQuestionParams {
    id: uuid::Uuid,
    answer: Option<String>,
}

pub(super) async fn handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<ApiRequest>,
) -> Response {
    if let Err(status) = authorize(&state, &headers) {
        return error_response(
            status,
            ErrorCode::Unauthorized,
            "invalid Runtime token",
            false,
            Some(request.request_id),
        );
    }
    if !request.is_protocol_compatible() {
        return error_response(
            StatusCode::UPGRADE_REQUIRED,
            ErrorCode::UnsupportedProtocol,
            format!(
                "unsupported protocol {}; server uses {}",
                request.protocol_version,
                willdeep_runtime_protocol::PROTOCOL_VERSION
            ),
            false,
            Some(request.request_id),
        );
    }
    if is_mutating_operation(&request.operation) {
        dispatch_idempotent(&state, request).await
    } else {
        dispatch(&state, request).await.into_response()
    }
}

async fn dispatch_idempotent(state: &ServerState, request: ApiRequest) -> Response {
    let fingerprint = request_fingerprint(&request);
    let mut cache = state.idempotency.state.lock().await;
    if let Some(cached) = cache.responses.get(&request.request_id) {
        if cached.fingerprint != fingerprint {
            return error_response(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "request_id was already used with different operation params",
                false,
                Some(request.request_id),
            );
        }
        return (cached.status, Json(cached.body.clone())).into_response();
    }
    let response = dispatch(state, request.clone()).await;
    cache.order.push_back(request.request_id);
    cache.responses.insert(
        request.request_id,
        CachedResponse {
            fingerprint,
            status: response.status,
            body: response.body.clone(),
        },
    );
    while cache.order.len() > IDEMPOTENCY_CACHE_LIMIT {
        if let Some(id) = cache.order.pop_front() {
            cache.responses.remove(&id);
        }
    }
    response.into_response()
}

async fn dispatch(state: &ServerState, request: ApiRequest) -> UnifiedResponse {
    let result = match request.operation.as_str() {
        "runtime.capabilities" => json(RuntimeCapabilities::current(willdeep_core::VERSION)),
        "runtime.status" => json(serde_json::json!({
            "status": "ok",
            "version": willdeep_core::VERSION,
            "pid": std::process::id(),
            "uptime_seconds": now().saturating_sub(state.started_at),
            "event_sequence": state.events.latest_sequence()
        })),
        "workspace.list" => json_result(state.workspaces.list()),
        "session.list" => json_result(
            state
                .sessions
                .list()
                .map(|sessions| sessions.into_iter().map(public_session).collect::<Vec<_>>()),
        ),
        "session.get" => match params::<IdParams>(&request) {
            Ok(params) => match state.sessions.get(params.id) {
                Ok(Some(session)) => json(public_session(session)),
                Ok(None) => Err(ApiFailure::not_found("Runtime Session not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "agent.list" => json_result(state.agents.list().map(|agents| {
            agents
                .into_iter()
                .map(|agent| public_agent(agent, false))
                .collect::<Vec<_>>()
        })),
        "agent.get" => match params::<IdParams>(&request) {
            Ok(params) => match state.agents.get(params.id) {
                Ok(Some(agent)) => json(public_agent(agent, true)),
                Ok(None) => Err(ApiFailure::not_found("Runtime Agent not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "agent.prompt" => agent_prompt(state, &request).await,
        "agent.wait" => agent_wait(state, &request).await,
        "agent.stop" => agent_command(state, &request, agent_control::AgentCommandKind::Stop).await,
        "agent.retry" => {
            agent_command(state, &request, agent_control::AgentCommandKind::Retry).await
        }
        "turn.get" => match params::<IdParams>(&request) {
            Ok(params) => match state.sessions.get_turn(params.id) {
                Ok(Some(turn)) => json(public_turn(turn)),
                Ok(None) => Err(ApiFailure::not_found("Runtime Turn not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "approval.list" => {
            let interactions = state
                .tasks
                .pending_interactions()
                .await
                .into_iter()
                .filter(|interaction| matches!(interaction.kind, InteractionKind::Approval { .. }))
                .collect::<Vec<_>>();
            json(interactions)
        }
        "approval.resolve" => resolve_approval(state, &request).await,
        "question.answer" => answer_question(state, &request).await,
        "event.list" => match params::<EventListParams>(&request) {
            Ok(params) => json_result(
                state
                    .events
                    .read_after(params.after, params.limit.clamp(1, 1_000))
                    .map(|events| {
                        events
                            .into_iter()
                            .map(event_stream::public_event)
                            .collect::<Vec<_>>()
                    }),
            ),
            Err(error) => Err(error),
        },
        _ => Err(ApiFailure {
            status: StatusCode::NOT_IMPLEMENTED,
            code: ErrorCode::UnsupportedOperation,
            message: format!(
                "operation is not available through the unified API: {}",
                request.operation
            ),
            retryable: false,
        }),
    };
    match result {
        Ok(data) => UnifiedResponse {
            status: StatusCode::OK,
            body: ApiResponse::ok(data, willdeep_core::VERSION, Some(request.request_id)),
        },
        Err(error) => UnifiedResponse {
            status: error.status,
            body: ApiResponse::error(
                error.code,
                error.message,
                error.retryable,
                willdeep_core::VERSION,
                Some(request.request_id),
            ),
        },
    }
}

struct UnifiedResponse {
    status: StatusCode,
    body: ApiResponse<serde_json::Value>,
}

impl UnifiedResponse {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn is_mutating_operation(operation: &str) -> bool {
    matches!(
        operation,
        "agent.prompt" | "agent.stop" | "agent.retry" | "approval.resolve" | "question.answer"
    )
}

fn request_fingerprint(request: &ApiRequest) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    request.operation.hash(&mut hasher);
    serde_json::to_vec(&request.params)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

fn public_session(
    session: session_store::RuntimeSession,
) -> willdeep_runtime_protocol::RuntimeSession {
    use session_store::RuntimeSessionStatus as Source;
    use willdeep_runtime_protocol::SessionStatus as Target;

    willdeep_runtime_protocol::RuntimeSession {
        id: session.id,
        root_agent_id: session.root_agent_id,
        workspace: Some(session.workspace.to_string_lossy().into_owned()),
        profile: session.profile,
        model: session.model,
        status: match session.status {
            Source::Idle => Target::Idle,
            Source::Queued => Target::Queued,
            Source::Running => Target::Running,
            Source::WaitingApproval => Target::WaitingApproval,
            Source::WaitingAnswer => Target::WaitingAnswer,
            Source::Failed => Target::Failed,
            Source::Interrupted => Target::Interrupted,
            Source::Archived => Target::Archived,
        },
        active_turn_id: session.active_turn_id,
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

fn public_turn(turn: session_store::RuntimeTurn) -> willdeep_runtime_protocol::RuntimeTurn {
    use session_store::RuntimeTurnStatus as Source;
    use willdeep_runtime_protocol::TurnStatus as Target;

    willdeep_runtime_protocol::RuntimeTurn {
        id: turn.id,
        session_id: turn.session_id,
        request_id: turn.request_id,
        queue_sequence: turn.queue_sequence,
        status: match turn.status {
            Source::Queued => Target::Queued,
            Source::Running => Target::Running,
            Source::WaitingApproval => Target::WaitingApproval,
            Source::WaitingAnswer => Target::WaitingAnswer,
            Source::Completed => Target::Completed,
            Source::Failed => Target::Failed,
            Source::Cancelled => Target::Cancelled,
            Source::Interrupted => Target::Interrupted,
        },
        active_task_id: turn.active_task_id,
        attempts: turn.attempts,
        created_at: turn.created_at,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
    }
}

fn public_agent(
    agent: agent_store::RuntimeAgent,
    include_detail: bool,
) -> willdeep_runtime_protocol::RuntimeAgent {
    use agent_store::RuntimeAgentStatus as Source;
    use willdeep_runtime_protocol::AgentStatus as Target;

    willdeep_runtime_protocol::RuntimeAgent {
        id: agent.id,
        parent_id: agent.parent_id,
        task_id: agent.task_id,
        label: agent.label,
        background: agent.background,
        workspace: Some(agent.workspace.to_string_lossy().into_owned()),
        root_workspace: include_detail
            .then(|| {
                agent
                    .root_workspace
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .flatten(),
        worktree_branch: agent.worktree_branch,
        dedicated_worktree: agent.dedicated_worktree,
        profile: agent.profile,
        status: match agent.status {
            Source::Queued => Target::Queued,
            Source::Running => Target::Running,
            Source::WaitingApproval => Target::WaitingApproval,
            Source::WaitingAnswer => Target::WaitingAnswer,
            Source::Blocked => Target::Blocked,
            Source::Completed => Target::Completed,
            Source::Failed => Target::Failed,
            Source::Cancelled => Target::Cancelled,
            Source::Interrupted => Target::Interrupted,
        },
        current_turn: agent.current_turn,
        current_tool: agent.current_tool,
        input_tokens: agent.input_tokens,
        output_tokens: agent.output_tokens,
        total_tokens: agent.total_tokens,
        max_turns: agent.max_turns,
        token_budget: agent.token_budget,
        timeout_seconds: agent.timeout_seconds,
        report: include_detail.then_some(agent.report).flatten(),
        created_at: agent.created_at,
        updated_at: agent.updated_at,
        completed_at: agent.completed_at,
    }
}

async fn agent_prompt(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<AgentPromptParams>(request)?;
    let message = params.message.trim();
    if message.is_empty() || message.len() > 16 * 1024 {
        return Err(ApiFailure::invalid("message must contain 1 to 16384 bytes"));
    }
    let command = agent_control::enqueue_agent_command_internal(
        state,
        params.id,
        agent_control::AgentCommandKind::Instruct,
        Some(message.to_owned()),
    )
    .await
    .map_err(ApiFailure::from_status)?;
    command_response(command)
}

async fn agent_command(
    state: &ServerState,
    request: &ApiRequest,
    kind: agent_control::AgentCommandKind,
) -> ApiResult {
    let params = params::<IdParams>(request)?;
    let command = agent_control::enqueue_agent_command_internal(state, params.id, kind, None)
        .await
        .map_err(ApiFailure::from_status)?;
    command_response(command)
}

fn command_response(command: agent_control::AgentCommand) -> ApiResult {
    json(serde_json::json!({
        "id": command.id,
        "task_id": command.task_id,
        "agent_id": command.agent_id,
        "kind": command.kind,
        "status": command.status,
        "created_at": command.created_at
    }))
}

async fn agent_wait(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<AgentWaitParams>(request)?;
    let timeout_ms = params.timeout_ms.clamp(1, 30_000);
    let started = std::time::Instant::now();
    loop {
        let agent = state
            .agents
            .get(params.id)
            .map_err(ApiFailure::internal)?
            .ok_or_else(|| ApiFailure::not_found("Runtime Agent not found"))?;
        if !matches!(
            agent.status,
            RuntimeAgentStatus::Queued
                | RuntimeAgentStatus::Running
                | RuntimeAgentStatus::WaitingApproval
                | RuntimeAgentStatus::WaitingAnswer
        ) {
            return json(public_agent(agent, true));
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(ApiFailure {
                status: StatusCode::REQUEST_TIMEOUT,
                code: ErrorCode::Unavailable,
                message: "Agent is still running; retry with the same Agent ID".to_owned(),
                retryable: true,
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn resolve_approval(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<ResolveApprovalParams>(request)?;
    let resolution = match params.decision {
        ApprovalDecisionParam::AllowOnce => InteractionResolution::AllowOnce,
        ApprovalDecisionParam::Deny => InteractionResolution::Deny,
        ApprovalDecisionParam::AlwaysAllow => InteractionResolution::AlwaysAllow,
    };
    resolve_interaction(state, params.id, resolution).await
}

async fn answer_question(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<AnswerQuestionParams>(request)?;
    if params
        .answer
        .as_deref()
        .is_some_and(|answer| answer.len() > 16 * 1024)
    {
        return Err(ApiFailure::invalid("answer exceeds 16384 bytes"));
    }
    resolve_interaction(
        state,
        params.id,
        InteractionResolution::Answer(params.answer),
    )
    .await
}

async fn resolve_interaction(
    state: &ServerState,
    id: uuid::Uuid,
    resolution: InteractionResolution,
) -> ApiResult {
    match state.tasks.resolve_interaction(id, resolution).await {
        Ok(Some(interaction)) => json(interaction),
        Ok(None) => Err(ApiFailure::not_found("Runtime Interaction not found")),
        Err(error) => Err(ApiFailure {
            status: StatusCode::CONFLICT,
            code: ErrorCode::Conflict,
            message: error.to_string(),
            retryable: false,
        }),
    }
}

fn params<T: serde::de::DeserializeOwned>(request: &ApiRequest) -> Result<T, ApiFailure> {
    serde_json::from_value(request.params.clone())
        .map_err(|error| ApiFailure::invalid(format!("invalid operation params: {error}")))
}

fn json(value: impl Serialize) -> ApiResult {
    serde_json::to_value(value).map_err(ApiFailure::internal)
}

fn json_result(value: anyhow::Result<impl Serialize>) -> ApiResult {
    value.map_err(ApiFailure::internal).and_then(json)
}

type ApiResult = Result<serde_json::Value, ApiFailure>;

struct ApiFailure {
    status: StatusCode,
    code: ErrorCode,
    message: String,
    retryable: bool,
}

impl ApiFailure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::InvalidRequest,
            message: message.into(),
            retryable: false,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: ErrorCode::NotFound,
            message: message.into(),
            retryable: false,
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        eprintln!("Runtime control API internal error: {error}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: ErrorCode::Internal,
            message: "internal Runtime error".to_owned(),
            retryable: false,
        }
    }

    fn from_status(status: StatusCode) -> Self {
        match status {
            StatusCode::NOT_FOUND => Self::not_found("Runtime object not found"),
            StatusCode::CONFLICT => Self {
                status,
                code: ErrorCode::Conflict,
                message: "Runtime object state conflicts with this operation".to_owned(),
                retryable: false,
            },
            _ => Self::internal(format!("HTTP {status}")),
        }
    }
}

fn error_response(
    status: StatusCode,
    code: ErrorCode,
    message: impl Into<String>,
    retryable: bool,
    request_id: Option<uuid::Uuid>,
) -> Response {
    (
        status,
        Json(ApiResponse::<serde_json::Value>::error(
            code,
            message,
            retryable,
            willdeep_core::VERSION,
            request_id,
        )),
    )
        .into_response()
}

fn default_event_limit() -> usize {
    200
}

fn default_wait_timeout_ms() -> u64 {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_request_fingerprint_never_retains_prompt_text() {
        let mut first = ApiRequest::new(
            "agent.prompt",
            serde_json::json!({"id": uuid::Uuid::new_v4(), "message": "secret prompt"}),
        );
        let first_fingerprint = request_fingerprint(&first);
        first.params["message"] = serde_json::Value::String("different prompt".to_owned());
        assert_ne!(first_fingerprint, request_fingerprint(&first));
        assert!(is_mutating_operation("agent.prompt"));
        assert!(!is_mutating_operation("session.list"));
    }

    #[test]
    fn operation_params_reject_unknown_fields() {
        let request = ApiRequest::new(
            "event.list",
            serde_json::json!({"after": 1, "limit": 10, "unexpected": true}),
        );
        let error = params::<EventListParams>(&request).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn internal_errors_are_redacted_from_clients() {
        let error = ApiFailure::internal("/private/path contains provider-secret");
        assert_eq!(error.message, "internal Runtime error");
    }
}
