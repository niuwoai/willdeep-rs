//! MCP 客户端：stdio 子进程与 Streamable HTTP 两种传输，同一个注册表。
//!
//! - stdio：TOML 给 `command`，宿主拉起子进程，stdout 走 JSON-RPC、stderr 继承到终端。
//!   本地工具，配置错了直接失败——那是用户机器上的事，早报比晚报好。
//! - Streamable HTTP：TOML 给 `url`，每条消息一个 POST（见 `http` 子模块）。凭据走
//!   `bearer_token_env` 或 OAuth（`willdeep mcp login <name>`，见 `oauth` 子模块）。
//!   远程服务连不上不该让整个宿主起不来：连接失败只警告、跳过这一个。
//!
//! 工具名统一成 `mcp__<server>__<tool>`；模型看到的是 `list_mcp_tools` / `call_mcp_tool`
//! 两个固定工具，按需搜索，schema 不随每次请求进上下文。

mod http;
pub mod oauth;
#[cfg(test)]
mod test_http;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::types::ToolDefinition;

/// 我们向服务端声明的协议版本。服务端可以回一个更旧的；HTTP 传输之后的请求按它
/// 回的那个带 `MCP-Protocol-Version` 头。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// stdio 传输：要启动的命令。与 `url` 二选一。
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Streamable HTTP 传输：MCP 端点 URL。与 `command` 二选一。
    #[serde(default)]
    pub url: Option<String>,
    /// 随每个 HTTP 请求带上的静态头。值里可以写 `${env:NAME}`；名字看着像凭据
    /// （token / key / secret …）的头必须这么写，明文不收。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 静态 Bearer token 所在的环境变量名。与 `oauth` 二选一。
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// OAuth 2.1 授权码 + PKCE；用 `willdeep mcp login <name>` 登录，token 存在
    /// `$WILLDEEP_HOME/mcp-oauth/<name>.json`。
    #[serde(default)]
    pub oauth: Option<McpOAuthConfig>,
    #[serde(default = "default_timeout")]
    pub startup_timeout_seconds: u64,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            bearer_token_env: None,
            oauth: None,
            startup_timeout_seconds: default_timeout(),
            enabled: enabled(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOAuthConfig {
    /// 预注册的 client_id；缺省走动态注册（RFC 7591）。
    #[serde(default)]
    pub client_id: Option<String>,
    /// 机密客户端的 client_secret 所在环境变量名；公共客户端不需要。
    #[serde(default)]
    pub client_secret_env: Option<String>,
    /// 申请的 scope；缺省用资源元数据里 `scopes_supported` 的全部。
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTransportKind {
    Stdio,
    StreamableHttp,
}

impl McpServerConfig {
    /// 校验并判定传输方式。错误信息带上服务名，配置校验与连接共用同一套规则。
    pub fn validated_transport(&self, name: &str) -> Result<McpTransportKind, McpError> {
        let invalid =
            |message: String| McpError::InvalidConfig(format!("mcp_servers.{name}: {message}"));
        let command = self
            .command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let url = self.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
        let kind = match (command, url) {
            (Some(_), Some(_)) => {
                return Err(invalid(
                    "declares both command and url; pick one transport".to_owned(),
                ));
            }
            (None, None) => {
                return Err(invalid(
                    "needs command (stdio) or url (Streamable HTTP)".to_owned(),
                ));
            }
            (Some(_), None) => McpTransportKind::Stdio,
            (None, Some(_)) => McpTransportKind::StreamableHttp,
        };
        if !(1..=300).contains(&self.startup_timeout_seconds) {
            return Err(invalid(
                "startup_timeout_seconds must be between 1 and 300".to_owned(),
            ));
        }
        if kind == McpTransportKind::Stdio {
            if !self.headers.is_empty() || self.bearer_token_env.is_some() || self.oauth.is_some() {
                return Err(invalid(
                    "headers, bearer_token_env and oauth only apply to url servers".to_owned(),
                ));
            }
            return Ok(kind);
        }
        if !self.args.is_empty() || !self.env.is_empty() {
            return Err(invalid(
                "args and env only apply to command servers".to_owned(),
            ));
        }
        let parsed = reqwest::Url::parse(url.unwrap_or_default())
            .map_err(|error| invalid(format!("url is not valid: {error}")))?;
        let loopback = matches!(
            parsed.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
        );
        if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback)) {
            return Err(invalid(
                "url must be https (plain http is only allowed for loopback hosts)".to_owned(),
            ));
        }
        if self.bearer_token_env.is_some() && self.oauth.is_some() {
            return Err(invalid(
                "bearer_token_env and oauth are exclusive".to_owned(),
            ));
        }
        for (header, value) in &self.headers {
            if header.eq_ignore_ascii_case("authorization") {
                return Err(invalid(
                    "do not put Authorization in headers; use bearer_token_env or oauth".to_owned(),
                ));
            }
            if reqwest::header::HeaderName::from_bytes(header.as_bytes()).is_err() {
                return Err(invalid(format!("header name {header:?} is not valid")));
            }
            if crate::judge::key_looks_sensitive(header) && !value.trim().starts_with("${env:") {
                return Err(invalid(format!(
                    "header {header} looks like a credential; write its value as ${{env:NAME}}"
                )));
            }
            expand_env_placeholders(value)
                .map_err(|error| invalid(format!("header {header}: {error}")))?;
        }
        Ok(kind)
    }

    pub fn validate(&self, name: &str) -> Result<(), McpError> {
        self.validated_transport(name).map(|_| ())
    }
}

