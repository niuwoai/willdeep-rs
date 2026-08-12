use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::agent::AgentInstructionInbox;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

type TaskFuture = Pin<Box<dyn Future<Output = TaskResult> + Send>>;
type TaskLauncher = Arc<dyn Fn() -> TaskFuture + Send + Sync>;
type TaskLifecycleFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type TaskLifecycleHook = Arc<dyn Fn(BackgroundTaskSnapshot) -> TaskLifecycleFuture + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    Shell,
    Subagent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Blocked,
    Completed,
    Failed,
    Killed,
    TimedOut,
    LaunchFailed,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackgroundTaskSnapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<uuid::Uuid>,
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
    retry: Option<TaskLauncher>,
    lifecycle: Option<TaskLifecycleHook>,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
}

struct RegistryState {
    tasks: Vec<TaskRecord>,
    pending: VecDeque<BackgroundTaskEvent>,
}

struct LaunchSpec {
    agent_id: Option<uuid::Uuid>,
    kind: BackgroundTaskKind,
    label: String,
    future: TaskFuture,
    retry: Option<TaskLauncher>,
    lifecycle: Option<TaskLifecycleHook>,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
}

#[derive(Clone)]
pub struct BackgroundTaskRegistry {
    inner: Arc<Mutex<RegistryState>>,
    events: broadcast::Sender<BackgroundTaskEvent>,
    lifecycle: Option<TaskLifecycleHook>,
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
            lifecycle: None,
        }
    }
}

impl BackgroundTaskRegistry {
    pub fn with_lifecycle_observer<H, HFut>(mut self, lifecycle: H) -> Self
    where
        H: Fn(BackgroundTaskSnapshot) -> HFut + Send + Sync + 'static,
        HFut: Future<Output = ()> + Send + 'static,
    {
        self.lifecycle = Some(Arc::new(move |snapshot| Box::pin(lifecycle(snapshot))));
        self
    }

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

    pub fn kill_agent(&self, agent_id: uuid::Uuid) -> bool {
        let state = self.inner.lock().expect("background registry");
        state
            .tasks
            .iter()
            .rev()
            .find(|task| {
                task.snapshot.agent_id == Some(agent_id)
                    && task.snapshot.status == BackgroundTaskStatus::Running
            })
            .is_some_and(|task| task.cancel.send(true).is_ok())
    }

    pub fn retry(&self, id: &str) -> Option<String> {
        let (agent_id, kind, label, launcher, lifecycle, instruction_inbox) = {
            let state = self.inner.lock().expect("background registry");
            let task = state.tasks.iter().find(|task| task.snapshot.id == id)?;
            if task.snapshot.status == BackgroundTaskStatus::Running {
                return None;
            }
            (
                task.snapshot.agent_id,
                task.snapshot.kind.clone(),
                task.snapshot.label.clone(),
                task.retry.clone()?,
                task.lifecycle.clone(),
                task.instruction_inbox.clone(),
            )
        };
        let future = launcher();
        Some(self.launch(LaunchSpec {
            agent_id,
            kind,
            label,
            future,
            retry: Some(launcher),
            lifecycle,
            instruction_inbox,
        }))
    }

    pub fn retry_agent(&self, agent_id: uuid::Uuid) -> Option<String> {
        let id = self
            .inner
            .lock()
            .expect("background registry")
            .tasks
            .iter()
            .rev()
            .find(|task| task.snapshot.agent_id == Some(agent_id))?
            .snapshot
            .id
            .clone();
        self.retry(&id)
    }

    pub fn instruct_agent(&self, agent_id: uuid::Uuid, instruction: String) -> bool {
        self.inner
            .lock()
            .expect("background registry")
            .tasks
            .iter()
            .rev()
            .find(|task| {
                task.snapshot.agent_id == Some(agent_id)
                    && task.snapshot.status == BackgroundTaskStatus::Running
            })
            .and_then(|task| task.instruction_inbox.as_ref())
            .is_some_and(|inbox| inbox.push(instruction))
    }

