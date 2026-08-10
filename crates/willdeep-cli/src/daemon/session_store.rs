use super::*;

const RUNTIME_SESSION_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeSessionStatus {
    Idle,
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Failed,
    Interrupted,
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeSession {
    pub schema: u32,
    pub id: uuid::Uuid,
    pub root_agent_id: uuid::Uuid,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub config: Option<PathBuf>,
    pub status: RuntimeSessionStatus,
    pub active_turn_id: Option<uuid::Uuid>,
    pub created_at: u64,
    pub updated_at: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CreateRuntimeSession {
    #[serde(default)]
    pub id: Option<uuid::Uuid>,
    pub workspace: PathBuf,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub config: Option<PathBuf>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeTurnStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeTurn {
    pub id: uuid::Uuid,
    pub session_id: uuid::Uuid,
    pub request_id: uuid::Uuid,
    #[serde(default)]
    pub queue_sequence: u64,
    pub status: RuntimeTurnStatus,
    pub active_task_id: Option<uuid::Uuid>,
    pub attempts: u32,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredRuntimeTurn {
    metadata: RuntimeTurn,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CreateRuntimeTurn {
    pub request_id: uuid::Uuid,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<willdeep_core::MessageAttachment>,
}

pub(super) struct ClaimedRuntimeTurn {
    pub metadata: RuntimeTurn,
    pub request: SubmitTask,
}

pub(super) struct CancelRuntimeTurn {
    pub task_id: Option<uuid::Uuid>,
    pub session_id: uuid::Uuid,
    pub cancelled_queued: bool,
}

pub(super) struct RuntimeSessionStore {
    path: PathBuf,
    turns_path: PathBuf,
    core: willdeep_core::SessionStore,
    sessions: Mutex<HashMap<uuid::Uuid, RuntimeSession>>,
    turns: Mutex<HashMap<uuid::Uuid, StoredRuntimeTurn>>,
}

impl RuntimeSessionStore {
    pub fn open(path: PathBuf, home: &Path) -> Result<Self> {
        let mut sessions = load_sessions(&path)?;
        let turns_path = path.with_file_name("turns.json");
        let mut turns = load_turns(&turns_path)?;
        let mut changed = false;
        for session in sessions.values_mut() {
            if matches!(
                session.status,
                RuntimeSessionStatus::Queued
                    | RuntimeSessionStatus::Running
                    | RuntimeSessionStatus::WaitingApproval
                    | RuntimeSessionStatus::WaitingAnswer
            ) {
                session.status = RuntimeSessionStatus::Interrupted;
                session.active_turn_id = None;
                session.updated_at = now();
                session.last_error = Some("Runtime restarted while Session was active".to_owned());
                changed = true;
            }
        }
        if changed {
            persist_sessions(&path, &sessions)?;
        }
        let mut turns_changed = false;
        for turn in turns.values_mut() {
            if matches!(
                turn.metadata.status,
                RuntimeTurnStatus::Running
                    | RuntimeTurnStatus::WaitingApproval
                    | RuntimeTurnStatus::WaitingAnswer
            ) {
                turn.metadata.status = RuntimeTurnStatus::Interrupted;
                turn.metadata.completed_at = Some(now());
                turn.metadata.error = Some("Runtime restarted while Turn was active".to_owned());
                turns_changed = true;
            }
        }
        if turns_changed {
            persist_turns(&turns_path, &turns)?;
        }
        Ok(Self {
            path,
            turns_path,
            core: willdeep_core::SessionStore::new(home),
            sessions: Mutex::new(sessions),
            turns: Mutex::new(turns),
        })
    }

    #[cfg(test)]
    pub fn create(&self, request: CreateRuntimeSession) -> Result<RuntimeSession> {
        Ok(self.ensure(request)?.0)
    }

    pub fn ensure(&self, request: CreateRuntimeSession) -> Result<(RuntimeSession, bool)> {
        let workspace = request
            .workspace
            .canonicalize()
            .with_context(|| format!("invalid workspace: {}", request.workspace.display()))?;
        if let Some(id) = request.id
            && let Some(existing) = self.get(id)?
        {
            if existing.workspace != workspace {
                bail!("Runtime Session workspace does not match existing metadata");
            }
            return Ok((existing, false));
        }
        let core = if let Some(id) = request.id {
            let core = self
                .core
                .load(id)
                .with_context(|| format!("adopt Core Session {id}"))?;
            if core.workspace.canonicalize()? != workspace {
                bail!("Core Session workspace does not match Runtime Session request");
            }
            core
        } else {
            let title = request
                .title
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "New Runtime session".to_owned());
            let mut core =
                willdeep_core::Session::new(workspace.clone(), request.profile.clone(), &title);
            self.core.save(&mut core)?;
            core
        };
        let timestamp = now();
        let session = RuntimeSession {
            schema: RUNTIME_SESSION_SCHEMA,
            id: core.id,
            root_agent_id: uuid::Uuid::new_v4(),
            workspace,
            profile: request.profile,
            config: request.config,
            status: RuntimeSessionStatus::Idle,
            active_turn_id: None,
            created_at: timestamp,
            updated_at: timestamp,
            last_error: None,
        };
        let mut sessions = self.lock()?;
        sessions.insert(session.id, session.clone());
        if let Err(error) = persist_sessions(&self.path, &sessions) {
            sessions.remove(&session.id);
            return Err(error);
        }
        Ok((session, true))
    }

    pub fn list(&self) -> Result<Vec<RuntimeSession>> {
        let mut sessions = self.lock()?.values().cloned().collect::<Vec<_>>();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        Ok(sessions)
    }

    pub fn get(&self, id: uuid::Uuid) -> Result<Option<RuntimeSession>> {
        Ok(self.lock()?.get(&id).cloned())
    }

    pub fn enqueue_turn(
        &self,
        session_id: uuid::Uuid,
        request: CreateRuntimeTurn,
    ) -> Result<(RuntimeTurn, bool)> {
        if request.prompt.trim().is_empty() && request.attachments.is_empty() {
            bail!("Turn prompt and attachments must not both be empty");
        }
        let session = self.get(session_id)?.context("Runtime Session not found")?;
        if session.status == RuntimeSessionStatus::Archived {
            bail!("Runtime Session is archived");
        }
        let mut turns = self.turns_lock()?;
        if let Some(turn) = turns.values().find(|turn| {
            turn.metadata.session_id == session_id && turn.metadata.request_id == request.request_id
        }) {
            return Ok((turn.metadata.clone(), false));
        }
        let timestamp = now();
        let queue_sequence = turns
            .values()
            .map(|turn| turn.metadata.queue_sequence)
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        let metadata = RuntimeTurn {
            id: uuid::Uuid::new_v4(),
            session_id,
            request_id: request.request_id,
            queue_sequence,
            status: RuntimeTurnStatus::Queued,
            active_task_id: None,
            attempts: 0,
            created_at: timestamp,
            started_at: None,
            completed_at: None,
            error: None,
        };
        turns.insert(
            metadata.id,
            StoredRuntimeTurn {
                metadata: metadata.clone(),
                prompt: request.prompt,
                attachments: request.attachments,
            },
        );
        persist_turns(&self.turns_path, &turns)?;
        Ok((metadata, true))
    }

    pub fn list_turns(&self, session_id: uuid::Uuid) -> Result<Vec<RuntimeTurn>> {
        let mut turns = self
            .turns_lock()?
            .values()
            .filter(|turn| turn.metadata.session_id == session_id)
            .map(|turn| turn.metadata.clone())
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| turn.queue_sequence);
        Ok(turns)
    }

    pub fn get_turn(&self, id: uuid::Uuid) -> Result<Option<RuntimeTurn>> {
        Ok(self
            .turns_lock()?
            .get(&id)
            .map(|turn| turn.metadata.clone()))
    }

    pub fn schedulable_sessions(&self) -> Result<Vec<uuid::Uuid>> {
        let sessions = self.lock()?;
        let turns = self.turns_lock()?;
        Ok(sessions
            .values()
            .filter(|session| {
                session.active_turn_id.is_none()
                    && session.status != RuntimeSessionStatus::Archived
                    && turns.values().any(|turn| {
                        turn.metadata.session_id == session.id
                            && turn.metadata.status == RuntimeTurnStatus::Queued
                    })
            })
            .map(|session| session.id)
            .collect())
    }

    pub fn claim_next(&self, session_id: uuid::Uuid) -> Result<Option<ClaimedRuntimeTurn>> {
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        if session.active_turn_id.is_some()
            || matches!(
                session.status,
                RuntimeSessionStatus::Queued
                    | RuntimeSessionStatus::Running
                    | RuntimeSessionStatus::WaitingApproval
                    | RuntimeSessionStatus::WaitingAnswer
                    | RuntimeSessionStatus::Archived
            )
        {
            return Ok(None);
        }
        let mut turns = self.turns_lock()?;
        let Some(turn) = turns
            .values_mut()
            .filter(|turn| {
                turn.metadata.session_id == session_id
                    && turn.metadata.status == RuntimeTurnStatus::Queued
            })
            .min_by_key(|turn| turn.metadata.queue_sequence)
        else {
            return Ok(None);
        };
        turn.metadata.attempts = turn.metadata.attempts.saturating_add(1);
        session.status = RuntimeSessionStatus::Queued;
        session.active_turn_id = Some(turn.metadata.id);
        session.updated_at = now();
        session.last_error = None;
        let claimed = ClaimedRuntimeTurn {
            metadata: turn.metadata.clone(),
            request: SubmitTask {
                prompt: turn.prompt.clone(),
                attachments: turn.attachments.clone(),
                workspace: session.workspace.clone(),
                profile: session.profile.clone(),
                config: session.config.clone(),
                session_id: Some(session.id),
                turn_id: Some(turn.metadata.id),
            },
        };
        persist_turns(&self.turns_path, &turns)?;
        persist_sessions(&self.path, &sessions)?;
        Ok(Some(claimed))
    }

    pub fn bind_task(&self, turn_id: uuid::Uuid, task_id: uuid::Uuid) -> Result<()> {
        let mut turns = self.turns_lock()?;
        let turn = turns.get_mut(&turn_id).context("Runtime Turn not found")?;
        turn.metadata.status = RuntimeTurnStatus::Running;
        turn.metadata.active_task_id = Some(task_id);
        turn.metadata.started_at = Some(now());
        turn.metadata.completed_at = None;
        turn.metadata.error = None;
        let session_id = turn.metadata.session_id;
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.status = RuntimeSessionStatus::Running;
        session.updated_at = now();
        persist_sessions(&self.path, &sessions)
    }

    pub fn complete_task(
        &self,
        task_id: uuid::Uuid,
        status: RuntimeTaskStatus,
        error: Option<String>,
    ) -> Result<Option<uuid::Uuid>> {
        let mut turns = self.turns_lock()?;
        let Some(turn) = turns
            .values_mut()
            .find(|turn| turn.metadata.active_task_id == Some(task_id))
        else {
            return Ok(None);
        };
        turn.metadata.status = match status {
            RuntimeTaskStatus::Completed => RuntimeTurnStatus::Completed,
            RuntimeTaskStatus::Cancelled => RuntimeTurnStatus::Cancelled,
            RuntimeTaskStatus::Interrupted => RuntimeTurnStatus::Interrupted,
            _ => RuntimeTurnStatus::Failed,
        };
        turn.metadata.completed_at = Some(now());
        turn.metadata.error = error.clone();
        if status == RuntimeTaskStatus::Completed {
            // The Core Session is the durable conversation source after a successful
            // Harness process exit. Do not retain a second private copy indefinitely.
            turn.prompt.clear();
            turn.attachments.clear();
        }
        let session_id = turn.metadata.session_id;
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.active_turn_id = None;
        session.status = if status == RuntimeTaskStatus::Completed {
            RuntimeSessionStatus::Idle
        } else {
            RuntimeSessionStatus::Failed
        };
        session.updated_at = now();
        session.last_error = error;
        persist_sessions(&self.path, &sessions)?;
        Ok(Some(session_id))
    }

    pub fn set_task_waiting(
        &self,
        task_id: uuid::Uuid,
        turn_status: RuntimeTurnStatus,
    ) -> Result<()> {
        let mut turns = self.turns_lock()?;
        let Some(turn) = turns
            .values_mut()
            .find(|turn| turn.metadata.active_task_id == Some(task_id))
        else {
            return Ok(());
        };
        turn.metadata.status = turn_status;
        let session_id = turn.metadata.session_id;
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.status = match turn_status {
            RuntimeTurnStatus::WaitingApproval => RuntimeSessionStatus::WaitingApproval,
            RuntimeTurnStatus::WaitingAnswer => RuntimeSessionStatus::WaitingAnswer,
            _ => RuntimeSessionStatus::Running,
        };
        session.updated_at = now();
        persist_sessions(&self.path, &sessions)
    }

    pub fn request_cancel(&self, turn_id: uuid::Uuid) -> Result<CancelRuntimeTurn> {
        let mut turns = self.turns_lock()?;
        let turn = turns.get_mut(&turn_id).context("Runtime Turn not found")?;
        let session_id = turn.metadata.session_id;
        if let Some(task_id) = turn.metadata.active_task_id
            && matches!(
                turn.metadata.status,
                RuntimeTurnStatus::Running
                    | RuntimeTurnStatus::WaitingApproval
                    | RuntimeTurnStatus::WaitingAnswer
            )
        {
            return Ok(CancelRuntimeTurn {
                task_id: Some(task_id),
                session_id,
                cancelled_queued: false,
            });
        }
        let mut cancelled_queued = false;
        if turn.metadata.status == RuntimeTurnStatus::Queued {
            turn.metadata.status = RuntimeTurnStatus::Cancelled;
            turn.metadata.completed_at = Some(now());
            persist_turns(&self.turns_path, &turns)?;
            cancelled_queued = true;
        }
        drop(turns);
        if cancelled_queued {
            let mut sessions = self.lock()?;
            if let Some(session) = sessions.get_mut(&session_id)
                && session.active_turn_id == Some(turn_id)
            {
                session.active_turn_id = None;
                session.status = RuntimeSessionStatus::Idle;
                session.updated_at = now();
                persist_sessions(&self.path, &sessions)?;
            }
        }
        Ok(CancelRuntimeTurn {
            task_id: None,
            session_id,
            cancelled_queued,
        })
    }

    pub fn complete_claim_failure(&self, turn_id: uuid::Uuid, error: String) -> Result<()> {
        let mut turns = self.turns_lock()?;
        let turn = turns.get_mut(&turn_id).context("Runtime Turn not found")?;
        turn.metadata.status = RuntimeTurnStatus::Failed;
        turn.metadata.completed_at = Some(now());
        turn.metadata.error = Some(error.clone());
        let session_id = turn.metadata.session_id;
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.active_turn_id = None;
        session.status = RuntimeSessionStatus::Failed;
        session.updated_at = now();
        session.last_error = Some(error);
        persist_sessions(&self.path, &sessions)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<uuid::Uuid, RuntimeSession>>> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime Session store lock poisoned"))
    }

    fn turns_lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<uuid::Uuid, StoredRuntimeTurn>>> {
        self.turns
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime Turn store lock poisoned"))
    }
}

pub(super) async fn sessions_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let sessions = state
        .sessions
        .list()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(sessions).into_response())
}

pub(super) async fn create_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<CreateRuntimeSession>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let (session, created) = state.sessions.ensure(request).map_err(|error| {
        eprintln!("create Runtime Session: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
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
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(session),
    )
        .into_response())
}

pub(super) async fn session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .sessions
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(super) async fn create_turn_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<uuid::Uuid>,
    Json(request): Json<CreateRuntimeTurn>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let (turn, created) = state
        .sessions
        .enqueue_turn(session_id, request)
        .map_err(|error| {
            eprintln!("enqueue Runtime Turn: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    if created {
        state
            .events
            .append(
                "turn.queued",
                format!(
                    "session_id={} agent_id={} turn_id={}",
                    session_id,
                    state
                        .sessions
                        .get(session_id)
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                        .ok_or(StatusCode::NOT_FOUND)?
                        .root_agent_id,
                    turn.id
                ),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        state
            .tasks
            .schedule_session(session_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let turn = state
        .sessions
        .get_turn(turn.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok((
        if created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(turn),
    )
        .into_response())
}

pub(super) async fn turns_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    if state
        .sessions
        .get(session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let turns = state
        .sessions
        .list_turns(session_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(turns).into_response())
}

pub(super) async fn turn_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .sessions
        .get_turn(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(super) async fn stop_turn_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let cancellation = state
        .sessions
        .request_cancel(id)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if let Some(task_id) = cancellation.task_id {
        state
            .tasks
            .cancel(task_id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    } else if cancellation.cancelled_queued {
        state
            .events
            .append(
                "turn.cancelled",
                format!(
                    "session_id={} turn_id={} task_id=none",
                    cancellation.session_id, id
                ),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        state
            .tasks
            .schedule_session(cancellation.session_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let turn = state
        .sessions
        .get_turn(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(turn).into_response())
}

pub(super) async fn create_session_cli(
    home: &Path,
    workspace: Option<PathBuf>,
    profile: Option<String>,
    config: Option<PathBuf>,
    title: Option<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/sessions", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&CreateRuntimeSession {
            id: None,
            workspace: workspace
                .unwrap_or(std::env::current_dir()?)
                .canonicalize()?,
            profile,
            config,
            title,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Session creation: {}",
            response.text().await?
        );
    }
    print_session(&response.json().await?);
    Ok(())
}

pub(super) async fn list_sessions_cli(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let sessions: Vec<RuntimeSession> = authorized_get(&state, "/v1/sessions").await?;
    for session in sessions {
        print_session(&session);
    }
    Ok(())
}

pub(super) async fn show_session_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let session: RuntimeSession = authorized_get(&state, &format!("/v1/sessions/{id}")).await?;
    print_session(&session);
    Ok(())
}

pub(super) async fn submit_turn_cli(
    home: &Path,
    session_id: uuid::Uuid,
    request_id: Option<uuid::Uuid>,
    prompt: Vec<String>,
) -> Result<()> {
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("Runtime Turn prompt must not be empty");
    }
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/sessions/{session_id}/turns",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .json(&CreateRuntimeTurn {
            request_id: request_id.unwrap_or_else(uuid::Uuid::new_v4),
            prompt,
            attachments: Vec::new(),
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Turn submission: {}",
            response.text().await?
        );
    }
    print_turn(&response.json().await?);
    Ok(())
}

pub(super) async fn list_turns_cli(home: &Path, session_id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turns: Vec<RuntimeTurn> =
        authorized_get(&state, &format!("/v1/sessions/{session_id}/turns")).await?;
    for turn in turns {
        print_turn(&turn);
    }
    Ok(())
}

pub(super) async fn show_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turn: RuntimeTurn = authorized_get(&state, &format!("/v1/turns/{id}")).await?;
    print_turn(&turn);
    Ok(())
}

pub(super) async fn stop_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/turns/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Turn cancellation: {}",
            response.text().await?
        );
    }
    print_turn(&response.json().await?);
    Ok(())
}

