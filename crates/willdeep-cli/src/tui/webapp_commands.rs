use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::i18n::Language;

const DEFAULT_WEBAPP_LISTEN: &str = "127.0.0.1:9847";

#[derive(Debug, Serialize, Deserialize)]
struct WebAppState {
    pid: u32,
    listen: SocketAddr,
    workspace: PathBuf,
}

pub(super) async fn handle_webapp_command(
    prompt: &str,
    home: &Path,
    workspace: &Path,
    config: Option<&Path>,
    profile: Option<&str>,
    language: Language,
) -> Result<Option<String>> {
    let value = prompt.trim();
    if value != "/webapp" && !value.starts_with("/webapp ") {
        return Ok(None);
    }
    let argument = value.strip_prefix("/webapp").unwrap_or_default().trim();
    let state_path = home.join("webapp-state.json");
    if argument.eq_ignore_ascii_case("status") {
        return Ok(Some(status_message(&state_path, language).await));
    }
    let listen = if argument.is_empty() || argument.eq_ignore_ascii_case("start") {
        DEFAULT_WEBAPP_LISTEN
            .parse()
            .expect("valid default address")
    } else {
        argument
            .parse::<SocketAddr>()
            .context("usage: /webapp [status|127.0.0.1:PORT]")?
    };
    if !listen.ip().is_loopback() {
        bail!(
            "/webapp only accepts a loopback address; use a reverse proxy or tunnel for remote access"
        );
    }
    if let Some(existing) = read_state(&state_path)
        && endpoint_is_live(existing.listen).await
    {
        return Ok(Some(format!(
            "{} · http://{}",
            language.text(
                "System: Web App 已在运行",
                "System: Web App is already running",
                "System: Web App はすでに実行中です",
            ),
            existing.listen
        )));
    }
    let state = start_webapp(home, workspace, config, profile, listen)?;
    write_private_json(&state_path, &state)?;
    for _ in 0..30 {
        if endpoint_is_live(listen).await {
            return Ok(Some(format!(
                "{} · http://{listen}",
                language.text(
                    "System: Web App 已启动",
                    "System: Web App started",
                    "System: Web App を起動しました",
                )
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!(
        "Web App did not become ready; inspect {}",
        home.join("webapp.log").display()
    )
}

fn start_webapp(
    home: &Path,
    workspace: &Path,
    config: Option<&Path>,
    profile: Option<&str>,
    listen: SocketAddr,
) -> Result<WebAppState> {
    std::fs::create_dir_all(home).context("create WillDeep home")?;
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolve Web App workspace: {}", workspace.display()))?;
    let log_path = home.join("webapp.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open Web App log: {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe().context("locate current WillDeep executable")?;
    let mut command = Command::new(executable);
    command
        .arg("--web")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--listen")
        .arg(listen.to_string());
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn embedded Web App")?;
    Ok(WebAppState {
        pid: child.id(),
        listen,
        workspace,
    })
}

async fn status_message(path: &Path, language: Language) -> String {
    let Some(state) = read_state(path) else {
        return language
            .text(
                "System: Web App 未启动；输入 /webapp 启动",
                "System: Web App is stopped; enter /webapp to start it",
                "System: Web App は停止中です。/webapp で起動できます",
            )
            .to_owned();
    };
    if endpoint_is_live(state.listen).await {
        format!(
            "{} · http://{} · pid {}",
            language.text(
                "System: Web App 正在运行",
                "System: Web App is running",
                "System: Web App は実行中です",
            ),
            state.listen,
            state.pid
        )
    } else {
        language
            .text(
                "System: Web App 已退出；输入 /webapp 可重新启动",
                "System: Web App has exited; enter /webapp to restart it",
                "System: Web App は終了しました。/webapp で再起動できます",
            )
            .to_owned()
    }
}

async fn endpoint_is_live(listen: SocketAddr) -> bool {
    tokio::time::timeout(
        std::time::Duration::from_millis(300),
        tokio::net::TcpStream::connect(listen),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

fn read_state(path: &Path) -> Option<WebAppState> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_private_json(path: &Path, value: &WebAppState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(&temporary)?, &bytes)?;
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_stopped_and_rejects_non_loopback_address() {
        let root = std::env::temp_dir().join(format!("willdeep-webapp-{}", uuid::Uuid::new_v4()));
        let message =
            handle_webapp_command("/webapp status", &root, &root, None, None, Language::ZhCn)
                .await
                .unwrap()
                .unwrap();
        assert!(message.contains("未启动"));
        assert!(
            handle_webapp_command(
                "/webapp 0.0.0.0:9847",
                &root,
                &root,
                None,
                None,
                Language::En,
            )
            .await
            .is_err()
        );
        assert!(
            handle_webapp_command("normal prompt", &root, &root, None, None, Language::En)
                .await
                .unwrap()
                .is_none()
        );
    }
}
