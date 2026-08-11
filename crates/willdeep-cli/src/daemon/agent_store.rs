use super::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeAgentStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Blocked,
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
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub background: bool,
    pub workspace: PathBuf,
    #[serde(default)]
    pub root_workspace: Option<PathBuf>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    #[serde(default)]
    pub dedicated_worktree: bool,
    #[serde(default)]
    pub worktree_merged_review_id: Option<String>,
    #[serde(default)]
    pub worktree_merged_child_snapshot_id: Option<String>,
    #[serde(default)]
    pub worktree_merged_at: Option<u64>,
    #[serde(default)]
    pub worktree_quarantined_at: Option<u64>,
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub status: RuntimeAgentStatus,
    pub current_turn: u64,
    pub current_tool: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub max_turns: Option<u64>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub report: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
    pub error: Option<String>,
}

pub(super) struct AgentStore {
    path: PathBuf,
    agents: Mutex<HashMap<uuid::Uuid, RuntimeAgent>>,
    recovered_after_restart: Mutex<Vec<RuntimeAgent>>,
}

impl AgentStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut agents = load_agents(&path)?;
        let mut changed = false;
        let mut recovered_after_restart = Vec::new();
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
                recovered_after_restart.push(agent.clone());
                changed = true;
            }
        }
        if changed {
            persist_agents(&path, &agents)?;
        }
        Ok(Self {
            path,
            agents: Mutex::new(agents),
            recovered_after_restart: Mutex::new(recovered_after_restart),
        })
    }

    pub fn take_recovered_after_restart(&self) -> Result<Vec<RuntimeAgent>> {
        let mut recovered = self
            .recovered_after_restart
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime recovered Agent index lock poisoned"))?;
        Ok(std::mem::take(&mut *recovered))
    }

    pub fn ensure_root(
        &self,
        task_id: uuid::Uuid,
        workspace: PathBuf,
        profile: Option<String>,
        model: Option<String>,
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
            label: Some("root".to_owned()),
            background: false,
            workspace,
            root_workspace: None,
            worktree_branch: None,
            dedicated_worktree: false,
            worktree_merged_review_id: None,
            worktree_merged_child_snapshot_id: None,
            worktree_merged_at: None,
            worktree_quarantined_at: None,
            profile,
            model,
            status,
            current_turn: 0,
            current_tool: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            report: None,
            created_at: timestamp,
            updated_at: timestamp,
            completed_at: None,
            error: None,
        };
        agents.insert(agent.id, agent.clone());
        persist_agents(&self.path, &agents)?;
        Ok(agent)
    }

    pub fn ensure_session_root(
        &self,
        id: uuid::Uuid,
        task_id: uuid::Uuid,
        workspace: PathBuf,
        profile: Option<String>,
        model: Option<String>,
        status: RuntimeAgentStatus,
    ) -> Result<RuntimeAgent> {
        let mut agents = self.lock()?;
        if let Some(agent) = agents.get_mut(&id) {
            agent.task_id = task_id;
            agent.workspace = workspace;
            agent.root_workspace = None;
            agent.worktree_branch = None;
            agent.dedicated_worktree = false;
            agent.worktree_merged_review_id = None;
            agent.worktree_merged_child_snapshot_id = None;
            agent.worktree_merged_at = None;
            agent.worktree_quarantined_at = None;
            agent.profile = profile;
            agent.model = model;
            agent.status = status;
            agent.current_turn = 0;
            agent.current_tool = None;
            agent.max_turns = None;
            agent.token_budget = None;
            agent.timeout_seconds = None;
            agent.report = None;
            agent.updated_at = now();
            agent.completed_at = None;
            agent.error = None;
            let agent = agent.clone();
            persist_agents(&self.path, &agents)?;
            return Ok(agent);
        }
        let timestamp = now();
        let agent = RuntimeAgent {
            id,
            parent_id: None,
            task_id,
            label: Some("root".to_owned()),
            background: false,
            workspace,
            root_workspace: None,
            worktree_branch: None,
            dedicated_worktree: false,
            worktree_merged_review_id: None,
            worktree_merged_child_snapshot_id: None,
            worktree_merged_at: None,
            worktree_quarantined_at: None,
            profile,
            model,
            status,
            current_turn: 0,
            current_tool: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            report: None,
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

    pub fn reserve_external_child(
        &self,
        id: uuid::Uuid,
        parent_id: uuid::Uuid,
        task_id: uuid::Uuid,
        profile: String,
        label: Option<String>,
    ) -> Result<RuntimeAgent> {
        let mut agents = self.lock()?;
        if agents.contains_key(&id) {
            bail!("Runtime Agent ID already exists");
        }
        let parent = agents
            .get(&parent_id)
            .filter(|agent| agent.parent_id.is_none() && agent.task_id == task_id)
            .context("active Runtime root agent not found")?
            .clone();
        let timestamp = now();
        let agent = RuntimeAgent {
            id,
            parent_id: Some(parent_id),
            task_id,
            label,
            background: true,
            workspace: parent.workspace.clone(),
            root_workspace: Some(parent.workspace),
            worktree_branch: None,
            dedicated_worktree: false,
            worktree_merged_review_id: None,
            worktree_merged_child_snapshot_id: None,
            worktree_merged_at: None,
            worktree_quarantined_at: None,
            profile: Some(profile),
            model: None,
            status: RuntimeAgentStatus::Queued,
            current_turn: 0,
            current_tool: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            report: None,
            created_at: timestamp,
            updated_at: timestamp,
            completed_at: None,
            error: None,
        };
        agents.insert(id, agent.clone());
        persist_agents(&self.path, &agents)?;
        Ok(agent)
    }

    pub fn reject_external_child(&self, id: uuid::Uuid, error: String) -> Result<()> {
        let mut agents = self.lock()?;
        let agent = agents.get_mut(&id).context("Runtime Agent not found")?;
        if !matches!(
            agent.status,
            RuntimeAgentStatus::Queued | RuntimeAgentStatus::Interrupted
        ) {
            return Ok(());
        }
        agent.status = RuntimeAgentStatus::Failed;
        agent.updated_at = now();
        agent.completed_at = Some(now());
        agent.error = Some(error);
        persist_agents(&self.path, &agents)
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

    pub fn mark_worktree_merged(
        &self,
        id: uuid::Uuid,
        review_id: String,
        child_snapshot_id: String,
    ) -> Result<()> {
        let mut agents = self.lock()?;
        let agent = agents.get_mut(&id).context("Runtime agent not found")?;
        agent.worktree_merged_review_id = Some(review_id);
        agent.worktree_merged_child_snapshot_id = Some(child_snapshot_id);
        agent.worktree_merged_at = Some(now());
        agent.updated_at = now();
        persist_agents(&self.path, &agents)
    }

    pub fn mark_worktree_quarantined(&self, id: uuid::Uuid, path: PathBuf) -> Result<()> {
        let mut agents = self.lock()?;
        let agent = agents.get_mut(&id).context("Runtime agent not found")?;
        agent.workspace = path;
        agent.worktree_quarantined_at = Some(now());
        agent.updated_at = now();
        persist_agents(&self.path, &agents)
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
                accumulate_usage(agent, &value);
                agent.updated_at = now();
            }),
            Some("subagent_started") => self.create_child_from_event(task_id, &value),
            Some("subagent_completed") => self.complete_child_from_event(&value),
            Some("subagent_turn_started") => self.update_child_from_event(&value, |agent| {
                agent.status = RuntimeAgentStatus::Running;
                agent.current_turn = value
                    .get("turn")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(agent.current_turn);
                agent.updated_at = now();
            }),
            Some("subagent_tool_requested") => self.update_child_from_event(&value, |agent| {
                agent.current_tool = value
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned);
                agent.updated_at = now();
            }),
            Some("subagent_tool_completed") => self.update_child_from_event(&value, |agent| {
                agent.current_tool = None;
                agent.updated_at = now();
            }),
            Some("subagent_usage") => self.update_child_from_event(&value, |agent| {
                accumulate_usage(agent, &value);
                agent.updated_at = now();
            }),
            _ => Ok(()),
        }
    }

    fn create_child_from_event(
        &self,
        task_id: uuid::Uuid,
        value: &serde_json::Value,
    ) -> Result<()> {
        let id = value
            .get("id")
            .and_then(|value| value.as_str())
            .context("subagent_started missing id")?
            .parse::<uuid::Uuid>()
            .context("subagent_started has invalid id")?;
        let mut agents = self.lock()?;
        if let Some(agent) = agents.get_mut(&id) {
            agent.profile = value
                .get("profile")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            agent.model = value
                .get("model")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            agent.label = value
                .get("label")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            agent.background = value
                .get("background")
                .and_then(|value| value.as_bool())
                .unwrap_or(agent.background);
            agent.workspace = event_path(value, "workspace").unwrap_or(agent.workspace.clone());
            agent.root_workspace = event_path(value, "root_workspace");
            agent.worktree_branch = value
                .get("worktree_branch")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned);
            agent.dedicated_worktree = value
                .get("dedicated_worktree")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            agent.worktree_merged_review_id = None;
            agent.worktree_merged_child_snapshot_id = None;
            agent.worktree_merged_at = None;
            agent.worktree_quarantined_at = None;
            agent.status = RuntimeAgentStatus::Running;
            agent.current_turn = 0;
            agent.current_tool = None;
            agent.max_turns = value.get("max_turns").and_then(|value| value.as_u64());
            agent.token_budget = value.get("token_budget").and_then(|value| value.as_u64());
            agent.timeout_seconds = value
                .get("timeout_seconds")
                .and_then(|value| value.as_u64());
            agent.report = None;
            agent.updated_at = now();
            agent.completed_at = None;
            agent.error = None;
            return persist_agents(&self.path, &agents);
        }
        let parent = agents
            .values()
            .find(|agent| agent.task_id == task_id && agent.parent_id.is_none())
            .context("Runtime root agent not found for subagent")?
            .clone();
        let timestamp = now();
        agents.insert(
            id,
            RuntimeAgent {
                id,
                parent_id: Some(parent.id),
                task_id,
                label: value
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                background: value
                    .get("background")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                workspace: event_path(value, "workspace")
                    .unwrap_or_else(|| parent.workspace.clone()),
                root_workspace: event_path(value, "root_workspace").or(Some(parent.workspace)),
                worktree_branch: value
                    .get("worktree_branch")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                dedicated_worktree: value
                    .get("dedicated_worktree")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                worktree_merged_review_id: None,
                worktree_merged_child_snapshot_id: None,
                worktree_merged_at: None,
                worktree_quarantined_at: None,
                profile: value
                    .get("profile")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                model: value
                    .get("model")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                status: RuntimeAgentStatus::Running,
                current_turn: 0,
                current_tool: None,
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
                max_turns: value.get("max_turns").and_then(|value| value.as_u64()),
                token_budget: value.get("token_budget").and_then(|value| value.as_u64()),
                timeout_seconds: value
                    .get("timeout_seconds")
                    .and_then(|value| value.as_u64()),
                report: None,
                created_at: timestamp,
                updated_at: timestamp,
                completed_at: None,
                error: None,
            },
        );
        persist_agents(&self.path, &agents)
    }

    fn complete_child_from_event(&self, value: &serde_json::Value) -> Result<()> {
        let id = value
            .get("id")
            .and_then(|value| value.as_str())
            .context("subagent_completed missing id")?
            .parse::<uuid::Uuid>()
            .context("subagent_completed has invalid id")?;
        let mut agents = self.lock()?;
        let Some(agent) = agents.get_mut(&id) else {
            return Ok(());
        };
        agent.status = match value.get("status").and_then(|value| value.as_str()) {
            Some("completed") => RuntimeAgentStatus::Completed,
            Some("blocked") => RuntimeAgentStatus::Blocked,
            Some("cancelled") => RuntimeAgentStatus::Cancelled,
            _ => RuntimeAgentStatus::Failed,
        };
        agent.updated_at = now();
        agent.completed_at = Some(now());
        agent.report = value
            .get("report")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
        persist_agents(&self.path, &agents)
    }

    fn update_child_from_event(
        &self,
        value: &serde_json::Value,
        update: impl FnOnce(&mut RuntimeAgent),
    ) -> Result<()> {
        let id = value
            .get("id")
            .and_then(|value| value.as_str())
            .context("subagent event missing id")?
            .parse::<uuid::Uuid>()
            .context("subagent event has invalid id")?;
        let mut agents = self.lock()?;
        let Some(agent) = agents.get_mut(&id) else {
            return Ok(());
        };
        update(agent);
        persist_agents(&self.path, &agents)
    }

    fn update_task_agent(
        &self,
        task_id: uuid::Uuid,
        update: impl FnOnce(&mut RuntimeAgent),
    ) -> Result<()> {
        let mut agents = self.lock()?;
        let Some(agent) = agents
            .values_mut()
            .find(|agent| agent.task_id == task_id && agent.parent_id.is_none())
        else {
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

fn accumulate_usage(agent: &mut RuntimeAgent, value: &serde_json::Value) {
    let input = value.get("input_tokens").and_then(|value| value.as_u64());
    let output = value.get("output_tokens").and_then(|value| value.as_u64());
    let total = value
        .get("total_tokens")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            (input.is_some() || output.is_some()).then(|| {
                input
                    .unwrap_or_default()
                    .saturating_add(output.unwrap_or_default())
            })
        });
    agent.input_tokens = saturating_optional_add(agent.input_tokens, input);
    agent.output_tokens = saturating_optional_add(agent.output_tokens, output);
    agent.total_tokens = saturating_optional_add(agent.total_tokens, total);
}

fn saturating_optional_add(current: Option<u64>, increment: Option<u64>) -> Option<u64> {
    increment.map_or(current, |increment| {
        Some(current.unwrap_or_default().saturating_add(increment))
    })
}

fn event_path(value: &serde_json::Value, field: &str) -> Option<PathBuf> {
    value
        .get(field)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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
