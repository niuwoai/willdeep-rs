//! `willdeep job`：看脱离父进程的后台作业。
//!
//! 这些作业活得比 Runtime 久:它们自成进程组、输出与退出码落盘,所以升级、
//! 重启、退出都不影响。这条命令读的是磁盘上那份记录,因此 Agent 没跑的时候
//! 也能查。

use anyhow::{Context, Result};
use clap::Subcommand;
use willdeep_core::detached_job::{DetachedJobStore, JobState, MAX_JOB_OUTPUT_BYTES};

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum JobAction {
    /// List detached background jobs, oldest first.
    List {
        /// Emit one JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Show one job with its captured output.
    Show {
        /// Job ID, as printed by `job list`.
        id: String,
        /// Bytes of output to show, counted from the end.
        #[arg(long, default_value_t = MAX_JOB_OUTPUT_BYTES)]
        tail_bytes: usize,
    },
    /// Delete the record of a job that has already finished.
    Forget {
        /// Job ID, as printed by `job list`.
        id: String,
    },
}

pub(crate) fn run(action: JobAction, home: &std::path::Path) -> Result<()> {
    let store = DetachedJobStore::new(home);
    match action {
        JobAction::List { json } => list(&store, json),
        JobAction::Show { id, tail_bytes } => show(&store, &id, tail_bytes),
        JobAction::Forget { id } => forget(&store, &id),
    }
}

fn state_label(state: JobState) -> String {
    match state {
        JobState::Running => "running".to_owned(),
        JobState::Finished { exit_code: 0 } => "done".to_owned(),
        JobState::Finished { exit_code } => format!("failed({exit_code})"),
        // 「不知道」和「失败」分开写:失败是有退出码的。
        JobState::Vanished => "unknown".to_owned(),
    }
}

fn list(store: &DetachedJobStore, json: bool) -> Result<()> {
    let jobs = store.list();
    if json {
        let rows: Vec<serde_json::Value> = jobs
            .iter()
            .map(|job| {
                serde_json::json!({
                    "id": job.id,
                    "pid": job.pid,
                    "state": state_label(store.state(job)),
                    "label": job.label,
                    "command": job.command,
                    "created_at": job.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if jobs.is_empty() {
        println!("no background jobs");
        return Ok(());
    }
    for job in &jobs {
        println!(
            "{:<40} {:<12} pid {:<8} {}",
            job.id,
            state_label(store.state(job)),
            job.pid,
            job.label.lines().next().unwrap_or_default()
        );
    }
    Ok(())
}

fn show(store: &DetachedJobStore, id: &str, tail_bytes: usize) -> Result<()> {
    let job = store
        .get(id)
        .with_context(|| format!("no background job {id}"))?;
    println!("id: {}", job.id);
    println!("state: {}", state_label(store.state(&job)));
    println!("pid: {}", job.pid);
    println!("workspace: {}", job.workspace.display());
    println!("command: {}", job.command);
    println!("---");
    println!("{}", store.output(&job.id, tail_bytes.clamp(1, 4 << 20)));
    Ok(())
}

fn forget(store: &DetachedJobStore, id: &str) -> Result<()> {
    match store.forget(id) {
        Ok(true) => {
            println!("{id} forgotten");
            Ok(())
        }
        Ok(false) => anyhow::bail!("no background job {id}"),
        // 还在跑的不给删:删了那个进程就没人认领了,连它的输出都找不回来。
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            anyhow::bail!("{id} is still running; wait for it or kill the process first")
        }
        Err(error) => Err(error).with_context(|| format!("forget background job {id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (DetachedJobStore, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("willdeep-job-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        (DetachedJobStore::new(&home), home)
    }

    /// 状态词把三件事分开:跑着、有退出码、不知道。
    #[test]
    fn state_labels_keep_failure_and_unknown_apart() {
        assert_eq!(state_label(JobState::Running), "running");
        assert_eq!(state_label(JobState::Finished { exit_code: 0 }), "done");
        assert_eq!(
            state_label(JobState::Finished { exit_code: 2 }),
            "failed(2)"
        );
        assert_eq!(state_label(JobState::Vanished), "unknown");
    }

    #[test]
    fn forgetting_an_unknown_job_is_reported() {
        let (store, _home) = store();
        let error = forget(&store, "job_missing").expect_err("unknown id");
        assert!(error.to_string().contains("no background job"));
    }

    /// 还在跑的作业不给删,并且说清楚为什么。
    #[test]
    fn a_running_job_refuses_to_be_forgotten() {
        let (store, home) = store();
        let job = store.spawn("sleep 30", "sleeper", &home).expect("spawn");
        let error = forget(&store, &job.id).expect_err("still running");
        assert!(error.to_string().contains("still running"), "{error}");
        let _ = std::process::Command::new("kill")
            .arg(job.pid.to_string())
            .status();
    }
}
