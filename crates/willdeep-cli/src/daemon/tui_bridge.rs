use super::*;

#[derive(Clone)]
pub(crate) enum RemoteGate {
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

impl RemoteGate {
    pub(crate) fn id(&self) -> uuid::Uuid {
        match self {
            Self::Approval { id, .. } | Self::Question { id, .. } => *id,
        }
    }

    pub(crate) fn task_id(&self) -> uuid::Uuid {
        match self {
            Self::Approval { task_id, .. } | Self::Question { task_id, .. } => *task_id,
        }
    }
}

pub(crate) struct RuntimeSnapshot {
    pub attention: Vec<willdeep_core::AttentionItem>,
    pub gates: Vec<RemoteGate>,
    pub agents: Vec<RemoteAgent>,
    pub tools: Vec<willdeep_runtime_protocol::RuntimeTool>,
    pub artifacts: Vec<willdeep_runtime_protocol::RuntimeArtifact>,
}

#[derive(Clone)]
pub(crate) struct RemoteAgent {
    pub id: uuid::Uuid,
    pub parent_id: Option<uuid::Uuid>,
    pub label: Option<String>,
    pub background: bool,
    pub profile: Option<String>,
    pub status: willdeep_core::RuntimeStatus,
    pub current_turn: u64,
    pub current_tool: Option<String>,
    pub total_tokens: Option<u64>,
    pub max_turns: Option<u64>,
    pub token_budget: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub report: Option<String>,
    pub workspace: PathBuf,
    pub worktree_branch: Option<String>,
    pub dedicated_worktree: bool,
}

#[derive(Clone)]
pub(crate) struct RemoteRuntimeEvent {
    pub sequence: u64,
    pub kind: String,
    pub message: String,
    pub visible: bool,
    pub session_id: Option<uuid::Uuid>,
}

#[derive(Clone)]
pub(crate) struct RemoteRuntimeSession {
    pub id: uuid::Uuid,
    pub root_agent_id: uuid::Uuid,
}

#[derive(Clone)]
pub(crate) struct RemoteRuntimeTurn {
    pub id: uuid::Uuid,
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteSessionState {
    pub id: uuid::Uuid,
    pub archived: bool,
    pub active: bool,
}

pub(crate) async fn remote_session_states(home: &Path) -> Result<Vec<RemoteSessionState>> {
    let state = ensure_running(home).await?;
    let sessions = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeSession>>(
                "session.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    Ok(sessions
        .into_iter()
        .map(|session| RemoteSessionState {
            id: session.id,
            archived: session.status == willdeep_runtime_protocol::SessionStatus::Archived,
            active: session.active_turn_id.is_some(),
        })
        .collect())
}

pub(crate) async fn rename_remote_session(
    home: &Path,
    id: uuid::Uuid,
    title: String,
) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.rename",
                &willdeep_runtime_protocol::RenameSessionParams { id, title },
                None,
            )
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn fork_remote_session(
    home: &Path,
    id: uuid::Uuid,
    title: Option<String>,
    through_turn_id: Option<uuid::Uuid>,
    provider_profile: Option<String>,
    model: Option<String>,
) -> Result<uuid::Uuid> {
    let state = ensure_running(home).await?;
    let session = api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.fork",
                &willdeep_runtime_protocol::ForkSessionParams {
                    id,
                    title,
                    through_turn_id,
                    provider_profile,
                    model,
                },
                None,
            )
            .await?,
    )?;
    Ok(session.id)
}

pub(crate) async fn set_remote_session_archived(
    home: &Path,
    id: uuid::Uuid,
    archived: bool,
) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.archive",
                &willdeep_runtime_protocol::ArchiveSessionParams { id, archived },
                None,
            )
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn delete_remote_session(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .call::<_, serde_json::Value>(
                "session.delete",
                &willdeep_runtime_protocol::DeleteSessionParams {
                    id,
                    confirmation: id,
                },
                None,
            )
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn export_remote_session(
    home: &Path,
    id: uuid::Uuid,
) -> Result<serde_json::Value> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .call::<_, serde_json::Value>("session.export", &serde_json::json!({"id": id}), None)
            .await?,
    )
}