fn print_session(session: &RuntimeSession) {
    println!(
        "{}\t{:?}\tagent={}\tactive_turn={}\t{}",
        session.id,
        session.status,
        session.root_agent_id,
        session
            .active_turn_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        session.workspace.display()
    );
}

fn print_turn(turn: &RuntimeTurn) {
    println!(
        "{}\t{:?}\tsession={}\trequest={}\tsequence={}\ttask={}\tattempts={}",
        turn.id,
        turn.status,
        turn.session_id,
        turn.request_id,
        turn.queue_sequence,
        turn.active_task_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        turn.attempts
    );
}

fn load_sessions(path: &Path) -> Result<HashMap<uuid::Uuid, RuntimeSession>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let sessions: Vec<RuntimeSession> = serde_json::from_slice(&std::fs::read(path)?)?;
    for session in &sessions {
        if session.schema != RUNTIME_SESSION_SCHEMA {
            bail!("unsupported Runtime Session schema {}", session.schema);
        }
    }
    Ok(sessions
        .into_iter()
        .map(|session| (session.id, session))
        .collect())
}

fn load_turns(path: &Path) -> Result<HashMap<uuid::Uuid, StoredRuntimeTurn>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let mut turns: Vec<StoredRuntimeTurn> = serde_json::from_slice(&std::fs::read(path)?)?;
    turns.sort_by_key(|turn| (turn.metadata.created_at, turn.metadata.id));
    for (index, turn) in turns.iter_mut().enumerate() {
        if turn.metadata.queue_sequence == 0 {
            turn.metadata.queue_sequence = index as u64 + 1;
        }
    }
    Ok(turns
        .into_iter()
        .map(|turn| (turn.metadata.id, turn))
        .collect())
}

