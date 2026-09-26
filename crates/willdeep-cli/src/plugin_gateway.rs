//! 插件 MCP 网关：外部 MCP 客户端（Claude Code、Codex……）经它按需激活插件。
//!
//! 契约 v1 见 `docs/decisions/2026-09-26-plugin-mcp-gateway.md`，与 macOS 宿主
//! 同一份，差别只在发现文件路径（`<WILLDEEP_HOME>/mcp-gateway.json`）与 `host`
//! 字段（`willdeep-rs`）。
//!
//! 为什么是一个**单独的回环监听**，而不是 Web 服务上的一条路由：Web 服务可以
//! 绑在非回环地址上给 nginx / VPN 用，而插件网关只该给本机进程；端口还要跨重启
//! 保持不变，外部客户端的配置才能长期有效。网关跑在 `willdeep web` 进程里，
//! 因为插件宿主（以及页面正在用的那个插件进程）在这里——页面、网关、聊天共用
//! 一个插件进程，不会有两个进程抢写同一份数据文件。
//!
//! 处理顺序：Host / Origin → 鉴权 → 方法 → 路由 → JSON-RPC 形状。`initialize`、
//! 通知、`ping` 网关自己回；其余先经宿主平时那条 MCP 连接发一次 `tools/list`
//! 确保插件在跑（顺手刷新聊天工具目录），再转发到插件自己的 HTTP 入口，没有入口
//! 才经 stdio 中转。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use willdeep_core::mcp::McpError;
use willdeep_core::plugin::PluginHost;
use willdeep_core::plugin::gateway::{
    DISCOVERY_VERSION, FORWARD_TIMEOUT, GatewayDiscovery, GatewayServerEntry, HOST_ID,
    PluginEndpoint, generate_token, read_discovery, read_plugin_endpoint, server_url,
    write_discovery,
};

/// 请求体上限。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 客户端没带 protocolVersion 时回的版本。
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
/// 首次没有保存的端口时随机挑的范围：41000–48999。
const RANDOM_PORT_START: u16 = 41_000;
const RANDOM_PORT_SPAN: u16 = 8_000;
const RANDOM_PORT_ATTEMPTS: usize = 6;

type DataDirs = dyn Fn(&str) -> Vec<PathBuf> + Send + Sync;

pub(crate) struct GatewayState {
    host: Arc<PluginHost>,
    token: String,
    /// 插件数据目录的查找顺序。可替换：测试不能去读真实的 `~/Library`。
    data_dirs: Box<DataDirs>,
    client: reqwest::Client,
}

/// 一个在跑的网关。插件启用、停用、卸载后调 [`publish`](Self::publish) 重写发现文件。
pub(crate) struct PluginGateway {
    home: PathBuf,
    url: String,
    token: String,
    host: Arc<PluginHost>,
}

impl PluginGateway {
    /// 绑端口、写发现文件、起服务。先试上次的端口、沿用上次的 token；
    /// 端口被占就换一个新的并重写文件。
    pub(crate) async fn start(home: &Path, host: Arc<PluginHost>) -> std::io::Result<Self> {
        let data_home = home.to_path_buf();
        Self::start_with(
            home,
            host.clone(),
            Box::new(move |plugin: &str| {
                willdeep_core::plugin::gateway::plugin_data_dirs(&data_home, plugin)
            }),
        )
        .await
    }

