//! MCP Streamable HTTP 传输（规范 2025-03-26 起，2025-06-18 加了协议版本头）。
//!
//! 每条 JSON-RPC 消息一个 POST。响应要么是一个 `application/json`（单条或批量），
//! 要么是一段 `text/event-stream`：服务端可以在里面先塞通知，再给这条请求的
//! 响应。我们只认 id 对得上的那条；服务端主动发来的请求（`sampling/*`、
//! `roots/*`）这一版不支持，读到就丢，宿主没有给远端代发模型请求的授权。
//!
//! 会话：`initialize` 的响应带 `Mcp-Session-Id`，之后每个请求都带回去；服务端
//! 用 404 表示会话没了，我们重新握手一次再重发。退出时 DELETE 一下，礼貌。
//! 鉴权：静态 Bearer 或 OAuth；401 时 OAuth 先拿 refresh token 换一张再重试一次，
//! 换不到就报「需要登录」，把 `WWW-Authenticate` 里的资源元数据地址带出来。

use std::path::Path;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use super::{McpError, McpServerConfig, oauth};
use crate::provider::sse::{SseDecoder, is_event_stream};

pub(super) const SESSION_HEADER: &str = "mcp-session-id";
pub(super) const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const USER_AGENT: &str = concat!("willdeep/", env!("CARGO_PKG_VERSION"));
/// 单个 JSON 响应体上限：工具结果再大也不该是几十 MB 的一整块。
const MAX_JSON_BODY_BYTES: usize = 16 * 1024 * 1024;
/// 错误响应体进错误信息的上限。
const ERROR_BODY_CHARS: usize = 300;

pub(super) enum McpAuth {
    None,
    Bearer(String),
    /// 装个 Box：OAuth 会话里带 reqwest 客户端与 token，比另外两个变体大一个量级。
    OAuth(Box<oauth::OAuthSession>),
}

pub(super) struct HttpConnection {
    server: String,
    client: reqwest::Client,
    url: reqwest::Url,
    headers: HeaderMap,
    auth: McpAuth,
    session_id: Option<String>,
    protocol_version: Option<String>,
    next_id: u64,
}