fn persist_sessions(path: &Path, sessions: &HashMap<uuid::Uuid, RuntimeSession>) -> Result<()> {
    let mut sessions = sessions.values().cloned().collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.created_at);
    write_json_atomic(path, &sessions)
}

fn persist_turns(path: &Path, turns: &HashMap<uuid::Uuid, StoredRuntimeTurn>) -> Result<()> {
    let mut turns = turns.values().cloned().collect::<Vec<_>>();
    turns.sort_by_key(|turn| turn.metadata.queue_sequence);
    write_json_atomic(path, &turns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_matching_core_session_and_recovers_active_state() {
        let root =
            std::env::temp_dir().join(format!("willdeep-runtime-session-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = root.join("runtime-sessions.json");
        let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
        let session = store
            .create(CreateRuntimeSession {
                id: None,
                workspace: workspace.clone(),
                profile: Some("some-im".to_owned()),
                config: Some(root.join("config.toml")),
                title: Some("Persistent work".to_owned()),
            })
            .unwrap();
        let core = willdeep_core::SessionStore::new(&root)
            .load(session.id)
            .unwrap();
        assert_eq!(core.id, session.id);
        assert_eq!(core.workspace, workspace.canonicalize().unwrap());
        assert_eq!(session.status, RuntimeSessionStatus::Idle);
        assert_eq!(store.list().unwrap(), vec![session.clone()]);

        {
            let mut sessions = store.lock().unwrap();
            let stored = sessions.get_mut(&session.id).unwrap();
            stored.status = RuntimeSessionStatus::Running;
            stored.active_turn_id = Some(uuid::Uuid::new_v4());
            persist_sessions(&path, &sessions).unwrap();
        }
        drop(store);
        let reopened = RuntimeSessionStore::open(path, &root).unwrap();
        let recovered = reopened.get(session.id).unwrap().unwrap();
        assert_eq!(recovered.status, RuntimeSessionStatus::Interrupted);
        assert!(recovered.active_turn_id.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adopts_an_existing_core_session_idempotently() {
        let root =
            std::env::temp_dir().join(format!("willdeep-runtime-adopt-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let core_store = willdeep_core::SessionStore::new(&root);
        let mut core = willdeep_core::Session::new(
            workspace.canonicalize().unwrap(),
            Some("mock".to_owned()),
            "existing",
        );
        core_store.save(&mut core).unwrap();
        let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
        let request = || CreateRuntimeSession {
            id: Some(core.id),
            workspace: workspace.clone(),
            profile: Some("mock".to_owned()),
            config: Some(root.join("config.toml")),
            title: Some("ignored for adoption".to_owned()),
        };

        let (adopted, created) = store.ensure(request()).unwrap();
        assert!(created);
        assert_eq!(adopted.id, core.id);
        let (same, created) = store.ensure(request()).unwrap();
        assert!(!created);
        assert_eq!(same, adopted);
        assert_eq!(store.list().unwrap(), vec![adopted]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn turn_queue_is_idempotent_strictly_serial_and_persistent() {
        let root =
            std::env::temp_dir().join(format!("willdeep-runtime-turns-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = root.join("runtime-sessions.json");
        let store = RuntimeSessionStore::open(path.clone(), &root).unwrap();
        let session = store
            .create(CreateRuntimeSession {
                id: None,
                workspace,
                profile: None,
                config: None,
                title: None,
            })
            .unwrap();
        let first_request = uuid::Uuid::new_v4();
        let (first, created) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    request_id: first_request,
                    prompt: "first".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        assert!(created);
        let (duplicate, created) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    request_id: first_request,
                    prompt: "must not replace first".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        assert!(!created);
        assert_eq!(duplicate.id, first.id);
        let (second, _) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    request_id: uuid::Uuid::new_v4(),
                    prompt: "second".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();

        let claimed = store.claim_next(session.id).unwrap().unwrap();
        assert_eq!(claimed.metadata.id, first.id);
        assert_eq!(claimed.request.prompt, "first");
        assert!(store.claim_next(session.id).unwrap().is_none());
        let task_id = uuid::Uuid::new_v4();
        store.bind_task(first.id, task_id).unwrap();
        assert_eq!(
            store.get_turn(first.id).unwrap().unwrap().status,
            RuntimeTurnStatus::Running
        );
        assert_eq!(
            store
                .complete_task(task_id, RuntimeTaskStatus::Completed, None)
                .unwrap(),
            Some(session.id)
        );
        {
            let turns = store.turns_lock().unwrap();
            let stored = turns.get(&first.id).unwrap();
            assert!(stored.prompt.is_empty());
            assert!(stored.attachments.is_empty());
        }
        assert_eq!(
            store.claim_next(session.id).unwrap().unwrap().metadata.id,
            second.id
        );
        drop(store);

        let reopened = RuntimeSessionStore::open(path, &root).unwrap();
        assert_eq!(reopened.list_turns(session.id).unwrap().len(), 2);
        assert_eq!(
            reopened.get_turn(first.id).unwrap().unwrap().status,
            RuntimeTurnStatus::Completed
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_a_claimed_turn_releases_the_session_and_next_turn() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-runtime-turn-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
        let session = store
            .create(CreateRuntimeSession {
                id: None,
                workspace,
                profile: None,
                config: None,
                title: None,
            })
            .unwrap();
        let enqueue = |prompt: &str| CreateRuntimeTurn {
            request_id: uuid::Uuid::new_v4(),
            prompt: prompt.to_owned(),
            attachments: Vec::new(),
        };
        let (first, _) = store.enqueue_turn(session.id, enqueue("first")).unwrap();
        let (second, _) = store.enqueue_turn(session.id, enqueue("second")).unwrap();
        assert_eq!(
            store.claim_next(session.id).unwrap().unwrap().metadata.id,
            first.id
        );

        let cancellation = store.request_cancel(first.id).unwrap();
        assert!(cancellation.task_id.is_none());
        assert!(cancellation.cancelled_queued);
        assert_eq!(cancellation.session_id, session.id);
        assert_eq!(
            store.get(session.id).unwrap().unwrap().status,
            RuntimeSessionStatus::Idle
        );
        assert_eq!(
            store.claim_next(session.id).unwrap().unwrap().metadata.id,
            second.id
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
