use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentCommandKind {
    Stop,
    Retry,
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
) -> Result<Option<AgentCommandWatcher>> {
    let Some(connection) = connection.cloned() else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()?;
    let task = tokio::spawn(async move {
        let mut resolved = HashMap::<uuid::Uuid, ResolveAgentCommand>::new();
        loop {
            let response = client
                .get(format!(
                    "{}/v1/tasks/{}/agent-commands",
                    connection.url, connection.task_id
                ))
                .header(TOKEN_HEADER, &connection.token)
                .send()
                .await;
            if let Ok(response) = response
                && response.status().is_success()
                && let Ok(commands) = response.json::<Vec<AgentCommand>>().await
            {
                for command in commands {
                    let resolution = resolved
                        .entry(command.id)
                        .or_insert_with(|| {
                            let (applied, error) = apply_command(&background, &command);
                            ResolveAgentCommand { applied, error }
                        })
                        .clone();
                    if let Ok(response) = client
                        .post(format!(
                            "{}/v1/tasks/{}/agent-commands/{}/resolve",
                            connection.url, connection.task_id, command.id
                        ))
                        .header(TOKEN_HEADER, &connection.token)
                        .json(&resolution)
                        .send()
                        .await
                        && response.status().is_success()
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

fn apply_command(
    background: &willdeep_core::BackgroundTaskRegistry,
    command: &AgentCommand,
) -> (bool, Option<String>) {
    match command.kind {
        AgentCommandKind::Stop if background.kill_agent(command.agent_id) => (true, None),
        AgentCommandKind::Retry if background.retry_agent(command.agent_id).is_some() => {
            (true, None)
        }
        AgentCommandKind::Stop => (
            false,
            Some("Agent is not an active background task in this Harness".to_owned()),
        ),
        AgentCommandKind::Retry => (
            false,
            Some("Agent has no retriable terminal background task in this Harness".to_owned()),
        ),
    }
}

pub(super) struct AgentCommandStore {
    path: PathBuf,
    commands: Mutex<HashMap<uuid::Uuid, AgentCommand>>,
}

impl AgentCommandStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut commands = load_commands(&path)?;
        let mut changed = false;
        for command in commands.values_mut() {
            if command.status == AgentCommandStatus::Pending {
                command.status = AgentCommandStatus::Rejected;
                command.resolved_at = Some(now());
                command.error = Some("Runtime restarted before command was applied".to_owned());
                changed = true;
            }
        }
        if changed {
            persist_commands(&path, &commands)?;
        }
        Ok(Self {
            path,
            commands: Mutex::new(commands),
        })
    }

    pub fn enqueue(
        &self,
        task_id: uuid::Uuid,
        agent_id: uuid::Uuid,
        kind: AgentCommandKind,
    ) -> Result<AgentCommand> {
        let mut commands = self.lock()?;
        if let Some(existing) = commands.values().find(|command| {
            command.task_id == task_id
                && command.agent_id == agent_id
                && command.kind == kind
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
            .enqueue(task_id, agent_id, AgentCommandKind::Stop)
            .unwrap();
        let duplicate = store
            .enqueue(task_id, agent_id, AgentCommandKind::Stop)
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