    /// Internal lifecycle entry. Callers must complete their own approval
    /// before registering work; no command-launch API is publicly exported.
    pub(crate) fn start_retriable<F, Fut>(
        &self,
        kind: BackgroundTaskKind,
        label: String,
        launcher: F,
    ) -> String
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TaskResult> + Send + 'static,
    {
        let launcher: TaskLauncher = Arc::new(move || Box::pin(launcher()));
        let future = launcher();
        self.launch(LaunchSpec {
            agent_id: None,
            kind,
            label,
            future,
            retry: Some(launcher),
            lifecycle: None,
            instruction_inbox: None,
        })
    }

    pub(crate) fn start_retriable_with_lifecycle<F, Fut, H, HFut>(
        &self,
        agent_id: uuid::Uuid,
        kind: BackgroundTaskKind,
        label: String,
        instruction_inbox: Arc<AgentInstructionInbox>,
        launcher: F,
        lifecycle: H,
    ) -> String
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TaskResult> + Send + 'static,
        H: Fn(BackgroundTaskSnapshot) -> HFut + Send + Sync + 'static,
        HFut: Future<Output = ()> + Send + 'static,
    {
        let launcher: TaskLauncher = Arc::new(move || Box::pin(launcher()));
        let lifecycle: TaskLifecycleHook = Arc::new(move |snapshot| Box::pin(lifecycle(snapshot)));
        let future = launcher();
        self.launch(LaunchSpec {
            agent_id: Some(agent_id),
            kind,
            label,
            future,
            retry: Some(launcher),
            lifecycle: Some(lifecycle),
            instruction_inbox: Some(instruction_inbox),
        })
    }

    fn launch(&self, spec: LaunchSpec) -> String {
        let LaunchSpec {
            agent_id,
            kind,
            label,
            future,
            retry,
            lifecycle,
            instruction_inbox,
        } = spec;
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
        let snapshot = BackgroundTaskSnapshot {
            id: id.clone(),
            agent_id,
            kind,
            label,
            status: BackgroundTaskStatus::Running,
            elapsed_millis: 0,
            exit_code: None,
            output_bytes: 0,
        };
        self.inner
            .lock()
            .expect("background registry")
            .tasks
            .push(TaskRecord {
                snapshot: snapshot.clone(),
                started: Instant::now(),
                output: String::new(),
                cancel,
                retry,
                lifecycle: lifecycle.clone(),
                instruction_inbox,
            });
        let registry = self.clone();
        let task_id = id.clone();
        let observer = self.lifecycle.clone();
        tokio::spawn(async move {
            if let Some(observer) = &observer {
                observer(snapshot.clone()).await;
            }
            if let Some(lifecycle) = &lifecycle {
                lifecycle(snapshot).await;
            }
            let result = tokio::select! {
                result = future => result,
                _ = cancelled.changed() => TaskResult { status: BackgroundTaskStatus::Killed, exit_code: None, output: "task cancelled".to_owned() },
            };
            if let Some(event) = registry.finish(&task_id, result) {
                if let Some(observer) = &observer {
                    observer(event.snapshot.clone()).await;
                }
                if let Some(lifecycle) = &lifecycle {
                    lifecycle(event.snapshot).await;
                }
            }
        });
        id
    }