pub(crate) async fn search_remote_sessions(
    home: &Path,
    parameters: &[(String, String)],
) -> Result<serde_json::Value> {
    let state = ensure_running(home).await?;
    let mut params = willdeep_runtime_protocol::SearchSessionsParams {
        query: None,
        workspace: None,
        status: None,
        profile: None,
        model: None,
        updated_after: None,
        updated_before: None,
    };
    for (key, value) in parameters {
        match key.as_str() {
            "q" => params.query = Some(value.clone()),
            "workspace" => params.workspace = Some(value.clone()),
            "status" => params.status = Some(parse_public_session_status(value)?),
            "profile" => params.profile = Some(value.clone()),
            "model" => params.model = Some(value.clone()),
            "updated_after" => params.updated_after = Some(value.parse()?),
            "updated_before" => params.updated_before = Some(value.parse()?),
            _ => bail!("unsupported Session search parameter: {key}"),
        }
    }
    let results = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::SessionSearchResult>>(
                "session.search",
                &params,
                None,
            )
            .await?,
    )?;
    Ok(serde_json::to_value(results)?)
}

pub(crate) async fn ensure_runtime_session(
    home: &Path,
    id: uuid::Uuid,
    workspace: &Path,
    profile: Option<String>,
    model: Option<String>,
    title: String,
) -> Result<RemoteRuntimeSession> {
    let state = ensure_running(home).await?;
    let session = api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.create",
                &willdeep_runtime_protocol::CreateSessionParams {
                    id: Some(id),
                    workspace: workspace.canonicalize()?.display().to_string(),
                    profile,
                    model,
                    title: Some(title),
                },
                Some(id),
            )
            .await?,
    )?;
    Ok(RemoteRuntimeSession {
        id: session.id,
        root_agent_id: session.root_agent_id,
    })
}

pub(crate) async fn submit_runtime_turn(
    home: &Path,
    session_id: uuid::Uuid,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
) -> Result<RemoteRuntimeTurn> {
    if prompt.trim().is_empty() && attachments.is_empty() {
        bail!("Runtime Turn prompt and attachments must not both be empty");
    }
    let request_id = uuid::Uuid::new_v4();
    let state = ensure_running(home).await?;
    let turn = api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeTurn>(
                "turn.submit",
                &willdeep_runtime_protocol::SubmitTurnParams {
                    session_id,
                    turn_request_id: request_id,
                    prompt,
                    attachments: attachments.into_iter().map(public_attachment).collect(),
                },
                Some(request_id),
            )
            .await?,
    )?;
    Ok(RemoteRuntimeTurn { id: turn.id })
}

