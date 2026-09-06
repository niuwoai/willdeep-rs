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

struct CaptureState {
    bytes: std::collections::VecDeque<u8>,
    limit: usize,
    omitted: u64,
    path: Option<std::path::PathBuf>,
}

impl CaptureState {
    fn new(limit: usize, path: Option<std::path::PathBuf>) -> Self {
        Self {
            bytes: std::collections::VecDeque::with_capacity(limit),
            limit,
            omitted: 0,
            path,
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
        if let Some(path) = &self.path {
            crate::detached_job::write_private_atomic(
                path,
                &self.bytes.iter().copied().collect::<Vec<_>>(),
            )?;
        }
        Ok(())
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
    match tokio::time::timeout(timeout, operation).await {
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