    fn finish(&self, id: &str, result: TaskResult) -> Option<BackgroundTaskEvent> {
        let event = {
            let mut state = self.inner.lock().expect("background registry");
            let task = state.tasks.iter_mut().find(|task| task.snapshot.id == id)?;
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
        let _ = self.events.send(event.clone());
        Some(event)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn completed_task_publishes_notice_and_output() {
        let registry = BackgroundTaskRegistry::default();
        let mut events = registry.subscribe();
        let id =
            registry.start_retriable(BackgroundTaskKind::Subagent, "scout".to_owned(), || async {
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
        let id = registry.start_retriable(
            BackgroundTaskKind::Shell,
            "long command".to_owned(),
            || async { std::future::pending::<TaskResult>().await },
        );
        assert!(registry.kill(&id));
        let event = events.recv().await.expect("event");
        assert_eq!(event.snapshot.status, BackgroundTaskStatus::Killed);
    }

    #[tokio::test]
    async fn retry_replays_launcher_as_a_new_task() {
        let registry = BackgroundTaskRegistry::default();
        let mut events = registry.subscribe();
        let attempts = Arc::new(AtomicUsize::new(0));
        let launch_attempts = attempts.clone();
        let id = registry.start_retriable(
            BackgroundTaskKind::Shell,
            "flaky command".to_owned(),
            move || {
                let attempt = launch_attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    TaskResult {
                        status: if attempt == 0 {
                            BackgroundTaskStatus::Failed
                        } else {
                            BackgroundTaskStatus::Completed
                        },
                        exit_code: Some(if attempt == 0 { 1 } else { 0 }),
                        output: format!("attempt {attempt}"),
                    }
                }
            },
        );
        assert_eq!(
            events.recv().await.unwrap().snapshot.status,
            BackgroundTaskStatus::Failed
        );

        let retried = registry.retry(&id).expect("retriable task");
        assert_ne!(retried, id);
        assert_eq!(
            events.recv().await.unwrap().snapshot.status,
            BackgroundTaskStatus::Completed
        );
        assert_eq!(registry.output(&retried, 5).as_deref(), Some("attempt 1"));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn lifecycle_hook_observes_cancel_and_retry_transitions() {
        let registry = BackgroundTaskRegistry::default();
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancelled_agent_id = uuid::Uuid::new_v4();
        registry.start_retriable_with_lifecycle(
            cancelled_agent_id,
            BackgroundTaskKind::Subagent,
            "cancelled agent".to_owned(),
            Arc::new(AgentInstructionInbox::default()),
            || async { std::future::pending::<TaskResult>().await },
            move |snapshot| {
                let tx = lifecycle_tx.clone();
                async move {
                    let _ = tx.send(snapshot);
                }
            },
        );
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Running
        );
        assert_eq!(registry.snapshots()[0].agent_id, Some(cancelled_agent_id));
        assert!(registry.kill_agent(cancelled_agent_id));
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Killed
        );

        let registry = BackgroundTaskRegistry::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let launch_attempts = attempts.clone();
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::unbounded_channel();
        let retried_agent_id = uuid::Uuid::new_v4();
        let id = registry.start_retriable_with_lifecycle(
            retried_agent_id,
            BackgroundTaskKind::Subagent,
            "retried agent".to_owned(),
            Arc::new(AgentInstructionInbox::default()),
            move || {
                let attempt = launch_attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    TaskResult {
                        status: if attempt == 0 {
                            BackgroundTaskStatus::Failed
                        } else {
                            BackgroundTaskStatus::Completed
                        },
                        exit_code: Some(if attempt == 0 { 1 } else { 0 }),
                        output: format!("attempt {attempt}"),
                    }
                }
            },
            move |snapshot| {
                let tx = lifecycle_tx.clone();
                async move {
                    let _ = tx.send(snapshot);
                }
            },
        );
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Running
        );
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Failed
        );
        let retried = registry
            .retry_agent(retried_agent_id)
            .expect("retriable agent");
        assert_ne!(retried, id);
        assert_eq!(registry.snapshots()[0].agent_id, Some(retried_agent_id));
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Running
        );
        assert_eq!(
            lifecycle_rx.recv().await.unwrap().status,
            BackgroundTaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn registry_lifecycle_observer_sees_shell_start_and_terminal_snapshot() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::unbounded_channel();
        let registry = BackgroundTaskRegistry::default().with_lifecycle_observer(move |snapshot| {
            let tx = lifecycle_tx.clone();
            async move {
                let _ = tx.send(snapshot);
            }
        });
        let id = registry.start_retriable(
            BackgroundTaskKind::Shell,
            "private command label".to_owned(),
            || async {
                TaskResult {
                    status: BackgroundTaskStatus::Completed,
                    exit_code: Some(0),
                    output: "private output".to_owned(),
                }
            },
        );

        let started = lifecycle_rx.recv().await.unwrap();
        assert_eq!(started.id, id);
        assert_eq!(started.status, BackgroundTaskStatus::Running);
        assert_eq!(started.output_bytes, 0);
        let completed = lifecycle_rx.recv().await.unwrap();
        assert_eq!(completed.id, id);
        assert_eq!(completed.status, BackgroundTaskStatus::Completed);
        assert_eq!(completed.exit_code, Some(0));
        assert_eq!(completed.output_bytes, "private output".len());
    }
}
