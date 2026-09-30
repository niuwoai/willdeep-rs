//! `willdeep-scheduler` 内置插件：`schedule_task` / `complete_scheduled_task` /
//! `list_scheduled_tasks`。
//!
//! 语义对齐 Xedit（`AgentToolsExpertsAndSchedules.swift`、`BundledSkills/scheduler`）：
//! 建任务不会立刻跑任何东西；到点由 daemon 的调度器开一个全新会话、把 prompt
//! 当第一句发出去。任务写进 `$WILLDEEP_HOME/schedules.json`，调度器读同一份。

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use willdeep_core::schedule::{Schedule, ScheduleStore, ScheduledTask, parse_hhmm};

use super::stdio_server::{BuiltinPlugin, HostClient, string_arg};

pub(crate) struct Scheduler {
    home: PathBuf,
    /// 没给 `workspace` 时的默认工作区：插件进程的当前目录。
    default_workspace: Option<PathBuf>,
}

impl Scheduler {
    pub(crate) fn new(home: PathBuf, default_workspace: Option<PathBuf>) -> Self {
        Self {
            home,
            default_workspace,
        }
    }

    fn store(&self) -> ScheduleStore {
        ScheduleStore::new(&self.home)
    }

    fn schedule_task(&self, arguments: &Value) -> Result<String, String> {
        let prompt = string_arg(arguments, "prompt").ok_or("prompt is required")?;
        let schedule = parse_schedule(arguments)?;
        schedule.validate()?;
        let goal = string_arg(arguments, "goal");
        let approval_mode = match string_arg(arguments, "approval_mode") {
            Some(mode) => Some(
                willdeep_core::ApprovalMode::parse(&mode)
                    .filter(|mode| *mode != willdeep_core::ApprovalMode::ReadOnly)
                    .ok_or_else(|| {
                        format!(
                            "approval_mode must be one of strict, smart, workspace-write, full-access (got {mode})"
                        )
                    })?
                    .as_str()
                    .to_owned(),
            ),
            None => None,
        };
        let workspace = match string_arg(arguments, "workspace") {
            Some(path) => PathBuf::from(path),
            None => self
                .default_workspace
                .clone()
                .ok_or("workspace is required: pass the absolute path of the project to run in")?,
        };
        if !workspace.is_absolute() || !workspace.is_dir() {
            return Err(format!(
                "workspace must be an existing absolute directory: {}",
                workspace.display()
            ));
        }
        let name = string_arg(arguments, "title")
            .or_else(|| goal.clone())
            .unwrap_or_else(|| prompt.clone())
            .chars()
            .take(48)
            .collect::<String>();
        let task = ScheduledTask {
            id: uuid::Uuid::new_v4(),
            name: name.clone(),
            prompt: prompt.clone(),
            workspace: workspace.clone(),
            schedule,
            goal: goal.clone(),
            approval_mode: approval_mode.clone(),
            enabled: true,
            created_at: now(),
            last_run_at: None,
            last_session_id: None,
            origin_session_id: None,
        };
        let id = task.id;
        self.store().update(|tasks| tasks.push(task))?;
        let goal_line = goal
            .map(|goal| format!("\nGoal (the task removes itself once met): {goal}"))
            .unwrap_or_default();
        Ok(format!(
            "Scheduled task \"{name}\" created (task_id: {id}); it will run {} in {} with approval mode {}. \
Each run starts a fresh session whose first message is this prompt: {prompt}{goal_line}\n\
Runs are fired by the WillDeep runtime daemon; nothing runs right now.",
            schedule.summary(),
            workspace.display(),
            approval_mode.as_deref().unwrap_or("of the workspace")
        ))
    }

    fn complete_scheduled_task(&self, arguments: &Value) -> Result<String, String> {
        let raw = string_arg(arguments, "task_id").ok_or("task_id is required")?;
        let id = uuid::Uuid::parse_str(&raw).map_err(|_| {
            "task_id must be a scheduled-task id returned by schedule_task".to_owned()
        })?;
        let reason = string_arg(arguments, "reason")
            .map(|reason| format!(" Reason: {reason}"))
            .unwrap_or_default();
        let removed = self.store().update(|tasks| {
            let before = tasks.len();
            tasks.retain(|task| task.id != id);
            before != tasks.len()
        })?;
        Ok(if removed {
            format!("Scheduled task {id} marked complete and removed.{reason}")
        } else {
            format!("No scheduled task with id {id} (it may already be removed).{reason}")
        })
    }

