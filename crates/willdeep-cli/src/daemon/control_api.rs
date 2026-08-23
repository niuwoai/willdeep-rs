use super::*;
use willdeep_runtime_protocol::{
    AgentPromptParams, AgentWaitParams, AnswerQuestionParams, ApiRequest, ApiResponse,
    ApprovalDecision, ErrorCode, EventListParams, IdParams, ResolveApprovalParams,
    SpawnAgentParams, WorkspaceEnsureParams,
};

const IDEMPOTENCY_CACHE_LIMIT: usize = 1_024;
const MAX_TURN_PROMPT_BYTES: usize = 1024 * 1024;
const MAX_TURN_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_TURN_ATTACHMENTS: usize = 12;
const MAX_TEXT_ATTACHMENT_CHARS: usize = 200_000;

pub(super) struct IdempotencyStore {
    state: AsyncMutex<IdempotencyState>,
    path: Option<PathBuf>,
}

#[derive(Default)]
struct IdempotencyState {
    responses: HashMap<uuid::Uuid, StoredRequest>,
    order: std::collections::VecDeque<uuid::Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedResponse {
    status: u16,
    body: ApiResponse<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredRequest {
    fingerprint: u64,
    response: Option<CachedResponse>,
}

impl Default for IdempotencyStore {
    fn default() -> Self {
        Self {
            state: AsyncMutex::new(IdempotencyState::default()),
            path: None,
        }
    }
}

impl IdempotencyStore {
    pub(super) fn open(path: PathBuf) -> anyhow::Result<Self> {
        let stored = if path.exists() {
            serde_json::from_slice::<Vec<(uuid::Uuid, StoredRequest)>>(&std::fs::read(&path)?)?
        } else {
            Vec::new()
        };
        let mut state = IdempotencyState::default();
        for (id, request) in stored.into_iter().rev().take(IDEMPOTENCY_CACHE_LIMIT).rev() {
            state.order.push_back(id);
            state.responses.insert(id, request);
        }
        Ok(Self {
            state: AsyncMutex::new(state),
            path: Some(path),
        })
    }

    fn persist(&self, state: &IdempotencyState) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let values = state
            .order
            .iter()
            .filter_map(|id| state.responses.get(id).cloned().map(|value| (*id, value)))
            .collect::<Vec<_>>();
        write_json_atomic(path, &values)
    }
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
    let work_guard = if is_work_producing_operation(&request.operation) {
        let guard = state.work_gate.read().await;
        if *guard {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                "Runtime is draining for version handoff; retry against the replacement Runtime",
                true,
                Some(request.request_id),
            );
        }
        Some(guard)
    } else {
        None
    };
    let response = if is_mutating_operation(&request.operation) {
        dispatch_idempotent(&state, request).await
    } else {
        dispatch(&state, request).await.into_response()
    };
    drop(work_guard);
    response
}

async fn dispatch_idempotent(state: &ServerState, request: ApiRequest) -> Response {
    let fingerprint = request_fingerprint(&request);
    let mut cache = state.idempotency.state.lock().await;
    if let Some(stored) = cache.responses.get(&request.request_id) {
        if stored.fingerprint != fingerprint {
            return error_response(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "request_id was already used with different operation params",
                false,
                Some(request.request_id),
            );
        }
        let Some(cached) = &stored.response else {
            return error_response(
                StatusCode::CONFLICT,
                ErrorCode::Unavailable,
                "request outcome is uncertain after Runtime interruption; inspect current state before using a new request_id",
                false,
                Some(request.request_id),
            );
        };
        return (
            StatusCode::from_u16(cached.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(cached.body.clone()),
        )
            .into_response();
    }
    cache.order.push_back(request.request_id);
    cache.responses.insert(
        request.request_id,
        StoredRequest {
            fingerprint,
            response: None,
        },
    );
    if let Err(error) = state.idempotency.persist(&cache) {
        eprintln!("persist pending Runtime request idempotency record: {error:#}");
        cache.responses.remove(&request.request_id);
        cache.order.retain(|id| *id != request.request_id);
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "failed to persist request idempotency record",
            false,
            Some(request.request_id),
        );
    }
    let response = dispatch(state, request.clone()).await;
    cache.responses.insert(
        request.request_id,
        StoredRequest {
            fingerprint,
            response: Some(CachedResponse {
                status: response.status.as_u16(),
                body: response.body.clone(),
            }),
        },
    );
    while cache.order.len() > IDEMPOTENCY_CACHE_LIMIT {
        if let Some(id) = cache.order.pop_front() {
            cache.responses.remove(&id);
        }
    }
    if let Err(error) = state.idempotency.persist(&cache) {
        eprintln!("persist completed Runtime request idempotency record: {error:#}");
    }
    response.into_response()
}

