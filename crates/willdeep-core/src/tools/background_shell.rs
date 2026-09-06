use super::*;
#[cfg(not(test))]
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Serialize, Deserialize)]
struct BackgroundSupervisorRequest {
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
    sandbox: SandboxSpec,
    #[serde(default)]
    detached_directory: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackgroundSupervisorResult {
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
    output: String,
}

#[cfg(not(test))]
pub(super) async fn run_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
    sandbox: SandboxSpec,
) -> TaskResult {
    match run_supervised_background_shell(command, workspace, timeout_seconds, sandbox).await {
        Ok(result) => TaskResult {
            status: result.status,
            exit_code: result.exit_code,
            output: result.output,
        },
        Err(error) => TaskResult {
            status: BackgroundTaskStatus::LaunchFailed,
            exit_code: Some(-1),
            output: format!("background supervisor failed: {error}"),
        },
    }
}

#[cfg(test)]
pub(super) async fn run_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
    sandbox: SandboxSpec,
) -> TaskResult {
    execute_background(&command, &workspace, timeout_seconds, &sandbox, None).await
}

async fn execute_background(
    command: &str,
    workspace: &Path,
    timeout_seconds: u64,
    sandbox: &SandboxSpec,
    log_directory: Option<&Path>,
) -> TaskResult {
    match crate::execution::run_capture_logged(
        command,
        workspace,
        sandbox,
        std::time::Duration::from_secs(timeout_seconds),
        MAX_COMMAND_OUTPUT_BYTES,
        log_directory,
    )
    .await
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            TaskResult {
                status: if output.status.success() {
                    BackgroundTaskStatus::Completed
                } else {
                    BackgroundTaskStatus::Failed
                },
                exit_code: output.status.code(),
                output: truncate_bytes(text, MAX_COMMAND_OUTPUT_BYTES),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => TaskResult {
            status: BackgroundTaskStatus::TimedOut,
            exit_code: None,
            output: format!("command timed out after {timeout_seconds} seconds: {error}"),
        },
        Err(error) => TaskResult {
            status: BackgroundTaskStatus::LaunchFailed,
            exit_code: Some(-1),
            output: error.to_string(),
        },
    }
}

#[cfg(not(test))]
async fn run_supervised_background_shell(
    command: String,
    workspace: PathBuf,
    timeout_seconds: u64,
    sandbox: SandboxSpec,
) -> anyhow::Result<BackgroundSupervisorResult> {
    let request = BackgroundSupervisorRequest {
        command,
        workspace,
        timeout_seconds,
        sandbox,
        detached_directory: None,
    };
    let payload = serde_json::to_vec(&request)?;
    anyhow::ensure!(
        payload.len() <= MAX_SUPERVISOR_REQUEST_BYTES,
        "background supervisor request is too large"
    );
    let executable = std::env::current_exe()?;
    let mut process = Command::new(executable);
    process
        .args(["daemon", "background-supervisor"])
        .env(BACKGROUND_SUPERVISOR_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let mut child = process.spawn()?;
    let mut liveness = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stdin is unavailable"))?;
    let length = u32::try_from(payload.len())?.to_be_bytes();
    liveness.write_all(&length).await?;
    liveness.write_all(&payload).await?;
    liveness.flush().await?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("background supervisor stderr is unavailable"))?;
    // JSON escaping can expand each retained byte to six bytes (e.g. NUL).
    const MAX_RESULT_ENVELOPE_BYTES: usize = MAX_COMMAND_OUTPUT_BYTES * 6 + 4096;
    let stdout_task = tokio::spawn(read_bounded(stdout, MAX_RESULT_ENVELOPE_BYTES));
    let stderr_task = tokio::spawn(read_bounded(stderr, MAX_COMMAND_OUTPUT_BYTES));
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_seconds.saturating_add(10)),
        child.wait(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("background supervisor did not stop after its deadline"))??;
    drop(liveness);
    let stdout = stdout_task.await??;
    let stderr = stderr_task.await??;
    anyhow::ensure!(
        status.success(),
        "background supervisor exited with {:?}: {}",
        status.code(),
        String::from_utf8_lossy(&stderr).trim()
    );
    serde_json::from_slice(&stdout)
        .map_err(|error| anyhow::anyhow!("decode background supervisor result: {error}"))
}

pub async fn run_background_supervisor() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var(BACKGROUND_SUPERVISOR_ENV).as_deref() == Ok("1"),
        "background supervisor is an internal command"
    );
    let mut input = tokio::io::stdin();
    let mut length = [0_u8; 4];
    input.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        length <= MAX_SUPERVISOR_REQUEST_BYTES,
        "background supervisor request is too large"
    );
    let mut payload = vec![0_u8; length];
    input.read_exact(&mut payload).await?;
    let request: BackgroundSupervisorRequest = serde_json::from_slice(&payload)?;
    anyhow::ensure!(
        !request.command.trim().is_empty(),
        "background command is empty"
    );
    anyhow::ensure!(
        request.timeout_seconds > 0 && request.timeout_seconds <= MAX_COMMAND_TIMEOUT_SECS,
        "background command timeout is invalid"
    );
    let workspace = request.workspace.canonicalize()?;
    anyhow::ensure!(
        workspace.is_dir(),
        "background Workspace is not a directory"
    );

    if let Some(directory) = request.detached_directory.as_ref() {
        let execution = execute_background(
            &request.command,
            &workspace,
            request.timeout_seconds,
            &request.sandbox,
            Some(directory),
        );
        #[cfg(unix)]
        let result = {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate())?;
            let mut interrupt = signal(SignalKind::interrupt())?;
            tokio::select! {
                result = execution => result,
                _ = terminate.recv() => cancelled_result(),
                _ = interrupt.recv() => cancelled_result(),
            }
        };
        #[cfg(not(unix))]
        let result = execution.await;
        crate::detached_job::record_result(directory, result)?;
        return Ok(());
    }

    drop(input);
    let mut parent_disconnect = watch_parent_disconnect()?;
    let execution = execute_background(
        &request.command,
        &workspace,
        request.timeout_seconds,
        &request.sandbox,
        None,
    );
    let task = tokio::select! {
        result = execution => result,
        parent = &mut parent_disconnect => {
            parent.map_err(|_| anyhow::anyhow!("background parent watcher stopped"))??;
            TaskResult {
                status: BackgroundTaskStatus::Killed,
                exit_code: None,
                output: "background command cancelled after parent disconnected".to_owned(),
            }
        }
    };
    let result = BackgroundSupervisorResult {
        status: task.status,
        exit_code: task.exit_code,
        output: task.output,
    };
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&serde_json::to_vec(&result)?).await?;
    stdout.flush().await?;
    Ok(())
}

#[cfg(unix)]
fn cancelled_result() -> TaskResult {
    TaskResult {
        status: BackgroundTaskStatus::Killed,
        exit_code: None,
        output: "detached command terminated by host signal".to_owned(),
    }
}

fn watch_parent_disconnect() -> anyhow::Result<tokio::sync::oneshot::Receiver<std::io::Result<()>>>
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("willdeep-parent-watch".to_owned())
        .spawn(move || {
            let mut input = std::io::stdin();
            let mut buffer = [0_u8; 64];
            let result = loop {
                match std::io::Read::read(&mut input, &mut buffer) {
                    Ok(0) => break Ok(()),
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
            };
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}

#[cfg(not(test))]
async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..read.min(remaining)]);
    }
}
