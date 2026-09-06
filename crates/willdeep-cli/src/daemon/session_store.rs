use super::*;

const RUNTIME_SESSION_SCHEMA: u32 = 2;
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
    Partial,
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
    #[serde(default)]
    pub message_generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredRuntimeTurn {
    metadata: RuntimeTurn,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
    #[serde(default)]
    replay_existing_user_message: bool,
    /// 谁提交的这一轮。排队的轮次可能等上很久才被认领，发起端要一路跟到
    /// 任务上，否则认领那一刻就丢了。旧记录读回来是 `None`。
    #[serde(default)]
    origin_client: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CreateRuntimeTurn {
    pub request_id: uuid::Uuid,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<willdeep_core::MessageAttachment>,
    /// 谁提交的这一轮，见 `RuntimeTask::origin_client`。
    #[serde(default)]
    pub origin_client: Option<String>,
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
    compression_generation: u64,
    compression_checkpoint: Option<willdeep_core::session::CompressionCheckpoint>,
}

/// 一条搜索结果是从哪个仓里捞出来的。
///
/// 历史面板此前只列 Runtime 登记过的会话，于是同一个工作区里由桌面版 Xedit
/// 写下的会话、以及 TUI 建了却从没提交过 Runtime 轮次的会话，全都不在列表里
/// ——但它们的文件就在旁边，`--resume` 也一直能打开。列表少一半等于列表说谎。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionOrigin {
    /// Runtime 登记过：有状态、有轮次队列、能派工。
    Runtime,
    /// 只有 Core 会话文件（rs 自己写的），继续聊时才会被 Runtime 领养。
    Local,
    /// 桌面版 Xedit 写的会话，rs 这侧是只读桥接。
    Xedit,
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
    pub(super) origin: SessionOrigin,
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
    #[cfg(test)]
    pub fn open(path: PathBuf, home: &Path) -> Result<Self> {
        Self::open_inner(path, home, &std::collections::HashSet::new())
    }

    pub(super) fn open_guarded(path: PathBuf, home: &Path, tools_path: &Path) -> Result<Self> {
        let tool_task_ids = load_tool_task_ids(tools_path)?;
        Self::open_inner(path, home, &tool_task_ids)
    }

    fn open_inner(
        path: PathBuf,
        home: &Path,
        tool_task_ids: &std::collections::HashSet<uuid::Uuid>,
    ) -> Result<Self> {
        let (mut sessions, migrated) = load_sessions(&path)?;
        if migrated {
            backup_sessions_before_migration(&path, 1)?;
            persist_sessions(&path, &sessions)?;
        }
        let turns_path = path.with_file_name("turns.json");
        let mut turns = load_turns(&turns_path)?;
        let core = willdeep_core::SessionStore::new(home);
        let mut turns_changed = false;
        for turn in turns.values_mut() {
            if matches!(
                turn.metadata.status,
                RuntimeTurnStatus::Running
                    | RuntimeTurnStatus::WaitingApproval
                    | RuntimeTurnStatus::WaitingAnswer
            ) {
                let has_tool_activity = turn
                    .metadata
                    .active_task_id
                    .is_some_and(|task_id| tool_task_ids.contains(&task_id));
                if !has_tool_activity && prepare_core_for_turn_replay(&core, turn)? {
                    turn.metadata.status = RuntimeTurnStatus::Queued;
                    turn.metadata.active_task_id = None;
                    turn.metadata.started_at = None;
                    turn.metadata.completed_at = None;
                    turn.metadata.error = None;
                    turn.metadata.message_start = None;
                    turn.metadata.message_end = None;
                } else {
                    turn.metadata.status = RuntimeTurnStatus::Interrupted;
                    turn.metadata.completed_at = Some(now());
                    turn.metadata.error =
                        Some("Runtime restarted after Turn history became ambiguous".to_owned());
                }
                turns_changed = true;
            }
        }
        if turns_changed {
            persist_turns(&turns_path, &turns)?;
        }
        let mut sessions_changed = false;
        for session in sessions.values_mut() {
            if matches!(
                session.status,
                RuntimeSessionStatus::Queued
                    | RuntimeSessionStatus::Running
                    | RuntimeSessionStatus::WaitingApproval
                    | RuntimeSessionStatus::WaitingAnswer
            ) {
                let replayable = session
                    .active_turn_id
                    .and_then(|turn_id| turns.get(&turn_id))
                    .is_some_and(|turn| turn.metadata.status == RuntimeTurnStatus::Queued);
                session.status = if replayable {
                    RuntimeSessionStatus::Idle
                } else {
                    RuntimeSessionStatus::Interrupted
                };
                session.active_turn_id = None;
                session.updated_at = now();
                session.last_error = Some(if replayable {
                    "Runtime restarted; active Turn was safely requeued".to_owned()
                } else {
                    "Runtime restarted while Session history could not be safely replayed"
                        .to_owned()
                });
                sessions_changed = true;
            }
        }
        if sessions_changed {
            persist_sessions(&path, &sessions)?;
        }
        Ok(Self {
            path,
            turns_path,
            core,
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
            let mut core = self
                .core
                .load(id)
                .with_context(|| format!("adopt Core Session {id}"))?;
            if core.workspace.canonicalize()? != workspace {
                bail!("Core Session workspace does not match Runtime Session request");
            }
            if let Some(profile) = request.profile.clone() {
                core.profile = Some(profile);
            }
            if let Some(model) = request.model.clone() {
                core.model = Some(model);
            }
            if core.config.is_none() {
                core.config = request.config.clone();
            }
            self.core.save(&mut core)?;
            // 领养一条已经存在的 Core 会话：它的 `title_source` 已经写在文件里，
            // 这里不覆盖。此前这里一律钉成 `Legacy`，于是每一条从 TUI 建出来
            // 再交给 Runtime 的会话都永久失去了自动标题——历史面板里那一屏
            // `New session` 就是这么来的。
            core
        } else {
            let explicit_title = request.title.filter(|value| !value.trim().is_empty());
            let mut core = willdeep_core::Session::new(
                workspace.clone(),
                request.profile.clone(),
                explicit_title.as_deref().unwrap_or_default(),
            );
            if let Some(title) = explicit_title {
                core.title = normalized_title(title)?;
                core.title_source = willdeep_core::TitleSource::User;
            }
            core.model = request.model.clone();
            core.config = request.config.clone();
            self.core.save(&mut core)?;
            core
        };
        let timestamp = now();
        let session = RuntimeSession {
            schema: RUNTIME_SESSION_SCHEMA,
            id: core.id,
            root_agent_id: uuid::Uuid::new_v4(),
            workspace,
            profile: core.profile.clone(),
            model: core.model.clone(),
            config: core.config.clone(),
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
        let mut sessions = self.lock()?;
        let session = sessions.get_mut(&id).context("Runtime Session not found")?;
        self.core.update(id, |core| {
            core.title = title;
            core.title_source = willdeep_core::TitleSource::User;
        })?;
        session.updated_at = now();
        let result = session.clone();
        persist_sessions(&self.path, &sessions)?;
        Ok(result)
    }

    pub fn update_model(&self, id: uuid::Uuid, model: String) -> Result<RuntimeSession> {
        self.ensure_manageable(id)?;
        let model = normalized_optional("Model", Some(model))?.context("Model is required")?;
        let mut sessions = self.lock()?;
        let session = sessions.get_mut(&id).context("Runtime Session not found")?;
        self.core.update(id, |core| {
            core.model = Some(model.clone());
        })?;
        session.model = Some(model);
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
            if turn.metadata.message_generation != core.compression_generation {
                bail!(
                    "Runtime Turn boundary predates the current compression checkpoint and cannot be forked exactly"
                );
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
        core = core.fork_snapshot();
        core.title = match title {
            Some(title) => normalized_title(title)?,
            None => default_fork_title(&core.title),
        };
        // 派生标题带着母会话的名字，属于既成事实，自动流程不该再动它。
        core.title_source = willdeep_core::TitleSource::User;
        core.created_at = timestamp;
        core.updated_at = timestamp;
        core.attention_read.clear();
        core.runtime_event_cursor = 0;
        core.runtime_managed = true;
        core.swift_source = None;
        core.profile = target_profile.clone();
        core.model = model.clone().or_else(|| source.model.clone());
        self.core.save(&mut core)?;
        let fork = RuntimeSession {
            schema: RUNTIME_SESSION_SCHEMA,
            id: core.id,
            root_agent_id: uuid::Uuid::new_v4(),
            workspace: source.workspace,
            profile: target_profile,
            model: core.model.clone(),
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
                compression_generation: core.compression_generation,
                compression_checkpoint: core.compression_checkpoint,
            },
            turns,
        })
    }

    pub fn search(&self, filters: SessionSearchQuery) -> Result<Vec<RuntimeSessionSearchResult>> {
        let query = filters
            .q
            .clone()
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
            .clone()
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
                origin: SessionOrigin::Runtime,
            });
            if results.len() >= MAX_SEARCH_RESULTS {
                break;
            }
        }
        self.extend_with_unmanaged(
            &mut results,
            query.as_deref(),
            workspace.as_deref(),
            &filters,
        );
        results.sort_by_key(|result| std::cmp::Reverse(result.updated_at));
        results.truncate(MAX_SEARCH_RESULTS);
        Ok(results)
    }

    /// 把 Runtime 没登记过的会话补进结果：Xedit 桥接的，以及只有 Core 文件的。
    ///
    /// 三条取舍：
    ///
    /// * **`--profile` / `--model` 过滤存在时整段跳过。** 这两样只记在 Runtime
    ///   元数据里，未登记的会话根本没有这个字段。把它们当「不匹配」剔掉是对的，
    ///   当「匹配」混进去则是让过滤器撒谎。
    /// * **状态一律按 `Idle` 记。** 它们没有轮次队列，也就没有别的状态可言。
    /// * **正文匹配才读全文。** 先用 digest（按 mtime+size 缓存、不物化正文）
    ///   过掉工作区和时间窗，剩下的才逐条 load。桌面版那个目录有几百个会话、
    ///   上百 MB，每敲一个键全量反序列化一遍是不能接受的。
    fn extend_with_unmanaged(
        &self,
        results: &mut Vec<RuntimeSessionSearchResult>,
        query: Option<&str>,
        workspace: Option<&Path>,
        filters: &SessionSearchQuery,
    ) {
        if filters.profile.is_some() || filters.model.is_some() {
            return;
        }
        if filters
            .status
            .is_some_and(|status| status != RuntimeSessionStatus::Idle)
        {
            return;
        }
        let known = results
            .iter()
            .map(|result| result.id)
            .collect::<HashSet<_>>();
        for digest in self.core.digests() {
            if known.contains(&digest.id) || results.len() >= MAX_SEARCH_RESULTS {
                continue;
            }
            let digest_workspace = digest
                .workspace
                .canonicalize()
                .unwrap_or_else(|_| digest.workspace.clone());
            if workspace.is_some_and(|value| value != digest_workspace)
                || filters
                    .updated_after
                    .is_some_and(|value| digest.updated_at < value)
                || filters
                    .updated_before
                    .is_some_and(|value| digest.updated_at > value)
            {
                continue;
            }
            let title_matches =
                query.is_none_or(|query| digest.title.to_lowercase().contains(query));
            let matching_message = match query {
                Some(query) if !title_matches => {
                    let Ok(core) = self.core.load(digest.id) else {
                        continue;
                    };
                    let Some(message) = core
                        .messages
                        .iter()
                        .find(|message| message.content.to_lowercase().contains(query))
                    else {
                        continue;
                    };
                    Some(bounded_snippet(&message.content))
                }
                _ => None,
            };
            results.push(RuntimeSessionSearchResult {
                id: digest.id,
                title: digest.title,
                workspace: digest_workspace,
                status: RuntimeSessionStatus::Idle,
                profile: None,
                model: None,
                updated_at: digest.updated_at,
                message_count: digest.message_count,
                snippet: matching_message,
                origin: if digest.bridged {
                    SessionOrigin::Xedit
                } else {
                    SessionOrigin::Local
                },
            });
        }
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

    #[cfg(test)]
    pub fn enqueue_turn(
        &self,
        session_id: uuid::Uuid,
        request: CreateRuntimeTurn,
    ) -> Result<(RuntimeTurn, bool)> {
        let (turn, created, _) = self.enqueue_turn_observed(session_id, request)?;
        Ok((turn, created))
    }

    pub(super) fn enqueue_turn_observed(
        &self,
        session_id: uuid::Uuid,
        request: CreateRuntimeTurn,
    ) -> Result<(RuntimeTurn, bool, bool)> {
        if request.prompt.trim().is_empty() && request.attachments.is_empty() {
            bail!("Turn prompt and attachments must not both be empty");
        }
        let session = self.get(session_id)?.context("Runtime Session not found")?;
        if session.status == RuntimeSessionStatus::Archived {
            bail!("Runtime Session is archived");
        }
        let title_changed =
            self.apply_auto_title(session_id, &request.prompt, !request.attachments.is_empty())?;
        let mut turns = self.turns_lock()?;
        if let Some(turn) = turns.values().find(|turn| {
            turn.metadata.session_id == session_id && turn.metadata.request_id == request.request_id
        }) {
            return Ok((turn.metadata.clone(), false, title_changed));
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
            message_generation: 0,
        };
        turns.insert(
            metadata.id,
            StoredRuntimeTurn {
                metadata: metadata.clone(),
                prompt: request.prompt,
                attachments: request.attachments,
                replay_existing_user_message: false,
                origin_client: request.origin_client,
            },
        );
        persist_turns(&self.turns_path, &turns)?;
        Ok((metadata, true, title_changed))
    }

    /// L1：轮次入队那一刻就把占位标题换掉，不等轮次跑完。
    ///
    /// 判定与 TUI 本地轮次共用 [`crate::titling`]，两条入口对同一条会话必须
    /// 给出同一个标题；来源位存在 Core 会话上，Runtime 这边不再另存一份。
    fn apply_auto_title(
        &self,
        session_id: uuid::Uuid,
        prompt: &str,
        has_attachments: bool,
    ) -> Result<bool> {
        let mut core = self
            .core
            .load(session_id)
            .with_context(|| format!("load Core Session {session_id} for automatic title"))?;
        if !crate::titling::apply_derived_title(&mut core, prompt, has_attachments) {
            return Ok(false);
        }
        self.core.save(&mut core)?;
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.updated_at = now();
        persist_sessions(&self.path, &sessions)?;
        Ok(true)
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
        let core = self
            .core
            .load(session_id)
            .with_context(|| format!("load Core Session {session_id}"))?;
        let core_message_count = core.messages.len();
        turn.metadata.message_generation = core.compression_generation;
        turn.metadata.message_start = Some(if turn.replay_existing_user_message {
            core_message_count
                .checked_sub(1)
                .context("recovered Turn is missing its persisted user message")?
        } else {
            core_message_count
        });
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
                origin_client: turn.origin_client.clone(),
                replay_existing_user_message: turn.replay_existing_user_message,
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

    /// Called after the Core execution lease is acquired, before any new message.
    pub fn prepare_execution(
        &self,
        task_id: uuid::Uuid,
        core: &willdeep_core::Session,
    ) -> Result<()> {
        let mut turns = self.turns_lock()?;
        let Some(turn) = turns
            .values_mut()
            .find(|turn| turn.metadata.active_task_id == Some(task_id))
        else {
            return Ok(()); // Legacy tasks need not belong to a Runtime Turn.
        };
        anyhow::ensure!(
            turn.metadata.session_id == core.id
                && turn.metadata.status == RuntimeTurnStatus::Running,
            "Runtime Turn is no longer eligible for execution"
        );
        if turn.replay_existing_user_message {
            anyhow::ensure!(
                turn.metadata.message_generation == core.compression_generation
                    && turn
                        .metadata
                        .message_start
                        .and_then(|start| start.checked_add(1))
                        == Some(core.messages.len()),
                "recovered Runtime Turn history changed before execution"
            );
            let message = core
                .messages
                .last()
                .context("recovered user message is missing")?;
            anyhow::ensure!(
                message.role == willdeep_core::Role::User
                    && message.content == turn.prompt
                    && serde_json::to_vec(&message.attachments)?
                        == serde_json::to_vec(&turn.attachments)?,
                "recovered Runtime Turn user message changed before execution"
            );
        } else {
            turn.metadata.message_start = Some(core.messages.len());
            turn.metadata.message_generation = core.compression_generation;
        }
        turn.metadata.message_end = None;
        persist_turns(&self.turns_path, &turns)
    }

    /// Preserve the completed Harness snapshot, not a later writer's history.
    pub fn record_execution_end(
        &self,
        task_id: uuid::Uuid,
        end: usize,
        generation: u64,
    ) -> Result<()> {
        let mut turns = self.turns_lock()?;
        let Some(turn) = turns
            .values_mut()
            .find(|turn| turn.metadata.active_task_id == Some(task_id))
        else {
            return Ok(());
        };
        anyhow::ensure!(
            turn.metadata.status == RuntimeTurnStatus::Running,
            "Runtime Turn is no longer running at execution completion"
        );
        turn.metadata.message_end = Some(end);
        turn.metadata.message_generation = generation;
        persist_turns(&self.turns_path, &turns)
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
            RuntimeTaskStatus::Partial => RuntimeTurnStatus::Partial,
            RuntimeTaskStatus::Cancelled => RuntimeTurnStatus::Cancelled,
            RuntimeTaskStatus::Interrupted => RuntimeTurnStatus::Interrupted,
            _ => RuntimeTurnStatus::Failed,
        };
        turn.metadata.completed_at = Some(now());
        turn.metadata.error = error.clone();
        if matches!(
            status,
            RuntimeTaskStatus::Completed | RuntimeTaskStatus::Partial
        ) {
            // The Core Session is the durable conversation source after a successful
            // Harness process exit. Do not retain a second private copy indefinitely.
            turn.prompt.clear();
            turn.attachments.clear();
            if turn.metadata.message_end.is_none() {
                let core = self
                    .core
                    .load(session_id)
                    .with_context(|| format!("load completed Core Session {session_id}"))?;
                turn.metadata.message_end = Some(core.messages.len());
                turn.metadata.message_generation = core.compression_generation;
            }
        }
        persist_turns(&self.turns_path, &turns)?;
        drop(turns);
        let mut sessions = self.lock()?;
        let session = sessions
            .get_mut(&session_id)
            .context("Runtime Session not found")?;
        session.active_turn_id = None;
        session.status = if matches!(
            status,
            RuntimeTaskStatus::Completed | RuntimeTaskStatus::Partial
        ) {
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
    authorize_internal(&state, &headers)?;
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
    let work_guard = state.work_gate.read().await;
    if *work_guard {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let (turn, created, title_changed) = state
        .sessions
        .enqueue_turn_observed(session_id, request)
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
    if title_changed {
        state
            .events
            .append("session.renamed", format!("session_id={session_id}"))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let turn = state
        .sessions
        .get_turn(turn.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    drop(work_guard);
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
                .create_session(
                    &willdeep_runtime_protocol::CreateSessionParams {
                        id: None,
                        workspace: workspace.display().to_string(),
                        profile,
                        model,
                        title,
                    },
                    uuid::Uuid::new_v4(),
                )
                .await?,
        )?;
        print_public_session(&session);
        return Ok(());
    }
    let session: RuntimeSession = internal_transport::InternalRuntimeClient::from_state(&state)?
        .post(
            "/v1/internal/sessions",
            &CreateRuntimeSession {
                id: None,
                workspace,
                profile,
                model,
                config,
                title,
            },
        )
        .await?;
    print_session(&session);
    Ok(())
}

pub(super) async fn list_sessions_cli(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let sessions = cli_api_data(runtime_client(&state)?.sessions().await?)?;
    for session in sessions {
        print_public_session(&session);
    }
    Ok(())
}

pub(super) async fn show_session_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let session = cli_api_data(runtime_client(&state)?.session(id).await?)?;
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
            .search_sessions(&willdeep_runtime_protocol::SearchSessionsParams {
                query,
                workspace,
                status,
                profile,
                model,
                updated_after,
                updated_before,
            })
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
            .rename_session(
                &willdeep_runtime_protocol::RenameSessionParams { id, title },
                uuid::Uuid::new_v4(),
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
            .archive_session(
                &willdeep_runtime_protocol::ArchiveSessionParams {
                    id,
                    archived: !unarchive,
                },
                uuid::Uuid::new_v4(),
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
    let export = cli_api_data(runtime_client(&state)?.export_session(id).await?)?;
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
    let result = cli_api_data(
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
    if result.id != id || result.status != willdeep_runtime_protocol::ObjectMutationStatus::Deleted
    {
        bail!("Runtime returned an invalid Session deletion result");
    }
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
            .submit_turn(
                &willdeep_runtime_protocol::SubmitTurnParams {
                    session_id,
                    turn_request_id,
                    prompt,
                    attachments: Vec::new(),
                    origin_client: Some(crate::client_identity(crate::Surface::Cli).to_owned()),
                },
                turn_request_id,
            )
            .await?,
    )?;
    print_public_turn(&turn);
    Ok(())
}

pub(super) async fn list_turns_cli(home: &Path, session_id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turns = cli_api_data(runtime_client(&state)?.turns(session_id).await?)?;
    for turn in turns {
        print_public_turn(&turn);
    }
    Ok(())
}

pub(super) async fn show_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turn = cli_api_data(runtime_client(&state)?.turn(id).await?)?;
    print_public_turn(&turn);
    Ok(())
}

pub(super) async fn stop_turn_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let turn = cli_api_data(
        runtime_client(&state)?
            .stop_turn(id, uuid::Uuid::new_v4())
            .await?,
    )?;
    print_public_turn(&turn);
    Ok(())
}

pub(super) async fn stop_session_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let client = runtime_client(&state)?;
    let session = client.session(id).await?.into_result()?;
    let turn_id = active_turn_for_stop(&session)?;
    let turn = client
        .stop_turn(turn_id, uuid::Uuid::new_v4())
        .await?
        .into_result()?;
    print_public_turn(&turn);
    Ok(())
}

fn active_turn_for_stop(session: &willdeep_runtime_protocol::RuntimeSession) -> Result<uuid::Uuid> {
    session
        .active_turn_id
        .context("Runtime Session has no active or queued Turn")
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

fn load_sessions(path: &Path) -> Result<(HashMap<uuid::Uuid, RuntimeSession>, bool)> {
    if !path.exists() {
        return Ok((HashMap::new(), false));
    }
    let mut sessions: Vec<RuntimeSession> = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut migrated = false;
    for session in &mut sessions {
        match session.schema {
            RUNTIME_SESSION_SCHEMA => {}
            1 => {
                session.schema = RUNTIME_SESSION_SCHEMA;
                migrated = true;
            }
            _ => {
                bail!("unsupported Runtime Session schema {}", session.schema);
            }
        }
    }
    Ok((
        sessions
            .into_iter()
            .map(|session| (session.id, session))
            .collect(),
        migrated,
    ))
}

fn prepare_core_for_turn_replay(
    core_store: &willdeep_core::SessionStore,
    turn: &mut StoredRuntimeTurn,
) -> Result<bool> {
    turn.replay_existing_user_message = false;
    let Some(message_start) = turn.metadata.message_start else {
        return Ok(false);
    };
    let Ok(core) = core_store.load(turn.metadata.session_id) else {
        return Ok(false);
    };
    if turn.metadata.message_generation != core.compression_generation {
        return Ok(false);
    }
    if core.messages.len() == message_start {
        return Ok(true);
    }
    if core.messages.len() != message_start.saturating_add(1) {
        return Ok(false);
    }
    let Some(message) = core.messages.last() else {
        return Ok(false);
    };
    let same_attachments =
        serde_json::to_vec(&message.attachments)? == serde_json::to_vec(&turn.attachments)?;
    if message.role != willdeep_core::Role::User
        || message.content != turn.prompt
        || !same_attachments
    {
        return Ok(false);
    }
    turn.replay_existing_user_message = true;
    Ok(true)
}

fn load_tool_task_ids(path: &Path) -> Result<std::collections::HashSet<uuid::Uuid>> {
    if !path.exists() {
        return Ok(std::collections::HashSet::new());
    }
    let records: Vec<willdeep_runtime_protocol::RuntimeTool> =
        serde_json::from_slice(&std::fs::read(path)?)
            .with_context(|| format!("read Tool activity replay guard: {}", path.display()))?;
    Ok(records.into_iter().map(|record| record.task_id).collect())
}

fn backup_sessions_before_migration(path: &Path, source_schema: u32) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("sessions.json");
    let backup = path.with_file_name(format!(
        "{file_name}.schema{source_schema}.{}.{}.backup",
        now(),
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = std::fs::read(path)?;
    write_private(&backup, &bytes).with_context(|| {
        format!(
            "backup Runtime Sessions before schema migration: {}",
            backup.display()
        )
    })
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
mod tests;