async fn dispatch(state: &ServerState, request: ApiRequest) -> UnifiedResponse {
    let result = match request.operation.as_str() {
        "runtime.capabilities" => json(runtime_capabilities(state)),
        "runtime.status" => json(willdeep_runtime_protocol::RuntimeStatus {
            status: if *state.work_gate.read().await {
                willdeep_runtime_protocol::RuntimeHealth::Draining
            } else {
                willdeep_runtime_protocol::RuntimeHealth::Ok
            },
            version: willdeep_core::VERSION.to_owned(),
            pid: std::process::id(),
            uptime_seconds: now().saturating_sub(state.started_at),
            event_sequence: state.events.latest_sequence(),
        }),
        "workspace.list" => json_result(state.workspaces.list().map(|workspaces| {
            workspaces
                .into_iter()
                .map(public_workspace)
                .collect::<Vec<_>>()
        })),
        "workspace.register" => workspace_register(state, &request),
        "workspace.ensure" => workspace_ensure(state, &request),
        "workspace.activate" => workspace_activate(state, &request),
        "workspace.remove" => workspace_remove(state, &request),
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
        "session.create" => session_create(state, &request),
        "session.rename" => session_rename(state, &request),
        "session.update_model" => session_update_model(state, &request),
        "session.fork" => session_fork(state, &request),
        "session.archive" => session_archive(state, &request),
        "session.delete" => session_delete(state, &request),
        "session.export" => match params::<IdParams>(&request) {
            Ok(params) => state
                .sessions
                .export(params.id)
                .map_err(|_| ApiFailure::not_found("Runtime Session not found"))
                .and_then(public_session_export),
            Err(error) => Err(error),
        },
        "session.search" => session_search(state, &request),
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
        "agent.spawn" => agent_spawn(state, &request),
        "agent.prompt" => agent_prompt(state, &request).await,
        "agent.wait" => agent_wait(state, &request).await,
        "agent.stop" => agent_command(state, &request, agent_control::AgentCommandKind::Stop).await,
        "agent.retry" => agent_retry(state, &request).await,
        "task.list" => json(
            state
                .tasks
                .list()
                .await
                .into_iter()
                .map(public_task)
                .collect::<Vec<_>>(),
        ),
        "task.get" => match params::<IdParams>(&request) {
            Ok(params) => state
                .tasks
                .get(params.id)
                .await
                .map(public_task)
                .map(json)
                .unwrap_or_else(|| Err(ApiFailure::not_found("Runtime Task not found"))),
            Err(error) => Err(error),
        },
        "task.diagnostics" => match params::<IdParams>(&request) {
            Ok(params) => task_diagnostics(state, params.id).await,
            Err(error) => Err(error),
        },
        "task.cancel" => match params::<IdParams>(&request) {
            Ok(params) => match state.tasks.cancel(params.id).await {
                Ok(Some(task)) => json(public_task(task)),
                Ok(None) => Err(ApiFailure::not_found("Runtime Task not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "turn.get" => match params::<IdParams>(&request) {
            Ok(params) => match state.sessions.get_turn(params.id) {
                Ok(Some(turn)) => json(public_turn(turn)),
                Ok(None) => Err(ApiFailure::not_found("Runtime Turn not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "turn.list" => match params::<willdeep_runtime_protocol::ListTurnsParams>(&request) {
            Ok(params) => match state.sessions.get(params.session_id) {
                Ok(Some(_)) => json_result(
                    state
                        .sessions
                        .list_turns(params.session_id)
                        .map(|turns| turns.into_iter().map(public_turn).collect::<Vec<_>>()),
                ),
                Ok(None) => Err(ApiFailure::not_found("Runtime Session not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "turn.submit" => turn_submit(state, &request).await,
        "turn.stop" => turn_stop(state, &request).await,
        "tool.list" => match params::<willdeep_runtime_protocol::ListToolsParams>(&request) {
            Ok(params) => json_result(state.tools.list(params)),
            Err(error) => Err(error),
        },
        "tool.get" => match params::<IdParams>(&request) {
            Ok(params) => match state.tools.get(params.id) {
                Ok(Some(tool)) => json(tool),
                Ok(None) => Err(ApiFailure::not_found("Runtime Tool activity not found")),
                Err(error) => Err(ApiFailure::internal(error)),
            },
            Err(error) => Err(error),
        },
        "artifact.list" => match params::<willdeep_runtime_protocol::ListArtifactsParams>(&request)
        {
            Ok(params) => json_result(diff_review::workspace_change_artifacts(&state.home, params)),
            Err(error) => Err(error),
        },
        "artifact.get" => match params::<IdParams>(&request) {
            Ok(params) => diff_review::workspace_change_artifacts(
                &state.home,
                willdeep_runtime_protocol::ListArtifactsParams {
                    limit: Some(1_000),
                    ..Default::default()
                },
            )
            .map_err(ApiFailure::internal)
            .and_then(|artifacts| {
                artifacts
                    .into_iter()
                    .find(|artifact| artifact.id == params.id)
                    .ok_or_else(|| ApiFailure::not_found("Runtime Artifact not found"))
            })
            .and_then(json),
            Err(error) => Err(error),
        },
        "approval.list" => {
            let interactions = state
                .tasks
                .pending_interactions()
                .await
                .into_iter()
                .filter_map(public_approval)
                .collect::<Vec<_>>();
            json(interactions)
        }
        "approval.resolve" => resolve_approval(state, &request).await,
        "question.list" => json(
            state
                .tasks
                .pending_interactions()
                .await
                .into_iter()
                .filter_map(public_question)
                .collect::<Vec<_>>(),
        ),
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
        "diff.snapshot" => {
            match params::<willdeep_runtime_protocol::DiffSnapshotParams>(&request) {
                Ok(params) => diff_review::unified_snapshot(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.content" => match params::<willdeep_runtime_protocol::DiffContentParams>(&request) {
            Ok(params) => diff_review::unified_content(state, params)
                .await
                .map_err(ApiFailure::from_diff_status)
                .and_then(json),
            Err(error) => Err(error),
        },
        "diff.reviews" => {
            match params::<willdeep_runtime_protocol::DiffSnapshotQueryParams>(&request) {
                Ok(params) => diff_review::unified_reviews(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.review" => match params::<willdeep_runtime_protocol::DiffReviewParams>(&request) {
            Ok(params) => diff_review::unified_review(state, params)
                .await
                .map_err(ApiFailure::from_diff_status)
                .and_then(json),
            Err(error) => Err(error),
        },
        "diff.verifications" => {
            match params::<willdeep_runtime_protocol::DiffSnapshotQueryParams>(&request) {
                Ok(params) => diff_review::unified_verifications(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.verification.record" => {
            match params::<willdeep_runtime_protocol::DiffVerificationParams>(&request) {
                Ok(params) => diff_review::unified_record_verification(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.attributions" => {
            match params::<willdeep_runtime_protocol::DiffSnapshotQueryParams>(&request) {
                Ok(params) => diff_review::unified_attributions(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.commit_preview" => {
            match params::<willdeep_runtime_protocol::DiffCommitPreviewParams>(&request) {
                Ok(params) => diff_review::unified_commit_preview(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "diff.revert" => match params::<willdeep_runtime_protocol::DiffRevertParams>(&request) {
            Ok(params) => diff_review::unified_revert(state, params)
                .await
                .map_err(ApiFailure::from_diff_status)
                .and_then(json),
            Err(error) => Err(error),
        },
        "worktree.review" => {
            match params::<willdeep_runtime_protocol::WorktreeReviewParams>(&request) {
                Ok(params) => worktree_review::unified_review(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "worktree.merge" => {
            match params::<willdeep_runtime_protocol::WorktreeMergeParams>(&request) {
                Ok(params) => worktree_review::unified_merge(state, params)
                    .await
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
        "worktree.audit" => match params::<willdeep_runtime_protocol::EmptyParams>(&request) {
            Ok(_) => worktree_maintenance::unified_audit(state)
                .map_err(ApiFailure::from_diff_status)
                .and_then(json),
            Err(error) => Err(error),
        },
        "worktree.quarantine" => {
            match params::<willdeep_runtime_protocol::WorktreeQuarantineParams>(&request) {
                Ok(params) => worktree_maintenance::unified_quarantine(state, params)
                    .map_err(ApiFailure::from_diff_status)
                    .and_then(json),
                Err(error) => Err(error),
            }
        }
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

const MUTATING_OPERATIONS: &[&str] = &[
    "agent.prompt",
    "agent.spawn",
    "agent.stop",
    "agent.retry",
    "task.cancel",
    "workspace.register",
    "workspace.ensure",
    "workspace.activate",
    "workspace.remove",
    "approval.resolve",
    "question.answer",
    "session.create",
    "session.rename",
    "session.update_model",
    "session.fork",
    "session.archive",
    "session.delete",
    "turn.submit",
    "turn.stop",
    "diff.review",
    "diff.verification.record",
    "diff.revert",
    "worktree.merge",
    "worktree.quarantine",
];

fn is_mutating_operation(operation: &str) -> bool {
    MUTATING_OPERATIONS.contains(&operation)
}

fn is_work_producing_operation(operation: &str) -> bool {
    matches!(
        operation,
        "turn.submit" | "agent.spawn" | "agent.prompt" | "agent.retry"
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
    willdeep_runtime_protocol::RuntimeSession {
        id: session.id,
        root_agent_id: session.root_agent_id,
        workspace: Some(session.workspace.to_string_lossy().into_owned()),
        profile: session.profile,
        model: session.model,
        status: public_session_status(session.status),
        active_turn_id: session.active_turn_id,
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

fn public_session_status(
    status: session_store::RuntimeSessionStatus,
) -> willdeep_runtime_protocol::SessionStatus {
    use session_store::RuntimeSessionStatus as Source;
    use willdeep_runtime_protocol::SessionStatus as Target;
    match status {
        Source::Idle => Target::Idle,
        Source::Queued => Target::Queued,
        Source::Running => Target::Running,
        Source::WaitingApproval => Target::WaitingApproval,
        Source::WaitingAnswer => Target::WaitingAnswer,
        Source::Failed => Target::Failed,
        Source::Interrupted => Target::Interrupted,
        Source::Archived => Target::Archived,
    }
}

fn local_session_status(
    status: willdeep_runtime_protocol::SessionStatus,
) -> session_store::RuntimeSessionStatus {
    use session_store::RuntimeSessionStatus as Target;
    use willdeep_runtime_protocol::SessionStatus as Source;
    match status {
        Source::Idle => Target::Idle,
        Source::Queued => Target::Queued,
        Source::Running => Target::Running,
        Source::WaitingApproval => Target::WaitingApproval,
        Source::WaitingAnswer => Target::WaitingAnswer,
        Source::Failed => Target::Failed,
        Source::Interrupted => Target::Interrupted,
        Source::Archived => Target::Archived,
    }
}

fn public_session_search_result(
    result: session_store::RuntimeSessionSearchResult,
) -> willdeep_runtime_protocol::SessionSearchResult {
    willdeep_runtime_protocol::SessionSearchResult {
        id: result.id,
        title: result.title,
        workspace: Some(result.workspace.to_string_lossy().into_owned()),
        status: public_session_status(result.status),
        profile: result.profile,
        model: result.model,
        updated_at: result.updated_at,
        message_count: result.message_count,
        snippet: result.snippet,
    }
}

fn local_attachment(
    attachment: willdeep_runtime_protocol::MessageAttachment,
) -> willdeep_core::MessageAttachment {
    match attachment {
        willdeep_runtime_protocol::MessageAttachment::Text { name, content } => {
            willdeep_core::MessageAttachment::Text { name, content }
        }
        willdeep_runtime_protocol::MessageAttachment::Image {
            name,
            media_type,
            data,
            width,
            height,
        } => willdeep_core::MessageAttachment::Image {
            name,
            media_type,
            data,
            width,
            height,
        },
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
        model: agent.model,
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
        verifier_passed: agent.verifier_passed,
        claims_checked: agent.claims_checked,
        claims_unverifiable: agent.claims_unverifiable,
        attempts: agent.attempts,
        repo_commit: agent.repo_commit,
        created_at: agent.created_at,
        updated_at: agent.updated_at,
        completed_at: agent.completed_at,
    }
}

/// 失败排查：把这个任务在持久事件日志里留下的失败痕迹**未脱敏**地取回来。
///
/// 这是本机专用的诊断口。公共事件流（`event.list`、SSE、Web 桥接、手机中继）
/// 一律走 [`event_stream::public_event`] 脱敏，那条边界不动；这里之所以能给原文，
/// 是因为调用方已经拿着本机 Runtime 的授权令牌——它本来就能导出整段会话转录。
async fn task_diagnostics(state: &ServerState, id: uuid::Uuid) -> ApiResult {
    /// 一个任务的事件不会无限多，但日志是全局的：从任务起点开始扫，扫够为止。
    const MAX_SCANNED_EVENTS: usize = 5_000;
    const MAX_FAILED_TOOLS: usize = 20;

    let Some(task) = state.tasks.get(id).await else {
        return Err(ApiFailure::not_found("Runtime Task not found"));
    };
    let marker = format!("task_id={id}");
    let mut failure = None;
    let mut failed_tools = Vec::new();
    let mut cursor = task.event_start_sequence.saturating_sub(1);
    let mut scanned = 0;
    while scanned < MAX_SCANNED_EVENTS {
        let events = match state.events.read_after(cursor, 1_000) {
            Ok(events) => events,
            Err(error) => return Err(ApiFailure::internal(error)),
        };
        if events.is_empty() {
            break;
        }
        for event in events {
            cursor = cursor.max(event.sequence);
            scanned += 1;
            if !event.message.starts_with(&marker) {
                continue;
            }
            match event.kind.as_str() {
                "task.failed" | "task.interrupted" => failure = Some(event.message.clone()),
                "task.output" if failed_tools.len() < MAX_FAILED_TOOLS => {
                    if let Some(failed) = failed_tool(event.sequence, &event.message) {
                        failed_tools.push(failed);
                    }
                }
                _ => {}
            }
        }
    }
    json(willdeep_runtime_protocol::RuntimeTaskDiagnostics {
        task: public_task(task),
        failure,
        failed_tools,
    })
}

fn failed_tool(
    sequence: u64,
    message: &str,
) -> Option<willdeep_runtime_protocol::RuntimeToolFailure> {
    let (_, payload) = message.split_once(' ')?;
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("tool_completed")
        || !value
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    Some(willdeep_runtime_protocol::RuntimeToolFailure {
        sequence,
        name: value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        arguments: value
            .get("arguments")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        output: value
            .get("output")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

fn public_task(task: RuntimeTask) -> willdeep_runtime_protocol::RuntimeTask {
    use willdeep_runtime_protocol::TaskStatus as Target;

    willdeep_runtime_protocol::RuntimeTask {
        id: task.id,
        session_id: task.session_id,
        turn_id: task.turn_id,
        agent_id: task.agent_id,
        event_start_sequence: task.event_start_sequence,
        status: match task.status {
            RuntimeTaskStatus::Queued => Target::Queued,
            RuntimeTaskStatus::Running => Target::Running,
            RuntimeTaskStatus::Cancelling => Target::Cancelling,
            RuntimeTaskStatus::WaitingApproval => Target::WaitingApproval,
            RuntimeTaskStatus::WaitingAnswer => Target::WaitingAnswer,
            RuntimeTaskStatus::Completed => Target::Completed,
            RuntimeTaskStatus::Failed => Target::Failed,
            RuntimeTaskStatus::Cancelled => Target::Cancelled,
            RuntimeTaskStatus::Interrupted => Target::Interrupted,
        },
        workspace: Some(task.workspace.to_string_lossy().into_owned()),
        profile: task.profile,
        created_at: task.created_at,
        started_at: task.started_at,
        completed_at: task.completed_at,
        exit_code: task.exit_code,
        failure_domain: task.failure_domain,
    }
}

fn public_approval(
    interaction: RuntimeInteraction,
) -> Option<willdeep_runtime_protocol::PendingApproval> {
    let InteractionKind::Approval {
        description,
        always_allow_available,
    } = interaction.kind
    else {
        return None;
    };
    Some(willdeep_runtime_protocol::PendingApproval {
        id: interaction.id,
        task_id: interaction.task_id,
        description,
        always_allow_available,
        created_at: interaction.created_at,
    })
}

fn public_question(
    interaction: RuntimeInteraction,
) -> Option<willdeep_runtime_protocol::PendingQuestion> {
    let InteractionKind::Question {
        question,
        options,
        multi_select,
    } = interaction.kind
    else {
        return None;
    };
    Some(willdeep_runtime_protocol::PendingQuestion {
        id: interaction.id,
        task_id: interaction.task_id,
        question,
        options,
        multi_select,
        created_at: interaction.created_at,
    })
}

fn public_workspace(
    workspace: workspace_store::RuntimeWorkspace,
) -> willdeep_runtime_protocol::RuntimeWorkspace {
    willdeep_runtime_protocol::RuntimeWorkspace {
        id: workspace.id,
        name: workspace.name,
        root: Some(workspace.root.to_string_lossy().into_owned()),
        access: match workspace.access {
            WorkspaceAccess::ReadOnly => willdeep_runtime_protocol::WorkspaceAccess::ReadOnly,
            WorkspaceAccess::Smart => willdeep_runtime_protocol::WorkspaceAccess::Smart,
            WorkspaceAccess::WorkspaceWrite => {
                willdeep_runtime_protocol::WorkspaceAccess::WorkspaceWrite
            }
        },
        provider_profile: workspace.provider_profile,
        skills: workspace.skills,
        mcp_servers: workspace.mcp_servers,
        created_at: workspace.created_at,
        updated_at: workspace.updated_at,
        active: workspace.active,
    }
}

fn workspace_register(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::RegisterWorkspaceParams>(request)?;
    let access = match params.access {
        willdeep_runtime_protocol::WorkspaceAccess::ReadOnly => WorkspaceAccess::ReadOnly,
        willdeep_runtime_protocol::WorkspaceAccess::Smart => WorkspaceAccess::Smart,
        willdeep_runtime_protocol::WorkspaceAccess::WorkspaceWrite => {
            WorkspaceAccess::WorkspaceWrite
        }
    };
    let (workspace, created) = state
        .workspaces
        .register(workspace_store::RegisterWorkspace {
            root: PathBuf::from(params.root),
            name: params.name,
            access,
            provider_profile: params.provider_profile,
            skills: params.skills,
            mcp_servers: params.mcp_servers,
        })
        .map_err(ApiFailure::internal)?;
    state
        .events
        .append(
            if created {
                "workspace.registered"
            } else {
                "workspace.updated"
            },
            format!("workspace_id={}", workspace.id),
        )
        .map_err(ApiFailure::internal)?;
    json(public_workspace(workspace))
}

fn workspace_ensure(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<WorkspaceEnsureParams>(request)?;
    state
        .workspaces
        .ensure_registered(Path::new(&params.root))
        .map(public_workspace)
        .map_err(ApiFailure::internal)
        .and_then(json)
}

fn workspace_activate(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<IdParams>(request)?;
    let workspace = state
        .workspaces
        .activate(params.id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Workspace not found"))?;
    state
        .events
        .append(
            "workspace.activated",
            format!("workspace_id={}", workspace.id),
        )
        .map_err(ApiFailure::internal)?;
    json(public_workspace(workspace))
}

fn workspace_remove(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<IdParams>(request)?;
    let workspace = state
        .workspaces
        .remove(params.id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Workspace not found"))?;
    state
        .events
        .append(
            "workspace.removed",
            format!("workspace_id={}", workspace.id),
        )
        .map_err(ApiFailure::internal)?;
    json(willdeep_runtime_protocol::ObjectMutationResult {
        id: workspace.id,
        status: willdeep_runtime_protocol::ObjectMutationStatus::Removed,
    })
}

fn session_create(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::CreateSessionParams>(request)?;
    let workspace = state
        .workspaces
        .ensure_registered(Path::new(&params.workspace))
        .map_err(|error| ApiFailure::invalid(format!("invalid Session Workspace: {error}")))?;
    let profile = params.profile.or(workspace.provider_profile);
    let (session, created) = state
        .sessions
        .ensure(session_store::CreateRuntimeSession {
            id: params.id,
            workspace: workspace.root,
            profile,
            model: params.model,
            config: None,
            title: params.title,
        })
        .map_err(|error| ApiFailure::invalid(format!("cannot create Session: {error}")))?;
    if created {
        state
            .events
            .append(
                "session.created",
                format!(
                    "session_id={} agent_id={}",
                    session.id, session.root_agent_id
                ),
            )
            .map_err(ApiFailure::internal)?;
    }
    json(public_session(session))
}

fn session_rename(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::RenameSessionParams>(request)?;
    let session = state
        .sessions
        .rename(params.id, params.title)
        .map_err(|error| ApiFailure::invalid(format!("cannot rename Session: {error}")))?;
    state
        .events
        .append("session.renamed", format!("session_id={}", params.id))
        .map_err(ApiFailure::internal)?;
    json(public_session(session))
}

fn session_update_model(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::UpdateSessionModelParams>(request)?;
    let session = state
        .sessions
        .update_model(params.id, params.model)
        .map_err(|error| ApiFailure::invalid(format!("cannot update Session model: {error}")))?;
    state
        .events
        .append("session.model_updated", format!("session_id={}", params.id))
        .map_err(ApiFailure::internal)?;
    json(public_session(session))
}

fn session_fork(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::ForkSessionParams>(request)?;
    let session = state
        .sessions
        .fork_through(
            params.id,
            params.title,
            params.through_turn_id,
            params.provider_profile,
            params.model,
        )
        .map_err(|error| ApiFailure::invalid(format!("cannot fork Session: {error}")))?;
    state
        .events
        .append(
            "session.forked",
            format!(
                "source_session_id={} through_turn_id={} session_id={} agent_id={}",
                params.id,
                params
                    .through_turn_id
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                session.id,
                session.root_agent_id
            ),
        )
        .map_err(ApiFailure::internal)?;
    json(public_session(session))
}

fn session_archive(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::ArchiveSessionParams>(request)?;
    let session = if params.archived {
        state.sessions.archive(params.id)
    } else {
        state.sessions.unarchive(params.id)
    }
    .map_err(|error| ApiFailure::invalid(format!("cannot update Session archive: {error}")))?;
    state
        .events
        .append(
            if params.archived {
                "session.archived"
            } else {
                "session.unarchived"
            },
            format!("session_id={}", params.id),
        )
        .map_err(ApiFailure::internal)?;
    json(public_session(session))
}

fn session_delete(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::DeleteSessionParams>(request)?;
    state
        .sessions
        .delete(params.id, params.confirmation)
        .map_err(|error| ApiFailure::invalid(format!("cannot delete Session: {error}")))?;
    state
        .events
        .append("session.deleted", format!("session_id={}", params.id))
        .map_err(ApiFailure::internal)?;
    json(willdeep_runtime_protocol::ObjectMutationResult {
        id: params.id,
        status: willdeep_runtime_protocol::ObjectMutationStatus::Deleted,
    })
}

fn public_session_export(export: session_store::RuntimeSessionExport) -> ApiResult {
    let value = serde_json::to_value(export).map_err(ApiFailure::internal)?;
    Ok(scrub_session_export_value(value))
}

fn scrub_session_export_value(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(session) = value
        .get_mut("session")
        .and_then(serde_json::Value::as_object_mut)
    {
        session.remove("config");
        session.remove("last_error");
    }
    value
}

fn session_search(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::SearchSessionsParams>(request)?;
    let status = params.status.map(local_session_status);
    let results = state
        .sessions
        .search(session_store::SessionSearchQuery {
            q: params.query,
            workspace: params.workspace.map(PathBuf::from),
            status,
            profile: params.profile,
            model: params.model,
            updated_after: params.updated_after,
            updated_before: params.updated_before,
        })
        .map_err(|error| ApiFailure::invalid(format!("invalid Session search: {error}")))?;
    json(
        results
            .into_iter()
            .map(public_session_search_result)
            .collect::<Vec<_>>(),
    )
}

async fn turn_submit(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::SubmitTurnParams>(request)?;
    validate_turn_submission(&params)?;
    let session_id = params.session_id;
    let attachments = params
        .attachments
        .into_iter()
        .map(local_attachment)
        .collect();
    let (turn, created, title_changed) = state
        .sessions
        .enqueue_turn_observed(
            session_id,
            session_store::CreateRuntimeTurn {
                request_id: params.turn_request_id,
                prompt: params.prompt,
                attachments,
            },
        )
        .map_err(|error| ApiFailure::invalid(format!("cannot submit Turn: {error}")))?;
    if created {
        let session = state
            .sessions
            .get(session_id)
            .map_err(ApiFailure::internal)?
            .ok_or_else(|| ApiFailure::not_found("Runtime Session not found"))?;
        state
            .events
            .append(
                "turn.queued",
                format!(
                    "session_id={} agent_id={} turn_id={}",
                    session_id, session.root_agent_id, turn.id
                ),
            )
            .map_err(ApiFailure::internal)?;
        state
            .tasks
            .schedule_session(session_id)
            .map_err(ApiFailure::internal)?;
    }
    if title_changed {
        state
            .events
            .append("session.renamed", format!("session_id={session_id}"))
            .map_err(ApiFailure::internal)?;
    }
    let turn = state
        .sessions
        .get_turn(turn.id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Turn not found"))?;
    json(public_turn(turn))
}

fn validate_turn_submission(
    params: &willdeep_runtime_protocol::SubmitTurnParams,
) -> Result<(), ApiFailure> {
    if params.prompt.len() > MAX_TURN_PROMPT_BYTES {
        return Err(ApiFailure::invalid("Turn prompt exceeds 1 MiB"));
    }
    if params.attachments.len() > MAX_TURN_ATTACHMENTS {
        return Err(ApiFailure::invalid("Turn accepts at most 12 attachments"));
    }
    for attachment in &params.attachments {
        match attachment {
            willdeep_runtime_protocol::MessageAttachment::Text { name, content } => {
                if name.len() > 512 || content.chars().count() > MAX_TEXT_ATTACHMENT_CHARS {
                    return Err(ApiFailure::invalid("text attachment is too large"));
                }
            }
            willdeep_runtime_protocol::MessageAttachment::Image {
                name,
                media_type,
                width,
                height,
                ..
            } => {
                if name.len() > 512
                    || !matches!(
                        media_type.as_str(),
                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                    )
                    || *width == 0
                    || *height == 0
                {
                    return Err(ApiFailure::invalid("image attachment is unsupported"));
                }
            }
        }
    }
    let attachment_bytes = params
        .attachments
        .iter()
        .map(|attachment| match attachment {
            willdeep_runtime_protocol::MessageAttachment::Text { name, content } => {
                name.len().saturating_add(content.len())
            }
            willdeep_runtime_protocol::MessageAttachment::Image {
                name,
                media_type,
                data,
                ..
            } => name
                .len()
                .saturating_add(media_type.len())
                .saturating_add(data.len()),
        })
        .fold(0usize, usize::saturating_add);
    if attachment_bytes > MAX_TURN_ATTACHMENT_BYTES {
        return Err(ApiFailure::invalid("Turn attachments exceed 10 MiB"));
    }
    Ok(())
}

async fn turn_stop(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<IdParams>(request)?;
    let cancellation = state
        .sessions
        .request_cancel(params.id)
        .map_err(|_| ApiFailure::not_found("Runtime Turn not found"))?;
    if let Some(task_id) = cancellation.task_id {
        state
            .tasks
            .cancel(task_id)
            .await
            .map_err(ApiFailure::internal)?;
    } else if cancellation.cancelled_queued {
        state
            .events
            .append(
                "turn.cancelled",
                format!(
                    "session_id={} turn_id={} task_id=none",
                    cancellation.session_id, params.id
                ),
            )
            .map_err(ApiFailure::internal)?;
        state
            .tasks
            .schedule_session(cancellation.session_id)
            .map_err(ApiFailure::internal)?;
    }
    let turn = state
        .sessions
        .get_turn(params.id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Turn not found"))?;
    json(public_turn(turn))
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
        None,
    )
    .await
    .map_err(ApiFailure::from_status)?;
    command_response(command)
}

fn agent_spawn(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<SpawnAgentParams>(request)?;
    let prompt = params.prompt.trim();
    if prompt.is_empty() || prompt.len() > MAX_TURN_PROMPT_BYTES {
        return Err(ApiFailure::invalid(format!(
            "prompt must contain 1 to {MAX_TURN_PROMPT_BYTES} bytes"
        )));
    }
    let profile = validate_external_spawn_profile(params.profile.as_deref())?;
    if params
        .label
        .as_deref()
        .is_some_and(|label| label.trim().is_empty() || label.len() > 128)
    {
        return Err(ApiFailure::invalid("label must contain 1 to 128 bytes"));
    }
    let session = state
        .sessions
        .get(params.session_id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Session not found"))?;
    let turn_id = session
        .active_turn_id
        .ok_or_else(|| ApiFailure::conflict("Runtime Session has no active Turn"))?;
    let turn = state
        .sessions
        .get_turn(turn_id)
        .map_err(ApiFailure::internal)?
        .ok_or_else(|| ApiFailure::not_found("Runtime Turn not found"))?;
    let task_id = turn
        .active_task_id
        .ok_or_else(|| ApiFailure::conflict("Runtime Turn has no active Task"))?;
    let child_id = uuid::Uuid::new_v4();
    let child = state
        .agents
        .reserve_external_child(
            child_id,
            session.root_agent_id,
            task_id,
            profile.clone(),
            params.label.map(|label| label.trim().to_owned()),
        )
        .map_err(ApiFailure::internal)?;
    if let Err(error) = state.agent_commands.enqueue_spawn(
        task_id,
        child_id,
        prompt.to_owned(),
        Some(profile),
        child.label.clone(),
    ) {
        let _ = state
            .agents
            .reject_external_child(child_id, "failed to queue external spawn".to_owned());
        return Err(ApiFailure::internal(error));
    }
    if let Err(error) = state.events.append(
            "agent.spawn_requested",
            format!(
                "session_id={} turn_id={turn_id} task_id={task_id} parent_agent_id={} agent_id={child_id}",
                session.id, session.root_agent_id
            ),
        ) {
        eprintln!("append external Agent spawn event: {error:#}");
    }
    json(public_agent(child, false))
}

fn validate_external_spawn_profile(profile: Option<&str>) -> Result<String, ApiFailure> {
    let profile = profile.unwrap_or("scout").trim().to_ascii_lowercase();
    if matches!(
        profile.as_str(),
        "scout" | "reader" | "log_inspector" | "git_detective"
    ) {
        Ok(profile)
    } else {
        Err(ApiFailure::invalid(
            "external agent.spawn only permits worker-tier read-only scout, reader, log_inspector, or git_detective profiles; deep requires a parent-issued escalation ticket",
        ))
    }
}

async fn agent_command(
    state: &ServerState,
    request: &ApiRequest,
    kind: agent_control::AgentCommandKind,
) -> ApiResult {
    let params = params::<IdParams>(request)?;
    let command = agent_control::enqueue_agent_command_internal(state, params.id, kind, None, None)
        .await
        .map_err(ApiFailure::from_status)?;
    command_response(command)
}

async fn agent_retry(state: &ServerState, request: &ApiRequest) -> ApiResult {
    let params = params::<willdeep_runtime_protocol::RetryAgentParams>(request)?;
    let model = params.model.map(|model| model.trim().to_owned());
    if model
        .as_deref()
        .is_some_and(|model| model.is_empty() || model.len() > 256)
    {
        return Err(ApiFailure::invalid(
            "model must contain 1 to 256 bytes when provided",
        ));
    }
    let command = agent_control::enqueue_agent_command_internal(
        state,
        params.id,
        agent_control::AgentCommandKind::Retry,
        None,
        model,
    )
    .await
    .map_err(ApiFailure::from_status)?;
    command_response(command)
}

fn command_response(command: agent_control::AgentCommand) -> ApiResult {
    if command.kind == agent_control::AgentCommandKind::Spawn {
        return Err(ApiFailure::internal(anyhow::anyhow!(
            "spawn command cannot be returned as an Agent control command"
        )));
    }
    json(willdeep_runtime_protocol::RuntimeAgentCommand {
        id: command.id,
        task_id: command.task_id,
        agent_id: command.agent_id,
        kind: match command.kind {
            agent_control::AgentCommandKind::Stop => {
                willdeep_runtime_protocol::AgentCommandKind::Stop
            }
            agent_control::AgentCommandKind::Retry => {
                willdeep_runtime_protocol::AgentCommandKind::Retry
            }
            agent_control::AgentCommandKind::Instruct => {
                willdeep_runtime_protocol::AgentCommandKind::Instruct
            }
            agent_control::AgentCommandKind::Spawn => unreachable!(),
        },
        status: match command.status {
            agent_control::AgentCommandStatus::Pending => {
                willdeep_runtime_protocol::AgentCommandStatus::Pending
            }
            agent_control::AgentCommandStatus::Applied => {
                willdeep_runtime_protocol::AgentCommandStatus::Applied
            }
            agent_control::AgentCommandStatus::Rejected => {
                willdeep_runtime_protocol::AgentCommandStatus::Rejected
            }
        },
        created_at: command.created_at,
        requested_model: command.model,
    })
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
        ApprovalDecision::AllowOnce => InteractionResolution::AllowOnce,
        ApprovalDecision::Deny => InteractionResolution::Deny,
        ApprovalDecision::AlwaysAllow => InteractionResolution::AlwaysAllow,
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
        Ok(Some(interaction)) => json(willdeep_runtime_protocol::RuntimeInteractionResult {
            id: interaction.id,
            task_id: interaction.task_id,
            status: willdeep_runtime_protocol::InteractionResultStatus::Resolved,
            resolved_at: interaction.resolved_at,
        }),
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

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: ErrorCode::Conflict,
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
            StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => {
                Self::invalid("Runtime rejected invalid or oversized operation parameters")
            }
            StatusCode::FORBIDDEN => Self {
                status,
                code: ErrorCode::Forbidden,
                message: "operation is outside an authorized Runtime Workspace".to_owned(),
                retryable: false,
            },
            StatusCode::NOT_FOUND => Self::not_found("Runtime object not found"),
            StatusCode::CONFLICT => Self {
                status,
                code: ErrorCode::Conflict,
                message: "Runtime object state conflicts with this operation".to_owned(),
                retryable: false,
            },
            StatusCode::UNPROCESSABLE_ENTITY => Self {
                status,
                code: ErrorCode::Conflict,
                message: "Runtime object cannot be changed in its current state".to_owned(),
                retryable: false,
            },
            _ => Self::internal(format!("HTTP {status}")),
        }
    }

    fn from_diff_status(status: StatusCode) -> Self {
        if status == StatusCode::CONFLICT {
            Self {
                status,
                code: ErrorCode::StaleSnapshot,
                message: "Diff snapshot changed; refresh before retrying this operation".to_owned(),
                retryable: false,
            }
        } else {
            Self::from_status(status)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 诊断只认「失败的工具」。成功的工具事件不带参数，也不该被算成失败痕迹。
    #[test]
    fn task_diagnostics_picks_up_failed_tools_only() {
        let failed = failed_tool(
            9,
            concat!(
                "task_id=abc ",
                r#"{"type":"tool_completed","name":"run_command","is_error":true,"#,
                r#""arguments":"{\"command\":\"cargo test\"}","output":"1 failed"}"#
            ),
        )
        .expect("failed tool");
        assert_eq!(failed.sequence, 9);
        assert_eq!(failed.name, "run_command");
        assert!(failed.arguments.unwrap().contains("cargo test"));
        assert_eq!(failed.output.as_deref(), Some("1 failed"));

        assert!(
            failed_tool(
                10,
                r#"task_id=abc {"type":"tool_completed","name":"read_file","is_error":false}"#
            )
            .is_none()
        );
        assert!(
            failed_tool(
                11,
                r#"task_id=abc {"type":"tool_requested","name":"read_file"}"#
            )
            .is_none()
        );
        assert!(failed_tool(12, "task_id=abc not-json").is_none());
        // 诊断是只读的：它绝不能被算成写操作而占用变更去重与工作闸门。
        assert!(!is_mutating_operation("task.diagnostics"));
        assert!(!is_work_producing_operation("task.diagnostics"));
    }

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
        assert!(is_mutating_operation("agent.spawn"));
        assert!(!is_mutating_operation("session.list"));
        assert!(is_work_producing_operation("turn.submit"));
        assert!(is_work_producing_operation("agent.spawn"));
        assert!(is_work_producing_operation("agent.prompt"));
        assert!(is_work_producing_operation("agent.retry"));
        assert!(!is_work_producing_operation("agent.stop"));
        assert!(!is_work_producing_operation("approval.resolve"));
    }

    #[test]
    fn every_mutating_operation_is_public_and_uses_the_idempotency_path() {
        assert!(MUTATING_OPERATIONS.iter().all(|operation| {
            willdeep_runtime_protocol::SUPPORTED_OPERATIONS.contains(operation)
                && is_mutating_operation(operation)
        }));
        assert_eq!(
            MUTATING_OPERATIONS
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            MUTATING_OPERATIONS.len()
        );
    }

    #[test]
    fn external_spawn_params_cannot_select_paths_or_writing_profiles() {
        let request = ApiRequest::new(
            "agent.spawn",
            serde_json::json!({
                "session_id": uuid::Uuid::new_v4(),
                "prompt": "inspect",
                "profile": "scout",
                "workspace": "/tmp/escape"
            }),
        );
        assert_eq!(
            params::<SpawnAgentParams>(&request).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            validate_external_spawn_profile(None).ok().as_deref(),
            Some("scout")
        );
        assert_eq!(
            validate_external_spawn_profile(Some(" SCOUT "))
                .ok()
                .as_deref(),
            Some("scout")
        );
        assert!(validate_external_spawn_profile(Some("editor")).is_err());
        assert!(validate_external_spawn_profile(Some("deep")).is_err());
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

    #[test]
    fn public_session_export_removes_internal_path_and_error_fields() {
        let value = scrub_session_export_value(serde_json::json!({
            "session": {
                "id": uuid::Uuid::nil(),
                "config": "/private/config.toml",
                "last_error": "provider secret failed"
            },
            "core": {"messages": []}
        }));
        assert!(value["session"].get("config").is_none());
        assert!(value["session"].get("last_error").is_none());
        assert!(value.get("core").is_some());
    }

    #[test]
    fn turn_submission_limits_are_enforced_at_the_runtime_boundary() {
        let mut params = willdeep_runtime_protocol::SubmitTurnParams {
            session_id: uuid::Uuid::new_v4(),
            turn_request_id: uuid::Uuid::new_v4(),
            prompt: "hello".to_owned(),
            attachments: Vec::new(),
        };
        assert!(validate_turn_submission(&params).is_ok());
        params.attachments = (0..=MAX_TURN_ATTACHMENTS)
            .map(|index| willdeep_runtime_protocol::MessageAttachment::Text {
                name: format!("{index}.txt"),
                content: "x".to_owned(),
            })
            .collect();
        assert_eq!(
            validate_turn_submission(&params).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        params.attachments = vec![willdeep_runtime_protocol::MessageAttachment::Image {
            name: "payload.svg".to_owned(),
            media_type: "image/svg+xml".to_owned(),
            data: "PHN2Zz4=".to_owned(),
            width: 1,
            height: 1,
        }];
        assert!(validate_turn_submission(&params).is_err());
    }

    #[tokio::test]
    async fn idempotency_store_persists_pending_and_completed_without_request_params() {
        let root =
            std::env::temp_dir().join(format!("willdeep-idempotency-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("idempotency.json");
        let id = uuid::Uuid::new_v4();
        let store = IdempotencyStore::open(path.clone()).unwrap();
        {
            let mut state = store.state.lock().await;
            state.order.push_back(id);
            state.responses.insert(
                id,
                StoredRequest {
                    fingerprint: 42,
                    response: None,
                },
            );
            store.persist(&state).unwrap();
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("secret prompt"));
        let reopened = IdempotencyStore::open(path.clone()).unwrap();
        assert!(
            reopened
                .state
                .lock()
                .await
                .responses
                .get(&id)
                .unwrap()
                .response
                .is_none()
        );
        {
            let mut state = reopened.state.lock().await;
            state.responses.get_mut(&id).unwrap().response = Some(CachedResponse {
                status: 200,
                body: ApiResponse::ok(serde_json::json!({"status": "resolved"}), "test", Some(id)),
            });
            reopened.persist(&state).unwrap();
        }
        let completed = IdempotencyStore::open(path).unwrap();
        assert!(
            completed
                .state
                .lock()
                .await
                .responses
                .get(&id)
                .unwrap()
                .response
                .is_some()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
