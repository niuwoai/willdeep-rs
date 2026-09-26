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
//!
//! 插件来源的 stdio 服务额外能**反向请求宿主**（出图、问模型），见
//! [`HostRequestHandler`]；已启用插件的工具经 [`LazyToolSource`] 挂进同一张表，
//! 用到时才拉起插件进程。

mod http;
pub mod oauth;
mod stdio;
#[cfg(test)]
mod test_http;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::types::ToolDefinition;

pub use stdio::HOST_REQUEST_MAX_SECONDS;

/// 我们向服务端声明的协议版本。服务端可以回一个更旧的；HTTP 传输之后的请求按它
/// 回的那个带 `MCP-Protocol-Version` 头。
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// 在 `initialize` 的 `capabilities.extensions` 里宣告反向请求能力用的键，
/// 与 macOS 宿主（`AgentMCPHostRequests.capabilityKey`）同名。
pub const HOST_REQUESTS_CAPABILITY: &str = "io.willdeep/host-requests";

/// 服务端（插件 MCP 进程）在连接上反向请求宿主时的处理器。
///
/// 只宣告真的实现了的方法：插件据此判断支不支持，没宣告的它会直接报
/// 「宿主不支持」，而不是发出来干等到超时。
#[async_trait]
pub trait HostRequestHandler: Send + Sync {
    /// 在 initialize 里宣告的方法。空表就不宣告扩展。
    fn methods(&self) -> Vec<String>;
    async fn handle(&self, method: &str, params: Value) -> Result<Value, HostRequestError>;
}

/// 反向请求的 JSON-RPC 错误。错误码与 macOS 宿主一致：
/// -32601 方法不存在、-32602 参数不对、-32000 处理失败、-32001 处理超时。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRequestError {
    pub code: i64,
    pub message: String,
}

impl HostRequestError {
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const FAILED: i64 = -32000;
    pub const TIMED_OUT: i64 = -32001;

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: Self::INVALID_PARAMS,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: Self::FAILED,
            message: message.into(),
        }
    }
}

/// 不在配置里、而是按需拉起的一批工具（已启用插件的 MCP 工具）。
///
/// 定义来自持久化的工具目录，建工具表时不拉进程；调用时才拉起。
#[async_trait]
pub trait LazyToolSource: Send + Sync {
    /// 目录里当前该暴露的工具（只含已启用、有权限运行的服务）。
    fn definitions(&self) -> Vec<ToolDefinition>;
    /// 有没有可能提供工具：有已启用、能运行的服务就算，哪怕目录还空着——
    /// 否则 `list_mcp_tools` 不出现，目录就永远没有机会被填上。
    fn available(&self) -> bool;
    /// 给目录里还没有条目的服务补一次 `tools/list`（会拉起进程）。
    async fn refresh_missing(&self);
    async fn call(&self, name: &str, arguments: Value) -> Result<String, McpError>;
}

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
    initialize_params_with(None)
}

/// `clientInfo.name` 必须保持 `willdeep`：短剧工坊按它认出 Web 宿主、切换媒体地址。
/// 带了反向请求处理器时，在 `capabilities.extensions` 里宣告它实现的方法。
pub(crate) fn initialize_params_with(handler: Option<&dyn HostRequestHandler>) -> Value {
    let methods = handler.map(HostRequestHandler::methods).unwrap_or_default();
    let capabilities = if methods.is_empty() {
        json!({})
    } else {
        json!({"extensions": {HOST_REQUESTS_CAPABILITY: {"methods": methods}}})
    };
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": capabilities,
        "clientInfo": {"name": "willdeep", "version": crate::VERSION}
    })
}

#[derive(Clone, Default)]
pub struct McpRegistry {
    servers: BTreeMap<String, Arc<McpServer>>,
    tools: BTreeMap<String, McpTool>,
    /// 连接时各服务 `tools/list` 的原始结果，插件宿主拿它刷新工具目录。
    listed: BTreeMap<String, Value>,
    lazy: Option<Arc<dyn LazyToolSource>>,
}

#[derive(Clone)]
struct McpTool {
    server: String,
    remote_name: String,
    definition: ToolDefinition,
}

struct McpServer {
    name: String,
    transport: Transport,
    timeout: Duration,
}

/// 两个变体都装 Box：一个带子进程、读任务与管子，一个带 reqwest 客户端、头表与
/// 鉴权状态，哪个裸着都是几百字节；枚举只留两个指针。stdio 自己管并发（读任务
/// 常驻、请求内部排队），HTTP 仍然整条连接一把锁。
enum Transport {
    Stdio(Box<stdio::StdioConnection>),
    Http(Box<Mutex<http::HttpConnection>>),
}

