use super::*;
use futures_util::StreamExt;

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
    let sessions: Vec<session_store::RuntimeSession> =
        authorized_get(&state, "/v1/sessions").await?;
    Ok(sessions
        .into_iter()
        .map(|session| RemoteSessionState {
            id: session.id,
            archived: session.status == session_store::RuntimeSessionStatus::Archived,
            active: session.active_turn_id.is_some(),
        })
        .collect())
}

pub(crate) async fn rename_remote_session(
    home: &Path,
    id: uuid::Uuid,
    title: String,
) -> Result<()> {
    post_remote_session_action(
        home,
        id,
        "rename",
        Some(&session_store::RenameRuntimeSession { title }),
    )
    .await
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
    let response = client()
        .post(format!("http://{}/v1/sessions/{id}/fork", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&session_store::ForkRuntimeSession {
            title,
            through_turn_id,
            provider_profile,
            model,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Session fork: {}", response.text().await?);
    }
    Ok(response.json::<session_store::RuntimeSession>().await?.id)
}

pub(crate) async fn set_remote_session_archived(
    home: &Path,
    id: uuid::Uuid,
    archived: bool,
) -> Result<()> {
    post_remote_session_action::<()>(
        home,
        id,
        if archived { "archive" } else { "unarchive" },
        None,
    )
    .await
}

pub(crate) async fn delete_remote_session(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .delete(format!("http://{}/v1/sessions/{id}", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&session_store::DeleteRuntimeSession { confirmation: id })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Session deletion: {}",
            response.text().await?
        );
    }
    Ok(())
}

pub(crate) async fn export_remote_session(
    home: &Path,
    id: uuid::Uuid,
) -> Result<serde_json::Value> {
    let state = ensure_running(home).await?;
    authorized_get(&state, &format!("/v1/sessions/{id}/export")).await
}

pub(crate) async fn search_remote_sessions(
    home: &Path,
    parameters: &[(String, String)],
) -> Result<serde_json::Value> {
    let state = ensure_running(home).await?;
    let response = client()
        .get(format!("http://{}/v1/sessions/search", state.address))
        .header(TOKEN_HEADER, &state.token)
        .query(parameters)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Session search: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
}

async fn post_remote_session_action<T: Serialize + ?Sized>(
    home: &Path,
    id: uuid::Uuid,
    action: &str,
    body: Option<&T>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let request = client()
        .post(format!(
            "http://{}/v1/sessions/{id}/{action}",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token);
    let response = match body {
        Some(body) => request.json(body).send().await?,
        None => request.send().await?,
    };
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Session {action}: {}",
            response.text().await?
        );
    }
    Ok(())
}