/// 把 `${env:NAME}` 换成环境变量的值。变量没设是错误，不是空串：一个空的
/// API key 头会让远端回一个让人摸不着头脑的 401。
pub(crate) fn expand_env_placeholders(value: &str) -> Result<String, McpError> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${env:") {
        output.push_str(&rest[..start]);
        let after = &rest[start + "${env:".len()..];
        let Some(end) = after.find('}') else {
            return Err(McpError::InvalidConfig(
                "unterminated ${env:...} placeholder".to_owned(),
            ));
        };
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(McpError::InvalidConfig(format!(
                "invalid environment variable name {name:?}"
            )));
        }
        let found = std::env::var(name)
            .ok()
            .filter(|found| !found.is_empty())
            .ok_or_else(|| {
                McpError::InvalidConfig(format!("environment variable {name} is not set"))
            })?;
        output.push_str(&found);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn default_timeout() -> u64 {
    30
}
fn enabled() -> bool {
    true
}

pub(super) fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "willdeep", "version": crate::VERSION}
    })
}

#[derive(Clone, Default)]
pub struct McpRegistry {
    servers: BTreeMap<String, Arc<McpServer>>,
    tools: BTreeMap<String, McpTool>,
}

#[derive(Clone)]
struct McpTool {
    server: String,
    remote_name: String,
    definition: ToolDefinition,
}

struct McpServer {
    name: String,
    transport: Mutex<Transport>,
    timeout: Duration,
}

/// 两个变体都装 Box：一个带子进程与三根管子，一个带 reqwest 客户端、头表与鉴权
/// 状态，哪个裸着都是几百字节；枚举只留两个指针。
enum Transport {
    Stdio(Box<StdioConnection>),
    Http(Box<http::HttpConnection>),
}

struct StdioConnection {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpRegistry {
    /// 没有家目录上下文的连接：插件宿主用它连插件自带的 stdio 服务。
    /// 配了 OAuth 的 url 服务在这条路上连不上（token 在家目录里）。
    pub async fn connect(configs: &BTreeMap<String, McpServerConfig>) -> Result<Self, McpError> {
        Self::connect_in(None, configs).await
    }

