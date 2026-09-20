//! 远程 MCP 服务的 OAuth 2.1：授权码 + PKCE，回环回调，token 落盘与刷新。
//!
//! 发现链按 MCP 授权规范走：向 MCP 端点发一次未鉴权的 `initialize`，401 的
//! `WWW-Authenticate` 里给出受保护资源元数据地址（RFC 9728，没给就按
//! `/.well-known/oauth-protected-resource` 猜），从中拿到授权服务器；再取授权
//! 服务器元数据（RFC 8414，退回 OIDC discovery，再退回固定端点）；没有预注册的
//! `client_id` 就动态注册（RFC 7591）。token 请求带 `resource`（RFC 8707），把
//! token 绑到这个 MCP 服务上，不能拿去别处用。
//!
//! 回调只监听 `127.0.0.1` 的随机端口、只收一次；`state` 对不上一律拒绝。
//! token 存 `$WILLDEEP_HOME/mcp-oauth/<server>.json`，0600、原子写。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{McpError, McpOAuthConfig};

const CLIENT_NAME: &str = "WillDeep";
const CALLBACK_PATH: &str = "/callback";
const USER_AGENT: &str = concat!("willdeep/", env!("CARGO_PKG_VERSION"));
/// 过期前这么多秒就当已过期，免得在飞行途中失效。
const EXPIRY_SKEW_SECONDS: u64 = 30;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_CALLBACK_BYTES: usize = 8 * 1024;
const TOKEN_DIRECTORY: &str = "mcp-oauth";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub registered_dynamically: bool,
    pub obtained_at: u64,
}

impl StoredTokens {
    pub fn expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| now() + EXPIRY_SKEW_SECONDS >= expires_at)
    }
}

pub fn token_path(home: &Path, server: &str) -> PathBuf {
    home.join(TOKEN_DIRECTORY)
        .join(format!("{}.json", super::sanitize(server)))
}