    pub(crate) async fn start_with(
        home: &Path,
        host: Arc<PluginHost>,
        data_dirs: Box<DataDirs>,
    ) -> std::io::Result<Self> {
        let previous = read_discovery(home);
        let token = previous
            .as_ref()
            .map(|discovery| discovery.token.clone())
            .unwrap_or_else(generate_token);
        let listener = bind(previous.as_ref().and_then(GatewayDiscovery::port)).await?;
        let address = listener.local_addr()?;
        let url = format!("http://127.0.0.1:{}", address.port());
        let state = Arc::new(GatewayState {
            host: host.clone(),
            token: token.clone(),
            data_dirs,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(FORWARD_TIMEOUT)
                .build()
                .map_err(std::io::Error::other)?,
        });
        let gateway = Self {
            home: home.to_path_buf(),
            url,
            token,
            host,
        };
        gateway.publish().await?;
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router(state)).await {
                eprintln!("warning: plugin_gateway_stopped error={error}");
            }
        });
        Ok(gateway)
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// 重写发现文件：`servers` 只列已启用插件里能运行的服务。宿主退出时不删它。
    pub(crate) async fn publish(&self) -> std::io::Result<()> {
        let servers = self
            .host
            .enabled_servers()
            .await
            .into_iter()
            .map(|(plugin_id, server)| GatewayServerEntry {
                url: server_url(&self.url, &plugin_id, &server),
                plugin_id,
                server,
            })
            .collect();
        write_discovery(
            &self.home,
            &GatewayDiscovery {
                version: DISCOVERY_VERSION,
                host: HOST_ID.to_owned(),
                url: self.url.clone(),
                token: self.token.clone(),
                servers,
            },
        )
    }
}

/// 候选端口：上次的端口，再是几个 41000–48999 里的随机端口（避开系统的临时
/// 端口段，免得和出站连接撞上），最后才交给系统挑。与 macOS 宿主同一套。
async fn bind(previous_port: Option<u16>) -> std::io::Result<TcpListener> {
    let mut candidates: Vec<u16> = previous_port.into_iter().collect();
    for _ in 0..RANDOM_PORT_ATTEMPTS {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let offset = u16::from_le_bytes([bytes[0], bytes[1]]) % RANDOM_PORT_SPAN;
        let candidate = RANDOM_PORT_START + offset;
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    for port in candidates {
        if let Ok(listener) = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await {
            return Ok(listener);
        }
    }
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await
}

pub(crate) fn router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/plugins/{plugin}/{server}/mcp", any(endpoint))
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

// ---------------------------------------------------------------- 守门

fn plain(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error": message}))).into_response()
}

/// Host 头只认 `127.0.0.1` / `localhost`（可带数字端口），防 DNS rebinding：
/// 浏览器被骗来访问时，Host 是攻击者的域名。
fn allowed_host(value: &str) -> bool {
    let trimmed = value.trim();
    let name = match trimmed.rsplit_once(':') {
        Some((name, port)) => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            name
        }
        None => trimmed,
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost"
    )
}

/// 带 Origin 时必须是本机来源（防浏览器里的网页替别人发请求）。
fn local_origin(value: &str) -> bool {
    reqwest::Url::parse(value.trim()).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && matches!(
                url.host_str().map(str::to_ascii_lowercase).as_deref(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
    })
}

fn local_request(headers: &HeaderMap) -> bool {
    let host_ok = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(allowed_host);
    let origin_ok = match headers.get(header::ORIGIN) {
        None => true,
        Some(origin) => origin.to_str().is_ok_and(local_origin),
    };
    host_ok && origin_ok
}

/// 插件 ID 会拼进数据目录路径，与生成媒体目录同一条规则。
fn valid_plugin_id(plugin: &str) -> bool {
    let bytes = plugin.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 常量时间比较：逐字节异或累加，不在第一个不同处提前返回。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| {
            // 认证方案名不分大小写（RFC 9110）。
            let (scheme, presented) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| presented.trim())
        })
        .filter(|presented| !presented.is_empty())
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), token.as_bytes()))
}

fn guard(state: &GatewayState, headers: &HeaderMap) -> Option<Response> {
    if !local_request(headers) {
        return Some(plain(StatusCode::FORBIDDEN, "forbidden"));
    }
    if !authorized(headers, &state.token) {
        let mut response = plain(StatusCode::UNAUTHORIZED, "unauthorized");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return Some(response);
    }
    None
}

