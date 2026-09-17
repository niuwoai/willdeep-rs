//! 按行流式读取命令输出（`monitor` 工具的事件源）。
//!
//! 与 [`super::run_capture_logged`] 共用同一套 shell 构造、沙箱、进程组与日志落盘：
//! 两份日志照样完整追加写、照样有 256MB 上限与 `.tail` / `.dropped` 旁挂文件。
//! 唯一的区别是 stdout 在落盘的同时按 `\n` 切成行送进通道；stderr 只进日志。

use std::{io, path::Path, process::Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot};

use super::{CaptureState, capture, shell};
use crate::sandbox::SandboxSpec;

/// 内存里只留这么多字节的尾巴；完整输出在日志里。
const STREAM_TAIL_BYTES: usize = 64 * 1024;

pub(crate) enum LineStreamEnd {
    /// 进程自己结束了。
    Exited(std::process::ExitStatus),
    /// 调用方要求停止，进程组已被杀掉。
    Stopped,
}

/// 把字节块切成行。行尾的 `\r` 去掉，非 UTF-8 按有损解码；一行超过
/// `max_chars` 个字符时截断并以 `…` 结尾（结果恰好 `max_chars` 个字符）。
///
/// 没有换行的超长行不会无限占内存：缓冲到上限之后丢弃后续字节，直到换行。
pub(crate) struct LineSplitter {
    pending: Vec<u8>,
    overflowed: bool,
    max_chars: usize,
}

impl LineSplitter {
    pub(crate) fn new(max_chars: usize) -> Self {
        Self {
            pending: Vec::new(),
            overflowed: false,
            max_chars: max_chars.max(1),
        }
    }

    fn byte_budget(&self) -> usize {
        // 一个字符最多 4 字节；多留一个字符的余量，才知道「确实超了」。
        (self.max_chars + 1) * 4
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let mut rest = chunk;
        while let Some(position) = rest.iter().position(|byte| *byte == b'\n') {
            self.append(&rest[..position]);
            lines.push(self.take_line());
            rest = &rest[position + 1..];
        }
        self.append(rest);
        lines
    }

    /// 流结束时剩下的半行。
    pub(crate) fn finish(&mut self) -> Option<String> {
        (!self.pending.is_empty() || self.overflowed).then(|| self.take_line())
    }

    fn append(&mut self, bytes: &[u8]) {
        let room = self.byte_budget().saturating_sub(self.pending.len());
        if bytes.len() > room {
            self.overflowed = true;
        }
        self.pending
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    fn take_line(&mut self) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        let overflowed = std::mem::replace(&mut self.overflowed, false);
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        let text = String::from_utf8_lossy(&bytes);
        truncate_line(&text, self.max_chars, overflowed)
    }
}

/// 截到 `max_chars` 个字符：超了就保留前 `max_chars - 1` 个再加 `…`。
pub(crate) fn truncate_line(line: &str, max_chars: usize, force_marker: bool) -> String {
    let count = line.chars().count();
    if count <= max_chars && !force_marker {
        return line.to_owned();
    }
    let kept = line
        .chars()
        .take(max_chars.saturating_sub(1).min(count))
        .collect::<String>();
    format!("{kept}…")
}

async fn capture_lines(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    state: &mut CaptureState,
    lines: &mpsc::Sender<String>,
    max_chars: usize,
) -> io::Result<()> {
    let mut splitter = LineSplitter::new(max_chars);
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            if let Some(line) = splitter.finish() {
                let _ = lines.send(line).await;
            }
            return Ok(());
        }
        state.push(&buffer[..count])?;
        for line in splitter.push(&buffer[..count]) {
            // 接收方不在了也要把流读干净：否则管道写满，命令会卡死在写上。
            let _ = lines.send(line).await;
        }
    }
}

