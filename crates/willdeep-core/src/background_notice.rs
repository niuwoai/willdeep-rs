//! 后台任务完成通知 v1。
//!
//! canonical 合同在 Xedit 仓库 `docs/BACKGROUND_TASK_CONTRACT.md`，金样在
//! `docs/contracts/background-task-notification.v1.txt`（本仓库存一份副本）。
//! macOS 与 CLI 渲染出来必须逐字一致：同一个模型换个前端，不该重新学一遍怎么
//! 读通知。字段顺序、取值拼写、尾巴长度都是合同的一部分，改这里要同时改两端
//! 和金样。
//!
//! 通知由宿主拼装，只有 `label` 和输出尾巴来自不可信来源；这两处在这里就地
//! 脱敏、中和标签，外层框架因此可以按宿主正文投递，不必整段转义。

use std::path::Path;

/// 失败类通知带多少行尾巴。
pub const FAILURE_TAIL_LINES: usize = 40;
/// 失败类通知尾巴的字符上限。
pub const FAILURE_TAIL_CHARS: usize = 4_000;
/// 成功通知带多少行尾巴：一句「PASS」够模型确认，省掉一次 `get_job_output`。
pub const SUCCESS_TAIL_LINES: usize = 10;
pub const SUCCESS_TAIL_CHARS: usize = 1_000;
/// `label` 只取第一行，并截到这么多字符。
const LABEL_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Shell,
    Subagent,
    Executor,
}

impl NoticeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Subagent => "subagent",
            Self::Executor => "executor",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Subagent => "subagent-report",
            Self::Shell | Self::Executor => "background-task-notification",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeStatus {
    Completed,
    Failed,
    Killed,
    TimedOut,
    LaunchFailed,
    /// 进程没了且没留下退出码。**不是失败**：失败是有退出码的。
    Vanished,
    /// 子 Agent 专用：交了部分结果 / 被卡住需要父 Agent 介入。
    Partial,
    Blocked,
}

impl NoticeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
            Self::TimedOut => "timed_out",
            Self::LaunchFailed => "launch_failed",
            Self::Vanished => "vanished",
            Self::Partial => "partial",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Notice<'a> {
    pub id: &'a str,
    pub kind: NoticeKind,
    pub label: &'a str,
    pub status: NoticeStatus,
    pub exit_code: Option<i32>,
    pub duration_seconds: Option<u64>,
    /// 完整日志（或 stdout）所在路径；宿主不落盘时为 `None`。
    pub output_path: Option<&'a Path>,
    /// stderr 单独落盘时的路径；合并在一个日志里时为 `None`。
    pub stderr_path: Option<&'a Path>,
    /// 因单日志上限没能落盘的字节数，通常为 0。
    pub omitted_bytes: u64,
    pub output: &'a str,
}

pub fn render(notice: &Notice<'_>) -> String {
    let tag = notice.kind.tag();
    // 子 Agent 的报告本身就是结论，成功也给长尾巴。
    let (lines, chars) =
        if notice.status == NoticeStatus::Completed && notice.kind != NoticeKind::Subagent {
            (SUCCESS_TAIL_LINES, SUCCESS_TAIL_CHARS)
        } else {
            (FAILURE_TAIL_LINES, FAILURE_TAIL_CHARS)
        };
    let mut rendered = format!(
        "<{tag}>\nid: {}\nkind: {}\nlabel: {}\nstatus: {}\nexit_code: {}\nduration: {}\noutput_path: {}\nstderr_path: {}\nomitted_bytes: {}\n",
        clean(notice.id),
        notice.kind.as_str(),
        label(notice.label),
        notice.status.as_str(),
        notice
            .exit_code
            .map_or_else(|| "unknown".to_owned(), |code| code.to_string()),
        notice
            .duration_seconds
            .map_or_else(|| "unknown".to_owned(), format_duration),
        path(notice.output_path),
        path(notice.stderr_path),
        notice.omitted_bytes,
    );
    let tail = tail(notice.output, lines, chars);
    if tail.is_empty() {
        rendered.push_str("output_tail: (empty)\n");
    } else {
        rendered.push_str(&format!(
            "output_tail (last {lines} lines, secrets redacted):\n```text\n{tail}\n```\n"
        ));
    }
    rendered.push_str(&format!("</{tag}>"));
    rendered
}

/// `65` → `1m5s`，`3725` → `1h2m5s`，`0` → `0s`。
pub fn format_duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m{seconds}s"),
        _ => format!("{hours}h{minutes}m{seconds}s"),
    }
}

pub(crate) fn label(value: &str) -> String {
    let first = value.lines().next().unwrap_or_default().trim();
    let mut label = clean(first);
    if label.chars().count() > LABEL_CHARS {
        label = label.chars().take(LABEL_CHARS).collect::<String>() + "…";
    }
    label
}

pub(crate) fn path(value: Option<&Path>) -> String {
    value.map_or_else(
        || "none".to_owned(),
        |path| clean(&path.display().to_string()),
    )
}

/// 最后 `lines` 行，再截到最后 `chars` 个字符；截过就以 `…` 开头。
fn tail(output: &str, lines: usize, chars: usize) -> String {
    let trimmed = output.trim_end_matches(['\n', '\r']);
    if trimmed.trim().is_empty() {
        return String::new();
    }
    let all = trimmed.lines().collect::<Vec<_>>();
    let kept = all[all.len().saturating_sub(lines)..].join("\n");
    let kept = clean(&kept);
    let count = kept.chars().count();
    if count <= chars {
        return kept;
    }
    let suffix = kept.chars().skip(count - chars).collect::<String>();
    format!("…{suffix}")
}

/// 脱敏并中和标签：不可信文本里的 `</background-task-notification>` 不能
/// 把框架提前关掉。
pub(crate) fn clean(value: &str) -> String {
    let redacted = redact_lines(value);
    let mut out = String::with_capacity(redacted.len());
    let mut chars = redacted.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '<'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '/')
        {
            out.push_str("&lt;");
        } else {
            out.push(character);
        }
    }
    out
}

/// 逐行脱敏。`redact_credentials` 按空白切词再拼回，会把缩进和对齐吃掉；
/// 只有真的命中了凭据的那一行才换成脱敏版本，其余行保持原样。
fn redact_lines(value: &str) -> String {
    value
        .split('\n')
        .map(|line| {
            let redacted = crate::judge::redact_credentials(line);
            if redacted.split(' ').eq(line.split_whitespace()) {
                line.to_owned()
            } else {
                redacted
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