async fn fallback(State(state): State<Arc<GatewayState>>, headers: HeaderMap) -> Response {
    guard(&state, &headers).unwrap_or_else(|| plain(StatusCode::NOT_FOUND, "not found"))
}

// ---------------------------------------------------------------- JSON-RPC

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn endpoint(
    State(state): State<Arc<GatewayState>>,
    AxumPath((plugin, server)): AxumPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(rejected) = guard(&state, &headers) {
        return rejected;
    }
    // 与 macOS 宿主同序：准入 → 路由（404）→ 方法（405）。
    let known = valid_plugin_id(&plugin)
        && !server.is_empty()
        && state.host.is_enabled(&plugin).await
        && state
            .host
            .runnable_servers(&plugin)
            .iter()
            .any(|item| item == &server);
    if !known {
        return plain(
            StatusCode::NOT_FOUND,
            "Unknown or disabled plugin MCP server.",
        );
    }
    if method != Method::POST {
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED, "Use POST for MCP JSON-RPC.");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &rpc_error(Value::Null, -32700, "Parse error"),
            );
        }
    };
    // 只收单条 JSON-RPC 对象：批量数组、裸值一律 -32600。
    let Some(object) = message.as_object() else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &rpc_error(
                Value::Null,
                -32600,
                "Invalid Request: expected a single JSON-RPC object",
            ),
        );
    };
    let id = object.get("id").cloned();
    if id
        .as_ref()
        .is_some_and(|id| !(id.is_null() || id.is_string() || id.is_number()))
    {
        return json_response(
            StatusCode::BAD_REQUEST,
            &rpc_error(Value::Null, -32600, "Invalid JSON-RPC id."),
        );
    }
    let Some(rpc_method) = object.get("method").and_then(Value::as_str) else {
        // 客户端对服务端请求的响应（有 id、没 method）：网关不发请求，收下即可。
        if id.is_some() && (object.contains_key("result") || object.contains_key("error")) {
            return StatusCode::ACCEPTED.into_response();
        }
        return json_response(
            StatusCode::BAD_REQUEST,
            &rpc_error(id.unwrap_or(Value::Null), -32600, "Invalid Request"),
        );
    };
    // 通知（含 notifications/initialized）：202、空响应体，不转发。
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match rpc_method {
        "initialize" => initialize(&state, &plugin, &server, id, &params),
        "ping" => json_response(
            StatusCode::OK,
            &json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        ),
        _ => dispatch(&state, &plugin, &server, id, rpc_method, params, body).await,
    }
}

/// 网关自己应答 initialize，不转给插件：第三方客户端的能力声明不能覆盖宿主在
/// stdio 那头宣告的反向请求能力。
fn initialize(
    state: &GatewayState,
    plugin: &str,
    server: &str,
    id: Value,
    params: &Value,
) -> Response {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    let version = state
        .host
        .package(plugin)
        .map(|package| package.version.clone())
        .unwrap_or_default();
    let mut response = json_response(
        StatusCode::OK,
        &json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": protocol,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": format!("{plugin}/{server}"), "version": version},
        }}),
    );
    if let Ok(session) = HeaderValue::from_str(&uuid::Uuid::new_v4().simple().to_string()) {
        response.headers_mut().insert(SESSION_HEADER, session);
    }
    response
}

