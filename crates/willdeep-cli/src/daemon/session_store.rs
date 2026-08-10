use super::*;

const RUNTIME_SESSION_SCHEMA: u32 = 1;
const SESSION_EXPORT_SCHEMA: u32 = 1;
const MAX_SESSION_TITLE_CHARS: usize = 200;
const MAX_SEARCH_QUERY_CHARS: usize = 200;
const MAX_SEARCH_RESULTS: usize = 100;
const MAX_SEARCH_SNIPPET_CHARS: usize = 160;

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
    #[serde(default)]
    pub model: Option<String>,
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
    pub model: Option<String>,
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
    #[serde(default)]
    pub message_start: Option<usize>,
    #[serde(default)]
    pub message_end: Option<usize>,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RenameRuntimeSession {
    pub title: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ForkRuntimeSession {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub through_turn_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub provider_profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DeleteRuntimeSession {
    pub confirmation: uuid::Uuid,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RuntimeSessionExport {
    schema: u32,
    app_version: String,
    exported_at: u64,
    session: RuntimeSession,
    core: ExportedCoreSession,
    turns: Vec<RuntimeTurn>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ExportedCoreSession {
    id: uuid::Uuid,
    title: String,
    workspace: PathBuf,
    profile: Option<String>,
    created_at: u64,
    updated_at: u64,
    messages: Vec<willdeep_core::Message>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RuntimeSessionSearchResult {
    pub(super) id: uuid::Uuid,
    pub(super) title: String,
    pub(super) workspace: PathBuf,
    pub(super) status: RuntimeSessionStatus,
    pub(super) profile: Option<String>,
    pub(super) model: Option<String>,
    pub(super) updated_at: u64,
    pub(super) message_count: usize,
    pub(super) snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SessionSearchQuery {
    #[serde(default)]
    pub(super) q: Option<String>,
    #[serde(default)]
    pub(super) workspace: Option<PathBuf>,
    #[serde(default)]
    pub(super) status: Option<RuntimeSessionStatus>,
    #[serde(default)]
    pub(super) profile: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) updated_after: Option<u64>,
    #[serde(default)]
    pub(super) updated_before: Option<u64>,
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

    pub fn ensure(&self, mut request: CreateRuntimeSession) -> Result<(RuntimeSession, bool)> {
        request.profile = normalized_optional("Provider profile", request.profile)?;
        request.model = normalized_optional("Model", request.model)?;
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
            core.config = request.config.clone();
            self.core.save(&mut core)?;
            core
        };
        let config = core.config.clone().or(request.config);
        let timestamp = now();
        let session = RuntimeSession {
            schema: RUNTIME_SESSION_SCHEMA,
            id: core.id,
            root_agent_id: uuid::Uuid::new_v4(),
            workspace,
            profile: request.profile,
            model: request.model,
            config,
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

    pub fn rename(&self, id: uuid::Uuid, title: String) -> Result<RuntimeSession> {
        self.ensure_manageable(id)?;
        let title = normalized_title(title)?;
        let mut core = self
            .core
            .load(id)
            .with_context(|| format!("load Core Session {id}"))?;
        core.title = title;
        self.core.save(&mut core)?;
        let mut sessions = self.lock()?;
        let session = sessions.get_mut(&id).context("Runtime Session not found")?;
        session.updated_at = now();
        let result = session.clone();
        persist_sessions(&self.path, &sessions)?;
        Ok(result)
    }

    pub fn fork_through(
        &self,
        id: uuid::Uuid,
        title: Option<String>,
        through_turn_id: Option<uuid::Uuid>,
        provider_profile: Option<String>,
        model: Option<String>,
    ) -> Result<RuntimeSession> {
        self.ensure_manageable(id)?;
        let provider_profile = normalized_optional("Provider profile", provider_profile)?;
        let model = normalized_optional("Model", model)?;
        let source = self.get(id)?.context("Runtime Session not found")?;
        let target_profile = provider_profile.or_else(|| source.profile.clone());
        let mut core = self
            .core
            .load(id)
            .with_context(|| format!("load Core Session {id}"))?;
        if let Some(turn_id) = through_turn_id {
            let turns = self.turns_lock()?;
            let turn = turns.get(&turn_id).context("Runtime Turn not found")?;
            if turn.metadata.session_id != id {
                bail!("Runtime Turn does not belong to the source Session");
            }
            if turn.metadata.status != RuntimeTurnStatus::Completed {
                bail!("only a completed Runtime Turn can be used as a Fork boundary");
            }
            let end = turn.metadata.message_end.context(
                "Runtime Turn predates durable message boundaries and cannot be forked exactly",
            )?;
            if end > core.messages.len() {
                bail!("Runtime Turn message boundary exceeds the Core Session snapshot");
            }
            core.messages.truncate(end);
        }
        let timestamp = now();
        core.id = uuid::Uuid::new_v4();
        core.title = match title {
            Some(title) => normalized_title(title)?,
            None => default_fork_title(&core.title),
        };
        core.created_at = timestamp;
        core.updated_at = timestamp;
        core.attention_read.clear();
        core.runtime_event_cursor = 0;
        core.runtime_managed = true;
        core.swift_source = None;
        core.profile = target_profile.clone();
        self.core.save(&mut core)?;
        let fork = RuntimeSession {
            schema: RUNTIME_SESSION_SCHEMA,
            id: core.id,
            root_agent_id: uuid::Uuid::new_v4(),
            workspace: source.workspace,
            profile: target_profile,
            model: model.or(source.model),
            config: source.config,
            status: RuntimeSessionStatus::Idle,
            active_turn_id: None,
            created_at: timestamp,
            updated_at: timestamp,
            last_error: None,
        };
        let mut sessions = self.lock()?;
        sessions.insert(fork.id, fork.clone());
        if let Err(error) = persist_sessions(&self.path, &sessions) {
            sessions.remove(&fork.id);
            let _ = self.core.delete(fork.id);
            return Err(error);
        }
        Ok(fork)
    }

    pub fn archive(&self, id: uuid::Uuid) -> Result<RuntimeSession> {
        self.ensure_manageable(id)?;
        self.set_archived(id, true)
    }

    pub fn unarchive(&self, id: uuid::Uuid) -> Result<RuntimeSession> {
        self.set_archived(id, false)
    }

    pub fn delete(&self, id: uuid::Uuid, confirmation: uuid::Uuid) -> Result<()> {
        if confirmation != id {
            bail!("Session deletion confirmation does not match target");
        }
        self.ensure_manageable(id)?;
        let mut sessions = self.lock()?;
        let removed_session = sessions.remove(&id).context("Runtime Session not found")?;
        let mut turns = self.turns_lock()?;
        let removed_turns = turns
            .extract_if(|_, turn| turn.metadata.session_id == id)
            .collect::<HashMap<_, _>>();
        if let Err(error) = persist_sessions(&self.path, &sessions)
            .and_then(|_| persist_turns(&self.turns_path, &turns))
        {
            sessions.insert(id, removed_session);
            turns.extend(removed_turns);
            let _ = persist_sessions(&self.path, &sessions);
            let _ = persist_turns(&self.turns_path, &turns);
            return Err(error);
        }
        if let Err(error) = self.core.delete(id) {
            sessions.insert(id, removed_session);
            turns.extend(removed_turns);
            persist_sessions(&self.path, &sessions)?;
            persist_turns(&self.turns_path, &turns)?;
            return Err(error.into());
        }
        Ok(())
    }

    pub fn export(&self, id: uuid::Uuid) -> Result<RuntimeSessionExport> {
        let session = self.get(id)?.context("Runtime Session not found")?;
        let core = self
            .core
            .load(id)
            .with_context(|| format!("load Core Session {id}"))?;
        let turns = self.list_turns(id)?;
        Ok(RuntimeSessionExport {
            schema: SESSION_EXPORT_SCHEMA,
            app_version: willdeep_core::VERSION.to_owned(),
            exported_at: now(),
            session,
            core: ExportedCoreSession {
                id: core.id,
                title: core.title,
                workspace: core.workspace,
                profile: core.profile,
                created_at: core.created_at,
                updated_at: core.updated_at,
                messages: core.messages,
            },
            turns,
        })
    }

    pub fn search(&self, filters: SessionSearchQuery) -> Result<Vec<RuntimeSessionSearchResult>> {
        let query = filters
            .q
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        if query
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_SEARCH_QUERY_CHARS)
        {
            bail!("Session search query is too long");
        }
        if filters
            .updated_after
            .zip(filters.updated_before)
            .is_some_and(|(updated_after, updated_before)| updated_after > updated_before)
        {
            bail!("updated_after must not exceed updated_before");
        }
        let workspace = filters
            .workspace
            .map(|value| value.canonicalize())
            .transpose()
            .context("invalid Session search workspace")?;
        if query.is_none()
            && workspace.is_none()
            && filters.status.is_none()
            && filters.profile.is_none()
            && filters.model.is_none()
            && filters.updated_after.is_none()
            && filters.updated_before.is_none()
        {
            bail!("Session search requires text or at least one filter");
        }
        let mut results = Vec::new();
        for session in self.list()? {
            if workspace
                .as_ref()
                .is_some_and(|value| *value != session.workspace)
                || filters.status.is_some_and(|value| value != session.status)
                || filters.profile.as_ref().is_some_and(|value| {
                    !session
                        .profile
                        .as_deref()
                        .is_some_and(|profile| profile.eq_ignore_ascii_case(value))
                })
                || filters.model.as_ref().is_some_and(|value| {
                    !session
                        .model
                        .as_deref()
                        .is_some_and(|model| model.eq_ignore_ascii_case(value))
                })
                || filters
                    .updated_after
                    .is_some_and(|value| session.updated_at < value)
                || filters
                    .updated_before
                    .is_some_and(|value| session.updated_at > value)
            {
                continue;
            }
            let Ok(core) = self.core.load(session.id) else {
                continue;
            };
            let title_matches = query
                .as_ref()
                .is_none_or(|query| core.title.to_lowercase().contains(query));
            let matching_message = query.as_ref().and_then(|query| {
                core.messages
                    .iter()
                    .find(|message| message.content.to_lowercase().contains(query))
            });
            if !title_matches && matching_message.is_none() {
                continue;
            }
            results.push(RuntimeSessionSearchResult {
                id: session.id,
                title: core.title,
                workspace: session.workspace,
                status: session.status,
                profile: session.profile,
                model: session.model,
                updated_at: session.updated_at.max(core.updated_at),
                message_count: core.messages.len(),
                snippet: matching_message.map(|message| bounded_snippet(&message.content)),
            });
            if results.len() >= MAX_SEARCH_RESULTS {
                break;
            }
        }
        results.sort_by_key(|result| std::cmp::Reverse(result.updated_at));
        Ok(results)
    }

    fn ensure_manageable(&self, id: uuid::Uuid) -> Result<()> {
        let sessions = self.lock()?;
        let session = sessions.get(&id).context("Runtime Session not found")?;
        if session.active_turn_id.is_some()
            || matches!(
                session.status,
                RuntimeSessionStatus::Queued
                    | RuntimeSessionStatus::Running
                    | RuntimeSessionStatus::WaitingApproval
                    | RuntimeSessionStatus::WaitingAnswer
            )
        {
            bail!("Runtime Session is active");
        }
        let turns = self.turns_lock()?;
        if turns.values().any(|turn| {
            turn.metadata.session_id == id && turn.metadata.status == RuntimeTurnStatus::Queued
        }) {
            bail!("Runtime Session has queued Turns");
        }
        Ok(())
    }

    fn set_archived(&self, id: uuid::Uuid, archived: bool) -> Result<RuntimeSession> {
        let mut sessions = self.lock()?;
        let session = sessions.get_mut(&id).context("Runtime Session not found")?;
        if archived && session.status == RuntimeSessionStatus::Archived {
            return Ok(session.clone());
        }
        if !archived && session.status != RuntimeSessionStatus::Archived {
            bail!("Runtime Session is not archived");
        }
        session.status = if archived {
            RuntimeSessionStatus::Archived
        } else {
            RuntimeSessionStatus::Idle
        };
        session.updated_at = now();
        session.last_error = None;
        let result = session.clone();
        persist_sessions(&self.path, &sessions)?;
        Ok(result)
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
            message_start: None,
            message_end: None,
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
        turn.metadata.message_start = Some(
            self.core
                .load(session_id)
                .with_context(|| format!("load Core Session {session_id}"))?
                .messages
                .len(),
        );
        turn.metadata.message_end = None;
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
                workspace_access: None,
                workspace_skills: None,
                workspace_mcp_servers: None,
                profile: session.profile.clone(),
                model: session.model.clone(),
                config: session.config.clone(),
                session_id: Some(session.id),
                turn_id: Some(turn.metadata.id),
            },
        };
        persist_turns(&self.turns_path, &turns)?;
        persist_sessions(&self.path, &sessions)?;
        Ok(Some(claimed))
    }

    pub fn bind_task(&self, turn_id: uuid::Uuid, task_id: uuid::Uuid) -> Result<bool> {
        let mut sessions = self.lock()?;
        let Some(session) = sessions
            .values_mut()
            .find(|session| session.active_turn_id == Some(turn_id))
        else {
            return Ok(false);
        };
        let mut turns = self.turns_lock()?;
        let turn = turns.get_mut(&turn_id).context("Runtime Turn not found")?;
        if turn.metadata.status != RuntimeTurnStatus::Queued {
            return Ok(false);
        }
        let session_id = turn.metadata.session_id;
        if session.id != session_id {
            return Ok(false);
        }
        turn.metadata.status = RuntimeTurnStatus::Running;
        turn.metadata.active_task_id = Some(task_id);
        turn.metadata.started_at = Some(now());
        turn.metadata.completed_at = None;
        turn.metadata.error = None;
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        session.status = RuntimeSessionStatus::Running;
        session.updated_at = now();
        persist_sessions(&self.path, &sessions)?;
        Ok(true)
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
        let session_id = turn.metadata.session_id;
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
            turn.metadata.message_end = Some(
                self.core
                    .load(session_id)
                    .with_context(|| format!("load completed Core Session {session_id}"))?
                    .messages
                    .len(),
            );
        }
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

fn normalized_title(title: String) -> Result<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        bail!("Session title must not be empty");
    }
    if title.chars().count() > MAX_SESSION_TITLE_CHARS {
        bail!("Session title must not exceed {MAX_SESSION_TITLE_CHARS} characters");
    }
    Ok(title)
}

fn normalized_optional(label: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    if value.chars().count() > MAX_SESSION_TITLE_CHARS {
        bail!("{label} must not exceed {MAX_SESSION_TITLE_CHARS} characters");
    }
    Ok(Some(value))
}

fn default_fork_title(source: &str) -> String {
    const SUFFIX: &str = " (fork)";
    let prefix_limit = MAX_SESSION_TITLE_CHARS.saturating_sub(SUFFIX.chars().count());
    let mut title = source.chars().take(prefix_limit).collect::<String>();
    title.push_str(SUFFIX);
    title
}

fn bounded_snippet(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut snippet = compact
        .chars()
        .take(MAX_SEARCH_SNIPPET_CHARS)
        .collect::<String>();
    if compact.chars().count() > MAX_SEARCH_SNIPPET_CHARS {
        snippet.push('…');
    }
    snippet
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
    Json(mut request): Json<CreateRuntimeSession>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = state
        .workspaces
        .ensure_registered(&request.workspace)
        .map_err(|error| {
            eprintln!("register Runtime Session Workspace: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    request.workspace = workspace.root;
    if request.profile.is_none() {
        request.profile = workspace.provider_profile;
    }
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

pub(super) async fn search_sessions_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<SessionSearchQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let results = state.sessions.search(query).map_err(|error| {
        eprintln!("search Runtime Sessions: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    Ok(Json(results).into_response())
}

pub(super) async fn rename_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<RenameRuntimeSession>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let session = state.sessions.rename(id, request.title).map_err(|error| {
        eprintln!("rename Runtime Session: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    state
        .events
        .append("session.renamed", format!("session_id={id}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(session).into_response())
}

pub(super) async fn fork_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<ForkRuntimeSession>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let session = state
        .sessions
        .fork_through(
            id,
            request.title,
            request.through_turn_id,
            request.provider_profile,
            request.model,
        )
        .map_err(|error| {
            eprintln!("fork Runtime Session: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    state
        .events
        .append(
            "session.forked",
            format!(
                "source_session_id={id} through_turn_id={} session_id={} agent_id={}",
                request
                    .through_turn_id
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                session.id,
                session.root_agent_id
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(session)).into_response())
}

pub(super) async fn archive_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let session = state.sessions.archive(id).map_err(|error| {
        eprintln!("archive Runtime Session: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    state
        .events
        .append("session.archived", format!("session_id={id}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(session).into_response())
}

pub(super) async fn unarchive_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let session = state.sessions.unarchive(id).map_err(|error| {
        eprintln!("unarchive Runtime Session: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    state
        .events
        .append("session.unarchived", format!("session_id={id}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(session).into_response())
}

pub(super) async fn export_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let export = state.sessions.export(id).map_err(|error| {
        eprintln!("export Runtime Session: {error:#}");
        StatusCode::NOT_FOUND
    })?;
    Ok(Json(export).into_response())
}

pub(super) async fn delete_session_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<DeleteRuntimeSession>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .sessions
        .delete(id, request.confirmation)
        .map_err(|error| {
            eprintln!("delete Runtime Session: {error:#}");
            StatusCode::BAD_REQUEST
        })?;
    state
        .events
        .append("session.deleted", format!("session_id={id}"))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT.into_response())
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
    model: Option<String>,
    config: Option<PathBuf>,
    title: Option<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let workspace = workspace_store::resolve_cli_root(home, workspace).await?;
    if config.is_none() {
        let session = cli_api_data(
            runtime_client(&state)?
                .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                    "session.create",
                    &willdeep_runtime_protocol::CreateSessionParams {
                        id: None,
                        workspace: workspace.display().to_string(),
                        profile,
                        model,
                        title,
                    },
                    Some(uuid::Uuid::new_v4()),
                )
                .await?,
        )?;
        print_public_session(&session);
        return Ok(());
    }
    let response = client()
        .post(format!("http://{}/v1/sessions", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&CreateRuntimeSession {
            id: None,
            workspace,
            profile,
            model,
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
    let sessions = cli_api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeSession>>(
                "session.list",
                &serde_json::json!({}),
                None,
            )
            .await?,
    )?;
    for session in sessions {
        print_public_session(&session);
    }
    Ok(())
}

pub(super) async fn show_session_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let session = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.get",
                &willdeep_runtime_protocol::IdParams { id },
                None,
            )
            .await?,
    )?;
    print_public_session(&session);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn search_sessions_cli(
    home: &Path,
    query: Vec<String>,
    workspace: Option<PathBuf>,
    status: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    updated_after: Option<u64>,
    updated_before: Option<u64>,
) -> Result<()> {
    let query = query.join(" ");
    let query = (!query.trim().is_empty()).then_some(query);
    let workspace = workspace
        .map(|workspace| workspace.canonicalize())
        .transpose()?
        .map(|workspace| workspace.display().to_string());
    let status = status
        .map(|status| {
            serde_json::from_value::<willdeep_runtime_protocol::SessionStatus>(
                serde_json::Value::String(status),
            )
            .context("invalid Session status filter")
        })
        .transpose()?;
    if query.is_none()
        && workspace.is_none()
        && status.is_none()
        && profile.is_none()
        && model.is_none()
        && updated_after.is_none()
        && updated_before.is_none()
    {
        bail!("Session search requires text or at least one filter");
    }
    let state = ensure_running(home).await?;
    let results = cli_api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::SessionSearchResult>>(
                "session.search",
                &willdeep_runtime_protocol::SearchSessionsParams {
                    query,
                    workspace,
                    status,
                    profile,
                    model,
                    updated_after,
                    updated_before,
                },
                None,
            )
            .await?,
    )?;
    for result in results {
        println!(
            "{}\t{:?}\tprofile={}\tmodel={}\tmessages={}\t{}\t{}\t{}",
            result.id,
            result.status,
            result.profile.as_deref().unwrap_or("default"),
            result.model.as_deref().unwrap_or("default"),
            result.message_count,
            result.title,
            result.workspace.as_deref().unwrap_or("private"),
            result.snippet.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

pub(super) async fn rename_session_cli(
    home: &Path,
    id: uuid::Uuid,
    title: Vec<String>,
) -> Result<()> {
    let title = title.join(" ");
    let state = ensure_running(home).await?;
    let session = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.rename",
                &willdeep_runtime_protocol::RenameSessionParams { id, title },
                Some(uuid::Uuid::new_v4()),
            )
            .await?,
    )?;
    print_public_session(&session);
    Ok(())
}

pub(super) async fn fork_session_cli(
    home: &Path,
    id: uuid::Uuid,
    title: Option<String>,
    through_turn_id: Option<uuid::Uuid>,
    provider_profile: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let session = cli_api_data(
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
                Some(uuid::Uuid::new_v4()),
            )
            .await?,
    )?;
    print_public_session(&session);
    Ok(())
}

pub(super) async fn archive_session_cli(
    home: &Path,
    id: uuid::Uuid,
    unarchive: bool,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let session = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeSession>(
                "session.archive",
                &willdeep_runtime_protocol::ArchiveSessionParams {
                    id,
                    archived: !unarchive,
                },
                Some(uuid::Uuid::new_v4()),
            )
            .await?,
    )?;
    print_public_session(&session);
    Ok(())
}

pub(super) async fn export_session_cli(
    home: &Path,
    id: uuid::Uuid,
    output: Option<PathBuf>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let export = cli_api_data(
        runtime_client(&state)?
            .call::<_, serde_json::Value>(
                "session.export",
                &willdeep_runtime_protocol::IdParams { id },
                None,
            )
            .await?,
    )?;
    let data = serde_json::to_vec_pretty(&export)?;
    if let Some(output) = output {
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, data)?;
        println!("{}", output.display());
    } else {
        println!("{}", String::from_utf8(data)?);
    }
    Ok(())
}

pub(super) async fn delete_session_cli(home: &Path, id: uuid::Uuid, yes: bool) -> Result<()> {
    if !yes {
        bail!("Session deletion is permanent; repeat with --yes for Session {id}");
    }
    let state = ensure_running(home).await?;
    cli_api_data(
        runtime_client(&state)?
            .call::<_, serde_json::Value>(
                "session.delete",
                &willdeep_runtime_protocol::DeleteSessionParams {
                    id,
                    confirmation: id,
                },
                Some(uuid::Uuid::new_v4()),
            )
            .await?,
    )?;
    println!("deleted\t{id}");
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
    let turn_request_id = request_id.unwrap_or_else(uuid::Uuid::new_v4);
    let turn = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeTurn>(
                "turn.submit",
                &willdeep_runtime_protocol::SubmitTurnParams {
                    session_id,
                    turn_request_id,
                    prompt,
                    attachments: Vec::new(),
                },
                Some(turn_request_id),
            )
            .await?,
    )?;
    print_public_turn(&turn);
    Ok(())
}

pub(super) async fn list_turns_cli(home: &Path, session_id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turns = cli_api_data(
        runtime_client(&state)?
            .call::<_, Vec<willdeep_runtime_protocol::RuntimeTurn>>(
                "turn.list",
                &willdeep_runtime_protocol::ListTurnsParams { session_id },
                None,
            )
            .await?,
    )?;
    for turn in turns {
        print_public_turn(&turn);
    }
    Ok(())
}

pub(super) async fn show_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turn = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeTurn>(
                "turn.get",
                &willdeep_runtime_protocol::IdParams { id },
                None,
            )
            .await?,
    )?;
    print_public_turn(&turn);
    Ok(())
}

pub(super) async fn stop_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turn = cli_api_data(
        runtime_client(&state)?
            .call::<_, willdeep_runtime_protocol::RuntimeTurn>(
                "turn.stop",
                &willdeep_runtime_protocol::IdParams { id },
                Some(uuid::Uuid::new_v4()),
            )
            .await?,
    )?;
    print_public_turn(&turn);
    Ok(())
}

fn cli_api_data<T>(response: willdeep_runtime_protocol::ApiResponse<T>) -> Result<T> {
    match response {
        willdeep_runtime_protocol::ApiResponse::Ok { data, .. } => Ok(data),
        willdeep_runtime_protocol::ApiResponse::Error { error, .. } => {
            bail!("Runtime API error: {}", error.message)
        }
    }
}

fn print_public_session(session: &willdeep_runtime_protocol::RuntimeSession) {
    println!(
        "{}\t{:?}\tprofile={}\tmodel={}\tagent={}\tactive_turn={}\t{}",
        session.id,
        session.status,
        session.profile.as_deref().unwrap_or("default"),
        session.model.as_deref().unwrap_or("default"),
        session.root_agent_id,
        session
            .active_turn_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        session.workspace.as_deref().unwrap_or("private")
    );
}

fn print_public_turn(turn: &willdeep_runtime_protocol::RuntimeTurn) {
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

fn print_session(session: &RuntimeSession) {
    println!(
        "{}\t{:?}\tprofile={}\tmodel={}\tagent={}\tactive_turn={}\t{}",
        session.id,
        session.status,
        session.profile.as_deref().unwrap_or("default"),
        session.model.as_deref().unwrap_or("default"),
        session.root_agent_id,
        session
            .active_turn_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        session.workspace.display()
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
    fn manages_session_snapshot_lifecycle_without_exporting_private_queue_data() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-runtime-session-management-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
        let session = store
            .create(CreateRuntimeSession {
                id: None,
                workspace,
                profile: Some("mock".to_owned()),
                model: Some("mock-model".to_owned()),
                config: Some(root.join("config.toml")),
                title: Some("Original title".to_owned()),
            })
            .unwrap();
        let core_store = willdeep_core::SessionStore::new(&root);
        let mut core = core_store.load(session.id).unwrap();
        core.messages
            .push(willdeep_core::Message::user("needle in durable history"));
        core.attention_read.insert("job-private".to_owned());
        core.runtime_event_cursor = 42;
        core_store.save(&mut core).unwrap();

        store
            .rename(session.id, "  Renamed   session  ".to_owned())
            .unwrap();
        assert_eq!(
            core_store.load(session.id).unwrap().title,
            "Renamed session"
        );
        let title_results = store
            .search(SessionSearchQuery {
                q: Some("renamed".to_owned()),
                workspace: None,
                status: None,
                profile: None,
                model: None,
                updated_after: None,
                updated_before: None,
            })
            .unwrap();
        assert_eq!(title_results.len(), 1);
        assert_eq!(title_results[0].id, session.id);
        let message_results = store
            .search(SessionSearchQuery {
                q: Some("needle".to_owned()),
                workspace: None,
                status: None,
                profile: None,
                model: None,
                updated_after: None,
                updated_before: None,
            })
            .unwrap();
        assert_eq!(message_results.len(), 1);
        assert!(
            message_results[0]
                .snippet
                .as_deref()
                .unwrap()
                .contains("needle")
        );
        let filtered_results = store
            .search(SessionSearchQuery {
                q: None,
                workspace: Some(root.join("workspace")),
                status: Some(RuntimeSessionStatus::Idle),
                profile: Some("MOCK".to_owned()),
                model: Some("MOCK-MODEL".to_owned()),
                updated_after: Some(0),
                updated_before: Some(u64::MAX),
            })
            .unwrap();
        assert_eq!(filtered_results.len(), 1);
        assert_eq!(filtered_results[0].id, session.id);
        assert_eq!(filtered_results[0].profile.as_deref(), Some("mock"));
        assert_eq!(filtered_results[0].model.as_deref(), Some("mock-model"));
        assert!(
            store
                .search(SessionSearchQuery {
                    q: None,
                    workspace: None,
                    status: None,
                    profile: Some("other".to_owned()),
                    model: None,
                    updated_after: None,
                    updated_before: None,
                })
                .unwrap()
                .is_empty()
        );

        let (queued, _) = store
            .enqueue_turn(
                session.id,
                CreateRuntimeTurn {
                    request_id: uuid::Uuid::new_v4(),
                    prompt: "private queued prompt".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        assert!(store.archive(session.id).is_err());
        assert!(
            store
                .fork_through(session.id, None, None, None, None)
                .is_err()
        );
        assert!(store.delete(session.id, session.id).is_err());
        store.request_cancel(queued.id).unwrap();

        assert_eq!(
            store.archive(session.id).unwrap().status,
            RuntimeSessionStatus::Archived
        );
        assert!(
            store
                .enqueue_turn(
                    session.id,
                    CreateRuntimeTurn {
                        request_id: uuid::Uuid::new_v4(),
                        prompt: "blocked while archived".to_owned(),
                        attachments: Vec::new(),
                    },
                )
                .is_err()
        );
        assert_eq!(
            store.unarchive(session.id).unwrap().status,
            RuntimeSessionStatus::Idle
        );

        let fork = store
            .fork_through(
                session.id,
                Some("Forked snapshot".to_owned()),
                None,
                None,
                None,
            )
            .unwrap();
        assert_ne!(fork.id, session.id);
        assert_ne!(fork.root_agent_id, session.root_agent_id);
        assert!(store.list_turns(fork.id).unwrap().is_empty());
        let fork_core = core_store.load(fork.id).unwrap();
        assert_eq!(fork_core.title, "Forked snapshot");
        assert_eq!(fork_core.messages.len(), core.messages.len());
        assert_eq!(fork_core.messages[0].content, core.messages[0].content);
        assert!(fork_core.attention_read.is_empty());
        assert_eq!(fork_core.runtime_event_cursor, 0);

        let export = store.export(session.id).unwrap();
        let export_json = serde_json::to_string(&export).unwrap();
        assert!(export_json.contains("needle in durable history"));
        assert!(!export_json.contains("private queued prompt"));
        assert!(!export_json.contains("job-private"));
        assert!(store.delete(fork.id, session.id).is_err());
        store.delete(fork.id, fork.id).unwrap();
        assert!(store.get(fork.id).unwrap().is_none());
        assert!(core_store.load(fork.id).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

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
                model: Some("qwen3".to_owned()),
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
        let stored_config = root.join("private-config.toml");
        core.config = Some(stored_config.clone());
        core_store.save(&mut core).unwrap();
        let store = RuntimeSessionStore::open(root.join("runtime-sessions.json"), &root).unwrap();
        let request = || CreateRuntimeSession {
            id: Some(core.id),
            workspace: workspace.clone(),
            profile: Some("mock".to_owned()),
            model: None,
            config: Some(root.join("client-supplied-config.toml")),
            title: Some("ignored for adoption".to_owned()),
        };

        let (adopted, created) = store.ensure(request()).unwrap();
        assert!(created);
        assert_eq!(adopted.id, core.id);
        assert_eq!(adopted.config.as_ref(), Some(&stored_config));
        let (same, created) = store.ensure(request()).unwrap();
        assert!(!created);
        assert_eq!(same, adopted);
        assert_eq!(store.list().unwrap(), vec![adopted]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn records_message_boundaries_and_forks_through_a_completed_turn() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-runtime-turn-fork-{}",
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
                model: Some("source-model".to_owned()),
                config: None,
                title: Some("Turn boundary".to_owned()),
            })
            .unwrap();
        let core_store = willdeep_core::SessionStore::new(&root);

        let complete_turn = |prompt: &str, answer: &str| {
            let (turn, _) = store
                .enqueue_turn(
                    session.id,
                    CreateRuntimeTurn {
                        request_id: uuid::Uuid::new_v4(),
                        prompt: prompt.to_owned(),
                        attachments: Vec::new(),
                    },
                )
                .unwrap();
            let claimed = store.claim_next(session.id).unwrap().unwrap();
            assert_eq!(claimed.request.model.as_deref(), Some("source-model"));
            let mut core = core_store.load(session.id).unwrap();
            core.messages.push(willdeep_core::Message::user(prompt));
            core.messages
                .push(willdeep_core::Message::assistant(answer, Vec::new()));
            core_store.save(&mut core).unwrap();
            let task_id = uuid::Uuid::new_v4();
            assert!(store.bind_task(turn.id, task_id).unwrap());
            store
                .complete_task(task_id, RuntimeTaskStatus::Completed, None)
                .unwrap();
            turn.id
        };

        let first = complete_turn("first", "first answer");
        let second = complete_turn("second", "second answer");
        let first_turn = store.get_turn(first).unwrap().unwrap();
        let second_turn = store.get_turn(second).unwrap().unwrap();
        assert_eq!(
            (first_turn.message_start, first_turn.message_end),
            (Some(0), Some(2))
        );
        assert_eq!(
            (second_turn.message_start, second_turn.message_end),
            (Some(2), Some(4))
        );

        let fork = store
            .fork_through(
                session.id,
                Some("Through first".to_owned()),
                Some(first),
                Some("research".to_owned()),
                Some("deep-model".to_owned()),
            )
            .unwrap();
        assert_eq!(fork.profile.as_deref(), Some("research"));
        assert_eq!(fork.model.as_deref(), Some("deep-model"));
        let fork_core = core_store.load(fork.id).unwrap();
        assert_eq!(fork_core.messages.len(), 2);
        assert_eq!(fork_core.messages[0].content, "first");
        assert_eq!(fork_core.messages[1].content, "first answer");
        assert!(store.list_turns(fork.id).unwrap().is_empty());
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
                model: None,
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
        assert!(store.bind_task(first.id, task_id).unwrap());
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
                model: None,
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
        assert!(!store.bind_task(first.id, uuid::Uuid::new_v4()).unwrap());
        assert_eq!(
            store.claim_next(session.id).unwrap().unwrap().metadata.id,
            second.id
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
