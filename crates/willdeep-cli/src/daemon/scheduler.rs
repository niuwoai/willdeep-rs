//! 定时任务的触发端（`willdeep-scheduler` 内置插件的宿主能力）。
//!
//! 插件的 MCP 进程只活一轮，没法自己计时；计时归常驻的 daemon。每
//! [`TICK`] 看一次 `$WILLDEEP_HOME/schedules.json`，到期的任务各开一个**全新**
//! Runtime 会话、把任务的 prompt 当第一句发出去（与 Xedit 的 Automations 一致）。
//!
//! 能力仍由插件开关控制：插件没启用，调度器什么都不做——停用插件就等于
//! 停掉所有定时任务，不必逐个删。

use std::time::Duration;

use anyhow::{Context, Result};
use willdeep_core::schedule::{ScheduleStore, ScheduledTask};

use super::{ServerState, session_store, workspace_store};

/// 内置定时任务插件的 id，见 `builtin_plugins::PACKAGES`。
pub(super) const SCHEDULER_PLUGIN_ID: &str = "willdeep-scheduler";

/// 调度粒度。任务最短间隔是 1 分钟，30 秒的检查足够准时。
pub(super) const TICK: Duration = Duration::from_secs(30);

/// daemon 启动后常驻：每 [`TICK`] 触发一次到期任务。IO 都是同步的小文件读写，
/// 放到阻塞线程池里做，不占事件循环。
pub(super) fn spawn(state: std::sync::Arc<ServerState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let state = state.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let now = now();
                let offset = willdeep_core::schedule::local_utc_offset(now);
                if let Err(error) = fire_due(&state, now, offset) {
                    let _ = state
                        .events
                        .append("schedule.error", format!("error={error:#}"));
                }
            })
            .await;
        }
    });
}

/// 触发 `now` 时刻所有到期的任务，返回触发的个数。
pub(super) fn fire_due(state: &ServerState, now: u64, utc_offset: i64) -> Result<usize> {
    let registry = willdeep_core::plugin::registry::PluginRegistry::load(
        &willdeep_core::plugin::registry::PluginRegistry::default_path(&state.home),
    )
    .context("read plugin registry")?;
    if !registry.is_enabled(SCHEDULER_PLUGIN_ID) {
        return Ok(0);
    }
    let store = ScheduleStore::new(&state.home);
    let due = store
        .list()
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .filter(|task| task.is_due(now, utc_offset))
        .collect::<Vec<_>>();
    let mut fired = 0;
    for task in due {
        // 上一次触发的会话还在跑：这一次跳过，免得同一个任务越堆越多。
        let still_running = task.last_session_id.is_some_and(|id| {
            state.sessions.list_turns(id).is_ok_and(|turns| {
                turns
                    .iter()
                    .any(|turn| !session_store::turn_status_is_terminal(turn.status))
            })
        });
        let outcome = if still_running {
            let _ = state.events.append(
                "schedule.skipped",
                format!("task_id={} reason=previous_run_active", task.id),
            );
            None
        } else {
            match fire(state, &task) {
                Ok(session_id) => {
                    fired += 1;
                    Some(session_id)
                }
                Err(error) => {
                    let _ = state.events.append(
                        "schedule.failed",
                        format!("task_id={} error={error:#}", task.id),
                    );
                    None
                }
            }
        };
        // 无论触发、跳过还是失败都记下这一刻：错过的与坏掉的任务都不会在
        // 下一个 tick 立刻再来一遍。
        store
            .update(|tasks| {
                if let Some(stored) = tasks.iter_mut().find(|stored| stored.id == task.id) {
                    stored.last_run_at = Some(now);
                    if let Some(session_id) = outcome {
                        stored.last_session_id = Some(session_id);
                    }
                }
            })
            .map_err(anyhow::Error::msg)?;
    }
    Ok(fired)
}

/// 开一个新会话，按任务的档位设好审批，把 prompt 排进去。
fn fire(state: &ServerState, task: &ScheduledTask) -> Result<uuid::Uuid> {
    let workspace = state
        .workspaces
        .ensure_registered(&task.workspace)
        .with_context(|| format!("register workspace {}", task.workspace.display()))?;
    let (session, created) = state.sessions.ensure(session_store::CreateRuntimeSession {
        id: None,
        workspace: workspace.root,
        profile: workspace.provider_profile,
        model: None,
        config: None,
        title: Some(format!("⏰ {}", task.name)),
    })?;
    if created {
        state.events.append(
            "session.created",
            format!(
                "session_id={} agent_id={}",
                session.id, session.root_agent_id
            ),
        )?;
    }
    if let Some(mode) = task
        .approval_mode
        .as_deref()
        .and_then(willdeep_core::ApprovalMode::parse)
    {
        let access = match mode {
            willdeep_core::ApprovalMode::ReadOnly => workspace_store::WorkspaceAccess::ReadOnly,
            willdeep_core::ApprovalMode::Strict => workspace_store::WorkspaceAccess::Strict,
            willdeep_core::ApprovalMode::Smart => workspace_store::WorkspaceAccess::Smart,
            willdeep_core::ApprovalMode::WorkspaceAccess => {
                workspace_store::WorkspaceAccess::WorkspaceWrite
            }
            willdeep_core::ApprovalMode::FullAccess => workspace_store::WorkspaceAccess::FullAccess,
        };
        state.sessions.update_approval_mode(session.id, access)?;
    }
    let (turn, _, _) = state.sessions.enqueue_turn_observed(
        session.id,
        session_store::CreateRuntimeTurn {
            request_id: uuid::Uuid::new_v4(),
            prompt: task.run_prompt(),
            attachments: Vec::new(),
            origin_client: Some(format!("schedule:{}", task.id)),
        },
    )?;
    state.events.append(
        "turn.queued",
        format!(
            "session_id={} agent_id={} turn_id={}",
            session.id, session.root_agent_id, turn.id
        ),
    )?;
    state.events.append(
        "schedule.fired",
        format!(
            "task_id={} session_id={} turn_id={}",
            task.id, session.id, turn.id
        ),
    )?;
    // 轮次已经落盘排队；叫醒调度通道失败也不算这次触发失败——它会被
    // 下一次调度（daemon 重启恢复排队轮次时）领走。
    if let Err(error) = state.tasks.schedule_session(session.id) {
        let _ = state.events.append(
            "schedule.warning",
            format!(
                "task_id={} session_id={} error={error:#}",
                task.id, session.id
            ),
        );
    }
    Ok(session.id)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}