pub(crate) async fn ensure_runtime_session(
    home: &Path,
    id: uuid::Uuid,
    workspace: &Path,
    profile: Option<String>,
    model: Option<String>,
    config: Option<PathBuf>,
    title: String,
) -> Result<RemoteRuntimeSession> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/sessions", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&session_store::CreateRuntimeSession {
            id: Some(id),
            workspace: workspace.canonicalize()?,
            profile,
            model,
            config,
            title: Some(title),
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Session adoption: {}",
            response.text().await?
        );
    }
    let session: session_store::RuntimeSession = response.json().await?;
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
    let response = client()
        .post(format!(
            "http://{}/v1/sessions/{session_id}/turns",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(&session_store::CreateRuntimeTurn {
            request_id,
            prompt,
            attachments,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Turn: {}", response.text().await?);
    }
    let turn: session_store::RuntimeTurn = response.json().await?;
    Ok(RemoteRuntimeTurn { id: turn.id })
}

pub(crate) async fn stop_remote_turn(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/turns/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected Turn stop: {}", response.text().await?);
    }
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
    let tasks: Vec<RuntimeTask> = authorized_get(&state, "/v1/tasks").await?;
    let visible_tasks = tasks
        .into_iter()
        .filter(|task| task.workspace == workspace)
        .map(|task| (task.id, task.session_id))
        .collect::<HashMap<_, _>>();
    Ok(fetch_events(&state, after)
        .await?
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
    let response = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .build()?
        .get(format!(
            "http://{}/v1/events/stream?after={}&limit=1000",
            state.address, *cursor
        ))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
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
    if !response.status().is_success() {
        bail!("Runtime event stream returned HTTP {}", response.status());
    }
    let mut stream = response.bytes_stream();
    let mut decoder = SseDecoder::default();
    let mut visible_tasks = HashMap::<uuid::Uuid, Option<uuid::Uuid>>::new();
    while let Some(chunk) = stream.next().await {
        for event in decoder.push(&chunk?) {
            *cursor = (*cursor).max(event.sequence);
            let session_id = if let Some(task_id) = event_task_id(&event.message) {
                if let Some(session_id) = visible_tasks.get(&task_id) {
                    *session_id
                } else {
                    let task =
                        authorized_get::<RuntimeTask>(&state, &format!("/v1/tasks/{task_id}"))
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
    }
    Ok(())
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<RuntimeEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if let Ok(event) = serde_json::from_slice::<RuntimeEvent>(data) {
                events.push(event);
            }
        }
        events
    }
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
            });
        }
    };
    let workspace = workspace.canonicalize()?;
    let mut agents = authorized_get::<Vec<super::agent_store::RuntimeAgent>>(&state, "/v1/agents")
        .await?
        .into_iter()
        .filter(|agent| agent.workspace == workspace)
        .map(remote_agent)
        .collect::<Vec<_>>();
    agents.sort_by_key(|agent| (agent.parent_id.is_some(), agent.parent_id, agent.id));
    let tasks: Vec<RuntimeTask> = authorized_get(&state, "/v1/tasks").await?;
    let visible_tasks = tasks
        .iter()
        .filter(|task| task.workspace == workspace)
        .map(|task| task.id)
        .collect::<std::collections::HashSet<_>>();
    let interactions: Vec<RuntimeInteraction> =
        authorized_get::<Vec<RuntimeInteraction>>(&state, "/v1/interactions")
            .await?
            .into_iter()
            .filter(|interaction| visible_tasks.contains(&interaction.task_id))
            .collect();
    let mut attention = tasks
        .into_iter()
        .filter(|task| visible_tasks.contains(&task.id))
        .filter(|task| runtime_task_visible(task, now()))
        .filter(|task| task.status != RuntimeTaskStatus::Queued)
        .map(runtime_task_attention)
        .collect::<Vec<_>>();
    let mut gates = Vec::new();
    for interaction in interactions {
        let item = match &interaction.kind {
            InteractionKind::Approval { description, .. } => {
                gates.push(RemoteGate::Approval {
                    id: interaction.id,
                    task_id: interaction.task_id,
                    description: description.clone(),
                    always_allow_available: matches!(
                        interaction.kind,
                        InteractionKind::Approval {
                            always_allow_available: true,
                            ..
                        }
                    ),
                });
                willdeep_core::AttentionItem::approval(description.clone())
            }
            InteractionKind::Question {
                question,
                options,
                multi_select,
            } => {
                gates.push(RemoteGate::Question {
                    id: interaction.id,
                    task_id: interaction.task_id,
                    question: question.clone(),
                    options: options.clone(),
                    multi_select: *multi_select,
                });
                willdeep_core::AttentionItem::question(question.clone())
            }
        };
        attention.push(willdeep_core::AttentionItem {
            id: format!("runtime-interaction:{}", interaction.id),
            title: item.title,
            detail: format!("Runtime task {}\n{}", interaction.task_id, item.detail),
            ..item
        });
    }
    Ok(RuntimeSnapshot {
        attention,
        gates,
        agents,
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
    super::agent_control::instruct_agent(home, id, message).await
}

async fn control_remote_agent(home: &Path, id: uuid::Uuid, action: &str) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/agents/{id}/{action}", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Agent {action}: {}",
            response.text().await?
        );
    }
    Ok(())
}

