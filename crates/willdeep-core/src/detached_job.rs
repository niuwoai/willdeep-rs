//! 脱离父进程的后台命令。
//!
//! 显式 `run_in_background` 的命令走这里：进程自成进程组、输出直接落文件、
//! 退出码由包装写下来。父进程可以升级、重启、退出，命令照跑;回来之后按记录
//! 取结果，不必重跑一遍。
//!
//! # 为什么要有一个「收尸」文件
//!
//! 进程一旦脱离，父进程就没有 `wait()` 可用了：等到它回来查的时候，那个 PID
//! 多半已经消失。光看「进程还在不在」只能区分「跑着」和「没了」，区分不出
//! 「成功」和「失败」。所以包装命令在真命令之后把退出码写进 `exit` 文件——
//! **文件在就是有结论，文件不在就是还没有**，这是唯一能跨进程存活的判据。
//!
//! # PID 会被复用
//!
//! 系统重启或长时间之后，同一个 PID 可能属于完全不相干的进程。只用 `kill -0`
//! 探活会把别人的进程当成自己的任务。所以记下启动时刻一并比对：PID 相同但
//! 启动时刻对不上，就是被复用了，按「进程已不在」处理。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
}

impl DetachedJobStore {
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            directory: home.as_ref().join(DIRECTORY),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
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
        let id = format!("job_{}", uuid::Uuid::new_v4().simple());
        let dir = self.directory.join(&id);
        std::fs::create_dir_all(&dir)?;
        let stdout = dir.join("stdout.log");
        let stderr = dir.join("stderr.log");
        let exit = dir.join("exit");

        // 包装脚本负责收尸。**用 `trap ... EXIT` 而不是把写入语句排在命令后
        // 面**：命令里一句 `exit 3` 会当场结束这个 shell，排在后面的语句根本
        // 不执行，于是一个明明有结论的作业永远显示「不知道」。EXIT trap 无论
        // 正常结束还是显式 exit 都会跑到。
        //
        // 路径走环境变量,不拼进脚本:目录名里有空格、引号、`$` 都不罕见,拼
        // 进去就等于让路径改写脚本。单引号让 `$WILLDEEP_JOB_EXIT_FILE` 留到
        // trap 执行时才展开。
        let script = format!("trap 'printf %s $? > \"$WILLDEEP_JOB_EXIT_FILE\"' EXIT\n{command}\n");
        let mut child = std::process::Command::new(shell_program());
        child
            .arg(shell_flag())
            .arg(&script)
            .env("WILLDEEP_JOB_EXIT_FILE", &exit)
            .current_dir(workspace)
            .stdin(std::process::Stdio::null())
            .stdout(std::fs::File::create(&stdout)?)
            .stderr(std::fs::File::create(&stderr)?);
        detach(&mut child);
        let handle = child.spawn()?;
        let pid = handle.id();
        // 句柄立刻丢掉：留着它父进程退出时会去 wait，而我们要的正是「不等」。
        drop(handle);

        let job = DetachedJob {
            id: id.clone(),
            command: command.to_owned(),
            label: label.to_owned(),
            workspace: workspace.to_path_buf(),
            pid,
            started_marker: process_start_marker(pid),
            created_at: now_seconds(),
        };
        write_private(&dir.join("meta.json"), &serde_json::to_vec_pretty(&job)?)?;
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
        let mut text = read_tail(&dir.join("stdout.log"), limit);
        let errors = read_tail(&dir.join("stderr.log"), limit);
        if !errors.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&errors);
        }
        text
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
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // 自成进程组：终端关闭时的 SIGHUP 发给的是前台进程组，不会波及到它。
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach(_command: &mut std::process::Command) {}

fn shell_program() -> &'static str {
    if cfg!(windows) { "cmd" } else { "/bin/sh" }
}

fn shell_flag() -> &'static str {
    if cfg!(windows) { "/C" } else { "-c" }
}

