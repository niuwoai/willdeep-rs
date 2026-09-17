//! `monitor` 工具：盯着一条命令的 stdout，每批新行作为一条宿主事件推给模型。
//!
//! 合同见 Xedit `docs/BACKGROUND_TASK_CONTRACT.md`「第三批 · C·一」，渲染在
//! [`crate::monitor_notice`]。这里是运行时那一半：
//!
//! - 审批与 `run_command` 走同一个 [`ToolRegistry::gate_command`]，不开新口子；
//! - 行合批（200ms / 50 行）、防刷屏（60 秒超过 30 个事件即停）、唤醒节流
//!   （每个监视器 30 秒最多唤醒一次）都长在监视器自己身上——宿主事件不走外部
//!   唤醒额度，这几道闸没有别处可放；
//! - 随宿主进程存活（不脱离），注册进 [`BackgroundTaskRegistry`]，于是
//!   `kill_job` 能停、无头运行会等它结束。

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use super::*;
use crate::execution::{LineStreamEnd, run_line_stream};
use crate::kernel::{DedupPolicy, EventKernel};
use crate::monitor_notice::{
    MonitorEndReason, MonitorEnded, MonitorEvent, ended_for_kernel, event_for_kernel, render_ended,
};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 1_800;
/// 每行最多这么多字符，超出以 `…` 结尾。
const MAX_LINE_CHARS: usize = 2_000;
const BATCH_WINDOW: Duration = Duration::from_millis(200);
const MAX_BATCH_LINES: usize = 50;
/// 内核事件正文上限是 24000 字符，被它从尾部截掉会连框架一起截坏；合批时
/// 提前收口。
const MAX_BATCH_CHARS: usize = 20_000;
const FLOOD_WINDOW: Duration = Duration::from_secs(60);
const FLOOD_MAX_EVENTS: usize = 30;
const WAKE_INTERVAL: Duration = Duration::from_secs(30);
/// 行通道容量。满了读端就停，管道写满后命令自己会等——背压，不是丢行。
const LINE_CHANNEL: usize = 1_024;
/// `get_job_output` 读日志时最多看末尾这么多字节。
const OUTPUT_READ_BYTES: u64 = 4 * 1024 * 1024;

pub(super) const DESCRIPTION: &str = "Watch a shell command and receive its new stdout lines as <monitor-event> notices while it runs; a <monitor-ended> notice follows when it exits, times out, floods or is stopped with kill_job. Use monitor when you need to react to in-progress output; when you only care about the final result, use run_command with run_in_background instead. Every line costs model attention: filter inside the command first (for example `tail -f build.log | grep --line-buffered -E 'Error|FAILED|Traceback|done'`) down to lines worth reacting to, and cover failure shapes such as Error, FAILED and Traceback, not only the success marker. Lines arriving within 200ms are batched (at most 50 per event); more than 30 events in 60 seconds stops the monitor as flooded. stderr is only written to the log; merge it with 2>&1 when it matters. Uses the same approval path as run_command.";

pub(super) fn definition() -> ToolDefinition {
    super::definition(
        "monitor",
        DESCRIPTION,
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command line whose stdout lines become events."},
                "label": {"type": "string", "description": "Short user-facing label shown in every event; never include secrets."},
                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Stop the command after this many seconds. Defaults to 300."}
            },
            "required": ["command", "label"], "additionalProperties": false
        }),
    )
}

/// 监视器事件的去处：哪个内核、记在哪个会话名下、日志落在哪。
#[derive(Clone)]
pub(super) struct MonitorEventSink {
    kernel: EventKernel,
    session_id: uuid::Uuid,
    log_root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MonitorArgs {
    command: String,
    label: String,
    timeout_seconds: Option<u64>,
}

impl ToolRegistry {
    /// 开放 `monitor` 工具：事件发进 `kernel`、记在 `session_id` 名下，日志写到
    /// `log_root/<mon_id>/`。不调用就没有这个工具（子 Agent 也拿不到）。
    pub fn with_monitor_events(
        mut self,
        kernel: EventKernel,
        session_id: uuid::Uuid,
        log_root: impl Into<PathBuf>,
    ) -> Self {
        self.monitors = Some(MonitorEventSink {
            kernel,
            session_id,
            log_root: log_root.into(),
        });
        self
    }