impl HttpConnection {
    pub(super) async fn open(
        home: Option<&Path>,
        server: &str,
        config: &McpServerConfig,
    ) -> Result<Self, McpError> {
        let invalid =
            |message: String| McpError::InvalidConfig(format!("mcp_servers.{server}: {message}"));
        let url = reqwest::Url::parse(config.url.as_deref().unwrap_or_default().trim())
            .map_err(|error| invalid(format!("url is not valid: {error}")))?;
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let value = super::expand_env_placeholders(value)
                .map_err(|error| invalid(format!("header {name}: {error}")))?;
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| invalid(format!("header name {name:?} is not valid")))?,
                HeaderValue::from_str(&value)
                    .map_err(|_| invalid(format!("header {name} has a non-ASCII value")))?,
            );
        }
        let auth = if let Some(variable) = &config.bearer_token_env {
            let token = std::env::var(variable)
                .ok()
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| invalid(format!("environment variable {variable} is not set")))?;
            McpAuth::Bearer(token.trim().to_owned())
        } else if let Some(oauth_config) = &config.oauth {
            let home = home.ok_or_else(|| {
                invalid(
                    "OAuth needs the WillDeep home directory; this entry point has none".to_owned(),
                )
            })?;
            match oauth::OAuthSession::load(home, server, oauth_config)? {
                Some(session) => McpAuth::OAuth(Box::new(session)),
                None => {
                    return Err(McpError::Unauthorized {
                        server: server.to_owned(),
                        hint: None,
                    });
                }
            }
        } else {
            McpAuth::None
        };
        Self::new(server, url, headers, auth)
    }

    pub(super) fn new(
        server: &str,
        url: reqwest::Url,
        headers: HeaderMap,
        auth: McpAuth,
    ) -> Result<Self, McpError> {
        Ok(Self {
            server: server.to_owned(),
            client: reqwest::Client::builder().user_agent(USER_AGENT).build()?,
            url,
            headers,
            auth,
            session_id: None,
            protocol_version: None,
            next_id: 1,
        })
    }

    /// `initialize` 成功后记下服务端选的协议版本，之后每个请求都带这个头。
    pub(super) fn note_initialized(&mut self, result: &Value) {
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }

    #[cfg(test)]
    pub(super) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    async fn bearer(&mut self) -> Result<Option<String>, McpError> {
        match &mut self.auth {
            McpAuth::None => Ok(None),
            McpAuth::Bearer(token) => Ok(Some(token.clone())),
            McpAuth::OAuth(session) => session.access_token().await.map(Some),
        }
    }

    async fn post(&mut self, body: &Value) -> Result<reqwest::Response, McpError> {
        let mut request = self
            .client
            .post(self.url.clone())
            .headers(self.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream");
        if let Some(id) = &self.session_id {
            request = request.header(SESSION_HEADER, id);
        }
        if let Some(version) = &self.protocol_version {
            request = request.header(PROTOCOL_HEADER, version);
        }
        if let Some(token) = self.bearer().await? {
            request = request.bearer_auth(token);
        }
        Ok(request.json(body).send().await?)
    }

    pub(super) async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let mut refreshed = false;
        let mut reinitialized = false;
        loop {
            let response = self.post(&body).await?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED {
                if !refreshed
                    && let McpAuth::OAuth(session) = &mut self.auth
                    && session.refresh().await?
                {
                    refreshed = true;
                    continue;
                }
                return Err(McpError::Unauthorized {
                    server: self.server.clone(),
                    hint: resource_metadata_hint(response.headers()),
                });
            }
            if status == reqwest::StatusCode::NOT_FOUND
                && self.session_id.is_some()
                && method != "initialize"
                && !reinitialized
            {
                // 会话过期或服务端重启：重新握手一次，再重发这条请求。
                self.session_id = None;
                reinitialized = true;
                Box::pin(self.handshake()).await?;
                continue;
            }
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(McpError::Http {
                    server: self.server.clone(),
                    status: status.as_u16(),
                    body: text.chars().take(ERROR_BODY_CHARS).collect(),
                });
            }
            if method == "initialize"
                && let Some(session) = response.headers().get(SESSION_HEADER)
                && let Ok(session) = session.to_str()
            {
                self.session_id = Some(session.to_owned());
            }
            return read_response(response, id).await;
        }
    }

    async fn handshake(&mut self) -> Result<(), McpError> {
        let result = self
            .request("initialize", super::initialize_params())
            .await?;
        self.note_initialized(&result);
        self.notify("notifications/initialized", json!({})).await
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let body = json!({"jsonrpc":"2.0","method":method,"params":params});
        let response = self.post(&body).await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized {
                server: self.server.clone(),
                hint: resource_metadata_hint(response.headers()),
            });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(McpError::Http {
                server: self.server.clone(),
                status: status.as_u16(),
                body: text.chars().take(ERROR_BODY_CHARS).collect(),
            });
        }
        // 通知的正常回答是 202 无正文；服务端若开了一条 SSE 流，我们不读，丢掉即断。
        Ok(())
    }
}