pub fn load_tokens(home: &Path, server: &str) -> Result<Option<StoredTokens>, McpError> {
    let path = token_path(home, server);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn save_tokens(home: &Path, server: &str, tokens: &StoredTokens) -> Result<(), McpError> {
    let path = token_path(home, server);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    crate::detached_job::write_private_atomic(
        &path,
        serde_json::to_vec_pretty(tokens)?.as_slice(),
    )?;
    Ok(())
}

/// 删掉 token 文件。返回是否真有东西可删。
pub fn forget_tokens(home: &Path, server: &str) -> Result<bool, McpError> {
    let path = token_path(home, server);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// 一个已登录服务的 token 持有者：到期自动刷新，刷新结果落盘。
pub struct OAuthSession {
    home: PathBuf,
    server: String,
    client: reqwest::Client,
    tokens: StoredTokens,
}

impl OAuthSession {
    pub(super) fn load(
        home: &Path,
        server: &str,
        _config: &McpOAuthConfig,
    ) -> Result<Option<Self>, McpError> {
        let Some(tokens) = load_tokens(home, server)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            home: home.to_path_buf(),
            server: server.to_owned(),
            client: http_client()?,
            tokens,
        }))
    }

    pub(super) fn cached_token(&self) -> &str {
        &self.tokens.access_token
    }

    pub async fn access_token(&mut self) -> Result<String, McpError> {
        if self.tokens.expired() && !self.refresh().await? {
            return Err(McpError::Unauthorized {
                server: self.server.clone(),
                hint: None,
            });
        }
        Ok(self.tokens.access_token.clone())
    }

    /// 有 refresh token 就换一张新的并落盘。没有、或授权服务器不认（400 / 401），
    /// 返回 `false`：那是「请重新登录」，不是传输故障。
    pub async fn refresh(&mut self) -> Result<bool, McpError> {
        let Some(refresh_token) = self.tokens.refresh_token.clone() else {
            return Ok(false);
        };
        let mut form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token),
            ("client_id", self.tokens.client_id.clone()),
        ];
        if let Some(secret) = &self.tokens.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        if let Some(resource) = &self.tokens.resource {
            form.push(("resource", resource.clone()));
        }
        let response = self
            .client
            .post(&self.tokens.token_endpoint)
            .form(&form)
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Ok(false);
        }
        if !status.is_success() {
            return Err(McpError::OAuth(format!(
                "token refresh for {} answered HTTP {}",
                self.server,
                status.as_u16()
            )));
        }
        let issued: TokenResponse = response.json().await?;
        self.tokens.apply(issued);
        save_tokens(&self.home, &self.server, &self.tokens)?;
        Ok(true)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl StoredTokens {
    fn apply(&mut self, issued: TokenResponse) {
        self.access_token = issued.access_token;
        self.expires_at = issued.expires_in.map(|seconds| now() + seconds);
        if issued.refresh_token.is_some() {
            self.refresh_token = issued.refresh_token;
        }
        if issued.scope.is_some() {
            self.scope = issued.scope;
        }
        self.obtained_at = now();
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct AuthorizationServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

pub struct LoginRequest<'a> {
    pub home: &'a Path,
    pub server: &'a str,
    pub url: &'a str,
    pub config: &'a McpOAuthConfig,
    /// 机密客户端的 secret，调用方从 `client_secret_env` 取好再传进来。
    pub client_secret: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOutcome {
    pub authorization_server: String,
    pub client_id: String,
    pub registered_dynamically: bool,
    pub scope: Option<String>,
    pub expires_at: Option<u64>,
}

/// 完整登录流程。`open_browser` 拿到授权 URL 去打开浏览器（打不开也行，URL 会
/// 通过 `progress` 打印出来）；`progress` 收每一步的人话。
pub async fn login(
    request: LoginRequest<'_>,
    open_browser: &(dyn Fn(&str) + Send + Sync),
    progress: &(dyn Fn(&str) + Send + Sync),
) -> Result<LoginOutcome, McpError> {
    let client = http_client()?;
    let mcp_url = reqwest::Url::parse(request.url)
        .map_err(|error| McpError::OAuth(format!("MCP url is not valid: {error}")))?;

    let resource = discover_resource(&client, &mcp_url).await?;
    let issuer = resource
        .authorization_servers
        .first()
        .cloned()
        .unwrap_or_else(|| origin(&mcp_url));
    progress(&format!("authorization server: {issuer}"));
    let metadata = discover_authorization_server(&client, &issuer).await?;
    if !metadata.code_challenge_methods_supported.is_empty()
        && !metadata
            .code_challenge_methods_supported
            .iter()
            .any(|method| method == "S256")
    {
        return Err(McpError::OAuth(
            "authorization server does not support PKCE S256".to_owned(),
        ));
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}{CALLBACK_PATH}",
        listener.local_addr()?.port()
    );

    let scopes = if request.config.scopes.is_empty() {
        resource.scopes_supported.clone()
    } else {
        request.config.scopes.clone()
    };
    let scope = (!scopes.is_empty()).then(|| scopes.join(" "));
    let (client_id, client_secret, registered_dynamically) = match &request.config.client_id {
        Some(client_id) => (client_id.clone(), request.client_secret.clone(), false),
        None => {
            let endpoint = metadata.registration_endpoint.clone().ok_or_else(|| {
                McpError::OAuth(
                    "authorization server has no registration endpoint; set oauth.client_id"
                        .to_owned(),
                )
            })?;
            progress("registering a client (RFC 7591)");
            let registered =
                register_client(&client, &endpoint, &redirect_uri, scope.as_deref()).await?;
            (registered.0, registered.1, true)
        }
    };

    let (verifier, challenge) = pkce_pair();
    let state = random_token();
    let resource_indicator = resource
        .resource
        .clone()
        .unwrap_or_else(|| canonical_resource(&mcp_url));
    let mut authorize = reqwest::Url::parse(&metadata.authorization_endpoint).map_err(|error| {
        McpError::OAuth(format!("authorization_endpoint is not valid: {error}"))
    })?;
    {
        let mut query = authorize.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &resource_indicator);
        if let Some(scope) = &scope {
            query.append_pair("scope", scope);
        }
    }
    progress(&format!("open this URL to authorize:\n{authorize}"));
    open_browser(authorize.as_str());

    let code = tokio::time::timeout(request.timeout, wait_for_callback(listener, &state))
        .await
        .map_err(|_| McpError::OAuth("timed out waiting for the browser callback".to_owned()))??;
    progress("authorization code received; exchanging it for tokens");

    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code),
        ("redirect_uri", redirect_uri.clone()),
        ("client_id", client_id.clone()),
        ("code_verifier", verifier),
        ("resource", resource_indicator.clone()),
    ];
    if let Some(secret) = &client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let response = client
        .post(&metadata.token_endpoint)
        .form(&form)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(McpError::OAuth(format!(
            "token endpoint answered HTTP {}: {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        )));
    }
    let issued: TokenResponse = response.json().await?;
    let mut tokens = StoredTokens {
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        scope: None,
        token_endpoint: metadata.token_endpoint.clone(),
        client_id: client_id.clone(),
        client_secret,
        resource: Some(resource_indicator),
        registered_dynamically,
        obtained_at: now(),
    };
    tokens.apply(issued);
    save_tokens(request.home, request.server, &tokens)?;
    Ok(LoginOutcome {
        authorization_server: issuer,
        client_id,
        registered_dynamically,
        scope: tokens.scope,
        expires_at: tokens.expires_at,
    })
}

fn http_client() -> Result<reqwest::Client, McpError> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// 未鉴权探一次 MCP 端点，拿 401 里的元数据地址；拿不到就按 RFC 9728 的路径猜。
async fn discover_resource(
    client: &reqwest::Client,
    mcp_url: &reqwest::Url,
) -> Result<ProtectedResourceMetadata, McpError> {
    let probe = client
        .post(mcp_url.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(&serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":super::initialize_params()}))
        .send()
        .await?;
    let mut candidates = Vec::new();
    if probe.status() == reqwest::StatusCode::UNAUTHORIZED
        && let Some(hint) = super::http::resource_metadata_hint(probe.headers())
    {
        candidates.push(hint);
    }
    drop(probe);
    let base = origin(mcp_url);
    let path = mcp_url.path().trim_end_matches('/');
    if !path.is_empty() {
        candidates.push(format!("{base}/.well-known/oauth-protected-resource{path}"));
    }
    candidates.push(format!("{base}/.well-known/oauth-protected-resource"));
    for candidate in candidates {
        if let Some(value) = fetch_json(client, &candidate).await? {
            let parsed: ProtectedResourceMetadata = serde_json::from_value(value)?;
            if !parsed.authorization_servers.is_empty() {
                return Ok(parsed);
            }
        }
    }
    // 没有资源元数据的老服务：授权服务器就是它自己（2025-03-26 的规矩）。
    Ok(ProtectedResourceMetadata::default())
}

/// RFC 8414 与 OIDC discovery 的候选地址；带路径的 issuer 两种插法都试。
pub(crate) fn authorization_server_candidates(issuer: &str) -> Vec<String> {
    let trimmed = issuer.trim_end_matches('/');
    let Ok(url) = reqwest::Url::parse(trimmed) else {
        return Vec::new();
    };
    let base = origin(&url);
    let path = url.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            format!("{base}/.well-known/oauth-authorization-server"),
            format!("{base}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{base}/.well-known/oauth-authorization-server{path}"),
            format!("{base}/.well-known/openid-configuration{path}"),
            format!("{base}{path}/.well-known/openid-configuration"),
        ]
    }
}

async fn discover_authorization_server(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<AuthorizationServerMetadata, McpError> {
    for candidate in authorization_server_candidates(issuer) {
        if let Some(value) = fetch_json(client, &candidate).await?
            && let Ok(metadata) = serde_json::from_value::<AuthorizationServerMetadata>(value)
        {
            return Ok(metadata);
        }
    }
    // 没有元数据：按规范的固定端点退回。
    let base = issuer.trim_end_matches('/');
    Ok(AuthorizationServerMetadata {
        authorization_endpoint: format!("{base}/authorize"),
        token_endpoint: format!("{base}/token"),
        registration_endpoint: Some(format!("{base}/register")),
        code_challenge_methods_supported: Vec::new(),
    })
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Option<Value>, McpError> {
    let Ok(response) = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    else {
        return Ok(None);
    };
    if !response.status().is_success() {
        return Ok(None);
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(McpError::OAuth(format!(
            "metadata at {url} exceeds {MAX_METADATA_BYTES} bytes"
        )));
    }
    Ok(serde_json::from_slice(&bytes).ok())
}

async fn register_client(
    client: &reqwest::Client,
    endpoint: &str,
    redirect_uri: &str,
    scope: Option<&str>,
) -> Result<(String, Option<String>), McpError> {
    let mut body = serde_json::json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(scope) = scope {
        body["scope"] = Value::String(scope.to_owned());
    }
    let response = client.post(endpoint).json(&body).send().await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(McpError::OAuth(format!(
            "dynamic client registration answered HTTP {}: {}",
            status.as_u16(),
            text.chars().take(300).collect::<String>()
        )));
    }
    let registered: Value = response.json().await?;
    let client_id = registered
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::OAuth("registration response has no client_id".to_owned()))?
        .to_owned();
    let client_secret = registered
        .get("client_secret")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((client_id, client_secret))
}

/// 等浏览器回来。只收一次，`state` 必须对上；`/favicon.ico` 之类的顺手 404。
async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, McpError> {
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buffer.windows(4).any(|window| window == b"\r\n\r\n")
            && buffer.len() < MAX_CALLBACK_BYTES
        {
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buffer).into_owned();
        let target = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_owned();
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        if path != CALLBACK_PATH {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let outcome = parse_callback(query, expected_state);
        let (status, text) = match &outcome {
            Ok(_) => (
                "200 OK",
                "WillDeep 登录完成，可以关掉这个页面了。 / Signed in; you can close this tab."
                    .to_owned(),
            ),
            Err(error) => ("400 Bad Request", format!("WillDeep 登录失败：{error}")),
        };
        let page =
            format!("<!doctype html><meta charset=\"utf-8\"><title>WillDeep</title><p>{text}</p>");
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.shutdown().await;
        return outcome;
    }
}

/// 回调查询串 → 授权码。`error=` 是授权服务器拒了；`state` 不对是别人打过来的。
pub(crate) fn parse_callback(query: &str, expected_state: &str) -> Result<String, McpError> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            "error_description" => description = Some(value),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Err(McpError::OAuth(format!(
            "authorization server refused: {error}{}",
            description
                .map(|text| format!(" ({text})"))
                .unwrap_or_default()
        )));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(McpError::OAuth("callback state mismatch".to_owned()));
    }
    code.filter(|code| !code.is_empty())
        .ok_or_else(|| McpError::OAuth("callback carried no authorization code".to_owned()))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        output.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        output.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// PKCE：verifier 是 32 字节随机数的 base64url（43 字符），challenge 是它 SHA-256 的 base64url。
