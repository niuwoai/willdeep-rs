//! `monitor` 工具的事件与结束通知 v1。
//!
//! canonical 合同在 Xedit 仓库 `docs/BACKGROUND_TASK_CONTRACT.md` 的「第三批 ·
//! C·一」，金样在 `docs/contracts/monitor-event.v1.txt`（本仓库存一份副本）。
//! 与后台任务通知同一套脱敏与标签中和规则（复用
//! [`crate::background_notice`] 的实现），两端渲染必须逐字一致。

use std::path::Path;

use uuid::Uuid;
use willdeep_runtime_protocol::kernel_event::KernelEvent;
use willdeep_runtime_protocol::{ContentProvenance, EventPriority, EventSource};

use crate::background_notice::{clean, format_duration, label, path};
use crate::kernel::{InterruptPolicy, NOTICE_CONTRACT_KEY};

/// 事件正文带上的合同标记，内核据此不再整段转义框架。
pub const MONITOR_EVENT_CONTRACT_V1: &str = "monitor-event.v1";
pub const MONITOR_ENDED_CONTRACT_V1: &str = "monitor-ended.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorEndReason {
    Exited,
    TimedOut,
    Killed,
    Flooded,
    LaunchFailed,
}

impl MonitorEndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::TimedOut => "timed_out",
            Self::Killed => "killed",
            Self::Flooded => "flooded",
            Self::LaunchFailed => "launch_failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MonitorEvent<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub seq: u64,
    pub lines: &'a [String],
}

#[derive(Clone, Debug)]
pub struct MonitorEnded<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub reason: MonitorEndReason,
    pub exit_code: Option<i32>,
    pub duration_seconds: Option<u64>,
    /// 交给模型的事件条数。
    pub events: u64,
    pub output_path: Option<&'a Path>,
}

pub fn render_event(event: &MonitorEvent<'_>) -> String {
    format!(
        "<monitor-event>\nid: {}\nlabel: {}\nseq: {}\nlines: {}\n```text\n{}\n```\n</monitor-event>",
        clean(event.id),
        label(event.label),
        event.seq,
        event.lines.len(),
        clean(&event.lines.join("\n")),
    )
}

pub fn render_ended(ended: &MonitorEnded<'_>) -> String {
    format!(
        "<monitor-ended>\nid: {}\nlabel: {}\nreason: {}\nexit_code: {}\nduration: {}\nevents: {}\noutput_path: {}\n</monitor-ended>",
        clean(ended.id),
        label(ended.label),
        ended.reason.as_str(),
        ended
            .exit_code
            .map_or_else(|| "unknown".to_owned(), |code| code.to_string()),
        ended
            .duration_seconds
            .map_or_else(|| "unknown".to_owned(), format_duration),
        ended.events,
        path(ended.output_path),
    )
}

/// 一批输出行 → 内核事件。`wake` 为假时只入队（[`InterruptPolicy::Enqueue`]），
/// 不唤醒空闲会话，下一个回合边界随别的事件一起交给模型。
pub fn event_for_kernel(session_id: Uuid, event: &MonitorEvent<'_>, wake: bool) -> KernelEvent {
    build(
        session_id,
        "monitor.event",
        EventPriority::Normal,
        if wake {
            InterruptPolicy::YieldAtBoundary
        } else {
            InterruptPolicy::Enqueue
        },
        event.label,
        render_event(event),
        format!("monitor:{}:{}", event.id, event.seq),
        MONITOR_EVENT_CONTRACT_V1,
    )
}

/// 结束通知 → 内核事件。不受唤醒节流限制。
pub fn ended_for_kernel(session_id: Uuid, ended: &MonitorEnded<'_>) -> KernelEvent {
    let clean_exit = ended.reason == MonitorEndReason::Exited && ended.exit_code == Some(0);
    let mut event = build(
        session_id,
        "monitor.ended",
        if clean_exit {
            EventPriority::Normal
        } else {
            EventPriority::Urgent
        },
        InterruptPolicy::YieldAtBoundary,
        ended.label,
        render_ended(ended),
        format!("monitor:{}:ended", ended.id),
        MONITOR_ENDED_CONTRACT_V1,
    );
    event
        .metadata
        .insert("reason".to_owned(), ended.reason.as_str().to_owned());
    event
}

#[allow(clippy::too_many_arguments)]
fn build(
    session_id: Uuid,
    kind: &str,
    priority: EventPriority,
    interrupt: InterruptPolicy,
    title: &str,
    body: String,
    dedup_key: String,
    contract: &str,
) -> KernelEvent {
    let mut event = crate::kernel::host_event(
        session_id,
        EventSource::Task,
        kind,
        priority,
        interrupt,
        title.lines().next().unwrap_or_default().to_owned(),
        Some(body),
        Some(dedup_key),
        false,
    );
    // 命令输出是工具产出；宿主转发不改变这一点。正文已就地净化，合同标记让
    // 内核不再整段转义框架。
    event.content_provenance = ContentProvenance::Tool;
    event
        .metadata
        .insert(NOTICE_CONTRACT_KEY.to_owned(), contract.to_owned());
    event
}

#[cfg(test)]
mod tests;