fn remote_agent(agent: super::agent_store::RuntimeAgent) -> RemoteAgent {
    RemoteAgent {
        id: agent.id,
        parent_id: agent.parent_id,
        label: agent.label,
        background: agent.background,
        profile: agent.profile,
        status: match agent.status {
            super::agent_store::RuntimeAgentStatus::Queued
            | super::agent_store::RuntimeAgentStatus::Running => {
                willdeep_core::RuntimeStatus::Working
            }
            super::agent_store::RuntimeAgentStatus::WaitingApproval => {
                willdeep_core::RuntimeStatus::WaitingApproval
            }
            super::agent_store::RuntimeAgentStatus::WaitingAnswer => {
                willdeep_core::RuntimeStatus::WaitingAnswer
            }
            super::agent_store::RuntimeAgentStatus::Blocked => {
                willdeep_core::RuntimeStatus::Blocked
            }
            super::agent_store::RuntimeAgentStatus::Completed => willdeep_core::RuntimeStatus::Done,
            super::agent_store::RuntimeAgentStatus::Failed
            | super::agent_store::RuntimeAgentStatus::Interrupted => {
                willdeep_core::RuntimeStatus::Failed
            }
            super::agent_store::RuntimeAgentStatus::Cancelled => {
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
        workspace: agent.workspace,
        worktree_branch: agent.worktree_branch,
        dedicated_worktree: agent.dedicated_worktree,
    }
}

fn runtime_task_attention(task: RuntimeTask) -> willdeep_core::AttentionItem {
    let status = match task.status {
        RuntimeTaskStatus::Queued | RuntimeTaskStatus::Running => {
            willdeep_core::RuntimeStatus::Working
        }
        RuntimeTaskStatus::Cancelling => willdeep_core::RuntimeStatus::Working,
        RuntimeTaskStatus::WaitingApproval => willdeep_core::RuntimeStatus::WaitingApproval,
        RuntimeTaskStatus::WaitingAnswer => willdeep_core::RuntimeStatus::WaitingAnswer,
        RuntimeTaskStatus::Completed => willdeep_core::RuntimeStatus::Done,
        RuntimeTaskStatus::Failed | RuntimeTaskStatus::Interrupted => {
            willdeep_core::RuntimeStatus::Failed
        }
        RuntimeTaskStatus::Cancelled => willdeep_core::RuntimeStatus::Cancelled,
    };
    willdeep_core::AttentionItem {
        id: format!("runtime-task:{}", task.id),
        source: willdeep_core::AttentionSource::BackgroundShell,
        status,
        title: format!("Runtime task {}", task.id),
        detail: format!(
            "Workspace: {}\nStatus: {:?}\nPID: {}\nError: {}",
            task.workspace.display(),
            task.status,
            task.pid
                .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
            task.error.unwrap_or_default()
        ),
        elapsed_millis: task
            .started_at
            .map(|started| now().saturating_sub(started).saturating_mul(1_000)),
    }
}

pub(super) fn runtime_task_visible(task: &RuntimeTask, timestamp: u64) -> bool {
    !matches!(
        task.status,
        RuntimeTaskStatus::Completed | RuntimeTaskStatus::Cancelled
    ) || task
        .completed_at
        .is_some_and(|completed| timestamp.saturating_sub(completed) <= 5 * 60)
}

pub(crate) async fn resolve_remote_approval(
    home: &Path,
    id: uuid::Uuid,
    decision: willdeep_core::ApprovalDecision,
) -> Result<()> {
    let resolution = match decision {
        willdeep_core::ApprovalDecision::AllowOnce => InteractionResolution::AllowOnce,
        willdeep_core::ApprovalDecision::Deny => InteractionResolution::Deny,
        willdeep_core::ApprovalDecision::AlwaysAllow => InteractionResolution::AlwaysAllow,
    };
    resolve_interaction_quiet(home, id, resolution).await
}

pub(crate) async fn answer_remote_question(
    home: &Path,
    id: uuid::Uuid,
    answer: Option<String>,
) -> Result<()> {
    resolve_interaction_quiet(home, id, InteractionResolution::Answer(answer)).await
}

pub(crate) async fn cancel_remote_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected task cancellation: HTTP {}",
            response.status()
        );
    }
    Ok(())
}

async fn resolve_interaction_quiet(
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
            "Runtime rejected interaction resolution: HTTP {}",
            response.status()
        );
    }
    Ok(())
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn sse_decoder_handles_split_utf8_and_multiple_events() {
        let first = RuntimeEvent {
            sequence: 7,
            timestamp: 1,
            kind: "模型.事件".to_owned(),
            message: "中文消息".to_owned(),
        };
        let second = RuntimeEvent {
            sequence: 8,
            timestamp: 2,
            kind: "task.completed".to_owned(),
            message: "done".to_owned(),
        };
        let payload = format!(
            "id: 7\nevent: runtime\ndata: {}\n\nid: 8\ndata: {}\n\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        let split = payload.find("中文").unwrap() + 1;
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&payload.as_bytes()[..split]).is_empty());
        assert_eq!(
            decoder.push(&payload.as_bytes()[split..]),
            vec![first, second]
        );
    }
}
