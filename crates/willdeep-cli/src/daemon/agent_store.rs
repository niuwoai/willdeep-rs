use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeAgentStatus {
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
pub(crate) struct RuntimeAgent {
    pub id: uuid::Uuid,
    pub parent_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub status: RuntimeAgentStatus,
    pub current_turn: u64,
    pub current_tool: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
    pub error: Option<String>,
}

pub(super) struct AgentStore {
    path: PathBuf,
    agents: Mutex<HashMap<uuid::Uuid, RuntimeAgent>>,
}

impl AgentStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut agents = load_agents(&path)?;
        let mut changed = false;
        for agent in agents.values_mut() {
            if matches!(
                agent.status,
                RuntimeAgentStatus::Queued
                    | RuntimeAgentStatus::Running
                    | RuntimeAgentStatus::WaitingApproval
                    | RuntimeAgentStatus::WaitingAnswer
            ) {
                agent.status = RuntimeAgentStatus::Interrupted;
                agent.completed_at = Some(now());
                agent.updated_at = now();
                agent.error = Some("Runtime restarted while agent was active".to_owned());
                changed = true;
            }
        }
        if changed {
            persist_agents(&path, &agents)?;
        }
        Ok(Self {
            path,
            agents: Mutex::new(agents),
        })
    }

    pub fn ensure_root(
        &self,
        task_id: uuid::Uuid,
        workspace: PathBuf,
        profile: Option<String>,
        status: RuntimeAgentStatus,
    ) -> Result<RuntimeAgent> {
        let mut agents = self.lock()?;
        if let Some(agent) = agents.values().find(|agent| agent.task_id == task_id) {
            return Ok(agent.clone());
        }
        let timestamp = now();
        let agent = RuntimeAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: None,
            task_id,
            workspace,
            profile,
            status,
            current_turn: 0,
            current_tool: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            created_at: timestamp,
            updated_at: timestamp,
            completed_at: None,
            error: None,
        };
        agents.insert(agent.id, agent.clone());
        persist_agents(&self.path, &agents)?;
        Ok(agent)
    }

    pub fn list(&self) -> Result<Vec<RuntimeAgent>> {
        let mut agents = self.lock()?.values().cloned().collect::<Vec<_>>();
        agents.sort_by_key(|agent| std::cmp::Reverse(agent.created_at));
        Ok(agents)
    }

    pub fn get(&self, id: uuid::Uuid) -> Result<Option<RuntimeAgent>> {
        Ok(self.lock()?.get(&id).cloned())
    }

    pub fn set_status_for_task(
        &self,
        task_id: uuid::Uuid,
        status: RuntimeAgentStatus,
        error: Option<String>,
    ) -> Result<()> {
        self.update_task_agent(task_id, |agent| {
            agent.status = status;
            agent.updated_at = now();
            agent.error = error;
            if matches!(
                status,
                RuntimeAgentStatus::Completed
                    | RuntimeAgentStatus::Failed
                    | RuntimeAgentStatus::Cancelled
                    | RuntimeAgentStatus::Interrupted
            ) {
                agent.completed_at = Some(now());
                agent.current_tool = None;
            }
        })
    }

    pub fn apply_harness_event(&self, task_id: uuid::Uuid, line: &str) -> Result<()> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return Ok(());
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("turn_started") => self.update_task_agent(task_id, |agent| {
                agent.status = RuntimeAgentStatus::Running;
                agent.current_turn = value
                    .get("turn")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(agent.current_turn);
                agent.updated_at = now();
            }),
            Some("tool_requested") => self.update_task_agent(task_id, |agent| {
                agent.current_tool = value
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned);
                agent.updated_at = now();
            }),
            Some("tool_completed") => self.update_task_agent(task_id, |agent| {
                agent.current_tool = None;
                agent.updated_at = now();
            }),
            Some("usage") => self.update_task_agent(task_id, |agent| {
                agent.input_tokens = value.get("input_tokens").and_then(|value| value.as_u64());
                agent.output_tokens = value.get("output_tokens").and_then(|value| value.as_u64());
                agent.total_tokens = value.get("total_tokens").and_then(|value| value.as_u64());
                agent.updated_at = now();
            }),
            _ => Ok(()),
        }
    }

    fn update_task_agent(
        &self,
        task_id: uuid::Uuid,
        update: impl FnOnce(&mut RuntimeAgent),
    ) -> Result<()> {
        let mut agents = self.lock()?;
        let Some(agent) = agents.values_mut().find(|agent| agent.task_id == task_id) else {
            return Ok(());
        };
        update(agent);
        persist_agents(&self.path, &agents)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<uuid::Uuid, RuntimeAgent>>> {
        self.agents
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime agent store lock poisoned"))
    }
}

fn load_agents(path: &Path) -> Result<HashMap<uuid::Uuid, RuntimeAgent>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let agents: Vec<RuntimeAgent> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(agents.into_iter().map(|agent| (agent.id, agent)).collect())
}

fn persist_agents(path: &Path, agents: &HashMap<uuid::Uuid, RuntimeAgent>) -> Result<()> {
    let mut agents = agents.values().cloned().collect::<Vec<_>>();
    agents.sort_by_key(|agent| agent.created_at);
    write_json_atomic(path, &agents)
}