    fn list_scheduled_tasks(&self) -> Result<String, String> {
        let tasks = self.store().list()?;
        if tasks.is_empty() {
            return Ok("No scheduled tasks.".to_owned());
        }
        let now = now();
        let offset = willdeep_core::schedule::local_utc_offset(now);
        Ok(tasks
            .iter()
            .map(|task| {
                let next = task
                    .next_fire(offset)
                    .map(|at| {
                        willdeep_core::format_iso8601((at as i64 + offset).max(0) as u64)
                            .trim_end_matches('Z')
                            .replace('T', " ")
                    })
                    .unwrap_or_else(|| "disabled".to_owned());
                format!(
                    "- {} · {} · {} · next {} (local) · workspace {}{}{}",
                    task.id,
                    task.name,
                    task.schedule.summary(),
                    next,
                    task.workspace.display(),
                    task.goal
                        .as_deref()
                        .map(|goal| format!(" · goal: {goal}"))
                        .unwrap_or_default(),
                    task.last_session_id
                        .map(|id| format!(" · last run session {id}"))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn parse_schedule(arguments: &Value) -> Result<Schedule, String> {
    let time = |key: &str| -> Result<Option<(u8, u8)>, String> {
        string_arg(arguments, key)
            .map(|raw| parse_hhmm(&raw).ok_or(format!("{key} must be HH:MM (24-hour local time)")))
            .transpose()
    };
    let interval = string_arg(arguments, "interval_minutes")
        .map(|raw| {
            raw.parse::<u32>()
                .ok()
                .filter(|minutes| *minutes > 0)
                .ok_or("interval_minutes must be a positive whole number of minutes".to_owned())
        })
        .transpose()?;
    let daily = time("daily_at")?;
    let weekdays = time("weekdays_at")?;
    let weekly = time("weekly_at")?;
    let chosen = [
        interval.is_some(),
        daily.is_some(),
        weekdays.is_some(),
        weekly.is_some(),
    ]
    .into_iter()
    .filter(|given| *given)
    .count();
    if chosen != 1 {
        return Err(
            "give exactly one schedule: interval_minutes (e.g. \"30\"), daily_at (\"HH:MM\"), weekdays_at (\"HH:MM\"), or weekly_at (\"HH:MM\") with weekday (1 = Sunday … 7 = Saturday)"
                .to_owned(),
        );
    }
    Ok(if let Some(minutes) = interval {
        Schedule::Interval { minutes }
    } else if let Some((hour, minute)) = daily {
        Schedule::DailyAt { hour, minute }
    } else if let Some((hour, minute)) = weekdays {
        Schedule::WeekdaysAt { hour, minute }
    } else {
        let (hour, minute) = weekly.expect("one schedule was given");
        let weekday = string_arg(arguments, "weekday")
            .and_then(|raw| raw.parse::<u8>().ok())
            .ok_or("weekly_at needs weekday (1 = Sunday … 7 = Saturday)")?;
        Schedule::WeeklyAt {
            weekday,
            hour,
            minute,
        }
    })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[async_trait]
impl BuiltinPlugin for Scheduler {
    fn server_name(&self) -> &'static str {
        "scheduler"
    }

    fn tools(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "schedule_task",
                "description": "Schedule a recurring task that runs on its own later, each time in a fresh session started by the WillDeep runtime daemon. Use when the user asks for something to happen repeatedly or to be watched over time (\"every 30 minutes check…\", \"every morning summarize…\", \"watch X until Y\"). The prompt must be self-contained: each run sees nothing but it. Give exactly one schedule. Optionally give a goal: each run then checks whether it is met and, if so, calls complete_scheduled_task to remove the task. This does NOT run anything now.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "description": "What each run should do: target, scope, expected output, safety constraints."},
                        "title": {"type": "string", "description": "Short display name. Defaults to the goal or prompt."},
                        "interval_minutes": {"type": "string", "description": "Run every N minutes, e.g. \"30\"."},
                        "daily_at": {"type": "string", "description": "Run every day at local HH:MM."},
                        "weekdays_at": {"type": "string", "description": "Run Monday–Friday at local HH:MM."},
                        "weekly_at": {"type": "string", "description": "Run once a week at local HH:MM; requires weekday."},
                        "weekday": {"type": "string", "description": "For weekly_at: 1 = Sunday … 7 = Saturday."},
                        "goal": {"type": "string", "description": "Optional completion condition, e.g. \"CI on main is green\"."},
                        "approval_mode": {"type": "string", "enum": ["strict", "smart", "workspace-write", "full-access"], "description": "Approval posture for the unattended runs. Defaults to the workspace's mode; pending approvals wait in the inbox."},
                        "workspace": {"type": "string", "description": "Absolute project path the runs work in. Defaults to the current workspace."}
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "complete_scheduled_task",
                "description": "Remove a scheduled task. A goal-bearing scheduled run calls this with its own task_id once the goal is met; it also cancels a task on the user's request.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"},
                        "reason": {"type": "string"}
                    },
                    "required": ["task_id"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "list_scheduled_tasks",
                "description": "List scheduled tasks with their schedule, next local run time, workspace, goal and last run session. Read-only.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "annotations": {"readOnlyHint": true}
            }),
        ]
    }

    async fn call(
        &self,
        name: &str,
        arguments: Value,
        _host: &HostClient,
    ) -> Result<String, String> {
        match name {
            "schedule_task" => self.schedule_task(&arguments),
            "complete_scheduled_task" => self.complete_scheduled_task(&arguments),
            "list_scheduled_tasks" => self.list_scheduled_tasks(),
            other => Err(format!("unknown tool {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler() -> (Scheduler, PathBuf) {
        let home =
            std::env::temp_dir().join(format!("willdeep-scheduler-{}", uuid::Uuid::new_v4()));
        let workspace = home.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        (Scheduler::new(home.clone(), Some(workspace)), home)
    }

    #[test]
    fn schedule_list_and_complete_like_xedit() {
        let (scheduler, home) = scheduler();
        let created = scheduler
            .schedule_task(&json!({
                "prompt": "Check whether CI on main is green",
                "interval_minutes": "30",
                "goal": "CI on main is green"
            }))
            .unwrap();
        assert!(created.contains("every 30 minutes"), "{created}");
        let tasks = ScheduleStore::new(&home).list().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "CI on main is green");
        assert_eq!(tasks[0].schedule, Schedule::Interval { minutes: 30 });
        assert!(created.contains(&tasks[0].id.to_string()));

        let listed = scheduler.list_scheduled_tasks().unwrap();
        assert!(
            listed.contains("every 30 minutes") && listed.contains("goal: CI on main is green")
        );

        let done = scheduler
            .complete_scheduled_task(
                &json!({"task_id": tasks[0].id.to_string(), "reason": "green"}),
            )
            .unwrap();
        assert!(done.contains("removed"));
        assert!(
            scheduler
                .complete_scheduled_task(&json!({"task_id": tasks[0].id.to_string()}))
                .unwrap()
                .contains("already be removed")
        );
        assert_eq!(
            scheduler.list_scheduled_tasks().unwrap(),
            "No scheduled tasks."
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn schedules_are_validated() {
        let (scheduler, home) = scheduler();
        for (arguments, needle) in [
            (json!({"prompt": "x"}), "exactly one schedule"),
            (
                json!({"prompt": "x", "interval_minutes": "5", "daily_at": "09:00"}),
                "exactly one schedule",
            ),
            (json!({"prompt": "x", "daily_at": "25:00"}), "HH:MM"),
            (json!({"prompt": "x", "interval_minutes": "0"}), "positive"),
            (json!({"prompt": "x", "weekly_at": "09:00"}), "weekday"),
            (json!({"interval_minutes": "5"}), "prompt is required"),
            (
                json!({"prompt": "x", "daily_at": "09:00", "approval_mode": "read-only"}),
                "approval_mode",
            ),
            (
                json!({"prompt": "x", "daily_at": "09:00", "workspace": "relative/path"}),
                "absolute",
            ),
        ] {
            let error = scheduler.schedule_task(&arguments).unwrap_err();
            assert!(error.contains(needle), "{arguments}: {error}");
        }
        let weekly = scheduler
            .schedule_task(&json!({"prompt": "x", "weekly_at": "09:30", "weekday": "2", "approval_mode": "full-access"}))
            .unwrap();
        assert!(
            weekly.contains("every Monday at 09:30") && weekly.contains("full-access"),
            "{weekly}"
        );
        assert!(ScheduleStore::new(&home).path().exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
