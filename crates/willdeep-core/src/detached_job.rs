//! 脱离父进程的后台命令。
//!
//! 显式 `run_in_background` 的命令走这里：独立 supervisor 继承执行策略，
//! 有界输出和退出码落盘。父进程可以升级、重启、退出，命令照跑;回来之后按记录
//! 取结果，不必重跑一遍。
//!
//! # 为什么要有一个「收尸」文件
//!
//! 进程一旦脱离，父进程就没有 `wait()` 可用了：等到它回来查的时候，那个 PID
//! 多半已经消失。光看「进程还在不在」只能区分「跑着」和「没了」，区分不出
//! 「成功」和「失败」。所以 supervisor 在保存结果之后原子发布 `exit` 文件——
//! **文件在就是有结论，文件不在就是还没有**，这是唯一能跨进程存活的判据。
//!
//! # PID 会被复用
//!
//! 系统重启或长时间之后，同一个 PID 可能属于完全不相干的进程。只用 `kill -0`
//! 探活会把别人的进程当成自己的任务。所以记下启动时刻一并比对：PID 相同但
//! 启动时刻对不上，就是被复用了，按「进程已不在」处理。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod contract;
pub use contract::{DEFAULT_TAIL_LINES, RETENTION_JOBS, RETENTION_SECONDS};

const DIRECTORY: &str = "background-jobs";
/// 一次读回多少输出。作业日志可能很长，回灌给模型的永远是尾部。
pub const MAX_JOB_OUTPUT_BYTES: usize = 16 * 1024;

/// 一个脱离进程的后台作业。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetachedJob {
    pub id: String,
    pub command: String,
    /// 打码后的展示名。原始命令行也在 `command` 里，两者都只留在本机。
    pub label: String,
    pub workspace: PathBuf,
    pub pid: u32,
    /// 这个 PID 的启动时刻，用来识别 PID 复用。取不到时为 `None`，那时只能
    /// 退回单看 PID——聊胜于无，但要知道它可能认错人。
    pub started_marker: Option<String>,
    pub created_at: u64,
    /// 起这个作业的会话。结束通知只投递给它：作业目录是全局的，不认主的话
    /// 一个会话会收到别的会话、甚至几周前的作业结论。旧记录没有这个字段，
    /// 按「无主」处理，不再补投。
    #[serde(default)]
    pub owner: Option<String>,
}

/// [`DetachedJobStore::kill`] 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillOutcome {
    /// 已向 supervisor 发出终止信号；结论（退出码 137）稍后落盘。
    Signalled,
    /// 作业已经有结论或进程已不在，没有可停的东西。
    NotRunning,
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// 进程还在跑。
    Running,
    /// 有 `exit` 文件：这是唯一可信的「有结论」。
    Finished { exit_code: i32 },
    /// 进程没了却没留下退出码：被 `kill -9`、机器断电，或者 PID 被复用。
    /// **不当成失败**——失败是有退出码的，这里是「不知道」。
    Vanished,
}

#[derive(Clone, Debug)]
pub struct JobReport {
    pub job: DetachedJob,
    pub state: JobState,
    pub output: String,
}

#[derive(Clone, Debug)]
pub struct DetachedJobStore {
    directory: PathBuf,
    supervisor_executable: Option<PathBuf>,
    owner: Option<String>,
}

