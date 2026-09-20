//! 示例共用：找到本机 Runtime，按 `daemon.json` 里的传输方式建客户端，拆响应信封。
//!
//! 发现 daemon 的方式是 `willdeep` CLI 的约定，不是 SDK 的一部分；这里照抄它：
//! `$WILLDEEP_HOME/runtime/daemon.json` 里有 Token、回环地址，以及可选的本机传输
//! （Unix socket 或 Windows named pipe）。有本机传输优先用本机传输。

#![allow(dead_code)]

use std::path::PathBuf;

use willdeep_runtime_client::RuntimeClient;
use willdeep_runtime_protocol::ApiResponse;

pub fn willdeep_home() -> PathBuf {
    if let Ok(home) = std::env::var("WILLDEEP_HOME") {
        return PathBuf::from(home);
    }
    let user_home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(user_home).join(".willdeep")
}

pub fn connect() -> Result<RuntimeClient, Box<dyn std::error::Error>> {
    let path = willdeep_home().join("runtime/daemon.json");
    let raw = std::fs::read(&path).map_err(|error| {
        format!(
            "read {} ({error}); is the Runtime running? try `willdeep daemon start`",
            path.display()
        )
    })?;
    let state: serde_json::Value = serde_json::from_slice(&raw)?;
    let token = state["token"]
        .as_str()
        .ok_or("daemon.json carries no token")?;
    let transport = &state["local_transport"];
    #[cfg(unix)]
    if transport["kind"] == "unix_socket"
        && let Some(socket) = transport["path"].as_str()
    {
        return Ok(RuntimeClient::new_unix_socket(socket, token)?);
    }
    #[cfg(windows)]
    if transport["kind"] == "windows_named_pipe"
        && let Some(name) = transport["name"].as_str()
    {
        return Ok(RuntimeClient::new_windows_named_pipe(name, token)?);
    }
    let address = state["address"]
        .as_str()
        .ok_or("daemon.json carries no address")?;
    Ok(RuntimeClient::new(format!("http://{address}"), token)?)
}

/// 拆信封：业务层的拒绝变成错误，带稳定错误码与 `retryable`。
pub fn unwrap<T>(response: ApiResponse<T>) -> Result<T, Box<dyn std::error::Error>> {
    match response {
        ApiResponse::Ok { data, .. } => Ok(data),
        ApiResponse::Error { error, .. } => Err(error.into()),
    }
}
