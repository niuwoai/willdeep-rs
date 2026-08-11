use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentCommandKind {
    Stop,
    Retry,
    Instruct,
    Spawn,
}

pub(super) async fn control_agent(
    home: &Path,
    id: uuid::Uuid,
    kind: AgentCommandKind,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let client = runtime_client(&state)?;
    let command = match kind {
        AgentCommandKind::Stop => client.stop_agent(id, uuid::Uuid::new_v4()).await?,
        AgentCommandKind::Retry => client.retry_agent(id, uuid::Uuid::new_v4()).await?,
        AgentCommandKind::Instruct | AgentCommandKind::Spawn => {
            bail!("Agent command must use its dedicated Runtime Client method")
        }
    };
    let command = command.into_result()?;
    println!(
        "agent_command\tid={}\tagent={}\taction={:?}\tstatus={:?}",
        command.id, command.agent_id, command.kind, command.status
    );
    Ok(())
}

pub(super) async fn instruct_agent(home: &Path, id: uuid::Uuid, message: String) -> Result<()> {
    let message = message.trim();
    if message.is_empty() || message.len() > 16 * 1024 {
        bail!("Agent instruction must contain 1 to 16384 bytes");
    }
    let state = ensure_running(home).await?;
    let command = runtime_client(&state)?
        .prompt_agent(
            &willdeep_runtime_protocol::AgentPromptParams {
                id,
                message: message.to_owned(),
            },
            uuid::Uuid::new_v4(),
        )
        .await?
        .into_result()?;
    println!(
        "agent_command\tid={}\tagent={}\taction=instruct\tstatus={:?}",
        command.id, command.agent_id, command.status
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AgentInstructionRequest {
    pub message: String,
}

pub(super) async fn stop_agent_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    enqueue_agent_command(&state, &headers, id, AgentCommandKind::Stop, None, None).await
}

pub(super) async fn retry_agent_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    enqueue_work_agent_command(&state, &headers, id, AgentCommandKind::Retry, None, None).await
}

pub(super) async fn instruct_agent_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
    Json(request): Json<AgentInstructionRequest>,
) -> Result<Response, StatusCode> {
    if request.message.trim().is_empty() || request.message.len() > 16 * 1024 {
        return Err(StatusCode::BAD_REQUEST);
    }
    enqueue_work_agent_command(
        &state,
        &headers,
        id,
        AgentCommandKind::Instruct,
        Some(request.message.trim().to_owned()),
        None,
    )
    .await
}

