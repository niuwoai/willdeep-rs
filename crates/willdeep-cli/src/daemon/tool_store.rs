use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use willdeep_runtime_protocol::{ListToolsParams, RuntimeTool, ToolStatus};

const MAX_TOOL_RECORDS: usize = 20_000;
const MAX_TOOL_NAME_CHARS: usize = 160;

pub(super) struct ToolStore {
    path: PathBuf,
    records: Mutex<Vec<RuntimeTool>>,
    active: Mutex<HashMap<String, uuid::Uuid>>,
    recovered_after_restart: Mutex<Vec<RuntimeTool>>,
}

pub(super) struct StartTool {
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub correlation: String,
    pub name: String,
}

pub(super) fn observe(
    store: &ToolStore,
    session_id: Option<uuid::Uuid>,
    turn_id: Option<uuid::Uuid>,
    task_id: uuid::Uuid,
    root_agent_id: uuid::Uuid,
    event: &willdeep_core::AgentEvent,
) -> Result<()> {
    use willdeep_core::AgentEvent;
    match event {
        AgentEvent::ToolRequested(call) => {
            store.start(StartTool {
                session_id,
                turn_id,
                task_id,
                agent_id: root_agent_id,
                correlation: format!("root:{}", call.id),
                name: call.name.clone(),
            })?;
        }
        AgentEvent::ToolCompleted { call, is_error, .. } => {
            store.finish(&format!("root:{}", call.id), *is_error)?;
        }
        AgentEvent::SubagentToolRequested { id, name } => {
            store.start(StartTool {
                session_id,
                turn_id,
                task_id,
                agent_id: *id,
                correlation: format!("child:{id}"),
                name: name.clone(),
            })?;
        }
        AgentEvent::SubagentToolCompleted { id, is_error, .. } => {
            store.finish(&format!("child:{id}"), *is_error)?;
        }
        AgentEvent::BackgroundShellStarted { id } => {
            store.start(StartTool {
                session_id,
                turn_id,
                task_id,
                agent_id: root_agent_id,
                correlation: format!("background:{id}"),
                name: format!("background_shell:{id}"),
            })?;
        }
        AgentEvent::BackgroundShellCompleted { id, status, .. } => {
            let status = match status {
                willdeep_core::BackgroundTaskStatus::Completed => ToolStatus::Completed,
                willdeep_core::BackgroundTaskStatus::Killed => ToolStatus::Interrupted,
                willdeep_core::BackgroundTaskStatus::Running => ToolStatus::Running,
                willdeep_core::BackgroundTaskStatus::Blocked
                | willdeep_core::BackgroundTaskStatus::Failed
                | willdeep_core::BackgroundTaskStatus::TimedOut
                | willdeep_core::BackgroundTaskStatus::LaunchFailed => ToolStatus::Failed,
            };
            store.finish_with_status(&format!("background:{id}"), status)?;
        }
        _ => {}
    }
    Ok(())
}