impl Drop for HttpConnection {
    fn drop(&mut self) {
        // 规范说客户端结束时应当 DELETE 会话。尽力而为：没有运行时、发不出去都无所谓。
        let (Some(session), Ok(handle)) = (
            self.session_id.take(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let mut request = self
            .client
            .delete(self.url.clone())
            .headers(self.headers.clone())
            .header(SESSION_HEADER, session);
        let token = match &self.auth {
            McpAuth::None => None,
            McpAuth::Bearer(token) => Some(token.clone()),
            McpAuth::OAuth(session) => Some(session.cached_token().to_owned()),
        };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        handle.spawn(async move {
            let _ = request.send().await;
        });
    }
}

/// `WWW-Authenticate: Bearer resource_metadata="https://…"` 里的地址。
pub(crate) fn resource_metadata_hint(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(parse_resource_metadata)
}

pub(crate) fn parse_resource_metadata(challenge: &str) -> Option<String> {
    let start = challenge.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = challenge[start..].trim_start();
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => rest.split([',', ' ']).next()?,
    };
    (!value.is_empty()).then(|| value.to_owned())
}

async fn read_response(response: reqwest::Response, id: u64) -> Result<Value, McpError> {
    if is_event_stream(&response) {
        let mut decoder = SseDecoder::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let events = decoder
                .push(&chunk?)
                .map_err(|error| McpError::Transport(error.to_string()))?;
            for event in events {
                if event.data.trim().is_empty() {
                    continue;
                }
                let message: Value = serde_json::from_str(&event.data)?;
                if let Some(result) = match_response(&message, id)? {
                    return Ok(result);
                }
            }
        }
        return Err(McpError::Remote(
            "event stream ended before the response arrived".to_owned(),
        ));
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_JSON_BODY_BYTES {
        return Err(McpError::Transport(format!(
            "response body exceeds {MAX_JSON_BODY_BYTES} bytes"
        )));
    }
    let message: Value = serde_json::from_slice(&bytes)?;
    match_response(&message, id)?
        .ok_or_else(|| McpError::Remote("response carried no message for this request".to_owned()))
}

/// 单条或批量里找 id 对得上的那条。别的（通知、服务端请求）这一版一律忽略。
fn match_response(message: &Value, id: u64) -> Result<Option<Value>, McpError> {
    let candidates: Vec<&Value> = match message {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    for candidate in candidates {
        if candidate.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = candidate.get("error") {
            return Err(McpError::Remote(error.to_string()));
        }
        return Ok(Some(
            candidate.get("result").cloned().unwrap_or(Value::Null),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::test_http::{Request, Response, spawn_http_server};
    use super::*;

    fn json_response(status: u16, body: Value) -> Response {
        Response {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.to_string().into_bytes(),
        }
    }

    fn initialize_result() -> Value {
        json!({"protocolVersion": "2025-03-26", "capabilities": {}, "serverInfo": {"name": "fake", "version": "1"}})
    }

    async fn connection(url: &str, auth: McpAuth) -> HttpConnection {
        HttpConnection::new(
            "fake",
            reqwest::Url::parse(url).unwrap(),
            HeaderMap::new(),
            auth,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn speaks_streamable_http_with_session_and_protocol_headers() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let (address, _server) = spawn_http_server(move |request: Request| {
            log.lock().unwrap().push(request.clone());
            let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
            match body["method"].as_str() {
                Some("initialize") => {
                    let mut response = json_response(200, json!({"jsonrpc":"2.0","id":body["id"],"result":initialize_result()}));
                    response.headers.push(("mcp-session-id".to_owned(), "sess-1".to_owned()));
                    response
                }
                Some("notifications/initialized") => Response { status: 202, headers: vec![], body: vec![] },
                Some("tools/list") => Response {
                    status: 200,
                    headers: vec![("content-type".to_owned(), "text/event-stream".to_owned())],
                    body: format!(
                        "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                        json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hi"}}),
                        json!({"jsonrpc":"2.0","id":body["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
                    )
                    .into_bytes(),
                },
                Some("tools/call") => json_response(200, json!({"jsonrpc":"2.0","id":body["id"],"result":{"content":[{"type":"text","text":"pong"}]}})),
                _ => json_response(400, json!({"error":"unexpected"})),
            }
        })
        .await;
        let mut connection = connection(
            &format!("http://{address}/mcp"),
            McpAuth::Bearer("tok".to_owned()),
        )
        .await;
        let init = connection
            .request("initialize", super::super::initialize_params())
            .await
            .unwrap();
        connection.note_initialized(&init);
        connection
            .notify("notifications/initialized", json!({}))
            .await
            .unwrap();
        assert_eq!(connection.session_id(), Some("sess-1"));

        let listed = connection.request("tools/list", json!({})).await.unwrap();
        assert_eq!(listed["tools"][0]["name"], "echo");
        let called = connection
            .request("tools/call", json!({"name":"echo","arguments":{}}))
            .await
            .unwrap();
        assert_eq!(called["content"][0]["text"], "pong");

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer tok")
        );
        assert!(
            requests[0]
                .headers
                .get("accept")
                .unwrap()
                .contains("text/event-stream")
        );
        assert!(
            !requests[0].headers.contains_key(SESSION_HEADER),
            "no session before initialize"
        );
        for request in &requests[1..] {
            assert_eq!(
                request.headers.get(SESSION_HEADER).map(String::as_str),
                Some("sess-1")
            );
            assert_eq!(
                request.headers.get(PROTOCOL_HEADER).map(String::as_str),
                Some("2025-03-26")
            );
        }
    }

    #[tokio::test]
    async fn unauthorized_carries_the_resource_metadata_hint() {
        let (address, _server) = spawn_http_server(|_request: Request| Response {
            status: 401,
            headers: vec![(
                "www-authenticate".to_owned(),
                "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\"".to_owned(),
            )],
            body: vec![],
        })
        .await;
        let mut connection = connection(&format!("http://{address}/mcp"), McpAuth::None).await;
        let error = connection
            .request("initialize", json!({}))
            .await
            .unwrap_err();
        match error {
            McpError::Unauthorized { server, hint } => {
                assert_eq!(server, "fake");
                assert_eq!(
                    hint.as_deref(),
                    Some("https://mcp.example.com/.well-known/oauth-protected-resource")
                );
            }
            other => panic!("expected Unauthorized, got {other}"),
        }
        assert!(error_text(&connection).contains("willdeep mcp login fake"));
    }

    fn error_text(connection: &HttpConnection) -> String {
        McpError::Unauthorized {
            server: connection.server.clone(),
            hint: None,
        }
        .to_string()
    }

    #[tokio::test]
    async fn an_expired_session_is_reinitialized_once() {
        let initializes = Arc::new(Mutex::new(0usize));
        let counter = initializes.clone();
        let (address, _server) = spawn_http_server(move |request: Request| {
            let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
            match body["method"].as_str() {
                Some("initialize") => {
                    let mut count = counter.lock().unwrap();
                    *count += 1;
                    let mut response = json_response(
                        200,
                        json!({"jsonrpc":"2.0","id":body["id"],"result":initialize_result()}),
                    );
                    response
                        .headers
                        .push(("mcp-session-id".to_owned(), format!("sess-{count}")));
                    response
                }
                Some("notifications/initialized") => Response {
                    status: 202,
                    headers: vec![],
                    body: vec![],
                },
                _ if request.headers.get(SESSION_HEADER).map(String::as_str) == Some("sess-1") => {
                    Response {
                        status: 404,
                        headers: vec![],
                        body: b"session expired".to_vec(),
                    }
                }
                _ => json_response(
                    200,
                    json!({"jsonrpc":"2.0","id":body["id"],"result":{"tools":[]}}),
                ),
            }
        })
        .await;
        let mut connection = connection(&format!("http://{address}/mcp"), McpAuth::None).await;
        let init = connection
            .request("initialize", super::super::initialize_params())
            .await
            .unwrap();
        connection.note_initialized(&init);
        assert_eq!(connection.session_id(), Some("sess-1"));
        let listed = connection.request("tools/list", json!({})).await.unwrap();
        assert_eq!(listed["tools"], json!([]));
        assert_eq!(connection.session_id(), Some("sess-2"));
        assert_eq!(*initializes.lock().unwrap(), 2);
    }

    #[test]
    fn parses_resource_metadata_challenges() {
        assert_eq!(
            parse_resource_metadata("Bearer realm=\"mcp\", resource_metadata=\"https://a/b\""),
            Some("https://a/b".to_owned())
        );
        assert_eq!(
            parse_resource_metadata("Bearer resource_metadata=https://a/b"),
            Some("https://a/b".to_owned())
        );
        assert_eq!(parse_resource_metadata("Bearer realm=\"x\""), None);
    }
}
