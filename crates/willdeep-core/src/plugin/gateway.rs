//! 插件 MCP 网关的共享部分：发现文件、插件自己的 HTTP 入口、客户端调用。
//!
//! 契约见 `docs/decisions/2026-09-26-plugin-mcp-gateway.md`（与短剧工坊仓库
//! `docs/decisions/0001-plugin-mcp-gateway.md` 同一份）。网关服务端在
//! `willdeep web` 进程里（它持有插件宿主），聊天 harness 在另一个进程，
//! 两边都要读这份发现文件，所以放在 core。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mcp::McpError;

pub const DISCOVERY_FILE: &str = "mcp-gateway.json";
pub const DISCOVERY_VERSION: u32 = 1;
/// 发现文件里的 `host` 字段；macOS 宿主写 `willdeep-macos`。
pub const HOST_ID: &str = "willdeep-rs";
/// 插件自己在数据目录写的 HTTP 入口文件（`{"url","token"}`）。
pub const PLUGIN_ENDPOINT_FILE: &str = "mcp-http.json";
/// 转发到插件自己 HTTP 入口的超时：出图、审核一次一两分钟，给足。
pub const FORWARD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayDiscovery {
    pub version: u32,
    pub host: String,
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub servers: Vec<GatewayServerEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayServerEntry {
    #[serde(rename = "pluginID")]
    pub plugin_id: String,
    pub server: String,
    pub url: String,
}

pub fn discovery_path(home: &Path) -> PathBuf {
    home.join(DISCOVERY_FILE)
}

/// 网关上某个插件服务的端点。
/// 两段都做百分号编码：服务名来自插件的 `mcp.json`，不保证是 URL 安全的字符。
pub fn server_url(base: &str, plugin_id: &str, server: &str) -> String {
    format!(
        "{}/plugins/{}/{}/mcp",
        base.trim_end_matches('/'),
        encode_segment(plugin_id),
        encode_segment(server)
    )
}

fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 64 位十六进制 token。两个 v4 UUID 各 122 位随机，拼起来远超所需。
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn is_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `http://127.0.0.1:<端口>` 形式的回环地址里的端口；别的形状一律不认。
pub fn loopback_port(url: &str) -> Option<u16> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "http" || parsed.host_str() != Some("127.0.0.1") {
        return None;
    }
    parsed.port()
}

impl GatewayDiscovery {
    /// 端口与 token 都合法才算数：一份被改坏的文件不该被沿用。
    pub fn port(&self) -> Option<u16> {
        (self.version == DISCOVERY_VERSION && is_token(&self.token))
            .then(|| loopback_port(&self.url))
            .flatten()
    }
}

pub fn read_discovery(home: &Path) -> Option<GatewayDiscovery> {
    let source = std::fs::read_to_string(discovery_path(home)).ok()?;
    let discovery: GatewayDiscovery = serde_json::from_str(&source).ok()?;
    discovery.port().map(|_| discovery)
}

/// 权限 0600 写入：先建临时文件（建时就是 0600，没有可被别人读到的窗口），再改名。
pub fn write_discovery(home: &Path, discovery: &GatewayDiscovery) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let path = discovery_path(home);
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(discovery).map_err(std::io::Error::other)?;
    write_private(&temporary, &body)?;
    std::fs::rename(&temporary, &path)
}

/// 以 0600 新建（或截断）文件并写入。已存在的文件先删掉，确保权限位是新建时给的。
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// 插件自己的数据目录，按查找顺序：先 willdeep-rs 自己的，再 macOS 宿主的——
/// 插件（短剧工坊）在 macOS 上固定把 `mcp-http.json` 写在后者。
pub fn plugin_data_dirs(home: &Path, plugin_id: &str) -> Vec<PathBuf> {
    let mut dirs = vec![home.join("plugin-data").join(plugin_id)];
    if let Some(user_home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        dirs.push(
            PathBuf::from(user_home)
                .join("Library")
                .join("Application Support")
                .join("WillDeep")
                .join("plugin-data")
                .join(plugin_id),
        );
    }
    dirs
}

/// 插件公布的本机 HTTP 入口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEndpoint {
    pub url: String,
    pub token: String,
}

/// 按顺序在数据目录里找 `mcp-http.json`。url 必须是 `http://127.0.0.1:<端口>/…`：
/// 这份文件在用户目录里，谁都能写，不能让它把请求（连同插件 token）引去别处。
pub fn read_plugin_endpoint(dirs: &[PathBuf]) -> Option<PluginEndpoint> {
    dirs.iter().find_map(|dir| {
        let source = std::fs::read_to_string(dir.join(PLUGIN_ENDPOINT_FILE)).ok()?;
        let value: Value = serde_json::from_str(&source).ok()?;
        let url = value.get("url")?.as_str()?.trim().to_owned();
        let token = value.get("token")?.as_str()?.trim().to_owned();
        (loopback_port(&url).is_some() && !token.is_empty())
            .then_some(PluginEndpoint { url, token })
    })
}