async fn dispatch(
    state: &GatewayState,
    plugin: &str,
    server: &str,
    id: Value,
    method: &str,
    params: Value,
    body: Bytes,
) -> Response {
    // 1. 确保插件在跑：经宿主平时那条连接发一次 tools/list（没在跑就拉起），
    //    结果顺手刷新聊天工具目录。
    if let Err(error) = state.host.refresh_tools(plugin, server).await {
        eprintln!(
            "warning: plugin_gateway_start_failed plugin={plugin} server={server} error={error}"
        );
        return json_response(
            StatusCode::OK,
            &rpc_error(
                id,
                -32603,
                &format!("plugin {plugin} could not be started: {error}"),
            ),
        );
    }
    // 2. 插件公布了自己的 HTTP 入口就原样转发过去（出图、审核要跑一两分钟，
    //    stdio 那头有单请求超时，也会堵住页面的请求）。「没送到」时重读一次
    //    入口文件再试——插件可能刚被重启，端口和 token 都换了。
    let dirs = (state.data_dirs)(plugin);
    if let Some(endpoint) = read_plugin_endpoint(&dirs) {
        match forward_once(state, &endpoint, &id, &body).await {
            Forwarded::Delivered(response) => return response,
            Forwarded::NotDelivered => {
                if let Some(retried) = read_plugin_endpoint(&dirs)
                    && let Forwarded::Delivered(response) =
                        forward_once(state, &retried, &id, &body).await
                {
                    return response;
                }
            }
        }
    }
    // 3. 没有入口、或两次都没送到：经宿主的 stdio 客户端中转。
    relay(state, plugin, server, id, method, params).await
}

enum Forwarded {
    /// 请求送到了插件手里（成功、或者插件可能已经在执行），这就是最终回答。
    Delivered(Response),
    /// 插件还没开始执行：连接被拒，或插件入口在入队前就回了 401 / 404（旧 token、
    /// 旧端口）。只有这种才可以换条路再发一遍。
    NotDelivered,
}

/// 转发一次。超时、连接中途断开这类「可能已经在执行」的失败**不重发、不中转**，
/// 直接回 -32603：出图、提交视频不是幂等的，重发会执行两遍。
async fn forward_once(
    state: &GatewayState,
    endpoint: &PluginEndpoint,
    id: &Value,
    body: &Bytes,
) -> Forwarded {
    let failed = |message: String| {
        Forwarded::Delivered(json_response(
            StatusCode::OK,
            &rpc_error(id.clone(), -32603, &message),
        ))
    };
    let sent = state
        .client
        .post(&endpoint.url)
        .bearer_auth(&endpoint.token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .body(body.clone())
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        // 连接没建立起来（端口没人听）：请求一个字节都没发出去。
        Err(error) if error.is_connect() => return Forwarded::NotDelivered,
        Err(error) => {
            let message = McpError::from(error).to_string();
            eprintln!("warning: plugin_gateway_forward_failed error={message}");
            return failed(format!("Plugin HTTP endpoint failed: {message}"));
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::NOT_FOUND {
        return Forwarded::NotDelivered;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("application/json")
        .to_owned();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            let message = McpError::from(error).to_string();
            eprintln!("warning: plugin_gateway_forward_failed error={message}");
            return failed(format!("Plugin HTTP endpoint failed: {message}"));
        }
    };
    if !status.is_success() {
        let detail: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(2_000)
            .collect();
        return failed(format!(
            "Plugin HTTP endpoint returned HTTP {}: {detail}",
            status.as_u16()
        ));
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK);
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    Forwarded::Delivered(response)
}

async fn relay(
    state: &GatewayState,
    plugin: &str,
    server: &str,
    id: Value,
    method: &str,
    params: Value,
) -> Response {
    if !(params.is_object() || params.is_null()) {
        return json_response(
            StatusCode::OK,
            &rpc_error(id, -32602, "JSON-RPC params must be an object."),
        );
    }
    let params = if params.is_null() { json!({}) } else { params };
    let outcome = match state.host.mcp(plugin).await {
        Ok(mcp) => mcp.request(server, method, params).await,
        Err(error) => Err(McpError::Transport(error.to_string())),
    };
    let body = match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        // 插件回的 JSON-RPC 错误原样带回去（McpError::Remote 装的就是那个对象）。
        Err(McpError::Remote(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(error) if error.is_object() => json!({"jsonrpc": "2.0", "id": id, "error": error}),
            _ => rpc_error(id, -32603, &text),
        },
        Err(error) => rpc_error(id, -32603, &error.to_string()),
    };
    json_response(StatusCode::OK, &body)
}

#[cfg(test)]
mod tests;