fn read_tail(path: &Path, limit: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    if bytes.len() <= limit {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let start = bytes.len() - limit;
    format!(
        "…[{} bytes omitted]…\n{}",
        start,
        String::from_utf8_lossy(&bytes[start..])
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

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (DetachedJobStore, PathBuf) {
        let home = std::env::temp_dir().join(format!("willdeep-jobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        (DetachedJobStore::new(&home), home)
    }

    fn wait_for_finish(store: &DetachedJobStore, job: &DetachedJob) -> JobState {
        for _ in 0..100 {
            let state = store.state(job);
            if state != JobState::Running {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("job never finished");
    }

    /// 退出码从文件里读回来，进程早已消失也照样有结论。
    #[test]
    fn a_finished_job_reports_its_exit_code_after_the_process_is_gone() {
        let (store, home) = store();
        let job = store
            .spawn("printf hello; exit 3", "greet", &home)
            .expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 3 }
        );
        assert!(
            store
                .output(&job.id, MAX_JOB_OUTPUT_BYTES)
                .contains("hello")
        );
    }

    /// 成功与失败靠退出码分，不靠「进程还在不在」。
    #[test]
    fn success_and_failure_are_told_apart_by_the_recorded_code() {
        let (store, home) = store();
        let ok = store.spawn("true", "ok", &home).expect("spawn");
        let bad = store.spawn("exit 7", "bad", &home).expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &ok),
            JobState::Finished { exit_code: 0 }
        );
        assert_eq!(
            wait_for_finish(&store, &bad),
            JobState::Finished { exit_code: 7 }
        );
    }

    /// 记录跨进程可读：换一个 store 实例（等价于重启）照样取得回结果。
    #[test]
    fn a_restart_reads_the_result_instead_of_rerunning() {
        let (store, home) = store();
        let job = store.spawn("printf done", "job", &home).expect("spawn");
        wait_for_finish(&store, &job);

        let reopened = DetachedJobStore::new(&home);
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        let report = reopened.report(&listed[0]);
        assert_eq!(report.state, JobState::Finished { exit_code: 0 });
        assert!(report.output.contains("done"));
        assert_eq!(report.job.command, "printf done");
    }

    /// 没留下退出码的进程是「不知道」，不是「失败」。
    #[test]
    fn a_vanished_process_is_unknown_not_failed() {
        let (store, home) = store();
        let mut job = store.spawn("true", "gone", &home).expect("spawn");
        wait_for_finish(&store, &job);
        // 手工抹掉收尸文件，模拟被 kill -9 或断电。
        std::fs::remove_file(store.directory().join(&job.id).join("exit")).expect("remove");
        // 顺便把 PID 改成一个几乎不可能存在的值。
        job.pid = 4_294_967_294;
        job.started_marker = None;
        assert_eq!(store.state(&job), JobState::Vanished);
    }

    /// PID 被复用时不能把别人的进程当成自己的作业。
    #[test]
    fn a_recycled_pid_does_not_look_like_a_running_job() {
        let (store, home) = store();
        let mut job = store.spawn("sleep 30", "sleeper", &home).expect("spawn");
        assert_eq!(store.state(&job), JobState::Running);
        // 同一个 PID，但启动时刻对不上：那是另一个进程。
        job.started_marker = Some("Thu Jan  1 00:00:00 1970".to_owned());
        assert_eq!(store.state(&job), JobState::Vanished);
        let _ = std::process::Command::new("kill")
            .arg(job.pid.to_string())
            .status();
    }

    /// 还在跑的作业不给删：删了那个进程就没人认领了。
    #[test]
    fn a_running_job_cannot_be_forgotten() {
        let (store, home) = store();
        let job = store.spawn("sleep 30", "sleeper", &home).expect("spawn");
        let error = store.forget(&job.id).expect_err("still running");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        let _ = std::process::Command::new("kill")
            .arg(job.pid.to_string())
            .status();
    }

    /// 路径里有空格和引号时，收尸文件仍然写在该写的地方。
    #[test]
    fn quoting_survives_awkward_paths() {
        let home = std::env::temp_dir().join(format!("willdeep jobs '{}'", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        let store = DetachedJobStore::new(&home);
        let job = store.spawn("exit 5", "quoted", &home).expect("spawn");
        assert_eq!(
            wait_for_finish(&store, &job),
            JobState::Finished { exit_code: 5 }
        );
    }
}