/// 经网关调用失败的两种情况：网关压根不在（或不认识这个插件），调用方可以
/// 退回本进程自己的插件宿主；网关在、但请求本身失败，照实报错。
#[derive(Debug)]
pub enum GatewayCallError {
    Unavailable(String),
    Failed(McpError),
}

/// 经网关向某个插件服务发一条 JSON-RPC 请求，返回 `result`。
pub async fn call_via_gateway(
    home: &Path,
    plugin_id: &str,
    server: &str,
    method: &str,
    params: Value,
) -> Result<Value, GatewayCallError> {
    let discovery = read_discovery(home)
        .ok_or_else(|| GatewayCallError::Unavailable("no gateway discovery file".to_owned()))?;
    let url = server_url(&discovery.url, plugin_id, server);
    let client = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .no_proxy()
        .build()
        .map_err(|error| GatewayCallError::Unavailable(error.without_url().to_string()))?;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response = client
        .post(&url)
        .bearer_auth(&discovery.token)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() {
                GatewayCallError::Unavailable("gateway is not running".to_owned())
            } else {
                GatewayCallError::Failed(McpError::from(error))
            }
        })?;
    let status = response.status();
    // 401：发现文件是另一个宿主实例留下的；404：网关进程不认识这个插件（比如
    // 它启动之后才装的）。两种都不是请求本身的错，退回本进程宿主。
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::NOT_FOUND {
        return Err(GatewayCallError::Unavailable(format!(
            "gateway answered HTTP {}",
            status.as_u16()
        )));
    }
    let text = response
        .text()
        .await
        .map_err(|error| GatewayCallError::Failed(McpError::from(error)))?;
    let value: Value = serde_json::from_str(&text).map_err(|_| {
        GatewayCallError::Failed(McpError::Http {
            server: server.to_owned(),
            status: status.as_u16(),
            body: text.chars().take(300).collect(),
        })
    })?;
    if let Some(error) = value.get("error") {
        return Err(GatewayCallError::Failed(McpError::Remote(
            error.to_string(),
        )));
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "willdeep-gateway-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        root
    }

    #[test]
    fn discovery_round_trips_with_private_permissions() {
        let home = scratch();
        let discovery = GatewayDiscovery {
            version: DISCOVERY_VERSION,
            host: HOST_ID.to_owned(),
            url: "http://127.0.0.1:47831".to_owned(),
            token: generate_token(),
            servers: vec![GatewayServerEntry {
                plugin_id: "demo".to_owned(),
                server: "srv".to_owned(),
                url: server_url("http://127.0.0.1:47831", "demo", "srv"),
            }],
        };
        write_discovery(&home, &discovery).expect("write");
        assert_eq!(read_discovery(&home), Some(discovery.clone()));
        assert_eq!(discovery.port(), Some(47831));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(discovery_path(&home))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(discovery_path(&home)).unwrap()).unwrap();
        assert_eq!(json["servers"][0]["pluginID"], "demo");
        assert_eq!(
            json["servers"][0]["url"],
            "http://127.0.0.1:47831/plugins/demo/srv/mcp"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn server_urls_percent_encode_their_segments() {
        assert_eq!(
            server_url("http://127.0.0.1:1/", "demo", "a b/c"),
            "http://127.0.0.1:1/plugins/demo/a%20b%2Fc/mcp"
        );
    }

    #[test]
    fn tampered_discovery_is_not_reused() {
        let home = scratch();
        for (url, token) in [
            ("http://example.com:47831", generate_token()),
            ("http://127.0.0.1:47831", "short".to_owned()),
            ("https://127.0.0.1:47831", generate_token()),
        ] {
            let discovery = GatewayDiscovery {
                version: DISCOVERY_VERSION,
                host: HOST_ID.to_owned(),
                url: url.to_owned(),
                token,
                servers: Vec::new(),
            };
            write_discovery(&home, &discovery).expect("write");
            assert!(read_discovery(&home).is_none(), "{url} must be rejected");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn plugin_endpoint_must_be_loopback_and_first_directory_wins() {
        let first = scratch();
        let second = scratch();
        std::fs::write(
            second.join(PLUGIN_ENDPOINT_FILE),
            r#"{"url":"http://127.0.0.1:5000/mcp","token":"b"}"#,
        )
        .unwrap();
        let dirs = vec![first.clone(), second.clone()];
        assert_eq!(
            read_plugin_endpoint(&dirs).map(|endpoint| endpoint.token),
            Some("b".to_owned())
        );
        std::fs::write(
            first.join(PLUGIN_ENDPOINT_FILE),
            r#"{"url":"http://evil.example:5000/mcp","token":"a"}"#,
        )
        .unwrap();
        assert_eq!(
            read_plugin_endpoint(&dirs).map(|endpoint| endpoint.token),
            Some("b".to_owned()),
            "a non-loopback url is skipped"
        );
        std::fs::write(
            first.join(PLUGIN_ENDPOINT_FILE),
            r#"{"url":"http://127.0.0.1:6000/mcp","token":"a"}"#,
        )
        .unwrap();
        assert_eq!(
            read_plugin_endpoint(&dirs).map(|endpoint| endpoint.token),
            Some("a".to_owned())
        );
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }
}
