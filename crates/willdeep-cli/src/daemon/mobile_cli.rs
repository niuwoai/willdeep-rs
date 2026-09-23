//! 手机中继的客户端侧：`willdeep daemon mobile …` 与 TUI 的 `/mobile` 共用。
//!
//! 中继本身归 Runtime（见 `mobile_gateway.rs`）；这里只是经本机控制面去开、关、查。

use super::*;

#[derive(Clone, Debug, Subcommand)]
pub enum MobileAction {
    /// Turn the relay on (it stays on across Runtime restarts) and print the pairing QR code.
    Enable,
    /// Turn the relay off. Paired phones reconnect once it is enabled again.
    Disable,
    /// Show whether the relay is on, connected, and whether a phone is active.
    Status,
}

/// 打开手机中继并取回配对 URL；必要时先拉起 Runtime。配对 URL 含 relay token，
/// 只用来出二维码。
pub(crate) async fn mobile_enable(
    home: &Path,
) -> Result<willdeep_runtime_protocol::MobileRelayEnabled> {
    let state = ensure_running(home).await?;
    mobile_result(runtime_client(&state)?.mobile_enable().await?)
}

/// 关掉手机中继。Runtime 没在跑时不为了「关」把它拉起来：中继本来就不在线，
/// 只要把开关落盘，下次启动就不会自动重连。
pub(crate) async fn mobile_disable(home: &Path) -> Result<()> {
    if mobile_status(home).await?.is_some() {
        let state = load_state(&DaemonPaths::new(home).state)?;
        mobile_result(runtime_client(&state)?.mobile_disable().await?)?;
        return Ok(());
    }
    if crate::mobile::RelayCredentials::load(home)?.is_some() {
        crate::mobile::RelayCredentials::save_enabled(home, false)?;
    }
    Ok(())
}

/// Runtime 在跑时报中继状态；没在跑时是 `None`（不会为了看一眼把它拉起来）。
pub(crate) async fn mobile_status(
    home: &Path,
) -> Result<Option<willdeep_runtime_protocol::MobileRelayStatus>> {
    let Ok(state) = load_state(&DaemonPaths::new(home).state) else {
        return Ok(None);
    };
    if probe(&state).await.is_err() {
        return Ok(None);
    }
    mobile_result(runtime_client(&state)?.mobile_status().await?).map(Some)
}

fn mobile_result<T>(response: willdeep_runtime_protocol::ApiResponse<T>) -> Result<T> {
    match response.into_result() {
        Ok(data) => Ok(data),
        Err(error) if error.code == willdeep_runtime_protocol::ErrorCode::UnsupportedOperation => {
            bail!(
                "the running Runtime predates the mobile relay; run `willdeep daemon upgrade` first"
            )
        }
        Err(error) => bail!("{}", error.message),
    }
}

pub(super) async fn mobile_cli(home: &Path, action: MobileAction) -> Result<()> {
    match action {
        MobileAction::Enable => {
            let enabled = mobile_enable(home).await?;
            println!("{}", crate::mobile::render_qr(&enabled.pairing_url)?);
            println!(
                "Mobile relay on{}. Scan with WillDeep Mobile; it stays on until `willdeep daemon mobile disable`.",
                enabled
                    .status
                    .relay_host
                    .as_deref()
                    .map(|host| format!(" via {host}"))
                    .unwrap_or_default()
            );
            println!(
                "The QR code carries the relay token: scan it only with your own phone and do not share screenshots."
            );
        }
        MobileAction::Disable => {
            mobile_disable(home).await?;
            println!("Mobile relay off");
        }
        MobileAction::Status => match mobile_status(home).await? {
            Some(status) => {
                println!(
                    "enabled={}\tconnected={}\tphone_active={}\trelay={}",
                    status.enabled,
                    status.connected,
                    status.phone_active,
                    status.relay_host.as_deref().unwrap_or("-"),
                );
            }
            None => {
                let enabled = crate::mobile::RelayCredentials::load(home)?
                    .is_some_and(|credentials| credentials.enabled());
                println!("Runtime is not running\tenabled={enabled}\tconnected=false");
            }
        },
    }
    Ok(())
}