    pub(super) async fn monitor(&self, args: MonitorArgs) -> Result<String, ToolError> {
        let Some(sink) = self.monitors.clone() else {
            return Err(ToolError::UnknownTool("monitor".to_owned()));
        };
        let command = args.command.trim().to_owned();
        let label = args.label.trim().to_owned();
        if command.is_empty() || label.is_empty() {
            return Err(ToolError::ApprovalDenied(
                "monitor needs a non-empty command and label".to_owned(),
            ));
        }
        let description = format!("{label}\ncommand: {command}");
        self.gate_command(&command, &description).await?;
        let timeout = Duration::from_secs(
            args.timeout_seconds
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS),
        );
        let run = MonitorRun {
            command,
            label: label.clone(),
            workspace: self.workspace.clone(),
            sandbox: self.sandbox.clone(),
            timeout,
            sink,
            registry: self.background.clone(),
        };
        let id = self
            .background
            .start_monitor(label, move |id, cancel| run_monitor(run, id, cancel));
        Ok(format!(
            "Monitor started: {id}. New stdout lines arrive as <monitor-event> notices and a <monitor-ended> notice follows; keep working instead of polling. Stop it with kill_job."
        ))
    }

    /// 监视器的进度：状态加日志末尾。不是监视器句柄就返回 `None`。
    pub(super) fn monitor_output(&self, id: &str, tail_lines: Option<usize>) -> Option<String> {
        let sink = self.monitors.as_ref()?;
        let suffix = id.strip_prefix("mon_")?;
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let status = self
            .background
            .snapshots()
            .into_iter()
            .find(|task| task.id == id)?
            .status;
        let path = sink.log_root.join(id).join("stdout.log");
        let lines = tail_lines
            .unwrap_or(crate::detached_job::DEFAULT_TAIL_LINES)
            .clamp(1, 2_000);
        let text = read_log_tail(&path).unwrap_or_default();
        let all = text.lines().collect::<Vec<_>>();
        let tail =
            crate::background_notice::clean(&all[all.len().saturating_sub(lines)..].join("\n"));
        Some(format!(
            "id: {id}\nkind: monitor\nstatus: {}\noutput_path: {}\nstdout (last {lines} lines, secrets redacted):\n{tail}",
            serde_json::to_value(&status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned()),
            crate::background_notice::path(Some(&path)),
        ))
    }
}

