use std::path::{Path, PathBuf};

#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum LocalTransportState {
    UnixSocket { path: PathBuf },
    WindowsNamedPipe { name: String },
}

#[cfg(unix)]
pub(super) type LocalListener = tokio::net::UnixListener;

#[cfg(windows)]
pub(super) type LocalListener = WindowsNamedPipeListener;

#[cfg(unix)]
pub(super) fn bind(path: &Path, _token: &str) -> Result<(LocalListener, LocalTransportState)> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to replace non-socket Runtime endpoint: {}",
                path.display()
            );
        }
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path).context("bind Runtime Unix Socket")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok((
        listener,
        LocalTransportState::UnixSocket {
            path: path.to_path_buf(),
        },
    ))
}

#[cfg(windows)]
pub(super) fn bind(_path: &Path, token: &str) -> Result<(LocalListener, LocalTransportState)> {
    let name = format!(r"\\.\pipe\willdeep-{token}");
    let listener =
        WindowsNamedPipeListener::bind(&name).context("bind Runtime Windows Named Pipe")?;
    Ok((listener, LocalTransportState::WindowsNamedPipe { name }))
}

#[cfg(unix)]
pub(super) fn remove_if_owned(path: &Path) {
    use std::os::unix::fs::FileTypeExt;

    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(windows)]
pub(super) fn remove_if_owned(_path: &Path) {}

/// 同时挂着等连接的管道实例数。
///
/// Named Pipe 的每个实例只服务一个客户端。客户端连上时如果没有空闲实例，拿到的是
/// `ERROR_PIPE_BUSY`，reqwest 的 Named Pipe 连接器不重试，直接报「error sending
/// request」。以前只挂一个实例、等它被连上后才建下一个，中间那段空窗里的并发连接
/// （事件流 + 状态查询 + 正在跑的 turn）就会随机失败。始终预留几个等待中的实例，
/// 一个被占用时其余的还能接住，同时补建一个。
#[cfg(windows)]
const PIPE_INSTANCE_BACKLOG: usize = 4;

#[cfg(windows)]
type PendingPipe = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = (
                    tokio::net::windows::named_pipe::NamedPipeServer,
                    std::io::Result<()>,
                ),
            > + Send,
    >,
>;

#[cfg(windows)]
pub(super) struct WindowsNamedPipeListener {
    name: std::ffi::OsString,
    pending: futures_util::stream::FuturesUnordered<PendingPipe>,
}

#[cfg(windows)]
impl WindowsNamedPipeListener {
    fn bind(name: impl Into<std::ffi::OsString>) -> std::io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let name = name.into();
        // 第一个实例带 first_pipe_instance：同名管道已被别人占着时直接失败，不和它共用。
        let first = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        let mut listener = Self {
            name,
            pending: futures_util::stream::FuturesUnordered::new(),
        };
        listener.pending.push(Self::wait_for_client(first));
        listener.refill()?;
        Ok(listener)
    }

    fn wait_for_client(server: tokio::net::windows::named_pipe::NamedPipeServer) -> PendingPipe {
        Box::pin(async move {
            let connected = server.connect().await;
            (server, connected)
        })
    }

    /// 把等待中的实例补回 [`PIPE_INSTANCE_BACKLOG`] 个。
    fn refill(&mut self) -> std::io::Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;

        while self.pending.len() < PIPE_INSTANCE_BACKLOG {
            let server = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&self.name)?;
            self.pending.push(Self::wait_for_client(server));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl axum::serve::Listener for WindowsNamedPipeListener {
    type Io = tokio::net::windows::named_pipe::NamedPipeServer;
    type Addr = String;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        use futures_util::StreamExt;

        loop {
            if let Err(error) = self.refill() {
                eprintln!("create Runtime Named Pipe instance: {error}");
                if self.pending.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            }
            let Some((server, connected)) = self.pending.next().await else {
                continue;
            };
            match connected {
                Ok(()) => {
                    // 先补一个新实例，再把这个交出去：交出之后到下一次 accept 之间，
                    // 等待中的实例数不低于 backlog - 1。
                    if let Err(error) = self.refill() {
                        eprintln!("create Runtime Named Pipe instance: {error}");
                    }
                    return (server, self.name.to_string_lossy().into_owned());
                }
                Err(error) => {
                    eprintln!("accept Runtime Named Pipe connection: {error}");
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.name.to_string_lossy().into_owned())
    }
}
