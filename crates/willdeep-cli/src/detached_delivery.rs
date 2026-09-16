//! 脱离作业的结束通知。
//!
//! `run_in_background` 的命令活得比宿主进程久，没有句柄可等，完成与否只能看
//! 磁盘上的记录。TUI 与无头运行都靠这里把「作业结束」变成内核事件；投递权用
//! 作业目录里的标记领取，同一次结束只向模型讲一遍，哪个前端先看到算哪个。

use willdeep_core::{DetachedJobStore, EventKernel, JobState};

/// 回灌给模型的输出尾部上限。完整日志留在作业目录，模型要看可以再读。
const NOTICE_OUTPUT_BYTES: usize = 4 * 1024;

/// 把会话名下已结束、尚未投递的作业发布成内核事件，返回这次新发布了几条。
///
/// 「不知道」不当成失败上报：失败是有退出码的，进程没留下退出码只说明我们
/// 不知道它怎么结束的，把它说成失败会让人去查一个并不存在的错误。
pub(crate) fn publish_finished_jobs(
    kernel: &EventKernel,
    jobs: &DetachedJobStore,
    session_id: uuid::Uuid,
) -> usize {
    let mut published = 0;
    for job in jobs.owned_by(&session_id.to_string()) {
        let state = jobs.state(&job);
        let (kind, title) = match state {
            JobState::Running => continue,
            JobState::Finished { exit_code: 0 } => ("job.completed", "后台作业完成"),
            JobState::Finished { .. } => ("job.failed", "后台作业失败"),
            JobState::Vanished => ("job.vanished", "后台作业没有留下结论"),
        };
        // 领取失败（磁盘错误）宁可这一秒不投，下一次轮询再试；绝不在拿不准
        // 的时候投，否则两个前端会各讲一遍。
        if !jobs.claim_delivery(&job).unwrap_or(false) {
            continue;
        }
        let mut event = willdeep_core::host_event(
            session_id,
            willdeep_runtime_protocol::EventSource::Task,
            kind,
            if matches!(state, JobState::Finished { exit_code: 0 }) {
                willdeep_runtime_protocol::EventPriority::Normal
            } else {
                willdeep_runtime_protocol::EventPriority::Urgent
            },
            willdeep_core::kernel::InterruptPolicy::YieldAtBoundary,
            format!("{title} · {}", job.label.lines().next().unwrap_or(&job.id)),
            Some(jobs.output(&job.id, NOTICE_OUTPUT_BYTES)),
            Some(format!("job:{}", job.id)),
            false,
        );
        // 命令输出是工具产出，不因为宿主转发就变成可信正文。
        event.content_provenance = willdeep_runtime_protocol::ContentProvenance::Tool;
        if !matches!(
            kernel.publish(event, willdeep_core::DedupPolicy::Once),
            willdeep_core::PublishOutcome::Duplicate(_)
        ) {
            published += 1;
        }
    }
    published
}

/// 会话名下还有没有在跑的作业。无头运行靠它决定要不要继续等。
pub(crate) fn has_running_jobs(jobs: &DetachedJobStore, session_id: uuid::Uuid) -> bool {
    jobs.owned_by(&session_id.to_string())
        .iter()
        .any(|job| jobs.state(job) == JobState::Running)
}
