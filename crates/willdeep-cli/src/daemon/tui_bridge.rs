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
    pub tasks: Vec<RemoteTask>,
    pub tools: Vec<willdeep_runtime_protocol::RuntimeTool>,
    pub artifacts: Vec<willdeep_runtime_protocol::RuntimeArtifact>,
    /// Version reported by the Runtime that actually executes tools. The TUI
    /// is only a front end: a long-lived daemon can be several releases
    /// behind this binary, and every policy change lives in the daemon.
    pub runtime_version: Option<String>,
}

#[derive(Clone)]
pub(crate) struct RemoteTask {
    pub id: uuid::Uuid,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub agent_id: Option<uuid::Uuid>,
    pub status: willdeep_runtime_protocol::TaskStatus,
    pub profile: Option<String>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub failure_domain: Option<willdeep_runtime_protocol::FailureDomain>,
}

#[derive(Clone)]
pub(crate) struct RemoteAgent {
    pub id: uuid::Uuid,
    pub parent_id: Option<uuid::Uuid>,
    pub label: Option<String>,
    pub background: bool,
    pub profile: Option<String>,
    pub model: Option<String>,
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
    pub created_at: u64,
    pub completed_at: Option<u64>,
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
    pub active_turn_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteActiveTurn {
    pub session_id: uuid::Uuid,
    pub turn_id: uuid::Uuid,
    pub task_id: Option<uuid::Uuid>,
    pub replay_after: u64,
}

pub(crate) async fn remote_session_states(home: &Path) -> Result<Vec<RemoteSessionState>> {
    let state = ensure_running(home).await?;
    let sessions = api_data(runtime_client(&state)?.sessions().await?)?;
    Ok(sessions
        .into_iter()
        .map(|session| RemoteSessionState {
            id: session.id,
            archived: session.status == willdeep_runtime_protocol::SessionStatus::Archived,
            active: session.active_turn_id.is_some(),
            active_turn_id: session.active_turn_id,
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
            .rename_session(
                &willdeep_runtime_protocol::RenameSessionParams { id, title },
                uuid::Uuid::new_v4(),
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
            .fork_session(
                &willdeep_runtime_protocol::ForkSessionParams {
                    id,
                    title,
                    through_turn_id,
                    provider_profile,
                    model,
                },
                uuid::Uuid::new_v4(),
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
            .archive_session(
                &willdeep_runtime_protocol::ArchiveSessionParams { id, archived },
                uuid::Uuid::new_v4(),
            )
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn delete_remote_session(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .delete_session(
                &willdeep_runtime_protocol::DeleteSessionParams {
                    id,
                    confirmation: id,
                },
                uuid::Uuid::new_v4(),
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
    api_data(runtime_client(&state)?.export_session(id).await?)
}

pub(crate) async fn search_remote_session_results(
    home: &Path,
    parameters: &[(String, String)],
) -> Result<Vec<willdeep_runtime_protocol::SessionSearchResult>> {
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
    let results = api_data(runtime_client(&state)?.search_sessions(&params).await?)?;
    Ok(results)
}

/// 让 Runtime 登记（或确认已登记）一条已经存在的 Core 会话。
///
/// **不带标题**。这是一次「领养」，Runtime 那边直接沿用会话文件里的标题；
/// 而幂等键是会话 ID 本身（同一条会话反复 ensure 必须是同一次调用），所以
/// 参数里塞进任何**会变**的字段都会变成地雷：标题一改，下一次 ensure 就会
/// 撞上「同一个 request_id 配了不同的参数」而整条失败。自动标题正是踩响它的
/// 那只脚——`/session rename` 早就能踩响，只是一直没人踩到。
pub(crate) async fn ensure_runtime_session(
    home: &Path,
    id: uuid::Uuid,
    workspace: &Path,
    profile: Option<String>,
    model: Option<String>,
) -> Result<RemoteRuntimeSession> {
    let state = ensure_running(home).await?;
    let session = api_data(
        runtime_client(&state)?
            .create_session(
                &willdeep_runtime_protocol::CreateSessionParams {
                    id: Some(id),
                    workspace: workspace.canonicalize()?.display().to_string(),
                    profile,
                    model,
                    title: None,
                },
                id,
            )
            .await?,
    )?;
    Ok(RemoteRuntimeSession {
        id: session.id,
        root_agent_id: session.root_agent_id,
    })
}

pub(crate) async fn update_remote_session_model(
    home: &Path,
    id: uuid::Uuid,
    model: String,
) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .update_session_model(
                &willdeep_runtime_protocol::UpdateSessionModelParams { id, model },
                uuid::Uuid::new_v4(),
            )
            .await?,
    )?;
    Ok(())
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
    let client = runtime_client(&state)?;
    let public_attachments = attachments
        .into_iter()
        .map(public_attachment)
        .collect::<Vec<_>>();
    let params = willdeep_runtime_protocol::SubmitTurnParams {
        session_id,
        turn_request_id: request_id,
        prompt,
        attachments: public_attachments,
    };
    let turn = api_data(client.submit_turn(&params, request_id).await?)?;
    Ok(RemoteRuntimeTurn { id: turn.id })
}

pub(crate) async fn stop_remote_turn(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .stop_turn(id, uuid::Uuid::new_v4())
            .await?,
    )?;
    Ok(())
}

/// Resolves the Session that owns `id`, so callers can authorize a Turn action
/// against their own Workspace allowlist. Returns `None` when the Runtime does
/// not know the Turn at all.
pub(crate) async fn remote_turn_session(home: &Path, id: uuid::Uuid) -> Result<Option<uuid::Uuid>> {
    let state = ensure_running(home).await?;
    match runtime_client(&state)?.turn(id).await? {
        willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => Ok(Some(data.session_id)),
        willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
            if error.code == willdeep_runtime_protocol::ErrorCode::NotFound {
                Ok(None)
            } else {
                bail!("Runtime API error: {}", error.message)
            }
        }
    }
}

pub(crate) async fn remote_active_turn(
    home: &Path,
    session_id: uuid::Uuid,
) -> Result<Option<RemoteActiveTurn>> {
    let state = ensure_running(home).await?;
    let client = runtime_client(&state)?;
    let session = api_data(client.session(session_id).await?)?;
    let Some(turn_id) = session.active_turn_id else {
        return Ok(None);
    };
    let turn = api_data(client.turn(turn_id).await?)?;
    if turn.session_id != session_id {
        bail!("Runtime active Turn does not belong to its Session")
    }
    let (task_id, replay_after) = if let Some(task_id) = turn.active_task_id {
        let task = api_data(client.task(task_id).await?)?;
        if task.session_id != Some(session_id) || task.turn_id != Some(turn_id) {
            bail!("Runtime active Task does not belong to its Turn")
        }
        (Some(task_id), task.event_start_sequence.saturating_sub(1))
    } else {
        let replay_after = probe(&state).await?.event_sequence;
        let refreshed_turn = api_data(client.turn(turn_id).await?)?;
        if let Some(task_id) = refreshed_turn.active_task_id {
            let task = api_data(client.task(task_id).await?)?;
            if task.session_id != Some(session_id) || task.turn_id != Some(turn_id) {
                bail!("Runtime active Task does not belong to its Turn")
            }
            (Some(task_id), task.event_start_sequence.saturating_sub(1))
        } else {
            (None, replay_after)
        }
    };
    if api_data(client.session(session_id).await?)?.active_turn_id != Some(turn_id) {
        return Ok(None);
    }
    Ok(Some(RemoteActiveTurn {
        session_id,
        turn_id,
        task_id,
        replay_after,
    }))
}

pub(crate) async fn remote_latest_turn(
    home: &Path,
    session_id: uuid::Uuid,
) -> Result<Option<willdeep_runtime_protocol::RuntimeTurn>> {
    let state = ensure_running(home).await?;
    let mut turns = api_data(runtime_client(&state)?.turns(session_id).await?)?;
    if turns.iter().any(|turn| turn.session_id != session_id) {
        bail!("Runtime returned a Turn owned by another Session")
    }
    turns.sort_by_key(|turn| turn.queue_sequence);
    Ok(turns.pop())
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
    let tasks = api_data(runtime_client(&state)?.tasks().await?)?;
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
            .events(&willdeep_runtime_protocol::EventListParams { after, limit: 200 })
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
                let client = runtime_client(&state)?;
                let (task_found, session_id) = match client.task(task_id).await {
                    Ok(response) => match api_data(response) {
                        Ok(task) => (
                            true,
                            (task.workspace.as_deref().map(Path::new) == Some(workspace))
                                .then_some(task.session_id)
                                .flatten(),
                        ),
                        Err(_) => (false, None),
                    },
                    Err(_) => (false, None),
                };
                if task_found {
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

/// `session` 是调用方当前打开的会话：工作区决定「能看到什么」，会话决定
/// 「哪些审批该在这里弹」。传 `None` 表示调用方就是工作区级视图（Web 端），
/// 此时保持旧的工作区口径。
pub(crate) async fn runtime_snapshot(
    home: &Path,
    workspace: &Path,
    session: Option<uuid::Uuid>,
) -> Result<RuntimeSnapshot> {
    let paths = DaemonPaths::new(home);
    let (state, runtime_version) = match load_state(&paths.state) {
        Ok(state) => match probe(&state).await {
            Ok(health) => (state, Some(health.version)),
            Err(_) => {
                return Ok(RuntimeSnapshot {
                    attention: Vec::new(),
                    gates: Vec::new(),
                    agents: Vec::new(),
                    tasks: Vec::new(),
                    tools: Vec::new(),
                    artifacts: Vec::new(),
                    runtime_version: None,
                });
            }
        },
        Err(_) => {
            return Ok(RuntimeSnapshot {
                attention: Vec::new(),
                gates: Vec::new(),
                agents: Vec::new(),
                tasks: Vec::new(),
                tools: Vec::new(),
                artifacts: Vec::new(),
                runtime_version: None,
            });
        }
    };
    let workspace = workspace.canonicalize()?;
    let response = runtime_client(&state)?.agents().await?;
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
    let tasks = api_data(runtime_client(&state)?.tasks().await?)?;
    // 同一个工作区可能同时开着多个 TUI。任务对本工作区可见，不代表它的审批该在
    // 这里弹——所以除了「哪些任务可见」，还要记住「每个任务归谁」。
    let visible_tasks = tasks
        .iter()
        .filter(|task| {
            task.workspace
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == workspace)
        })
        .map(|task| (task.id, task.session_id))
        .collect::<std::collections::HashMap<_, _>>();
    let remote_tasks = tasks
        .iter()
        .filter(|task| visible_tasks.contains_key(&task.id))
        .filter(|task| runtime_task_visible(task, now()))
        .filter(|task| task.status != willdeep_runtime_protocol::TaskStatus::Queued)
        .map(|task| RemoteTask {
            id: task.id,
            session_id: task.session_id,
            turn_id: task.turn_id,
            agent_id: task.agent_id,
            status: task.status,
            profile: task.profile.clone(),
            created_at: task.created_at,
            started_at: task.started_at,
            completed_at: task.completed_at,
            exit_code: task.exit_code,
            failure_domain: task.failure_domain,
        })
        .collect();
    let tools = api_data(
        runtime_client(&state)?
            .tools(&willdeep_runtime_protocol::ListToolsParams {
                limit: Some(100),
                ..Default::default()
            })
            .await?,
    )?
    .into_iter()
    .filter(|tool| visible_tasks.contains_key(&tool.task_id))
    .collect();
    let artifacts = api_data(
        runtime_client(&state)?
            .artifacts(&willdeep_runtime_protocol::ListArtifactsParams {
                limit: Some(100),
                ..Default::default()
            })
            .await?,
    )?
    .into_iter()
    .filter(|artifact| visible_tasks.contains_key(&artifact.task_id))
    .collect();
    let approvals = api_data(runtime_client(&state)?.approvals().await?)?;
    let questions = api_data(runtime_client(&state)?.questions().await?)?;
    let mut attention = tasks
        .into_iter()
        .filter(|task| visible_tasks.contains_key(&task.id))
        .filter(|task| runtime_task_visible(task, now()))
        .filter(|task| task.status != willdeep_runtime_protocol::TaskStatus::Queued)
        .map(runtime_task_attention)
        .collect::<Vec<_>>();
    // 审批和提问会弹成独占对话框，还能被就地解掉——归属别的会话的，绝不能在这里
    // 弹出来：那等于替一条你没看见上下文的命令签字。没有会话归属的任务（例如
    // headless 提交的）仍然全弹，否则没有任何客户端能解开它，任务就吊死了。
    let owned = |task_id: &uuid::Uuid| gate_belongs_here(session, visible_tasks.get(task_id));
    let mut gates = Vec::new();
    for approval in approvals
        .into_iter()
        .filter(|approval| owned(&approval.task_id))
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
        .filter(|question| owned(&question.task_id))
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
        tasks: remote_tasks,
        tools,
        artifacts,
        runtime_version,
    })
}

pub(crate) async fn remote_agent_detail(home: &Path, id: uuid::Uuid) -> Result<RemoteAgent> {
    let state = ensure_running(home).await?;
    api_data(runtime_client(&state)?.agent(id).await?).map(remote_agent)
}

pub(crate) async fn stop_remote_agent(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .stop_agent(id, uuid::Uuid::new_v4())
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn retry_remote_agent(home: &Path, id: uuid::Uuid) -> Result<()> {
    retry_remote_agent_with_model(home, id, None).await
}

pub(crate) async fn retry_remote_agent_with_model(
    home: &Path,
    id: uuid::Uuid,
    model: Option<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    api_data(
        runtime_client(&state)?
            .retry_agent_with_model(id, model, uuid::Uuid::new_v4())
            .await?,
    )?;
    Ok(())
}

pub(crate) async fn spawn_remote_agent(
    home: &Path,
    session_id: uuid::Uuid,
    prompt: String,
    profile: String,
    label: Option<String>,
) -> Result<RemoteAgent> {
    let state = ensure_running(home).await?;
    let agent = api_data(
        runtime_client(&state)?
            .spawn_agent(
                &willdeep_runtime_protocol::SpawnAgentParams {
                    session_id,
                    prompt,
                    profile: Some(profile),
                    label,
                },
                uuid::Uuid::new_v4(),
            )
            .await?,
    )?;
    Ok(remote_agent(agent))
}

pub(crate) async fn instruct_remote_agent(
    home: &Path,
    id: uuid::Uuid,
    message: String,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .prompt_agent(
            &willdeep_runtime_protocol::AgentPromptParams { id, message },
            uuid::Uuid::new_v4(),
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
        model: agent.model,
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
        created_at: agent.created_at,
        completed_at: agent.completed_at,
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

/// 一个 gate（审批/提问）该不该在这个客户端弹出来。
///
/// `viewer` 是调用方打开的会话，`owner` 是该 gate 所属任务的归属会话
/// （`None` 表示任务不在本工作区，外层 `get` 就没命中）。
fn gate_belongs_here(viewer: Option<uuid::Uuid>, owner: Option<&Option<uuid::Uuid>>) -> bool {
    match owner {
        // 任务不在本工作区：本来就看不见。
        None => false,
        // 工作区级视图（Web）：维持旧口径，工作区里的都归它管。
        Some(_) if viewer.is_none() => true,
        // 没有会话归属的任务（headless 提交的）：谁都能解，否则没人解得开。
        Some(None) => true,
        Some(owner) => *owner == viewer,
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
        // 退出码和失败域本来就在手上，早先却被扔掉——一条失败任务只写
        // 「Status: Failed」，看了等于没看。
        detail: [
            Some(format!(
                "Workspace: {}",
                task.workspace.as_deref().unwrap_or("-")
            )),
            Some(format!("Status: {:?}", task.status)),
            task.exit_code.map(|code| format!("Exit code: {code}")),
            task.failure_domain
                .map(|domain| format!("Failure domain: {domain:?}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
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

/// 取回一个任务的失败排查材料。只有本机 Runtime 提供，旧版本 Daemon 没有这个
/// 操作，调用方要能接受失败。
pub(crate) async fn remote_task_diagnostics(
    home: &Path,
    id: uuid::Uuid,
) -> Result<willdeep_runtime_protocol::RuntimeTaskDiagnostics> {
    let state = ensure_running(home).await?;
    api_data(runtime_client(&state)?.task_diagnostics(id).await?)
}

pub(crate) async fn resolve_remote_approval(
    home: &Path,
    id: uuid::Uuid,
    decision: willdeep_core::ApprovalDecision,
) -> Result<()> {
    let decision = match decision {
        willdeep_core::ApprovalDecision::AllowOnce => {
            willdeep_runtime_protocol::ApprovalDecision::AllowOnce
        }
        willdeep_core::ApprovalDecision::Deny => willdeep_runtime_protocol::ApprovalDecision::Deny,
        willdeep_core::ApprovalDecision::AlwaysAllow => {
            willdeep_runtime_protocol::ApprovalDecision::AlwaysAllow
        }
    };
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .resolve_approval(
            &willdeep_runtime_protocol::ResolveApprovalParams { id, decision },
            uuid::Uuid::new_v4(),
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
    let response = runtime_client(&state)?
        .answer_question(
            &willdeep_runtime_protocol::AnswerQuestionParams { id, answer },
            uuid::Uuid::new_v4(),
        )
        .await?;
    api_data(response).map(|_| ())
}

pub(crate) async fn cancel_remote_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = runtime_client(&state)?
        .cancel_task(id, uuid::Uuid::new_v4())
        .await?;
    api_data(response).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一个目录开两个 TUI 时，A 会话的审批框曾经在 B 会话照弹，
    /// 而且在 B 里点「允许」会真的把 A 的审批解掉——等于替一条没看见的命令签字。
    #[test]
    fn approval_gates_only_surface_in_the_session_that_raised_them() {
        let mine = uuid::Uuid::new_v4();
        let theirs = uuid::Uuid::new_v4();

        assert!(gate_belongs_here(Some(mine), Some(&Some(mine))));
        assert!(
            !gate_belongs_here(Some(mine), Some(&Some(theirs))),
            "另一个会话的审批不能在这里弹"
        );
        // headless 提交的任务没有会话归属：不放行就没有任何客户端解得开它。
        assert!(gate_belongs_here(Some(mine), Some(&None)));
        // 工作区级视图（Web）维持旧口径。
        assert!(gate_belongs_here(None, Some(&Some(theirs))));
        // 不在本工作区的任务，连看都看不到。
        assert!(!gate_belongs_here(Some(mine), None));
    }
}
