//! 脱离作业按后台任务合同 v1 对外呈现：完成通知、`get_job_output`、保留策略。

use std::path::Path;

use super::{DetachedJob, DetachedJobStore, JobState, now_seconds};
use crate::background_notice::{Notice, NoticeKind, NoticeStatus};

/// 作业结束多久之后清理。
pub const RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
/// 一个家目录最多留多少个已结束作业。
pub const RETENTION_JOBS: usize = 200;
/// `get_job_output` 不带 `tail_lines` 时返回的行数。
pub const DEFAULT_TAIL_LINES: usize = 200;
pub const MAX_TAIL_LINES: usize = 2_000;
/// 读尾部时最多读多少字节，再从里面切行。
const TAIL_READ_BYTES: usize = 256 * 1024;
/// 通知里每条流读多少字节再交给渲染器切行。
const NOTICE_READ_BYTES: usize = 16 * 1024;

#[derive(serde::Deserialize, Default)]
struct RecordedResult {
    status: Option<String>,
    finished_at: Option<u64>,
}

impl DetachedJobStore {
    /// 结束通知的正文（合同 v1）。还在跑的作业没有通知可发，返回 `None`。
    pub fn notice(&self, job: &DetachedJob) -> Option<String> {
        let state = self.state(job);
        let status = self.notice_status(job, state)?;
        let dir = self.directory.join(&job.id);
        let stdout = dir.join("stdout.log");
        let stderr = dir.join("stderr.log");
        Some(crate::background_notice::render(&Notice {
            id: &job.id,
            kind: NoticeKind::Shell,
            label: &job.label,
            status,
            exit_code: match state {
                JobState::Finished { exit_code } => Some(exit_code),
                _ => None,
            },
            duration_seconds: self.duration_seconds(job),
            output_path: Some(&stdout),
            stderr_path: existing(&stderr),
            omitted_bytes: self.dropped_bytes(&job.id),
            output: &self.output(&job.id, NOTICE_READ_BYTES),
        }))
    }

    /// `get_job_output` 的正文：状态头 + 每条流最后 `tail_lines` 行。
    pub fn render_output(&self, job: &DetachedJob, tail_lines: Option<usize>) -> String {
        let lines = tail_lines
            .unwrap_or(DEFAULT_TAIL_LINES)
            .clamp(1, MAX_TAIL_LINES);
        let state = self.state(job);
        let status = self
            .notice_status(job, state)
            .map_or("running", NoticeStatus::as_str);
        let exit_code = match state {
            JobState::Finished { exit_code } => exit_code.to_string(),
            _ => "unknown".to_owned(),
        };
        let duration = self.duration_seconds(job).map_or_else(
            || "unknown".to_owned(),
            crate::background_notice::format_duration,
        );
        let dir = self.directory.join(&job.id);
        let mut rendered = format!(
            "{}\nstatus: {status}\nexit_code: {exit_code}\nduration: {duration}\noutput_path: {}\nstderr_path: {}\nomitted_bytes: {}\n",
            job.id,
            dir.join("stdout.log").display(),
            existing(&dir.join("stderr.log"))
                .map_or_else(|| "none".to_owned(), |path| path.display().to_string()),
            self.dropped_bytes(&job.id),
        );
        for stream in ["stdout", "stderr"] {
            let text = super::read_stream_tail(&dir.join(format!("{stream}.log")), TAIL_READ_BYTES);
            let all = text
                .trim_end_matches(['\n', '\r'])
                .lines()
                .collect::<Vec<_>>();
            if all.is_empty() {
                continue;
            }
            rendered.push_str(&format!(
                "--- {stream} (last {lines} lines) ---\n{}\n",
                all[all.len().saturating_sub(lines)..].join("\n")
            ));
        }
        rendered
    }

    /// 清理过期作业，返回删了几个。**还在跑的一律不动**：删了记录，进程就没人
    /// 认领了。
    pub fn prune(&self) -> usize {
        let now = now_seconds();
        let mut finished = self
            .list()
            .into_iter()
            .filter(|job| self.state(job) != JobState::Running)
            .map(|job| {
                let settled = self.recorded(&job.id).finished_at.unwrap_or(job.created_at);
                (settled, job)
            })
            .collect::<Vec<_>>();
        finished.sort_by_key(|(settled, _)| *settled);
        let overflow = finished.len().saturating_sub(RETENTION_JOBS);
        let mut removed = 0;
        for (index, (settled, job)) in finished.iter().enumerate() {
            if (index < overflow || now.saturating_sub(*settled) > RETENTION_SECONDS)
                && std::fs::remove_dir_all(self.directory.join(&job.id)).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }

    fn notice_status(&self, job: &DetachedJob, state: JobState) -> Option<NoticeStatus> {
        let exit_code = match state {
            JobState::Running => return None,
            JobState::Vanished => return Some(NoticeStatus::Vanished),
            JobState::Finished { exit_code } => exit_code,
        };
        Some(match self.recorded(&job.id).status.as_deref() {
            Some("completed") => NoticeStatus::Completed,
            Some("killed") => NoticeStatus::Killed,
            Some("timed_out") => NoticeStatus::TimedOut,
            Some("launch_failed") => NoticeStatus::LaunchFailed,
            Some(_) => NoticeStatus::Failed,
            // 老记录没有 result.json 里的状态：只能按退出码推。
            None => match exit_code {
                0 => NoticeStatus::Completed,
                124 => NoticeStatus::TimedOut,
                137 => NoticeStatus::Killed,
                _ => NoticeStatus::Failed,
            },
        })
    }

    /// 跑了多久。还在跑的算到现在；结束了的算到落下结论的那一刻。
    fn duration_seconds(&self, job: &DetachedJob) -> Option<u64> {
        let end = match self.state(job) {
            JobState::Running => now_seconds(),
            _ => self.recorded(&job.id).finished_at?,
        };
        Some(end.saturating_sub(job.created_at))
    }

    fn dropped_bytes(&self, id: &str) -> u64 {
        let dir = self.directory.join(id);
        ["stdout.log", "stderr.log"]
            .iter()
            .filter_map(|name| {
                std::fs::read_to_string(crate::execution::sidecar(&dir.join(name), "dropped")).ok()
            })
            .filter_map(|raw| raw.trim().parse::<u64>().ok())
            .sum()
    }

    fn recorded(&self, id: &str) -> RecordedResult {
        std::fs::read(self.directory.join(id).join("result.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
}

/// 有内容的文件才算数：空的 stderr 不值得在通知里占一行路径。
fn existing(path: &Path) -> Option<&Path> {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.len() > 0)
        .then_some(path)
}
