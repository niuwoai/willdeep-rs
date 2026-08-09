use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{broadcast, watch};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    Shell,
    Subagent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
    Killed,
    TimedOut,
    LaunchFailed,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackgroundTaskSnapshot {
    pub id: String,
    pub kind: BackgroundTaskKind,
    pub label: String,
    pub status: BackgroundTaskStatus,
    pub elapsed_millis: u64,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct BackgroundTaskEvent {
    pub snapshot: BackgroundTaskSnapshot,
    pub notice: String,
}

struct TaskRecord {
    snapshot: BackgroundTaskSnapshot,
    started: Instant,
    output: String,
    cancel: watch::Sender<bool>,
}

struct RegistryState {
    tasks: Vec<TaskRecord>,
    pending: VecDeque<BackgroundTaskEvent>,
}

#[derive(Clone)]
pub struct BackgroundTaskRegistry {
    inner: Arc<Mutex<RegistryState>>,
    events: broadcast::Sender<BackgroundTaskEvent>,
}

impl Default for BackgroundTaskRegistry {
    fn default() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Mutex::new(RegistryState {
                tasks: Vec::new(),
                pending: VecDeque::new(),
            })),
            events,
        }
    }
}

impl BackgroundTaskRegistry {
    pub fn subscribe(&self) -> broadcast::Receiver<BackgroundTaskEvent> {
        self.events.subscribe()
    }

    pub fn snapshots(&self) -> Vec<BackgroundTaskSnapshot> {
        self.inner
            .lock()
            .expect("background registry")
            .tasks
            .iter()
            .rev()
            .take(50)
            .map(|task| {
                let mut value = task.snapshot.clone();
                if value.status == BackgroundTaskStatus::Running {
                    value.elapsed_millis = task.started.elapsed().as_millis() as u64;
                }
                value
            })
            .collect()
    }

    pub fn drain_pending(&self) -> Vec<BackgroundTaskEvent> {
        self.inner
            .lock()
            .expect("background registry")
            .pending
            .drain(..)
            .collect()
    }

    pub fn output(&self, id: &str, tail_lines: usize) -> Option<String> {
        let state = self.inner.lock().expect("background registry");
        let output = &state
            .tasks
            .iter()
            .find(|task| task.snapshot.id == id)?
            .output;
        let lines = output.lines().collect::<Vec<_>>();
        Some(lines[lines.len().saturating_sub(tail_lines.max(1))..].join("\n"))
    }

    pub fn kill(&self, id: &str) -> bool {
        let state = self.inner.lock().expect("background registry");
        state
            .tasks
            .iter()
            .find(|task| {
                task.snapshot.id == id && task.snapshot.status == BackgroundTaskStatus::Running
            })
            .is_some_and(|task| task.cancel.send(true).is_ok())
    }

    /// Internal lifecycle entry. Callers must complete their own approval
    /// before registering work; no command-launch API is publicly exported.
    pub(crate) fn start<F>(&self, kind: BackgroundTaskKind, label: String, future: F) -> String
    where
        F: std::future::Future<Output = TaskResult> + Send + 'static,
    {
        let prefix = if kind == BackgroundTaskKind::Shell {
            "job"
        } else {
            "agent"
        };
        let id = format!(
            "{prefix}_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        );
        let (cancel, mut cancelled) = watch::channel(false);
        self.inner
            .lock()
            .expect("background registry")
            .tasks
            .push(TaskRecord {
                snapshot: BackgroundTaskSnapshot {
                    id: id.clone(),
                    kind,
                    label,
                    status: BackgroundTaskStatus::Running,
                    elapsed_millis: 0,
                    exit_code: None,
                    output_bytes: 0,
                },
                started: Instant::now(),
                output: String::new(),
                cancel,
            });
        let registry = self.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                result = future => result,
                _ = cancelled.changed() => TaskResult { status: BackgroundTaskStatus::Killed, exit_code: None, output: "task cancelled".to_owned() },
            };
            registry.finish(&task_id, result);
        });
        id
    }

    fn finish(&self, id: &str, result: TaskResult) {
        let event = {
            let mut state = self.inner.lock().expect("background registry");
            let Some(task) = state.tasks.iter_mut().find(|task| task.snapshot.id == id) else {
                return;
            };
            task.output = truncate(result.output);
            task.snapshot.status = result.status;
            task.snapshot.exit_code = result.exit_code;
            task.snapshot.elapsed_millis = task.started.elapsed().as_millis() as u64;
            task.snapshot.output_bytes = task.output.len();
            let snapshot = task.snapshot.clone();
            BackgroundTaskEvent {
                notice: completion_notice(&snapshot, &task.output),
                snapshot,
            }
        };
        self.inner
            .lock()
            .expect("background registry")
            .pending
            .push_back(event.clone());
        let _ = self.events.send(event);
    }
}

pub(crate) struct TaskResult {
    pub status: BackgroundTaskStatus,
    pub exit_code: Option<i32>,
    pub output: String,
}

fn completion_notice(task: &BackgroundTaskSnapshot, output: &str) -> String {
    let tag = if task.kind == BackgroundTaskKind::Subagent {
        "subagent-report"
    } else {
        "background-task-notification"
    };
    let tail = output
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<{tag}>\n{} `{}` finished: status={:?}, exit={:?}, elapsed={}ms.\n{}\n</{tag}>",
        if task.kind == BackgroundTaskKind::Subagent {
            "Subagent"
        } else {
            "Background task"
        },
        task.id,
        task.status,
        task.exit_code,
        task.elapsed_millis,
        tail
    )
}

fn truncate(value: String) -> String {
    if value.len() <= MAX_OUTPUT_BYTES {
        return value;
    }
    let mut boundary = MAX_OUTPUT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[output truncated]", &value[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_task_publishes_notice_and_output() {
        let registry = BackgroundTaskRegistry::default();
        let mut events = registry.subscribe();
        let id = registry.start(BackgroundTaskKind::Subagent, "scout".to_owned(), async {
            TaskResult {
                status: BackgroundTaskStatus::Completed,
                exit_code: Some(0),
                output: "found src/main.rs".to_owned(),
            }
        });

        let event = events.recv().await.expect("event");
        assert_eq!(event.snapshot.id, id);
        assert!(event.notice.contains("<subagent-report>"));
        assert_eq!(
            registry.output(&id, 20).as_deref(),
            Some("found src/main.rs")
        );
        assert_eq!(registry.drain_pending().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_moves_task_to_killed() {
        let registry = BackgroundTaskRegistry::default();
        let mut events = registry.subscribe();
        let id = registry.start(
            BackgroundTaskKind::Shell,
            "long command".to_owned(),
            async { std::future::pending::<TaskResult>().await },
        );
        assert!(registry.kill(&id));
        let event = events.recv().await.expect("event");
        assert_eq!(event.snapshot.status, BackgroundTaskStatus::Killed);
    }
}