impl DetachedJobStore {
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            directory: home.as_ref().join(DIRECTORY),
            supervisor_executable: None,
            owner: None,
        }
    }

    /// 之后由这个 store 起的作业都记在 `owner` 名下。
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Select the host executable implementing `daemon background-supervisor`.
    /// Embedded hosts must supply a compatible executable explicitly.
    pub fn with_supervisor_executable(mut self, executable: PathBuf) -> Self {
        self.supervisor_executable = Some(executable);
        self
    }

    /// 起一个脱离父进程的命令。
    ///
    /// 三件事一起做才算数：自成进程组（终端关闭时的 SIGHUP 波及不到它）、
    /// 输出重定向到文件（管道会随父进程一起关掉）、退出码落盘（父进程回来时
    /// 才有判据）。少任何一件，父进程一退出这个作业就等于白跑。
    pub fn spawn(
        &self,
        command: &str,
        label: &str,
        workspace: &Path,
    ) -> std::io::Result<DetachedJob> {
        self.spawn_with_policy(
            command,
            label,
            workspace,
            &crate::sandbox::SandboxSpec::new(crate::sandbox::SandboxPolicy::Off, []),
            60,
        )
    }

    pub fn spawn_with_policy(
        &self,
        command: &str,
        label: &str,
        workspace: &Path,
        sandbox: &crate::sandbox::SandboxSpec,
        timeout_seconds: u64,
    ) -> std::io::Result<DetachedJob> {
        use std::io::Write;
        // Validate before creating a job or starting any process.
        let _ = crate::execution::shell(command, sandbox)?;
        if command.trim().is_empty() || !(1..=600).contains(&timeout_seconds) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid detached command or timeout",
            ));
        }
        let id = format!("job_{}", uuid::Uuid::new_v4().simple());
        let dir = self.directory.join(&id);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&dir)?;
        let payload = serde_json::to_vec(&serde_json::json!({
            "command": command, "workspace": workspace.canonicalize()?,
            "timeout_seconds": timeout_seconds, "sandbox": sandbox,
            "detached_directory": dir.canonicalize()?,
        }))?;
        if payload.len() > 256 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "detached command request is too large",
            ));
        }
        let executable = self
            .supervisor_executable
            .clone()
            .map(Ok)
            .unwrap_or_else(std::env::current_exe)?;
        let mut child = std::process::Command::new(executable);
        child
            .args(["daemon", "background-supervisor"])
            .env("WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR", "1")
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach(&mut child);
        let mut handle = child.spawn()?;
        let pid = handle.id();

        let job = DetachedJob {
            id: id.clone(),
            command: command.to_owned(),
            label: label.to_owned(),
            workspace: workspace.to_path_buf(),
            pid,
            started_marker: process_start_marker(pid),
            created_at: now_seconds(),
            owner: self.owner.clone(),
        };
        // Journal before sending the command: a metadata failure cannot leave
        // an unrecorded side effect running in the background.
        let start = (|| -> std::io::Result<()> {
            write_private(&dir.join("meta.json"), &serde_json::to_vec_pretty(&job)?)?;
            let mut input = handle
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("missing supervisor input"))?;
            input.write_all(&(payload.len() as u32).to_be_bytes())?;
            input.write_all(&payload)?;
            input.flush()
        })();
        if let Err(error) = start {
            let _ = handle.kill();
            let _ = handle.wait();
            return Err(error);
        }
        // Reap while this host lives, without tying child lifetime to the host.
        std::thread::spawn(move || {
            let _ = handle.wait();
        });
        Ok(job)
    }

    pub fn list(&self) -> Vec<DetachedJob> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut jobs: Vec<DetachedJob> = entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.path().join("meta.json");
                let bytes = std::fs::read(meta).ok()?;
                serde_json::from_slice(&bytes).ok()
            })
            .collect();
        jobs.sort_by_key(|job| job.created_at);
        jobs
    }

    /// 某个会话名下的作业。
    pub fn owned_by(&self, owner: &str) -> Vec<DetachedJob> {
        let mut jobs = self.list();
        jobs.retain(|job| job.owner.as_deref() == Some(owner));
        jobs
    }

    pub fn get(&self, id: &str) -> Option<DetachedJob> {
        let bytes = std::fs::read(self.directory.join(id).join("meta.json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 现在这个作业是什么状态。
    ///
    /// 判定顺序不能反：**先看退出码文件，再看进程**。反过来的话，一个刚结束
    /// 但还没被回收的进程会被读成「还在跑」，而它其实已经有结论了。
    pub fn state(&self, job: &DetachedJob) -> JobState {
        if let Some(code) = self.exit_code(&job.id) {
            return JobState::Finished { exit_code: code };
        }
        if process_alive(job) {
            JobState::Running
        } else {
            JobState::Vanished
        }
    }

    pub fn report(&self, job: &DetachedJob) -> JobReport {
        JobReport {
            state: self.state(job),
            output: self.output(&job.id, MAX_JOB_OUTPUT_BYTES),
            job: job.clone(),
        }
    }

    fn exit_code(&self, id: &str) -> Option<i32> {
        let raw = std::fs::read_to_string(self.directory.join(id).join("exit")).ok()?;
        raw.trim().parse().ok()
    }

    /// 作业输出的**尾部**。日志可能很长，而回灌给模型的窗口有限；掐头留尾是
    /// 因为失败原因几乎总在末尾。
    pub fn output(&self, id: &str, limit: usize) -> String {
        let dir = self.directory.join(id);
        let mut text = read_stream_tail(&dir.join("stdout.log"), limit);
        let errors = read_stream_tail(&dir.join("stderr.log"), limit);
        if !errors.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&errors);
        }
        let status = read_tail(&dir.join("status.log"), limit);
        if !status.is_empty() {
            text.push('\n');
            text.push_str(&status);
        }
        text
    }

    /// 停掉一个还在跑的作业。
    ///
    /// 信号发给 supervisor 而不是命令本身：supervisor 收到 SIGTERM 会连同命令
    /// 所在的进程组一起收掉，并照常落下退出码——直接杀命令的话，supervisor
    /// 记下的是「命令失败」，分不出是被人叫停的。发信号前先按启动时刻核对
    /// PID，防止把复用了这个 PID 的无关进程杀掉。
    pub fn kill(&self, id: &str) -> std::io::Result<KillOutcome> {
        let Some(job) = self.get(id) else {
            return Ok(KillOutcome::NotFound);
        };
        if self.state(&job) != JobState::Running {
            return Ok(KillOutcome::NotRunning);
        }
        terminate(job.pid)?;
        Ok(KillOutcome::Signalled)
    }

    /// 领取一个已结束作业的投递权。只有第一个调用者拿到 `true`。
    ///
    /// TUI、无头运行、守护进程可能同时看着同一个作业目录；进程内的去重跨不了
    /// 进程也跨不了重启，所以用 `create_new` 在作业目录里落一个标记——同一次
    /// 结束只向模型讲一遍。还在跑的作业不许领取。
    pub fn claim_delivery(&self, job: &DetachedJob) -> std::io::Result<bool> {
        if self.state(job) == JobState::Running {
            return Ok(false);
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(self.directory.join(&job.id).join("delivered")) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// 删掉一个作业的记录。**只删已经有结论的**：还在跑的删了就再也找不回来，
    /// 那个进程会变成没人认领的孤儿。
    pub fn forget(&self, id: &str) -> std::io::Result<bool> {
        let Some(job) = self.get(id) else {
            return Ok(false);
        };
        if self.state(&job) == JobState::Running {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "job is still running",
            ));
        }
        std::fs::remove_dir_all(self.directory.join(id))?;
        Ok(true)
    }
}