impl McpRegistry {
    /// 没有家目录上下文的连接：插件宿主用它连插件自带的 stdio 服务。
    /// 配了 OAuth 的 url 服务在这条路上连不上（token 在家目录里）。
    pub async fn connect(configs: &BTreeMap<String, McpServerConfig>) -> Result<Self, McpError> {
        Self::connect_inner(None, configs, None).await
    }

    /// 插件宿主的连接：stdio 服务可以反向请求宿主，`handler` 决定宣告哪些方法、
    /// 怎么处理。用户配置的服务不走这里，它们的反向请求一律回 method not found。
    pub async fn connect_with_host_requests(
        configs: &BTreeMap<String, McpServerConfig>,
        handler: Arc<dyn HostRequestHandler>,
    ) -> Result<Self, McpError> {
        Self::connect_inner(None, configs, Some(handler)).await
    }

    /// 带家目录的连接：OAuth token 从 `<home>/mcp-oauth/<name>.json` 取。
    pub async fn connect_in(
        home: Option<&Path>,
        configs: &BTreeMap<String, McpServerConfig>,
    ) -> Result<Self, McpError> {
        Self::connect_inner(home, configs, None).await
    }

    async fn connect_inner(
        home: Option<&Path>,
        configs: &BTreeMap<String, McpServerConfig>,
        handler: Option<Arc<dyn HostRequestHandler>>,
    ) -> Result<Self, McpError> {
        let mut registry = Self::default();
        for (name, config) in configs.iter().filter(|(_, c)| c.enabled) {
            let kind = config.validated_transport(name)?;
            let server = match McpServer::start(home, name, config, kind, handler.clone()).await {
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
            registry.listed.insert(name.clone(), listed.clone());
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
    /// 挂上一批按需拉起的工具（已启用插件的 MCP 工具）。与配置里的工具重名时
    /// 配置优先：那是用户亲手写的。
    pub fn with_lazy_tools(mut self, source: Arc<dyn LazyToolSource>) -> Self {
        self.lazy = Some(source);
        self
    }

    fn lazy_definitions(&self) -> Vec<ToolDefinition> {
        self.lazy
            .as_ref()
            .map(|lazy| {
                lazy.definitions()
                    .into_iter()
                    .filter(|definition| !self.tools.contains_key(&definition.name))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<ToolDefinition> =
            self.tools.values().map(|v| v.definition.clone()).collect();
        definitions.extend(self.lazy_definitions());
        definitions
    }
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && !self.lazy.as_ref().is_some_and(|lazy| lazy.available())
    }

    /// 与 [`search`](Self::search) 相同，但先给目录里还空着的插件服务补一次
    /// `tools/list`。只在模型明确要找工具时走这条，建工具表不走。
    pub async fn search_refreshing(&self, query: Option<&str>, max_results: usize) -> String {
        if let Some(lazy) = &self.lazy {
            lazy.refresh_missing().await;
        }
        self.search(query, max_results)
    }

    /// Search the MCP index on demand. Full input schemas are returned only
    /// for matching tools instead of riding in every provider request.
    pub fn search(&self, query: Option<&str>, max_results: usize) -> String {
        let query = query.unwrap_or_default().trim().to_ascii_lowercase();
        let matches = self
            .definitions()
            .into_iter()
            .filter(|definition| {
                query.is_empty()
                    || format!("{} {}", definition.name, definition.description)
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .take(max_results.clamp(1, 20))
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
            || self
                .lazy_definitions()
                .iter()
                .any(|definition| definition.name == name)
    }
    pub async fn call(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let Some(tool) = self.tools.get(name) else {
            return match &self.lazy {
                Some(lazy) if self.handles(name) => lazy.call(name, arguments).await,
                _ => Err(McpError::UnknownTool(name.to_owned())),
            };
        };
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

    /// 连接时这个服务 `tools/list` 的原始结果；没列出来（只提供资源）返回 None。
    pub fn listed_tools(&self, server: &str) -> Option<&Value> {
        self.listed.get(server)
    }

    /// 全部 stdio 服务都还活着。插件进程被结束之后，宿主据此丢掉缓存的连接、
    /// 下一次用时重新拉起。
    pub fn is_alive(&self) -> bool {
        self.servers.values().all(|server| match &server.transport {
            Transport::Stdio(connection) => connection.is_alive(),
            Transport::Http(_) => true,
        })
    }

    /// 对某个服务发一条任意方法的请求，返回原始结果。插件 MCP 网关的 stdio
    /// 中转走这里；方法由调用方把关。
    pub async fn request(
        &self,
        server: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        self.server(server)?.request(method, params).await
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
        handler: Option<Arc<dyn HostRequestHandler>>,
    ) -> Result<Self, McpError> {
        // 反向请求只开给 stdio：宿主要把响应写回这条连接的 stdin。
        let (transport, initialize) = match kind {
            McpTransportKind::Stdio => (
                Transport::Stdio(Box::new(stdio::StdioConnection::spawn(
                    name,
                    config,
                    handler.clone(),
                )?)),
                initialize_params_with(handler.as_deref()),
            ),
            McpTransportKind::StreamableHttp => (
                Transport::Http(Box::new(Mutex::new(
                    http::HttpConnection::open(home, name, config).await?,
                ))),
                initialize_params(),
            ),
        };
        let server = Self {
            name: name.to_owned(),
            transport,
            timeout: Duration::from_secs(config.startup_timeout_seconds.clamp(1, 300)),
        };
        let result = server.request("initialize", initialize).await?;
        if let Transport::Http(http) = &server.transport {
            http.lock().await.note_initialized(&result);
        }
        server
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(server)
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        match &self.transport {
            // stdio 自己算超时：排队不计时，宿主处理反向请求期间也不计时。
            Transport::Stdio(connection) => {
                connection
                    .request(&self.name, method, params, self.timeout)
                    .await
            }
            Transport::Http(connection) => tokio::time::timeout(self.timeout, async {
                connection.lock().await.request(method, params).await
            })
            .await
            .map_err(|_| McpError::Timeout(self.name.clone()))?,
        }
    }
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        tokio::time::timeout(self.timeout, async {
            match &self.transport {
                Transport::Stdio(connection) => connection.notify(method, params).await,
                Transport::Http(connection) => connection.lock().await.notify(method, params).await,
            }
        })
        .await
        .map_err(|_| McpError::Timeout(self.name.clone()))?
    }
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

    const FAKE_PLUGIN: &str = include_str!("../tests/fixtures/fake_plugin_mcp.py");

    struct FakePlugin {
        dir: std::path::PathBuf,
    }

    impl FakePlugin {
        fn new() -> Option<Self> {
            if std::process::Command::new("python3")
                .arg("--version")
                .output()
                .is_err()
            {
                eprintln!("python3 not found; skipping");
                return None;
            }
            let dir = std::env::temp_dir().join(format!(
                "willdeep-mcp-fake-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("scratch");
            std::fs::write(dir.join("server.py"), FAKE_PLUGIN).expect("script");
            Some(Self { dir })
        }

        fn configs(&self, timeout: u64) -> BTreeMap<String, McpServerConfig> {
            let mut env = BTreeMap::new();
            env.insert(
                "FAKE_MCP_LOG".to_owned(),
                self.dir.join("log.jsonl").display().to_string(),
            );
            BTreeMap::from([(
                "fake".to_owned(),
                McpServerConfig {
                    command: Some("python3".to_owned()),
                    args: vec![self.dir.join("server.py").display().to_string()],
                    env,
                    startup_timeout_seconds: timeout,
                    ..McpServerConfig::default()
                },
            )])
        }

        fn events(&self, name: &str) -> Vec<Value> {
            std::fs::read_to_string(self.dir.join("log.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|event| event["event"] == name)
                .collect()
        }
    }

    impl Drop for FakePlugin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 测试用处理器：宣告出图，处理时故意比服务端超时还慢。
    struct SlowImages {
        delay: Duration,
        seen: std::sync::Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl HostRequestHandler for SlowImages {
        fn methods(&self) -> Vec<String> {
            vec!["willdeep/images/generate".to_owned()]
        }
        async fn handle(&self, method: &str, params: Value) -> Result<Value, HostRequestError> {
            self.seen.lock().unwrap().push(params.clone());
            tokio::time::sleep(self.delay).await;
            if params.get("fail").is_some() {
                return Err(HostRequestError::invalid_params("bad prompt"));
            }
            Ok(json!({"mediaURL": "/plugin-media/demo/x.png", "method": method}))
        }
    }

    fn text_of(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn plugin_servers_advertise_and_answer_reverse_requests_without_timing_out() {
        let Some(fake) = FakePlugin::new() else {
            return;
        };
        let handler = Arc::new(SlowImages {
            delay: Duration::from_millis(1_600),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let registry = McpRegistry::connect_with_host_requests(&fake.configs(1), handler.clone())
            .await
            .expect("connect");
        let initialize = &fake.events("initialize")[0]["params"];
        assert_eq!(
            initialize["capabilities"]["extensions"][HOST_REQUESTS_CAPABILITY]["methods"],
            json!(["willdeep/images/generate"])
        );
        assert_eq!(initialize["clientInfo"]["name"], "willdeep");

        // 服务端超时 1 秒，宿主处理反向请求用了 1.6 秒：处理期间不计时。
        let result = registry
            .call_tool_on("fake", "draw", json!({"params": {"prompt": "a cat"}}))
            .await
            .expect("draw");
        let answer = text_of(&result);
        assert_eq!(answer["result"]["mediaURL"], "/plugin-media/demo/x.png");
        assert_eq!(handler.seen.lock().unwrap()[0]["prompt"], "a cat");

        // 处理器报的错原样回给插件。
        let failed = text_of(
            &registry
                .call_tool_on("fake", "draw", json!({"params": {"fail": true}}))
                .await
                .expect("draw"),
        );
        assert_eq!(failed["error"]["code"], HostRequestError::INVALID_PARAMS);

        // 没宣告的方法：method not found，不交给处理器。
        let unsupported = text_of(
            &registry
                .call_tool_on(
                    "fake",
                    "draw",
                    json!({"method": "willdeep/audio/synthesize"}),
                )
                .await
                .expect("draw"),
        );
        assert_eq!(
            unsupported["error"]["code"],
            HostRequestError::METHOD_NOT_FOUND
        );
        assert_eq!(handler.seen.lock().unwrap().len(), 2);

        // 超时本身仍然有效：服务端自己不动弹就是超时。
        let slow = registry
            .call_tool_on("fake", "sleep", json!({"seconds": 2.5}))
            .await;
        assert!(matches!(slow, Err(McpError::Timeout(_))), "{slow:?}");
    }

    #[tokio::test]
    async fn reverse_requests_sent_while_the_host_is_idle_are_answered() {
        let Some(fake) = FakePlugin::new() else {
            return;
        };
        let handler = Arc::new(SlowImages {
            delay: Duration::from_millis(10),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let registry = McpRegistry::connect_with_host_requests(&fake.configs(5), handler.clone())
            .await
            .expect("connect");
        registry
            .call_tool_on(
                "fake",
                "later",
                json!({"params": {"prompt": "idle"}, "delay": 0.2}),
            )
            .await
            .expect("later");
        // 这时宿主没有任何在途请求；读任务仍然要把反向请求接下来并回话。
        let mut answered = Vec::new();
        for _ in 0..50 {
            answered = fake.events("host_response");
            if !answered.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(answered.len(), 1, "idle reverse request was never answered");
        assert_eq!(
            answered[0]["message"]["result"]["mediaURL"],
            "/plugin-media/demo/x.png"
        );
        assert_eq!(handler.seen.lock().unwrap()[0]["prompt"], "idle");
        drop(registry);
    }

    #[tokio::test]
    async fn configured_servers_get_method_not_found_and_no_extension() {
        let Some(fake) = FakePlugin::new() else {
            return;
        };
        let registry = McpRegistry::connect(&fake.configs(5))
            .await
            .expect("connect");
        assert_eq!(
            fake.events("initialize")[0]["params"]["capabilities"],
            json!({})
        );
        let answer = text_of(
            &registry
                .call_tool_on("fake", "draw", json!({}))
                .await
                .expect("draw"),
        );
        assert_eq!(answer["error"]["code"], HostRequestError::METHOD_NOT_FOUND);
        assert!(registry.is_alive());
        // 假插件回完这句就退出：客户端读写刚好撞上进程退出时会得到 BrokenPipe 或
        // Exited，这取决于时序（macOS 上偶发）。这里只关心退出之后的状态。
        let exit = registry.call_tool_on("fake", "exit", json!({})).await;
        assert!(
            match &exit {
                Ok(_) | Err(McpError::Exited(_)) => true,
                Err(McpError::Io(error)) => error.kind() == std::io::ErrorKind::BrokenPipe,
                Err(_) => false,
            },
            "{exit:?}"
        );
        for _ in 0..50 {
            if !registry.is_alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!registry.is_alive(), "an exited server must not look alive");
        let after = registry.call_tool_on("fake", "echo", json!({})).await;
        assert!(matches!(after, Err(McpError::Exited(_))), "{after:?}");
    }

    #[tokio::test]
    async fn lazy_tools_join_search_handles_and_call() {
        struct Lazy;
        #[async_trait]
        impl LazyToolSource for Lazy {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "mcp__plug__draw".to_owned(),
                    description: "Draw via plugin".to_owned(),
                    parameters: json!({"type": "object"}),
                }]
            }
            fn available(&self) -> bool {
                true
            }
            async fn refresh_missing(&self) {}
            async fn call(&self, name: &str, _arguments: Value) -> Result<String, McpError> {
                Ok(format!("called {name}"))
            }
        }
        let registry = McpRegistry::default().with_lazy_tools(Arc::new(Lazy));
        assert!(!registry.is_empty());
        assert!(registry.handles("mcp__plug__draw"));
        assert!(!registry.handles("mcp__plug__other"));
        assert!(
            registry
                .search_refreshing(Some("draw"), 5)
                .await
                .contains("mcp__plug__draw")
        );
        assert_eq!(
            registry.call("mcp__plug__draw", json!({})).await.unwrap(),
            "called mcp__plug__draw"
        );
        assert!(matches!(
            registry.call("mcp__plug__other", json!({})).await,
            Err(McpError::UnknownTool(_))
        ));
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