pub(crate) async fn stop_remote_turn(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeTurn>(
                "turn.stop",
                &serde_json::json!({"id": id}),
                None,
            )
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn runtime_event_head(home: &Path) -> Result<u64> {
    let state = match load_state(&DaemonPaths::new(home).state) {
        Ok(state) => state,
        Err(_) => return Ok(0),
    };
    Ok(probe(&state).await?.event_sequence)
}

pub(crate) async fn runtime_events(
    home: &Path,
    after: u64,
    workspace: &Path,
) -> Result<Vec<RemoteRuntimeEvent>> {
    let state = match load_state(&DaemonPaths::new(home).state) {
        Ok(state) => state,
        Err(_) => return Ok(Vec::new()),
    };
    probe(&state).await?;
    let workspace = workspace.canonicalize()?;
    let tasks = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeTask>>(
                "task.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    let visible_tasks = tasks
        .into_iter()
        .filter(|task| {
            task.workspace
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == workspace)
        })
        .map(|task| (task.id, task.session_id))
        .collect::<HashMap<_, _>>();
    let events = api_data(
        runtime_client(&state)?
            .call::<_, Vec<RuntimeEvent>>(
                "event.list",
                &serde_json::json!({"after": after, "limit": 200}),
                None,
            )
            .await?,
    )?;
    Ok(events
        .into_iter()
        .map(|event| {
            let session_id = event_task_id(&event.message)
                .and_then(|task_id| visible_tasks.get(&task_id).copied().flatten());
            RemoteRuntimeEvent {
                sequence: event.sequence,
                kind: event.kind,
                message: event.message,
                visible: session_id.is_some(),
                session_id,
            }
        })
        .collect())
}

pub(crate) struct RuntimeEventFollower(tokio::task::JoinHandle<()>);

impl Drop for RuntimeEventFollower {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn start_runtime_event_follower(
    home: PathBuf,
    after: u64,
    workspace: PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<RemoteRuntimeEvent>>,
) -> RuntimeEventFollower {
    RuntimeEventFollower(tokio::spawn(async move {
        let _ = follow_runtime_events(home, after, workspace, tx).await;
    }))
}

async fn follow_runtime_events(
    home: PathBuf,
    after: u64,
    workspace: PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<RemoteRuntimeEvent>>,
) -> Result<()> {
    let workspace = workspace.canonicalize()?;
    let mut cursor = after;
    while !tx.is_closed() {
        if follow_runtime_events_once(&home, &mut cursor, &workspace, &tx)
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

async fn follow_runtime_events_once(
    home: &Path,
    cursor: &mut u64,
    workspace: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<RemoteRuntimeEvent>>,
) -> Result<()> {
    let state = load_state(&DaemonPaths::new(home).state)?;
    let mut stream = match runtime_client(&state)?
        .stream_events(*cursor, 1_000, None)
        .await
    {
        Ok(stream) => stream,
        Err(willdeep_runtime_client::ClientError::HttpStatus(404)) => {
            let events = runtime_events(home, *cursor, workspace).await?;
            *cursor = events
                .iter()
                .map(|event| event.sequence)
                .max()
                .unwrap_or(*cursor);
            let _ = tx.send(events);
            tokio::time::sleep(Duration::from_secs(1)).await;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let mut visible_tasks = HashMap::<uuid::Uuid, Option<uuid::Uuid>>::new();
    while let Some(response) = stream.next::<RuntimeEvent>().await? {
        let event = match response {
            willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => data,
            willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
                bail!("Runtime event stream failed: {}", error.message)
            }
        };
        *cursor = (*cursor).max(event.sequence);
        let session_id = if let Some(task_id) = event_task_id(&event.message) {
            if let Some(session_id) = visible_tasks.get(&task_id) {
                *session_id
            } else {
                let task = authorized_get::<RuntimeTask>(&state, &format!("/v1/tasks/{task_id}"))
                    .await
                    .ok();
                let session_id = task
                    .as_ref()
                    .filter(|task| task.workspace == workspace)
                    .and_then(|task| task.session_id);
                if task.is_some() {
                    visible_tasks.insert(task_id, session_id);
                }
                session_id
            }
        } else {
            None
        };
        if tx
            .send(vec![RemoteRuntimeEvent {
                sequence: event.sequence,
                kind: event.kind,
                message: event.message,
                visible: session_id.is_some(),
                session_id,
            }])
            .is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

fn event_task_id(message: &str) -> Option<uuid::Uuid> {
    message
        .split_whitespace()
        .find_map(|part| part.strip_prefix("task_id="))?
        .parse()
        .ok()
}

pub(crate) async fn runtime_snapshot(home: &Path, workspace: &Path) -> Result<RuntimeSnapshot> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) if probe(&state).await.is_ok() => state,
        _ => {
            return Ok(RuntimeSnapshot {
                attention: Vec::new(),
                gates: Vec::new(),
                agents: Vec::new(),
                tools: Vec::new(),
                artifacts: Vec::new(),
            });
        }
    };
    let workspace = workspace.canonicalize()?;
    let response: willdeep_runtime_protocol::ApiResponse<
        Vec<willdeep_runtime_protocol::RuntimeAgent>,
    > = runtime_client(&state)?
        .call("agent.list", &serde_json::json!({}), None)
        .await?;
    let mut agents = api_data(response)?
        .into_iter()
        .filter(|agent| {
            agent
                .workspace
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == workspace)
        })
        .map(remote_agent)
        .collect::<Vec<_>>();
    agents.sort_by_key(|agent| (agent.parent_id.is_some(), agent.parent_id, agent.id));
    let tasks = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeTask>>(
                "task.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    let visible_tasks = tasks
        .iter()
        .filter(|task| {
            task.workspace
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == workspace)
        })
        .map(|task| task.id)
        .collect::<std::collections::HashSet<_>>();
    let tools = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeTool>>(
                "tool.list",
                &willdeep_runtime_protocol::ListToolsParams {
                    limit: Some(100),
                    ..Default::default()
                },
                None,
            )
            .await?,
    )?
    .into_iter()
    .filter(|tool| visible_tasks.contains(&tool.task_id))
    .collect();
    let artifacts = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeArtifact>>(
                "artifact.list",
                &willdeep_runtime_protocol::ListArtifactsParams {
                    limit: Some(100),
                    ..Default::default()
                },
                None,
            )
            .await?,
    )?
    .into_iter()
    .filter(|artifact| visible_tasks.contains(&artifact.task_id))
    .collect();
    let approvals = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::PendingApproval>>(
                "approval.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    let questions = api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::PendingQuestion>>(
                "question.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    let mut attention = tasks
        .into_iter()
        .filter(|task| visible_tasks.contains(&task.id))
        .filter(|task| runtime_task_visible(task, now()))
        .filter(|task| task.status != willdeep_runtime_protocol::TaskStatus::Queued)
        .map(runtime_task_attention)
        .collect::<Vec<_>>();
    let mut gates = Vec::new();
    for approval in approvals
        .into_iter()
        .filter(|approval| visible_tasks.contains(&approval.task_id))
    {
        gates.push(RemoteGate::Approval {
            id: approval.id,
            task_id: approval.task_id,
            description: approval.description.clone(),
            always_allow_available: approval.always_allow_available,
        });
        let item = willdeep_core::AttentionItem::approval(approval.description);
        attention.push(willdeep_core::AttentionItem {
            id: format!("runtime-interaction:{}", approval.id),
            title: item.title,
            detail: format!("Runtime task {}\n{}", approval.task_id, item.detail),
            ..item
        });
    }
    for question in questions
        .into_iter()
        .filter(|question| visible_tasks.contains(&question.task_id))
    {
        gates.push(RemoteGate::Question {
            id: question.id,
            task_id: question.task_id,
            question: question.question.clone(),
            options: question.options.clone(),
            multi_select: question.multi_select,
        });
        let item = willdeep_core::AttentionItem::question(question.question);
        attention.push(willdeep_core::AttentionItem {
            id: format!("runtime-interaction:{}", question.id),
            title: item.title,
            detail: format!("Runtime task {}\n{}", question.task_id, item.detail),
            ..item
        });
    }
    Ok(RuntimeSnapshot {
        attention,
        gates,
        agents,
        tools,
        artifacts,
    })
}

pub(crate) async fn stop_remote_agent(home: &Path, id: uuid::Uuid) -> Result<()> {
    control_remote_agent(home, id, "stop").await
}

pub(crate) async fn retry_remote_agent(home: &Path, id: uuid::Uuid) -> Result<()> {
    control_remote_agent(home, id, "retry").await
}

pub(crate) async fn instruct_remote_agent(
    home: &Path,
    id: uuid::Uuid,
    message: String,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response: willdeep_runtime_protocol::ApiResponse<serde_json::Value> =
        runtime_client(&state)?
            .call(
                "agent.prompt",
                &serde_json::json!({"id": id, "message": message}),
                None,
            )
            .await?;
    api_data(response).map(|_| ())
}

async fn control_remote_agent(home: &Path, id: uuid::Uuid, action: &str) -> Result<()> {
    let state = ensure_running(home).await?;
    let response: willdeep_runtime_protocol::ApiResponse<serde_json::Value> =
        runtime_client(&state)?
            .call(
                format!("agent.{action}"),
                &serde_json::json!({"id": id}),
                None,
            )
            .await?;
    api_data(response).map(|_| ())
}

fn remote_agent(agent: willdeep_runtime_protocol::RuntimeAgent) -> RemoteAgent {
    RemoteAgent {
        id: agent.id,
        parent_id: agent.parent_id,
        label: agent.label,
        background: agent.background,
        profile: agent.profile,
        status: match agent.status {
            willdeep_runtime_protocol::AgentStatus::Queued
            | willdeep_runtime_protocol::AgentStatus::Running => {
                willdeep_core::RuntimeStatus::Working
            }
            willdeep_runtime_protocol::AgentStatus::WaitingApproval => {
                willdeep_core::RuntimeStatus::WaitingApproval
            }
            willdeep_runtime_protocol::AgentStatus::WaitingAnswer => {
                willdeep_core::RuntimeStatus::WaitingAnswer
            }
            willdeep_runtime_protocol::AgentStatus::Blocked => {
                willdeep_core::RuntimeStatus::Blocked
            }
            willdeep_runtime_protocol::AgentStatus::Completed => willdeep_core::RuntimeStatus::Done,
            willdeep_runtime_protocol::AgentStatus::Failed
            | willdeep_runtime_protocol::AgentStatus::Interrupted => {
                willdeep_core::RuntimeStatus::Failed
            }
            willdeep_runtime_protocol::AgentStatus::Cancelled => {
                willdeep_core::RuntimeStatus::Cancelled
            }
        },
        current_turn: agent.current_turn,
        current_tool: agent.current_tool,
        total_tokens: agent.total_tokens,
        max_turns: agent.max_turns,
        token_budget: agent.token_budget,
        timeout_seconds: agent.timeout_seconds,
        report: agent.report,
        workspace: agent.workspace.map(PathBuf::from).unwrap_or_default(),
        worktree_branch: agent.worktree_branch,
        dedicated_worktree: agent.dedicated_worktree,
    }
}

fn api_data<T>(response: willdeep_runtime_protocol::ApiResponse<T>) -> Result<T> {
    match response {
        willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => Ok(data),
        willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
            bail!("Runtime API error: {}", error.message)
        }
    }
}

fn public_attachment(
    attachment: willdeep_core::MessageAttachment,
) -> willdeep_runtime_protocol::MessageAttachment {
    match attachment {
        willdeep_core::MessageAttachment::Text { name, content } => {
            willdeep_runtime_protocol::MessageAttachment::Text { name, content }
        }
        willdeep_core::MessageAttachment::Image {
            name,
            media_type,
            data,
            width,
            height,
        } => willdeep_runtime_protocol::MessageAttachment::Image {
            name,
            media_type,
            data,
            width,
            height,
        },
    }
}

fn parse_public_session_status(value: &str) -> Result<willdeep_runtime_protocol::SessionStatus> {
    use willdeep_runtime_protocol::SessionStatus;
    match value.trim().to_ascii_lowercase().as_str() {
        "idle" => Ok(SessionStatus::Idle),
        "queued" => Ok(SessionStatus::Queued),
        "running" => Ok(SessionStatus::Running),
        "waiting_approval" => Ok(SessionStatus::WaitingApproval),
        "waiting_answer" => Ok(SessionStatus::WaitingAnswer),
        "failed" => Ok(SessionStatus::Failed),
        "interrupted" => Ok(SessionStatus::Interrupted),
        "archived" => Ok(SessionStatus::Archived),
        _ => bail!("invalid Session status: {value}"),
    }
}

fn runtime_task_attention(
    task: willdeep_runtime_protocol::RuntimeTask,
) -> willdeep_core::AttentionItem {
    use willdeep_runtime_protocol::TaskStatus;

    let status = match task.status {
        TaskStatus::Queued | TaskStatus::Running => willdeep_core::RuntimeStatus::Working,
        TaskStatus::Cancelling => willdeep_core::RuntimeStatus::Working,
        TaskStatus::WaitingApproval => willdeep_core::RuntimeStatus::WaitingApproval,
        TaskStatus::WaitingAnswer => willdeep_core::RuntimeStatus::WaitingAnswer,
        TaskStatus::Completed => willdeep_core::RuntimeStatus::Done,
        TaskStatus::Failed | TaskStatus::Interrupted => willdeep_core::RuntimeStatus::Failed,
        TaskStatus::Cancelled => willdeep_core::RuntimeStatus::Cancelled,
    };
    willdeep_core::AttentionItem {
        id: format!("runtime-task:{}", task.id),
        source: willdeep_core::AttentionSource::BackgroundShell,
        status,
        title: format!("Runtime task {}", task.id),
        detail: format!(
            "Workspace: {}\nStatus: {:?}",
            task.workspace.as_deref().unwrap_or("-"),
            task.status
        ),
        elapsed_millis: task
            .started_at
            .map(|started| now().saturating_sub(started).saturating_mul(1_000)),
    }
}

pub(super) fn runtime_task_visible(
    task: &willdeep_runtime_protocol::RuntimeTask,
    timestamp: u64,
) -> bool {
    !matches!(
        task.status,
        willdeep_runtime_protocol::TaskStatus::Completed
            | willdeep_runtime_protocol::TaskStatus::Cancelled
    ) || task
        .completed_at
        .is_some_and(|completed| timestamp.saturating_sub(completed) <= 5 * 60)
}

pub(crate) async fn resolve_remote_approval(
    home: &Path,
    id: uuid::Uuid,
    decision: willdeep_core::ApprovalDecision,
) -> Result<()> {
    let decision = match decision {
        willdeep_core::ApprovalDecision::AllowOnce => "allow_once",
        willdeep_core::ApprovalDecision::Deny => "deny",
        willdeep_core::ApprovalDecision::AlwaysAllow => "always_allow",
    };
    let state = ensure_running(home).await?;
    let response: willdeep_runtime_protocol::ApiResponse<serde_json::Value> =
        runtime_client(&state)?
            .call(
                "approval.resolve",
                &serde_json::json!({"id": id, "decision": decision}),
                None,
            )
            .await?;
    api_data(response).map(|_| ())
}

pub(crate) async fn answer_remote_question(
    home: &Path,
    id: uuid::Uuid,
    answer: Option<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response: willdeep_runtime_protocol::ApiResponse<serde_json::Value> =
        runtime_client(&state)?
            .call(
                "question.answer",
                &serde_json::json!({"id": id, "answer": answer}),
                None,
            )
            .await?;
    api_data(response).map(|_| ())
}

pub(crate) async fn cancel_remote_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response: willdeep_runtime_protocol::ApiResponse<willdeep_runtime_protocol::RuntimeTask> =
        runtime_client(&state)?
            .call("task.cancel", &serde_json::json!({"id": id}), None)
            .await?;
    api_data(response).map(|_| ())
}