async fn enqueue_work_agent_command(
    state: &ServerState,
    headers: &HeaderMap,
    id: uuid::Uuid,
    kind: AgentCommandKind,
    message: Option<String>,
    model: Option<String>,
) -> Result<Response, StatusCode> {
    authorize(state, headers)?;
    let work_guard = state.work_gate.read().await;
    if *work_guard {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let command = enqueue_agent_command_internal(state, id, kind, message, model).await?;
    drop(work_guard);
    Ok((StatusCode::ACCEPTED, Json(command)).into_response())
}

async fn enqueue_agent_command(
    state: &ServerState,
    headers: &HeaderMap,
    id: uuid::Uuid,
    kind: AgentCommandKind,
    message: Option<String>,
    model: Option<String>,
) -> Result<Response, StatusCode> {
    authorize(state, headers)?;
    let command = enqueue_agent_command_internal(state, id, kind, message, model).await?;
    Ok((StatusCode::ACCEPTED, Json(command)).into_response())
}

pub(super) async fn enqueue_agent_command_internal(
    state: &ServerState,
    id: uuid::Uuid,
    kind: AgentCommandKind,
    message: Option<String>,
    model: Option<String>,
) -> Result<AgentCommand, StatusCode> {
    let agent = state
        .agents
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if agent.parent_id.is_none() || !agent.background {
        return Err(StatusCode::CONFLICT);
    }
    let task = state
        .tasks
        .get(agent.task_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    if !matches!(
        task.status,
        RuntimeTaskStatus::Running
            | RuntimeTaskStatus::WaitingApproval
            | RuntimeTaskStatus::WaitingAnswer
    ) {
        return Err(StatusCode::CONFLICT);
    }
    let valid_status = match kind {
        AgentCommandKind::Stop => agent.status == RuntimeAgentStatus::Running,
        AgentCommandKind::Retry => matches!(
            agent.status,
            RuntimeAgentStatus::Blocked
                | RuntimeAgentStatus::Completed
                | RuntimeAgentStatus::Failed
                | RuntimeAgentStatus::Cancelled
                | RuntimeAgentStatus::Interrupted
        ),
        AgentCommandKind::Instruct => agent.status == RuntimeAgentStatus::Running,
        AgentCommandKind::Spawn => false,
    };
    if !valid_status {
        return Err(StatusCode::CONFLICT);
    }
    let command = state
        .agent_commands
        .enqueue(agent.task_id, agent.id, kind, message, model)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .events
        .append(
            "agent.command_requested",
            format!(
                "task_id={} agent_id={} command_id={} kind={kind:?}",
                agent.task_id, agent.id, command.id
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(command)
}

pub(super) async fn agent_commands_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(task_id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize_internal(&state, &headers)?;
    if state.tasks.get(task_id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let commands = state
        .agent_commands
        .pending_for_task(task_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(commands).into_response())
}

pub(super) async fn resolve_agent_command_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath((task_id, command_id)): AxumPath<(uuid::Uuid, uuid::Uuid)>,
    Json(resolution): Json<ResolveAgentCommand>,
) -> Result<Response, StatusCode> {
    authorize_internal(&state, &headers)?;
    let command = state
        .agent_commands
        .resolve(task_id, command_id, resolution)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if command.kind == AgentCommandKind::Spawn && command.status == AgentCommandStatus::Rejected {
        state
            .agents
            .reject_external_child(
                command.agent_id,
                command
                    .error
                    .clone()
                    .unwrap_or_else(|| "external spawn was rejected".to_owned()),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    state
        .events
        .append(
            "agent.command_resolved",
            format!(
                "task_id={} agent_id={} command_id={} status={:?}",
                command.task_id, command.agent_id, command.id, command.status
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(command).into_response())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentCommandStatus {
    Pending,
    Applied,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentCommand {
    pub id: uuid::Uuid,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub kind: AgentCommandKind,
    pub status: AgentCommandStatus,
    pub created_at: u64,
    pub resolved_at: Option<u64>,
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ResolveAgentCommand {
    pub applied: bool,
    #[serde(default)]
    pub error: Option<String>,
}

pub(crate) struct AgentCommandWatcher {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AgentCommandWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) fn start_agent_command_watcher(
    connection: Option<&RuntimeConnection>,
    background: Arc<willdeep_core::BackgroundTaskRegistry>,
    subagents: Arc<willdeep_core::SubagentCatalog>,
) -> Result<Option<AgentCommandWatcher>> {
    let Some(connection) = connection.cloned() else {
        return Ok(None);
    };
    let client = internal_transport::InternalRuntimeClient::new(
        connection.url.clone(),
        connection.token.clone(),
    )?;
    let task = tokio::spawn(async move {
        let mut resolved = HashMap::<uuid::Uuid, ResolveAgentCommand>::new();
        loop {
            let commands = tokio::time::timeout(
                Duration::from_secs(3),
                client.get::<Vec<AgentCommand>>(&format!(
                    "/v1/internal/tasks/{}/agent-commands",
                    connection.task_id
                )),
            )
            .await;
            if let Ok(Ok(commands)) = commands {
                for command in commands {
                    let resolution = if let Some(resolution) = resolved.get(&command.id) {
                        resolution.clone()
                    } else {
                        let (applied, error) =
                            apply_command(&background, &subagents, &command).await;
                        let value = ResolveAgentCommand { applied, error };
                        resolved.insert(command.id, value.clone());
                        value
                    };
                    if let Ok(Ok(_)) = tokio::time::timeout(
                        Duration::from_secs(3),
                        client.post::<_, AgentCommand>(
                            &format!(
                                "/v1/internal/tasks/{}/agent-commands/{}/resolve",
                                connection.task_id, command.id
                            ),
                            &resolution,
                        ),
                    )
                    .await
                    {
                        resolved.remove(&command.id);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
    Ok(Some(AgentCommandWatcher { task }))
}

async fn apply_command(
    background: &willdeep_core::BackgroundTaskRegistry,
    subagents: &willdeep_core::SubagentCatalog,
    command: &AgentCommand,
) -> (bool, Option<String>) {
    match command.kind {
        AgentCommandKind::Stop if background.kill_agent(command.agent_id) => (true, None),
        AgentCommandKind::Retry => match subagents
            .retry_background_agent(command.agent_id, command.model.as_deref())
        {
            Ok(Some(_)) => (true, None),
            Ok(None) => (
                false,
                Some("Agent has no retriable terminal background task in this Harness".to_owned()),
            ),
            Err(error) => (false, Some(error.to_string())),
        },
        AgentCommandKind::Instruct
            if command
                .message
                .clone()
                .is_some_and(|message| background.instruct_agent(command.agent_id, message)) =>
        {
            (true, None)
        }
        AgentCommandKind::Stop => (
            false,
            Some("Agent is not an active background task in this Harness".to_owned()),
        ),
        AgentCommandKind::Instruct => (
            false,
            Some("Agent is not running or cannot accept instructions".to_owned()),
        ),
        AgentCommandKind::Spawn => {
            let Some(prompt) = command.message.clone() else {
                return (
                    false,
                    Some("Spawn command is missing its prompt".to_owned()),
                );
            };
            match subagents
                .spawn_external_read_only(
                    command.agent_id,
                    prompt,
                    command.label.clone(),
                    command.profile.clone(),
                )
                .await
            {
                Ok(()) => (true, None),
                Err(error) => (false, Some(error.to_string())),
            }
        }
    }
}

pub(super) struct AgentCommandStore {
    path: PathBuf,
    commands: Mutex<HashMap<uuid::Uuid, AgentCommand>>,
    recovered_after_restart: Mutex<Vec<AgentCommand>>,
}

impl AgentCommandStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut commands = load_commands(&path)?;
        let mut changed = false;
        let mut recovered_after_restart = Vec::new();
        for command in commands.values_mut() {
            if command.status == AgentCommandStatus::Pending {
                command.status = AgentCommandStatus::Rejected;
                command.resolved_at = Some(now());
                command.error = Some("Runtime restarted before command was applied".to_owned());
                command.message = None;
                command.profile = None;
                command.label = None;
                command.model = None;
                recovered_after_restart.push(command.clone());
                changed = true;
            }
        }
        if changed {
            persist_commands(&path, &commands)?;
        }
        Ok(Self {
            path,
            commands: Mutex::new(commands),
            recovered_after_restart: Mutex::new(recovered_after_restart),
        })
    }

    pub fn take_recovered_after_restart(&self) -> Result<Vec<AgentCommand>> {
        let mut recovered = self
            .recovered_after_restart
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime recovered Agent command index lock poisoned"))?;
        Ok(std::mem::take(&mut *recovered))
    }

    pub fn enqueue(
        &self,
        task_id: uuid::Uuid,
        agent_id: uuid::Uuid,
        kind: AgentCommandKind,
        message: Option<String>,
        model: Option<String>,
    ) -> Result<AgentCommand> {
        let mut commands = self.lock()?;
        if let Some(existing) = commands.values().find(|command| {
            command.task_id == task_id
                && command.agent_id == agent_id
                && command.kind == kind
                && command.message == message
                && command.model == model
                && command.status == AgentCommandStatus::Pending
        }) {
            return Ok(existing.clone());
        }
        let command = AgentCommand {
            id: uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            kind,
            status: AgentCommandStatus::Pending,
            created_at: now(),
            resolved_at: None,
            error: None,
            message,
            profile: None,
            label: None,
            model,
        };
        commands.insert(command.id, command.clone());
        persist_commands(&self.path, &commands)?;
        Ok(command)
    }

    pub fn enqueue_spawn(
        &self,
        task_id: uuid::Uuid,
        agent_id: uuid::Uuid,
        prompt: String,
        profile: Option<String>,
        label: Option<String>,
    ) -> Result<AgentCommand> {
        let mut commands = self.lock()?;
        let command = AgentCommand {
            id: uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            kind: AgentCommandKind::Spawn,
            status: AgentCommandStatus::Pending,
            created_at: now(),
            resolved_at: None,
            error: None,
            message: Some(prompt),
            profile,
            label,
            model: None,
        };
        commands.insert(command.id, command.clone());
        persist_commands(&self.path, &commands)?;
        Ok(command)
    }

    pub fn pending_for_task(&self, task_id: uuid::Uuid) -> Result<Vec<AgentCommand>> {
        let mut commands = self
            .lock()?
            .values()
            .filter(|command| {
                command.task_id == task_id && command.status == AgentCommandStatus::Pending
            })
            .cloned()
            .collect::<Vec<_>>();
        commands.sort_by_key(|command| command.created_at);
        Ok(commands)
    }

    pub fn resolve(
        &self,
        task_id: uuid::Uuid,
        id: uuid::Uuid,
        resolution: ResolveAgentCommand,
    ) -> Result<Option<AgentCommand>> {
        let mut commands = self.lock()?;
        let Some(command) = commands.get_mut(&id) else {
            return Ok(None);
        };
        if command.task_id != task_id {
            return Ok(None);
        }
        if command.status != AgentCommandStatus::Pending {
            return Ok(Some(command.clone()));
        }
        command.status = if resolution.applied {
            AgentCommandStatus::Applied
        } else {
            AgentCommandStatus::Rejected
        };
        command.resolved_at = Some(now());
        command.error = resolution.error;
        command.message = None;
        command.profile = None;
        command.label = None;
        command.model = None;
        let command = command.clone();
        persist_commands(&self.path, &commands)?;
        Ok(Some(command))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<uuid::Uuid, AgentCommand>>> {
        self.commands
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime agent command store lock poisoned"))
    }
}

fn load_commands(path: &Path) -> Result<HashMap<uuid::Uuid, AgentCommand>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let commands: Vec<AgentCommand> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(commands
        .into_iter()
        .map(|command| (command.id, command))
        .collect())
}

fn persist_commands(path: &Path, commands: &HashMap<uuid::Uuid, AgentCommand>) -> Result<()> {
    let mut commands = commands.values().cloned().collect::<Vec<_>>();
    commands.sort_by_key(|command| command.created_at);
    write_json_atomic(path, &commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_deduplicates_and_resolves_commands() {
        let root =
            std::env::temp_dir().join(format!("willdeep-agent-command-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("commands.json");
        let store = AgentCommandStore::open(path.clone()).unwrap();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let first = store
            .enqueue(task_id, agent_id, AgentCommandKind::Stop, None, None)
            .unwrap();
        let duplicate = store
            .enqueue(task_id, agent_id, AgentCommandKind::Stop, None, None)
            .unwrap();
        assert_eq!(first.id, duplicate.id);
        assert_eq!(
            store.pending_for_task(task_id).unwrap(),
            vec![first.clone()]
        );

        let resolved = store
            .resolve(
                task_id,
                first.id,
                ResolveAgentCommand {
                    applied: true,
                    error: None,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(resolved.status, AgentCommandStatus::Applied);
        assert!(store.pending_for_task(task_id).unwrap().is_empty());

        let reopened = AgentCommandStore::open(path).unwrap();
        assert!(reopened.pending_for_task(task_id).unwrap().is_empty());

        let store = AgentCommandStore::open(root.join("instruction-commands.json")).unwrap();
        let instruction = store
            .enqueue(
                task_id,
                agent_id,
                AgentCommandKind::Instruct,
                Some("inspect tests too".to_owned()),
                None,
            )
            .unwrap();
        assert_eq!(instruction.message.as_deref(), Some("inspect tests too"));
        let resolved = store
            .resolve(
                task_id,
                instruction.id,
                ResolveAgentCommand {
                    applied: true,
                    error: None,
                },
            )
            .unwrap()
            .unwrap();
        assert!(resolved.message.is_none());

        let retry = store
            .enqueue(
                task_id,
                agent_id,
                AgentCommandKind::Retry,
                None,
                Some("new-model".to_owned()),
            )
            .unwrap();
        assert_eq!(retry.model.as_deref(), Some("new-model"));
        let resolved = store
            .resolve(
                task_id,
                retry.id,
                ResolveAgentCommand {
                    applied: true,
                    error: None,
                },
            )
            .unwrap()
            .unwrap();
        assert!(resolved.model.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
