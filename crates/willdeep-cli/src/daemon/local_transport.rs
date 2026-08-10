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

#[cfg(windows)]
pub(super) struct WindowsNamedPipeListener {
    name: std::ffi::OsString,
    server: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
impl WindowsNamedPipeListener {
    fn bind(name: impl Into<std::ffi::OsString>) -> std::io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let name = name.into();
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        Ok(Self { name, server })
    }

    fn next(
        name: &std::ffi::OsStr,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        use tokio::net::windows::named_pipe::ServerOptions;

        ServerOptions::new()
            .reject_remote_clients(true)
            .create(name)
    }
}

#[cfg(windows)]
impl axum::serve::Listener for WindowsNamedPipeListener {
    type Io = tokio::net::windows::named_pipe::NamedPipeServer;
    type Addr = String;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Err(error) = self.server.connect().await {
                eprintln!("accept Runtime Named Pipe connection: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            match Self::next(&self.name) {
                Ok(next) => {
                    let connected = std::mem::replace(&mut self.server, next);
                    return (connected, self.name.to_string_lossy().into_owned());
                }
                Err(error) => {
                    eprintln!("create next Runtime Named Pipe instance: {error}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.name.to_string_lossy().into_owned())
    }
}
