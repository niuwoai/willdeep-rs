//! Shared shell construction and bounded capture for runtime commands.
use std::{io, path::Path, process::Stdio, time::Duration};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::{io::AsyncReadExt, process::Command};

use crate::sandbox::SandboxSpec;

#[cfg(windows)]
mod windows_job;

pub(crate) fn shell(command: &str, sandbox: &SandboxSpec) -> io::Result<Command> {
    match sandbox.command_line(crate::tools::SHELL_PROGRAM, command) {
        Some(argv) => {
            let mut process = Command::new(&argv[0]);
            process.args(&argv[1..]);
            Ok(process)
        }
        None if sandbox.policy.is_enforcing() => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "requested OS sandbox is unavailable; command was not started",
        )),
        None => Ok(crate::tools::platform_shell(command)),
    }
}

/// 一个后台作业单条日志流最多落多少字节（后台任务合同 v1）。
///
/// 超过之后不再追加，但内存里的尾部照常滚动，结束时另存成 `.tail`——
/// 「末尾永远可读」不能被上限破坏，失败原因几乎总在最后几行。
pub(crate) const MAX_LOG_BYTES: u64 = 256 * 1024 * 1024;

struct CaptureState {
    bytes: std::collections::VecDeque<u8>,
    limit: usize,
    omitted: u64,
    path: Option<std::path::PathBuf>,
    /// 追加写的完整日志。第一次 `push` 时创建。
    log: Option<std::fs::File>,
    logged: u64,
    /// 因为 [`MAX_LOG_BYTES`] 没能写进日志的字节数。
    dropped: u64,
    log_cap: u64,
}

impl CaptureState {
    fn new(limit: usize, path: Option<std::path::PathBuf>) -> Self {
        Self {
            bytes: std::collections::VecDeque::with_capacity(limit),
            limit,
            omitted: 0,
            path,
            log: None,
            logged: 0,
            dropped: 0,
            log_cap: MAX_LOG_BYTES,
        }
    }

    fn push(&mut self, input: &[u8]) -> io::Result<()> {
        let chunk = &input[input.len().saturating_sub(self.limit)..];
        let discard = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.limit);
        self.omitted = self
            .omitted
            .saturating_add((discard + input.len() - chunk.len()) as u64);
        self.bytes.drain(..discard);
        self.bytes.extend(chunk);
        self.append_log(input)
    }

    /// 完整输出追加进日志。此前是每来一块就把尾部整份原子重写一次：日志只剩
    /// 尾巴，而且长输出下每块都是 O(n) 的无用功。
    fn append_log(&mut self, input: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let Some(path) = &self.path else {
            return Ok(());
        };
        if self.log.is_none() {
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            self.log = Some(options.open(path)?);
        }
        let room = self.log_cap.saturating_sub(self.logged);
        let written = (input.len() as u64).min(room) as usize;
        if written > 0 {
            self.log
                .as_mut()
                .expect("log opened above")
                .write_all(&input[..written])?;
            self.logged += written as u64;
        }
        self.dropped += (input.len() - written) as u64;
        Ok(())
    }

    /// 流结束时收尾：日志被上限截断过，就把真实末尾和丢弃字节数另存。
    fn finish(&self) -> io::Result<()> {
        let (Some(path), true) = (&self.path, self.dropped > 0) else {
            return Ok(());
        };
        crate::detached_job::write_private_atomic(&sidecar(path, "tail"), &self.snapshot())?;
        crate::detached_job::write_private_atomic(
            &sidecar(path, "dropped"),
            self.dropped.to_string().as_bytes(),
        )
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    fn describe(&self) -> String {
        let prefix = if self.omitted == 0 {
            String::new()
        } else {
            format!("[{} earlier bytes omitted]\n", self.omitted)
        };
        format!("{prefix}{}", String::from_utf8_lossy(&self.snapshot()))
    }
}

/// `stdout.log` → `stdout.log.tail` 这类旁挂文件。
pub(crate) fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Drains the entire stream while retaining only a bounded tail.
async fn capture(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    state: &mut CaptureState,
) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        state.push(&buffer[..count])?;
    }
}

/// Owns a freshly created process group, including descendants on cancellation.
#[cfg(unix)]
struct ProcessGroup(Option<u32>);