impl ToolStore {
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create Tool activity directory at {}", parent.display())
            })?;
        }
        let mut records = if path.exists() {
            serde_json::from_slice::<Vec<RuntimeTool>>(&std::fs::read(&path)?)?
        } else {
            Vec::new()
        };
        let completed_at = now_ms();
        let mut changed = false;
        let mut recovered_after_restart = Vec::new();
        for record in &mut records {
            if record.status == ToolStatus::Running {
                record.status = ToolStatus::Interrupted;
                record.completed_at_ms = Some(completed_at);
                recovered_after_restart.push(record.clone());
                changed = true;
            }
        }
        let store = Self {
            path,
            records: Mutex::new(records),
            active: Mutex::new(HashMap::new()),
            recovered_after_restart: Mutex::new(recovered_after_restart),
        };
        if changed {
            store.persist()?;
        }
        Ok(store)
    }

    pub(super) fn take_recovered_after_restart(&self) -> Result<Vec<RuntimeTool>> {
        let mut recovered = self
            .recovered_after_restart
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime recovered Tool index lock poisoned"))?;
        Ok(std::mem::take(&mut *recovered))
    }

    pub(super) fn start(&self, input: StartTool) -> Result<RuntimeTool> {
        let name = bounded_name(&input.name);
        let record = RuntimeTool {
            id: uuid::Uuid::new_v4(),
            session_id: input.session_id,
            turn_id: input.turn_id,
            task_id: input.task_id,
            agent_id: input.agent_id,
            name,
            status: ToolStatus::Running,
            started_at_ms: now_ms(),
            completed_at_ms: None,
        };
        self.active
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity index lock poisoned"))?
            .insert(input.correlation, record.id);
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity store lock poisoned"))?;
        records.push(record.clone());
        if records.len() > MAX_TOOL_RECORDS {
            let excess = records.len() - MAX_TOOL_RECORDS;
            records.drain(..excess);
        }
        persist_records(&self.path, &records)?;
        Ok(record)
    }

    pub(super) fn finish(&self, correlation: &str, failed: bool) -> Result<Option<RuntimeTool>> {
        self.finish_with_status(
            correlation,
            if failed {
                ToolStatus::Failed
            } else {
                ToolStatus::Completed
            },
        )
    }

    fn finish_with_status(
        &self,
        correlation: &str,
        status: ToolStatus,
    ) -> Result<Option<RuntimeTool>> {
        let Some(id) = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity index lock poisoned"))?
            .remove(correlation)
        else {
            return Ok(None);
        };
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity store lock poisoned"))?;
        let Some(record) = records.iter_mut().find(|record| record.id == id) else {
            return Ok(None);
        };
        record.status = status;
        record.completed_at_ms = Some(now_ms());
        let result = record.clone();
        persist_records(&self.path, &records)?;
        Ok(Some(result))
    }

    pub(super) fn get(&self, id: uuid::Uuid) -> Result<Option<RuntimeTool>> {
        Ok(self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity store lock poisoned"))?
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    pub(super) fn list(&self, params: ListToolsParams) -> Result<Vec<RuntimeTool>> {
        let limit = params.limit.unwrap_or(200).clamp(1, 1_000);
        let mut records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity store lock poisoned"))?
            .iter()
            .filter(|record| {
                params
                    .session_id
                    .is_none_or(|id| record.session_id == Some(id))
            })
            .filter(|record| params.turn_id.is_none_or(|id| record.turn_id == Some(id)))
            .filter(|record| params.task_id.is_none_or(|id| record.task_id == id))
            .filter(|record| params.agent_id.is_none_or(|id| record.agent_id == id))
            .filter(|record| params.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse(record.started_at_ms));
        records.truncate(limit);
        Ok(records)
    }

    fn persist(&self) -> Result<()> {
        let records = self
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("Tool activity store lock poisoned"))?;
        persist_records(&self.path, &records)
    }
}

fn bounded_name(value: &str) -> String {
    let value = value.trim();
    let mut result = value.chars().take(MAX_TOOL_NAME_CHARS).collect::<String>();
    if value.chars().count() > MAX_TOOL_NAME_CHARS {
        result.push('…');
    }
    result
}

fn persist_records(path: &Path, records: &[RuntimeTool]) -> Result<()> {
    super::write_json_atomic(path, records)
        .with_context(|| format!("persist Tool activity at {}", path.display()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_filters_and_interrupts_running_tools_without_payloads() {
        let root = std::env::temp_dir().join(format!("willdeep-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("tools.json");
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let store = ToolStore::open(path.clone()).unwrap();
        let completed = store
            .start(StartTool {
                session_id: None,
                turn_id: None,
                task_id,
                agent_id,
                correlation: "root:one".to_owned(),
                name: "read_file".to_owned(),
            })
            .unwrap();
        store.finish("root:one", false).unwrap();
        store
            .start(StartTool {
                session_id: None,
                turn_id: None,
                task_id,
                agent_id,
                correlation: "root:two".to_owned(),
                name: "run_command".to_owned(),
            })
            .unwrap();
        let reopened = ToolStore::open(path.clone()).unwrap();
        assert_eq!(
            reopened.get(completed.id).unwrap().unwrap().status,
            ToolStatus::Completed
        );
        let records = reopened
            .list(ListToolsParams {
                task_id: Some(task_id),
                ..ListToolsParams::default()
            })
            .unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|record| record.status == ToolStatus::Interrupted)
        );
        let json = String::from_utf8(std::fs::read(path).unwrap()).unwrap();
        assert!(!json.contains("arguments"));
        assert!(!json.contains("output"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