fn read_log_tail(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(OUTPUT_READ_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

struct MonitorRun {
    command: String,
    label: String,
    workspace: PathBuf,
    sandbox: SandboxSpec,
    timeout: Duration,
    sink: MonitorEventSink,
    registry: Arc<BackgroundTaskRegistry>,
}

/// 任意 60 秒内最多放行 30 个事件。
#[derive(Default)]
pub(super) struct FloodGuard {
    admitted: VecDeque<Instant>,
}

impl FloodGuard {
    pub(super) fn admit(&mut self, now: Instant) -> bool {
        while self
            .admitted
            .front()
            .is_some_and(|at| now.duration_since(*at) >= FLOOD_WINDOW)
        {
            self.admitted.pop_front();
        }
        if self.admitted.len() >= FLOOD_MAX_EVENTS {
            return false;
        }
        self.admitted.push_back(now);
        true
    }
}

/// 每 30 秒最多唤醒一次。
#[derive(Default)]
pub(super) struct WakeThrottle {
    last: Option<Instant>,
}

impl WakeThrottle {
    pub(super) fn admit(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < WAKE_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// 一批待发的行。第一行到达时开 200ms 窗口；满 50 行（或正文逼近事件上限）
/// 立刻收口，多出来的进下一批。
#[derive(Default)]
pub(super) struct Batch {
    lines: Vec<String>,
    chars: usize,
    deadline: Option<Instant>,
}

impl Batch {
    /// 收下一行，返回这一批是否已经满了。只有空白的行不算事件。
    pub(super) fn push(&mut self, line: String, now: Instant) -> bool {
        if line.trim().is_empty() {
            return false;
        }
        if self.lines.is_empty() {
            self.deadline = Some(now + BATCH_WINDOW);
        }
        self.chars += line.chars().count() + 1;
        self.lines.push(line);
        self.lines.len() >= MAX_BATCH_LINES || self.chars >= MAX_BATCH_CHARS
    }

    pub(super) fn take(&mut self) -> Vec<String> {
        self.chars = 0;
        self.deadline = None;
        std::mem::take(&mut self.lines)
    }
}

struct MonitorState {
    batch: Batch,
    flood: FloodGuard,
    wake: WakeThrottle,
    seq: u64,
}

impl MonitorState {
    /// 把当前这一批发出去。刷屏闸拒绝时返回 `false`，这一批不再投递。
    fn flush(&mut self, run: &MonitorRun, id: &str) -> bool {
        if self.batch.lines.is_empty() {
            self.batch.deadline = None;
            return true;
        }
        let now = Instant::now();
        if !self.flood.admit(now) {
            return false;
        }
        self.seq += 1;
        let lines = self.batch.take();
        let wake = self.wake.admit(now);
        let event = MonitorEvent {
            id,
            label: &run.label,
            seq: self.seq,
            lines: &lines,
        };
        run.sink.kernel.publish(
            event_for_kernel(run.sink.session_id, &event, wake),
            DedupPolicy::Once,
        );
        run.registry.signal_monitor_event();
        true
    }
}

async fn run_monitor(run: MonitorRun, id: String, mut cancel: watch::Receiver<bool>) -> TaskResult {
    let started = Instant::now();
    let log_dir = run.sink.log_root.join(&id);
    if let Err(error) = create_private_dir(&log_dir) {
        return finish(
            &run,
            &id,
            MonitorEndReason::LaunchFailed,
            None,
            started,
            0,
            None,
            &format!("monitor log directory could not be created: {error}"),
        );
    }
    let output_path = log_dir.join("stdout.log");
    let (line_tx, mut line_rx) = mpsc::channel(LINE_CHANNEL);
    let (stop_tx, stop_rx) = oneshot::channel();
    let process = run_line_stream(
        &run.command,
        &run.workspace,
        &run.sandbox,
        &log_dir,
        line_tx,
        MAX_LINE_CHARS,
        stop_rx,
    );
    tokio::pin!(process);
    let deadline = tokio::time::sleep(run.timeout);
    tokio::pin!(deadline);
    let mut state = MonitorState {
        batch: Batch::default(),
        flood: FloodGuard::default(),
        wake: WakeThrottle::default(),
        seq: 0,
    };
    let mut process_result: Option<std::io::Result<LineStreamEnd>> = None;
    let mut stdout_open = true;
    let stopped = loop {
        if process_result.is_some() && !stdout_open {
            break (!state.flush(&run, &id)).then_some(MonitorEndReason::Flooded);
        }
        let batch_deadline = state.batch.deadline;
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    break Some(MonitorEndReason::Killed);
                }
            }
            () = &mut deadline => break Some(MonitorEndReason::TimedOut),
            result = &mut process, if process_result.is_none() => process_result = Some(result),
            line = line_rx.recv(), if stdout_open => match line {
                Some(line) => {
                    if state.batch.push(line, Instant::now()) && !state.flush(&run, &id) {
                        break Some(MonitorEndReason::Flooded);
                    }
                }
                None => stdout_open = false,
            },
            () = tokio::time::sleep_until(batch_deadline.unwrap_or_else(Instant::now).into()), if batch_deadline.is_some() => {
                if !state.flush(&run, &id) {
                    break Some(MonitorEndReason::Flooded);
                }
            }
        }
    };
    if stopped.is_some() && process_result.is_none() {
        let _ = stop_tx.send(());
        process_result = Some((&mut process).await);
    }
    drop(line_rx);
    let exit_code = match &process_result {
        Some(Ok(LineStreamEnd::Exited(status))) => status.code(),
        _ => None,
    };
    let (reason, note) = match (stopped, process_result) {
        (Some(reason), _) => (reason, String::new()),
        (None, Some(Ok(LineStreamEnd::Exited(_)))) => (MonitorEndReason::Exited, String::new()),
        (None, Some(Ok(LineStreamEnd::Stopped))) => (MonitorEndReason::Killed, String::new()),
        (None, Some(Err(error))) => (
            MonitorEndReason::LaunchFailed,
            format!("monitor command failed: {error}"),
        ),
        (None, None) => (MonitorEndReason::LaunchFailed, String::new()),
    };
    finish(
        &run,
        &id,
        reason,
        exit_code,
        started,
        state.seq,
        Some(&output_path),
        &note,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    run: &MonitorRun,
    id: &str,
    reason: MonitorEndReason,
    exit_code: Option<i32>,
    started: Instant,
    events: u64,
    output_path: Option<&Path>,
    note: &str,
) -> TaskResult {
    let ended = MonitorEnded {
        id,
        label: &run.label,
        reason,
        exit_code,
        duration_seconds: Some(started.elapsed().as_secs()),
        events,
        output_path,
    };
    run.sink.kernel.publish(
        ended_for_kernel(run.sink.session_id, &ended),
        DedupPolicy::Once,
    );
    run.registry.signal_monitor_event();
    let mut output = render_ended(&ended);
    if !note.is_empty() {
        output.push('\n');
        output.push_str(&crate::background_notice::clean(note));
    }
    TaskResult {
        status: match reason {
            MonitorEndReason::Exited if exit_code == Some(0) => BackgroundTaskStatus::Completed,
            MonitorEndReason::Exited => BackgroundTaskStatus::Failed,
            MonitorEndReason::TimedOut => BackgroundTaskStatus::TimedOut,
            MonitorEndReason::Killed | MonitorEndReason::Flooded => BackgroundTaskStatus::Killed,
            MonitorEndReason::LaunchFailed => BackgroundTaskStatus::LaunchFailed,
        },
        exit_code,
        output,
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests;