fn pkce_pair() -> (String, String) {
    let verifier = random_token();
    let challenge = code_challenge(&verifier);
    (verifier, challenge)
}

pub(crate) fn code_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64url(&bytes)
}

pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let buffer = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = (buffer[0] as u32) << 16 | (buffer[1] as u32) << 8 | buffer[2] as u32;
        output.push(ALPHABET[(value >> 18) as usize & 63] as char);
        output.push(ALPHABET[(value >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(value >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[value as usize & 63] as char);
        }
    }
    output
}

fn origin(url: &reqwest::Url) -> String {
    let mut origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        origin.push_str(&format!(":{port}"));
    }
    origin
}

/// RFC 8707 的资源标识：去掉片段，路径原样。
fn canonical_resource(url: &reqwest::Url) -> String {
    let mut canonical = url.clone();
    canonical.set_fragment(None);
    canonical.to_string()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::test_http::{Request, Response, spawn_http_server};
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_vector() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(code_challenge(&verifier), challenge);
    }

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn derives_discovery_urls_for_issuers_with_and_without_paths() {
        assert_eq!(
            authorization_server_candidates("https://as.example.com/"),
            vec![
                "https://as.example.com/.well-known/oauth-authorization-server",
                "https://as.example.com/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            authorization_server_candidates("https://as.example.com/tenant/a"),
            vec![
                "https://as.example.com/.well-known/oauth-authorization-server/tenant/a",
                "https://as.example.com/.well-known/openid-configuration/tenant/a",
                "https://as.example.com/tenant/a/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn callback_parsing_checks_state_and_surfaces_errors() {
        assert_eq!(
            parse_callback("code=abc%20d&state=s1", "s1").unwrap(),
            "abc d"
        );
        assert!(parse_callback("code=abc&state=other", "s1").is_err());
        assert!(parse_callback("state=s1", "s1").is_err());
        let refused = parse_callback("error=access_denied&error_description=nope&state=s1", "s1")
            .unwrap_err();
        assert!(refused.to_string().contains("access_denied"));
    }

    #[test]
    fn tokens_round_trip_privately_and_know_when_they_expire() {
        let home = std::env::temp_dir().join(format!("willdeep-oauth-{}", uuid::Uuid::new_v4()));
        let mut tokens = StoredTokens {
            access_token: "at".to_owned(),
            refresh_token: Some("rt".to_owned()),
            expires_at: Some(now() + 3600),
            scope: None,
            token_endpoint: "https://as/token".to_owned(),
            client_id: "cid".to_owned(),
            client_secret: None,
            resource: Some("https://mcp/x".to_owned()),
            registered_dynamically: true,
            obtained_at: now(),
        };
        save_tokens(&home, "remote/one", &tokens).unwrap();
        let path = token_path(&home, "remote/one");
        assert!(path.ends_with("mcp-oauth/remote_one.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            load_tokens(&home, "remote/one").unwrap(),
            Some(tokens.clone())
        );
        assert!(!tokens.expired());
        tokens.expires_at = Some(now() + 10);
        assert!(tokens.expired(), "inside the skew window counts as expired");
        assert!(forget_tokens(&home, "remote/one").unwrap());
        assert!(!forget_tokens(&home, "remote/one").unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 端到端：假的 MCP 端点回 401 指向资源元数据，假的授权服务器提供元数据、
    /// 动态注册、token 交换；「浏览器」是一个直接打回调地址的任务。
    #[tokio::test]
    async fn logs_in_against_a_fake_authorization_server() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let (address, _server) = spawn_http_server(move |request: Request| {
            log.lock().unwrap().push(request.clone());
            let base = request.headers.get("host").cloned().unwrap_or_default();
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/mcp") => Response {
                    status: 401,
                    headers: vec![(
                        "www-authenticate".to_owned(),
                        format!("Bearer resource_metadata=\"http://{base}/.well-known/oauth-protected-resource/mcp\""),
                    )],
                    body: vec![],
                },
                ("GET", "/.well-known/oauth-protected-resource/mcp") => json(200, serde_json::json!({
                    "resource": format!("http://{base}/mcp"),
                    "authorization_servers": [format!("http://{base}/auth")],
                    "scopes_supported": ["mcp:read", "mcp:write"]
                })),
                ("GET", "/.well-known/oauth-authorization-server/auth") => json(200, serde_json::json!({
                    "issuer": format!("http://{base}/auth"),
                    "authorization_endpoint": format!("http://{base}/auth/authorize"),
                    "token_endpoint": format!("http://{base}/auth/token"),
                    "registration_endpoint": format!("http://{base}/auth/register"),
                    "code_challenge_methods_supported": ["S256"]
                })),
                ("POST", "/auth/register") => json(201, serde_json::json!({"client_id": "dyn-client"})),
                ("POST", "/auth/token") => {
                    let form: std::collections::BTreeMap<String, String> = request
                        .body
                        .split('&')
                        .filter_map(|pair| pair.split_once('='))
                        .map(|(k, v)| (k.to_owned(), percent_decode(v)))
                        .collect();
                    assert_eq!(form.get("grant_type").map(String::as_str), Some("authorization_code"));
                    assert_eq!(form.get("code").map(String::as_str), Some("the-code"));
                    assert_eq!(form.get("client_id").map(String::as_str), Some("dyn-client"));
                    assert_eq!(form.get("resource").map(String::as_str), Some(format!("http://{base}/mcp").as_str()));
                    assert!(form.get("code_verifier").is_some_and(|v| v.len() == 43));
                    json(200, serde_json::json!({"access_token": "at-1", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt-1", "scope": "mcp:read mcp:write"}))
                }
                _ => json(404, serde_json::json!({"error": "not found"})),
            }
        })
        .await;

        let home =
            std::env::temp_dir().join(format!("willdeep-oauth-login-{}", uuid::Uuid::new_v4()));
        let config = McpOAuthConfig::default();
        let url = format!("http://{address}/mcp");
        let browser = |authorize_url: &str| {
            // 假浏览器：解析授权 URL 里的 state 和 redirect_uri，直接打回调。
            let parsed = reqwest::Url::parse(authorize_url).unwrap();
            let pairs: std::collections::BTreeMap<_, _> =
                parsed.query_pairs().into_owned().collect();
            assert_eq!(
                pairs.get("code_challenge_method").map(String::as_str),
                Some("S256")
            );
            assert_eq!(
                pairs.get("scope").map(String::as_str),
                Some("mcp:read mcp:write")
            );
            let redirect = format!(
                "{}?code=the-code&state={}",
                pairs["redirect_uri"], pairs["state"]
            );
            tokio::spawn(async move {
                let _ = reqwest::Client::new().get(redirect).send().await;
            });
        };
        let outcome = login(
            LoginRequest {
                home: &home,
                server: "remote",
                url: &url,
                config: &config,
                client_secret: None,
                timeout: Duration::from_secs(10),
            },
            &browser,
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(outcome.client_id, "dyn-client");
        assert!(outcome.registered_dynamically);
        assert_eq!(outcome.scope.as_deref(), Some("mcp:read mcp:write"));
        let stored = load_tokens(&home, "remote").unwrap().unwrap();
        assert_eq!(stored.access_token, "at-1");
        assert_eq!(stored.refresh_token.as_deref(), Some("rt-1"));
        assert!(stored.expires_at.is_some_and(|at| at > now() + 3000));
        let registration = seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.path == "/auth/register")
            .cloned()
            .unwrap();
        let body: Value = serde_json::from_str(&registration.body).unwrap();
        assert!(
            body["redirect_uris"][0]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    fn json(status: u16, body: Value) -> Response {
        Response {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.to_string().into_bytes(),
        }
    }
}