/// 跑一条命令，stdout 按行送进 `lines`，两条流完整落盘到 `log_directory`
/// 下的 `stdout.log` / `stderr.log`。`stop` 收到信号（或发送端被丢弃）时杀掉
/// 整个进程组并返回 [`LineStreamEnd::Stopped`]。
///
/// 返回时 `lines` 的发送端已经释放，接收方把通道读空就能看到结束。
pub(crate) async fn run_line_stream(
    command: &str,
    workspace: &Path,
    sandbox: &SandboxSpec,
    log_directory: &Path,
    lines: mpsc::Sender<String>,
    max_line_chars: usize,
    stop: oneshot::Receiver<()>,
) -> io::Result<LineStreamEnd> {
    let mut stdout_state =
        CaptureState::new(STREAM_TAIL_BYTES, Some(log_directory.join("stdout.log")));
    let mut stderr_state =
        CaptureState::new(STREAM_TAIL_BYTES, Some(log_directory.join("stderr.log")));
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
    let mut group = super::windows_job::Job::new(&mut process)?;
    let mut child = process.spawn()?;
    #[cfg(unix)]
    let mut group = super::ProcessGroup(child.id());
    #[cfg(windows)]
    group.attach_and_resume(&child)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = {
        let operation = async {
            #[cfg(unix)]
            let wait = async {
                super::observe_exit(child.id().expect("spawned child")).await?;
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
                capture_lines(stdout, &mut stdout_state, &lines, max_line_chars),
                capture(stderr, &mut stderr_state)
            )?;
            Ok::<_, io::Error>(status)
        };
        tokio::select! {
            result = operation => Some(result),
            _ = stop => None,
        }
    };
    drop(lines);
    group.terminate();
    stdout_state.finish()?;
    stderr_state.finish()?;
    match result {
        Some(status) => Ok(LineStreamEnd::Exited(status?)),
        None => Ok(LineStreamEnd::Stopped),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_joins_chunks_strips_carriage_returns_and_caps_long_lines() {
        let mut splitter = LineSplitter::new(5);
        assert!(splitter.push(b"par").is_empty());
        assert_eq!(splitter.push(b"tial\r\nok\n"), vec!["part…", "ok"]);
        assert_eq!(splitter.push(b"12345\n"), vec!["12345"]);
        assert_eq!(splitter.push(&vec![b'x'; 10_000]), Vec::<String>::new());
        assert_eq!(splitter.push(b"\ntail"), vec!["xxxx…"]);
        assert_eq!(splitter.finish().as_deref(), Some("tail"));
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        let line = "发".repeat(2_500);
        let cut = truncate_line(&line, 2_000, false);
        assert_eq!(cut.chars().count(), 2_000);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate_line("短", 2_000, false), "短");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_lines_stream_before_exit_and_both_streams_are_logged() {
        let dir = std::env::temp_dir().join(format!("willdeep-lines-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let (_stop_tx, stop_rx) = oneshot::channel();
        let command = "printf 'first\\n'; printf 'to-stderr\\n' >&2; sleep 1; printf 'second'";
        let sandbox = SandboxSpec::new(crate::sandbox::SandboxPolicy::Off, []);
        let run = run_line_stream(
            command,
            Path::new("/tmp"),
            &sandbox,
            &dir,
            tx,
            2_000,
            stop_rx,
        );
        tokio::pin!(run);
        let first = tokio::select! {
            line = rx.recv() => line,
            _ = &mut run => panic!("the process finished before its first line arrived"),
        };
        assert_eq!(first.as_deref(), Some("first"));
        assert!(matches!(run.await.unwrap(), LineStreamEnd::Exited(status) if status.success()));
        assert_eq!(rx.recv().await.as_deref(), Some("second"));
        assert_eq!(rx.recv().await, None);
        assert_eq!(
            std::fs::read_to_string(dir.join("stdout.log")).unwrap(),
            "first\nsecond"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("stderr.log")).unwrap(),
            "to-stderr\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_kills_the_process_group() {
        let dir = std::env::temp_dir().join(format!("willdeep-lines-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("late");
        let (tx, _rx) = mpsc::channel(16);
        let (stop_tx, stop_rx) = oneshot::channel();
        let command = format!("(sleep 1; touch '{}') & sleep 30", marker.display());
        let sandbox = SandboxSpec::new(crate::sandbox::SandboxPolicy::Off, []);
        let started = std::time::Instant::now();
        let run = tokio::spawn(async move {
            run_line_stream(
                &command,
                Path::new("/tmp"),
                &sandbox,
                &dir,
                tx,
                2_000,
                stop_rx,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        stop_tx.send(()).unwrap();
        assert!(matches!(
            run.await.unwrap().unwrap(),
            LineStreamEnd::Stopped
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        assert!(!marker.exists(), "a stopped monitor left a live descendant");
    }
}