/// 进程还在不在。
///
/// `kill -0` 只回答「这个 PID 现在有没有进程」，回答不了「是不是同一个进程」。
/// 启动时刻对不上就是 PID 被复用了，按不在处理——把别人的进程当成自己的任务
/// 会让一个早就没了的作业永远显示「运行中」。
fn process_alive(job: &DetachedJob) -> bool {
    if !pid_exists(job.pid) {
        return false;
    }
    match (&job.started_marker, process_start_marker(job.pid)) {
        (Some(recorded), Some(current)) => recorded == &current,
        // 记不下启动时刻的平台上只能单看 PID。
        _ => true,
    }
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` 只做权限与存在性检查，不投递信号。
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn pid_exists(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// 这个 PID 的启动时刻，用来识别复用。取不到就返回 `None`。
fn process_start_marker(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let marker = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!marker.is_empty()).then_some(marker)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

#[cfg(unix)]
fn terminate(pid: u32) -> std::io::Result<()> {
    // SAFETY: 只向一个刚核对过启动时刻的 PID 投递 SIGTERM。
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn terminate(pid: u32) -> std::io::Result<()> {
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "taskkill exited with {status}"
        )))
    }
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // 自成进程组：终端关闭时的 SIGHUP 发给的是前台进程组，不会波及到它。
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach(_command: &mut std::process::Command) {}

pub(crate) fn record_result(
    directory: &Path,
    result: crate::background::TaskResult,
) -> std::io::Result<()> {
    use crate::background::BackgroundTaskStatus;
    let exit_code = match result.status {
        BackgroundTaskStatus::TimedOut => 124,
        BackgroundTaskStatus::Killed => 137,
        _ => result.exit_code.unwrap_or(125),
    };
    if !directory.join("stdout.log").exists() {
        write_private(&directory.join("stdout.log"), result.output.as_bytes())?;
    } else if !matches!(
        result.status,
        BackgroundTaskStatus::Completed | BackgroundTaskStatus::Failed
    ) {
        // Stream logs already retain the output; keep the status separate
        // without duplicating those tails in every subsequent report.
        write_private(
            &directory.join("status.log"),
            result.output.lines().next().unwrap_or_default().as_bytes(),
        )?;
    }
    write_private(
        &directory.join("result.json"),
        &serde_json::to_vec(&serde_json::json!({
            "status": result.status, "exit_code": result.exit_code,
            "finished_at": now_seconds(),
        }))?,
    )?;
    // The final marker is published only after output and detailed status.
    write_private(
        &directory.join("exit.pending"),
        exit_code.to_string().as_bytes(),
    )?;
    std::fs::rename(directory.join("exit.pending"), directory.join("exit"))
}

/// 一条日志流的真实末尾：日志被单文件上限截断过时，末尾在 `.tail` 旁挂文件里。
fn read_stream_tail(path: &Path, limit: usize) -> String {
    let tail = crate::execution::sidecar(path, "tail");
    if tail.exists() {
        return read_tail(&tail, limit);
    }
    read_tail(path, limit)
}

fn read_tail(path: &Path, limit: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    let start = metadata.len().saturating_sub(limit as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.take(limit as u64).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    if start == 0 {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    format!(
        "…[{} bytes omitted]…\n{}",
        start,
        String::from_utf8_lossy(&bytes)
    )
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

pub(crate) fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        options.open(&temporary)?.write_all(data)?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}