    /// 带家目录的连接：OAuth token 从 `<home>/mcp-oauth/<name>.json` 取。
    pub async fn connect_in(
        home: Option<&Path>,
        configs: &BTreeMap<String, McpServerConfig>,
    ) -> Result<Self, McpError> {
        let mut registry = Self::default();
        for (name, config) in configs.iter().filter(|(_, c)| c.enabled) {
            let kind = config.validated_transport(name)?;
            let server = match McpServer::start(home, name, config, kind).await {
                Ok(server) => Arc::new(server),
                // 远程服务连不上、没登录、对方 5xx：都不该让宿主起不来。
                // 报到 stderr，跳过这一个，其余服务照常。
                Err(error) if kind == McpTransportKind::StreamableHttp => {
                    eprintln!("warning: MCP server {name} is unavailable: {error}");
                    continue;
                }
                Err(error) => return Err(error),
            };
            // 一个只提供 Resource 的 MCP 服务是合法的（插件的声明式侧栏与
            // MCP App 页面就只用 resources）。它没有 tools/list 不该把整份
            // 注册表拖垮，但也不能静默——报到 stderr，服务照常注册。
            let listed = match server.request("tools/list", json!({})).await {
                Ok(listed) => listed,
                Err(error) => {
                    eprintln!("warning: MCP server {name} has no usable tool list: {error}");
                    registry.servers.insert(name.clone(), server);
                    continue;
                }
            };
            for item in listed
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(remote_name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let local_name = format!("mcp__{}__{}", sanitize(name), sanitize(remote_name));
                let definition = ToolDefinition {
                    name: local_name.clone(),
                    description: item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("MCP tool")
                        .to_owned(),
                    parameters: item
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object"})),
                };
                registry.tools.insert(
                    local_name,
                    McpTool {
                        server: name.clone(),
                        remote_name: remote_name.to_owned(),
                        definition,
                    },
                );
            }
            registry.servers.insert(name.clone(), server);
        }
        Ok(registry)
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|v| v.definition.clone()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
    /// Search the MCP index on demand. Full input schemas are returned only
    /// for matching tools instead of riding in every provider request.
    pub fn search(&self, query: Option<&str>, max_results: usize) -> String {
        let query = query.unwrap_or_default().trim().to_ascii_lowercase();
        let matches = self
            .tools
            .values()
            .filter(|tool| {
                query.is_empty()
                    || format!("{} {}", tool.definition.name, tool.definition.description)
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .take(max_results.clamp(1, 20))
            .map(|tool| tool.definition.clone())
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return "No matching MCP tools.".to_owned();
        }
        let rendered = serde_json::to_string_pretty(&matches)
            .unwrap_or_else(|_| "MCP tool index serialization failed.".to_owned());
        const MAX_CHARS: usize = 48_000;
        if rendered.chars().count() <= MAX_CHARS {
            rendered
        } else {
            format!(
                "{}\n[truncated; narrow the query]",
                rendered.chars().take(MAX_CHARS).collect::<String>()
            )
        }
    }
    pub fn handles(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
    pub async fn call(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| McpError::UnknownTool(name.to_owned()))?;
        let server = self
            .servers
            .get(&tool.server)
            .ok_or_else(|| McpError::MissingServer(tool.server.clone()))?;
        let result = server
            .request(
                "tools/call",
                json!({"name": tool.remote_name, "arguments": arguments}),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }

    pub fn server_names(&self) -> Vec<&str> {
        self.servers.keys().map(String::as_str).collect()
    }

    pub fn has_server(&self, server: &str) -> bool {
        self.servers.contains_key(server)
    }

    fn server(&self, server: &str) -> Result<&Arc<McpServer>, McpError> {
        self.servers
            .get(server)
            .ok_or_else(|| McpError::MissingServer(server.to_owned()))
    }

    /// 直接对某个服务调用工具，返回原始结果。
    ///
    /// 与 `call` 的区别是不走 `mcp__server__tool` 命名空间：插件页面拿到的
    /// 是插件清单里的工具名，宿主必须自己确认这个服务属于这个插件——所以
    /// 每个插件持有的是**它自己的** registry 实例，隔离靠的是实例边界，
    /// 不是名字前缀。
    pub async fn call_tool_on(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, McpError> {
        self.server(server)?
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }

    pub async fn list_resources(&self, server: &str) -> Result<Value, McpError> {
        self.server(server)?
            .request("resources/list", json!({}))
            .await
    }

    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, McpError> {
        self.server(server)?
            .request("resources/read", json!({"uri": uri}))
            .await
    }

    /// 订阅一条资源。服务不支持订阅时返回错误，调用方按"不支持"处理即可——
    /// 宿主还有进入目的地与手动刷新两条读取时机，不做高频轮询。
    pub async fn subscribe_resource(&self, server: &str, uri: &str) -> Result<(), McpError> {
        self.server(server)?
            .request("resources/subscribe", json!({"uri": uri}))
            .await
            .map(|_| ())
    }
}

/// 从 `resources/read` 的结果里取出第一条内容的文本与 MIME。
/// 结构是 MCP 标准的 `{"contents":[{"uri":..,"mimeType":..,"text":..}]}`。
pub fn resource_text(result: &Value, expected_uri: &str) -> Option<(String, String)> {
    let contents = result.get("contents")?.as_array()?;
    let entry = contents
        .iter()
        .find(|item| item.get("uri").and_then(Value::as_str) == Some(expected_uri))
        .or_else(|| contents.first())?;
    let text = entry.get("text")?.as_str()?.to_owned();
    let mime = entry
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("text/plain")
        .to_owned();
    Some((text, mime))
}

impl McpServer {
    async fn start(
        home: Option<&Path>,
        name: &str,
        config: &McpServerConfig,
        kind: McpTransportKind,
    ) -> Result<Self, McpError> {
        let transport = match kind {
            McpTransportKind::Stdio => Transport::Stdio(Box::new(spawn_stdio(config)?)),
            McpTransportKind::StreamableHttp => Transport::Http(Box::new(
                http::HttpConnection::open(home, name, config).await?,
            )),
        };
        let server = Self {
            name: name.to_owned(),
            transport: Mutex::new(transport),
            timeout: Duration::from_secs(config.startup_timeout_seconds.clamp(1, 300)),
        };
        let result = server.request("initialize", initialize_params()).await?;
        if let Transport::Http(http) = &mut *server.transport.lock().await {
            http.note_initialized(&result);
        }
        server
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(server)
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        tokio::time::timeout(self.timeout, async {
            let mut transport = self.transport.lock().await;
            match &mut *transport {
                Transport::Stdio(connection) => {
                    stdio_request(&self.name, connection, method, params).await
                }
                Transport::Http(connection) => connection.request(method, params).await,
            }
        })
        .await
        .map_err(|_| McpError::Timeout(self.name.clone()))?
    }
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        tokio::time::timeout(self.timeout, async {
            let mut transport = self.transport.lock().await;
            match &mut *transport {
                Transport::Stdio(connection) => {
                    write_message(
                        &mut connection.stdin,
                        &json!({"jsonrpc":"2.0","method":method,"params":params}),
                    )
                    .await
                }
                Transport::Http(connection) => connection.notify(method, params).await,
            }
        })
        .await
        .map_err(|_| McpError::Timeout(self.name.clone()))?
    }
}

fn spawn_stdio(config: &McpServerConfig) -> Result<StdioConnection, McpError> {
    let program = config.command.as_deref().unwrap_or_default();
    let mut command = Command::new(program);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take().ok_or(McpError::MissingPipe)?;
    let stdout = BufReader::new(child.stdout.take().ok_or(McpError::MissingPipe)?);
    Ok(StdioConnection {
        _child: child,
        stdin,
        stdout,
        next_id: 1,
    })
}

async fn stdio_request(
    server: &str,
    connection: &mut StdioConnection,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    let id = connection.next_id;
    connection.next_id += 1;
    write_message(
        &mut connection.stdin,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )
    .await?;
    loop {
        let mut line = String::new();
        if connection.stdout.read_line(&mut line).await? == 0 {
            return Err(McpError::Exited(server.to_owned()));
        }
        let value: Value = serde_json::from_str(&line)?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(McpError::Remote(error.to_string()));
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

async fn write_message(stdin: &mut ChildStdin, value: &Value) -> Result<(), McpError> {
    stdin
        .write_all(format!("{}\n", serde_json::to_string(value)?).as_bytes())
        .await?;
    stdin.flush().await?;
    Ok(())
}

pub(crate) fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("invalid MCP configuration: {0}")]
    InvalidConfig(String),
    #[error("MCP process is missing a stdio pipe")]
    MissingPipe,
    #[error("MCP server exited: {0}")]
    Exited(String),
    #[error("MCP server timed out: {0}")]
    Timeout(String),
    #[error("MCP server returned an error: {0}")]
    Remote(String),
    #[error("unknown MCP tool: {0}")]
    UnknownTool(String),
    #[error("MCP server is missing: {0}")]
    MissingServer(String),
    /// 远端要求登录。带上它在 `WWW-Authenticate` 里给的资源元数据地址，
    /// 登录流程从那里开始发现授权服务器。
    #[error("MCP server {server} requires authorization; run `willdeep mcp login {server}`{}", hint.as_deref().map(|h| format!(" (resource metadata: {h})")).unwrap_or_default())]
    Unauthorized {
        server: String,
        hint: Option<String>,
    },
    #[error("MCP server {server} answered HTTP {status}: {body}")]
    Http {
        server: String,
        status: u16,
        body: String,
    },
    #[error("MCP transport failed: {0}")]
    Transport(String),
    #[error("MCP OAuth failed: {0}")]
    OAuth(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<reqwest::Error> for McpError {
    fn from(error: reqwest::Error) -> Self {
        // reqwest 的错误里会带完整 URL；URL 可能含 token 之类的查询串，只留说明。
        Self::Transport(error.without_url().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespaces_external_tool_names() {
        assert_eq!(sanitize("github.com"), "github_com");
        assert_eq!(sanitize("create-issue"), "create-issue");
    }
    #[test]
    fn parses_stdio_server_config() {
        let config: McpServerConfig = serde_json::from_value(
            json!({"command":"npx","args":["server"],"env":{"TOKEN":"from-env"}}),
        )
        .unwrap();
        assert!(config.enabled);
        assert_eq!(config.startup_timeout_seconds, 30);
        assert_eq!(
            config.validated_transport("x").unwrap(),
            McpTransportKind::Stdio
        );
    }

    #[test]
    fn validates_http_server_config() {
        let ok: McpServerConfig = serde_json::from_value(json!({
            "url": "https://mcp.example.com/mcp",
            "headers": {"X-Trace": "willdeep"},
            "bearer_token_env": "MCP_TOKEN"
        }))
        .unwrap();
        assert_eq!(
            ok.validated_transport("remote").unwrap(),
            McpTransportKind::StreamableHttp
        );

        let cases: Vec<(&str, Value)> = vec![
            ("both", json!({"command": "x", "url": "https://a/b"})),
            ("neither", json!({})),
            ("plain http", json!({"url": "http://mcp.example.com/mcp"})),
            (
                "authorization header",
                json!({"url": "https://a/b", "headers": {"Authorization": "Bearer x"}}),
            ),
            (
                "credential-looking header in plaintext",
                json!({"url": "https://a/b", "headers": {"X-Api-Key": "sk-live"}}),
            ),
            (
                "bearer and oauth",
                json!({"url": "https://a/b", "bearer_token_env": "T", "oauth": {}}),
            ),
            (
                "stdio with headers",
                json!({"command": "x", "headers": {"X": "y"}}),
            ),
            (
                "http with args",
                json!({"url": "https://a/b", "args": ["x"]}),
            ),
        ];
        for (label, value) in cases {
            let config: McpServerConfig = serde_json::from_value(value).unwrap();
            assert!(config.validate("s").is_err(), "{label} should be rejected");
        }
        let loopback: McpServerConfig =
            serde_json::from_value(json!({"url": "http://127.0.0.1:8080/mcp"})).unwrap();
        assert!(loopback.validate("local").is_ok());
        let unknown: Result<McpServerConfig, _> =
            serde_json::from_value(json!({"url": "https://a/b", "transport": "sse"}));
        assert!(unknown.is_err(), "unknown keys stay rejected");
    }

    #[test]
    fn expands_env_placeholders_and_rejects_unset_ones() {
        // SAFETY: 测试进程内设置一个只有本测试用的变量名。
        unsafe { std::env::set_var("WILLDEEP_MCP_TEST_HEADER", "v1") };
        assert_eq!(
            expand_env_placeholders("prefix-${env:WILLDEEP_MCP_TEST_HEADER}-suffix").unwrap(),
            "prefix-v1-suffix"
        );
        assert!(expand_env_placeholders("${env:WILLDEEP_MCP_TEST_UNSET_XYZ}").is_err());
        assert!(expand_env_placeholders("${env:BROKEN").is_err());
        assert_eq!(expand_env_placeholders("plain").unwrap(), "plain");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completes_stdio_handshake_discovery_and_call() {
        let script = r#"read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}'
read initialized
read list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}]}}'
read call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}]}}'
"#;
        let mut configs = BTreeMap::new();
        configs.insert(
            "mock".to_owned(),
            McpServerConfig {
                command: Some("/bin/sh".to_owned()),
                args: vec!["-c".to_owned(), script.to_owned()],
                startup_timeout_seconds: 5,
                ..McpServerConfig::default()
            },
        );
        let registry = McpRegistry::connect(&configs).await.unwrap();
        assert!(registry.handles("mcp__mock__echo"));
        assert!(!registry.search(Some("echo"), 5).contains("inputSchema"));
        assert!(registry.search(Some("echo"), 5).contains("parameters"));
        assert_eq!(
            registry.search(Some("missing"), 5),
            "No matching MCP tools."
        );
        let result = registry
            .call("mcp__mock__echo", json!({"text":"ping"}))
            .await
            .unwrap();
        assert!(result.contains("pong"));
    }
}