#[cfg(unix)]
impl ProcessGroup {
    fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.take() {
            // The leader remains unreaped until this signal is sent, so its PID
            // cannot be recycled into an unrelated process group.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
async fn observe_exit(pid: u32) -> io::Result<()> {
    const OBSERVATION_INTERVAL: Duration = Duration::from_millis(20);
    loop {
        if has_exited(pid)? {
            return Ok(());
        }
        tokio::time::sleep(OBSERVATION_INTERVAL).await;
    }
}

#[cfg(unix)]
fn has_exited(pid: u32) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(info.si_signo != 0)
}

pub(crate) async fn run_capture(
    command: &str,
    workspace: &Path,
    sandbox: &SandboxSpec,
    timeout: Duration,
    output_limit: usize,
) -> io::Result<std::process::Output> {
    run_capture_logged(command, workspace, sandbox, timeout, output_limit, None).await
}

pub(crate) async fn run_capture_logged(
    command: &str,
    workspace: &Path,
    sandbox: &SandboxSpec,
    timeout: Duration,
    output_limit: usize,
    log_directory: Option<&Path>,
) -> io::Result<std::process::Output> {
    let mut stdout_state = CaptureState::new(
        output_limit,
        log_directory.map(|dir| dir.join("stdout.log")),
    );
    let mut stderr_state = CaptureState::new(
        output_limit,
        log_directory.map(|dir| dir.join("stderr.log")),
    );
    // Create both logs before executing user code; publication errors cannot
    // silently produce an unobservable command.
    stdout_state.push(&[])?;
    stderr_state.push(&[])?;
    let mut process = shell(command, sandbox)?;
    process
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    process.as_std_mut().process_group(0);
    #[cfg(windows)]
    let mut group = windows_job::Job::new(&mut process)?;
    let mut child = process.spawn()?;
    #[cfg(unix)]
    let mut group = ProcessGroup(child.id());
    #[cfg(windows)]
    group.attach_and_resume(&child)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let operation = async {
        #[cfg(unix)]
        let wait = async {
            observe_exit(child.id().expect("spawned child")).await?;
            group.terminate();
            child.wait().await
        };
        #[cfg(windows)]
        let wait = async {
            let result = child.wait().await;
            group.terminate();
            result
        };
        let (status, (), ()) = tokio::try_join!(
            wait,
            capture(stdout, &mut stdout_state),
            capture(stderr, &mut stderr_state)
        )?;
        Ok(std::process::Output {
            status,
            stdout: stdout_state.describe().into_bytes(),
            stderr: stderr_state.describe().into_bytes(),
        })
    };
    let result = tokio::time::timeout(timeout, operation).await;
    // 超时也要收尾：被截断的日志正是最需要真实末尾的那一种。
    stdout_state.finish()?;
    stderr_state.finish()?;
    match result {
        Ok(Ok(output)) => Ok(output),
        result => {
            let error = match result {
                Ok(Err(error)) => error,
                Err(_) => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command execution deadline exceeded",
                ),
                Ok(Ok(_)) => unreachable!(),
            };
            Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}\nstdout:\n{}\nstderr:\n{}",
                    stdout_state.describe(),
                    stderr_state.describe()
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::sandbox::SandboxPolicy;

    #[tokio::test]
    async fn capture_drains_large_stream_and_retains_tail() {
        let data = vec![b'x'; 100_000];
        let mut state = CaptureState::new(31, None);
        capture(data.as_slice(), &mut state).await.unwrap();
        assert_eq!(state.snapshot(), vec![b'x'; 31]);
        assert_eq!(state.omitted, 99_969);
        let mut empty = CaptureState::new(0, None);
        capture(data.as_slice(), &mut empty).await.unwrap();
        assert!(empty.snapshot().is_empty());
    }

    /// 日志是完整追加的；撞上单文件上限就停写，但真实末尾另存进 `.tail`。
    #[tokio::test]
    async fn capped_log_keeps_its_real_end_in_a_sidecar() {
        let dir = std::env::temp_dir().join(format!("willdeep-capture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stdout.log");
        let mut state = CaptureState::new(8, Some(path.clone()));
        state.log_cap = 20;
        let data = b"0123456789abcdefghijklmnopqrstuvwxyzEND".to_vec();
        capture(data.as_slice(), &mut state).await.unwrap();
        state.finish().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), &data[..20]);
        assert_eq!(std::fs::read(sidecar(&path, "tail")).unwrap(), b"vwxyzEND");
        assert_eq!(
            std::fs::read_to_string(sidecar(&path, "dropped")).unwrap(),
            (data.len() - 20).to_string()
        );

        let uncapped = dir.join("stderr.log");
        let mut state = CaptureState::new(4, Some(uncapped.clone()));
        capture(data.as_slice(), &mut state).await.unwrap();
        state.finish().unwrap();
        assert_eq!(std::fs::read(&uncapped).unwrap(), data);
        assert!(!sidecar(&uncapped, "tail").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descendant_holding_output_pipe_is_cleaned_after_leader_exit() {
        let started = std::time::Instant::now();
        let result = run_capture(
            "sleep 30 &",
            Path::new("/tmp"),
            &SandboxSpec::new(SandboxPolicy::Off, []),
            Duration::from_millis(100),
            100,
        )
        .await;
        assert!(result.unwrap().status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn running_command_obeys_deadline() {
        let result = run_capture(
            "sleep 30",
            Path::new("/tmp"),
            &SandboxSpec::new(SandboxPolicy::Off, []),
            Duration::from_millis(100),
            100,
        )
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_preserves_both_output_streams() {
        let result = run_capture(
            "printf before-timeout; printf failure-context >&2; sleep 30",
            Path::new("/tmp"),
            &SandboxSpec::new(SandboxPolicy::Off, []),
            Duration::from_millis(200),
            100,
        )
        .await;
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("before-timeout"));
        assert!(error.to_string().contains("failure-context"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_descendants_before_delayed_write() {
        let marker = std::env::temp_dir().join(format!("willdeep-cancel-{}", uuid::Uuid::new_v4()));
        let command = format!("(sleep 1; touch '{}') & wait", marker.display());
        let task = tokio::spawn(async move {
            run_capture(
                &command,
                Path::new("/tmp"),
                &SandboxSpec::new(SandboxPolicy::Off, []),
                Duration::from_secs(10),
                100,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(!marker.exists(), "cancelled command left a live descendant");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn both_command_streams_are_bounded_without_deadlock() {
        let output = run_capture(
            "i=0; while [ $i -lt 10000 ]; do printf abcdefgh; printf ijklmnop >&2; i=$((i+1)); done",
            Path::new("/tmp"), &SandboxSpec::new(SandboxPolicy::Off, []), Duration::from_secs(10), 127
        ).await.unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() < 200);
        assert!(output.stderr.len() < 200);
        assert!(String::from_utf8_lossy(&output.stdout).contains("earlier bytes omitted"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("earlier bytes omitted"));
    }
}
