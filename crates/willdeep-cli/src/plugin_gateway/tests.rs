//! 网关的端到端测试：真的绑回环端口、真的拉起假插件进程（python3）。
//! 家目录、插件数据目录一律在临时目录里，不碰真实的 `~/.willdeep` 与 `~/Library`。

use std::sync::Mutex as StdMutex;

use super::*;
use willdeep_core::plugin::gateway::{DISCOVERY_FILE, PLUGIN_ENDPOINT_FILE, discovery_path};
use willdeep_core::plugin::test_support::{
    events, install_fake_plugin, python_available, scratch_home,
};

struct Fixture {
    home: PathBuf,
    host: Arc<PluginHost>,
    gateway: PluginGateway,
    data: PathBuf,
    log: PathBuf,
    client: reqwest::Client,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Fixture {
    async fn new(label: &str) -> Option<Self> {
        if !python_available() {
            eprintln!("python3 not found; skipping gateway test");
            return None;
        }
        let home = scratch_home(label);
        let log = install_fake_plugin(&home, "demo", r#"["process.execute"]"#, r#"["srv"]"#);
        install_fake_plugin(&home, "off", r#"["process.execute"]"#, r#"["srv"]"#);
        let host = Arc::new(PluginHost::discover(&home).expect("host"));
        host.approve("demo", 1).await.expect("approve");
        host.set_enabled("demo", true)
            .await
            .expect("write")
            .expect("enabled");
        let data = home.join("plugin-data-test");
        let dirs = data.clone();
        let gateway = PluginGateway::start_with(
            &home,
            host.clone(),
            Box::new(move |plugin: &str| vec![dirs.join(plugin)]),
        )
        .await
        .expect("gateway");
        Some(Self {
            home,
            host,
            gateway,
            data,
            log,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        })
    }

    fn token(&self) -> String {
        read_discovery(&self.home).expect("discovery").token
    }

    fn url(&self, plugin: &str, server: &str) -> String {
        server_url(self.gateway.url(), plugin, server)
    }

    fn post(&self, plugin: &str, server: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.url(plugin, server))
            .bearer_auth(self.token())
    }

    async fn rpc(&self, body: Value) -> (StatusCode, Value) {
        let response = self
            .post("demo", "srv")
            .json(&body)
            .send()
            .await
            .expect("send");
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        let value = response.json::<Value>().await.unwrap_or(Value::Null);
        (status, value)
    }

    fn write_endpoint(&self, url: &str, token: &str) {
        let dir = self.data.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(PLUGIN_ENDPOINT_FILE),
            json!({"url": url, "token": token}).to_string(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn discovery_file_is_private_lists_enabled_servers_and_reuses_port_and_token() {
    let Some(fixture) = Fixture::new("gw-discovery").await else {
        return;
    };
    let discovery = read_discovery(&fixture.home).expect("discovery");
    assert_eq!(discovery.host, "willdeep-rs");
    assert_eq!(discovery.url, fixture.gateway.url());
    assert_eq!(discovery.token.len(), 64);
    assert_eq!(
        discovery.servers,
        vec![GatewayServerEntry {
            plugin_id: "demo".to_owned(),
            server: "srv".to_owned(),
            url: fixture.url("demo", "srv"),
        }],
        "only enabled plugins are listed"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(discovery_path(&fixture.home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // 原端口被占（第一个网关还开着）：换新端口，沿用 token，重写文件。
    let second = PluginGateway::start_with(
        &fixture.home,
        fixture.host.clone(),
        Box::new(|_: &str| Vec::new()),
    )
    .await
    .expect("second gateway");
    assert_ne!(second.url(), fixture.gateway.url());
    let rewritten = read_discovery(&fixture.home).expect("rewritten");
    assert_eq!(rewritten.token, discovery.token);
    assert_eq!(rewritten.url, second.url());

    // 原端口空着：原样沿用端口与 token。
    let free_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let pinned = GatewayDiscovery {
        url: format!("http://127.0.0.1:{free_port}"),
        ..rewritten.clone()
    };
    write_discovery(&fixture.home, &pinned).unwrap();
    let third = PluginGateway::start_with(
        &fixture.home,
        fixture.host.clone(),
        Box::new(|_: &str| Vec::new()),
    )
    .await
    .expect("third gateway");
    assert_eq!(third.url(), format!("http://127.0.0.1:{free_port}"));
    assert_eq!(
        read_discovery(&fixture.home).unwrap().token,
        discovery.token
    );

    // 停用后重写：servers 变空。
    fixture
        .host
        .set_enabled("demo", false)
        .await
        .unwrap()
        .unwrap();
    third.publish().await.unwrap();
    assert!(read_discovery(&fixture.home).unwrap().servers.is_empty());
    assert!(fixture.home.join(DISCOVERY_FILE).is_file());
}

#[tokio::test]
async fn guards_host_origin_auth_methods_routes_and_shapes() {
    let Some(fixture) = Fixture::new("gw-guards").await else {
        return;
    };
    let url = fixture.url("demo", "srv");
    let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let status = |response: reqwest::Response| response.status().as_u16();

    let unauthenticated = fixture
        .client
        .post(&url)
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(status(unauthenticated), 401);
    let wrong = fixture
        .client
        .post(&url)
        .bearer_auth("0".repeat(64))
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(status(wrong), 401);

    let rebinding = fixture
        .post("demo", "srv")
        .header(reqwest::header::HOST, "evil.example")
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(status(rebinding), 403);
    let foreign_origin = fixture
        .post("demo", "srv")
        .header(reqwest::header::ORIGIN, "https://evil.example")
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(status(foreign_origin), 403);
    let local_origin = fixture
        .post("demo", "srv")
        .header(reqwest::header::ORIGIN, "http://localhost:5173")
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(status(local_origin), 202);

    for method in [reqwest::Method::GET, reqwest::Method::DELETE] {
        let response = fixture
            .client
            .request(method, &url)
            .bearer_auth(fixture.token())
            .send()
            .await
            .unwrap();
        assert_eq!(status(response), 405);
    }
    for (plugin, server) in [("ghost", "srv"), ("off", "srv"), ("demo", "nope")] {
        let response = fixture
            .post(plugin, server)
            .json(&notification)
            .send()
            .await
            .unwrap();
        assert_eq!(status(response), 404, "{plugin}/{server}");
    }
    // 路由先于方法：未知插件上的 GET 是 404，不是 405（与 macOS 宿主同序）。
    let unknown_get = fixture
        .client
        .get(fixture.url("ghost", "srv"))
        .bearer_auth(fixture.token())
        .send()
        .await
        .unwrap();
    assert_eq!(status(unknown_get), 404);
    let lowercase_scheme = fixture
        .client
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("bearer {}", fixture.token()),
        )
        .json(&notification)
        .send()
        .await
        .unwrap();
    assert_eq!(
        status(lowercase_scheme),
        202,
        "auth scheme is case-insensitive"
    );
    let (code, bad_id) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": {"nested": true}, "method": "ping"}))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(bad_id["error"]["code"], -32600);
    let stray = fixture
        .client
        .post(format!("{}/elsewhere", fixture.gateway.url()))
        .bearer_auth(fixture.token())
        .send()
        .await
        .unwrap();
    assert_eq!(status(stray), 404);

    let (code, batch) = fixture
        .rpc(json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(batch["error"]["code"], -32600);
    let garbage = fixture
        .post("demo", "srv")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(garbage.status().as_u16(), 400);
    assert_eq!(
        garbage.json::<Value>().await.unwrap()["error"]["code"],
        -32700
    );

    let huge = fixture
        .post("demo", "srv")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(vec![b' '; MAX_BODY_BYTES + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(status(huge), 413);
    assert!(
        events(&fixture.log, "spawned").is_empty(),
        "nothing above needs the plugin"
    );
}

#[tokio::test]
async fn initialize_notifications_and_ping_never_reach_the_plugin() {
    let Some(fixture) = Fixture::new("gw-local").await else {
        return;
    };
    let response = fixture
        .post("demo", "srv")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {"sampling": {}}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.headers().get(SESSION_HEADER).is_some());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(body["result"]["capabilities"], json!({"tools": {}}));
    assert_eq!(body["result"]["serverInfo"]["name"], "demo/srv");
    assert_eq!(body["result"]["serverInfo"]["version"], "1.0.0");

    let (_, defaulted) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {}}))
        .await;
    assert_eq!(
        defaulted["result"]["protocolVersion"],
        DEFAULT_PROTOCOL_VERSION
    );

    let accepted = fixture
        .post("demo", "srv")
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status().as_u16(), 202);
    assert!(accepted.bytes().await.unwrap().is_empty());

    let (_, pong) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}))
        .await;
    assert_eq!(pong, json!({"jsonrpc": "2.0", "id": 9, "result": {}}));
    assert!(
        events(&fixture.log, "spawned").is_empty(),
        "initialize / notifications / ping must not start the plugin"
    );
}

#[tokio::test]
async fn relays_over_stdio_when_the_plugin_has_no_endpoint() {
    let Some(fixture) = Fixture::new("gw-relay").await else {
        return;
    };
    let (status, listed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "echo"))
    );
    // 确保插件在跑的那次 tools/list 顺手刷新了聊天工具目录。
    assert!(
        fixture
            .host
            .tool_catalog()
            .load()
            .contains_key("plugin:demo:srv")
    );

    let (_, echoed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": "x", "method": "tools/call",
            "params": {"name": "echo", "arguments": {"say": "hi"}}}))
        .await;
    assert_eq!(echoed["id"], "x");
    assert!(
        echoed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hi")
    );

    // 插件回的 JSON-RPC 错误原样带回。
    let (_, failed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "missing"}}))
        .await;
    assert_eq!(failed["error"]["code"], -32602);
    assert_eq!(
        events(&fixture.log, "spawned").len(),
        1,
        "one process for all calls"
    );
}

#[derive(Default)]
struct Seen {
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn fake_endpoint(status: StatusCode, reply: &'static str) -> (String, Arc<StdMutex<Seen>>) {
    let seen = Arc::new(StdMutex::new(Seen::default()));
    let recorder = seen.clone();
    let app = Router::new().route(
        "/mcp",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let recorder = recorder.clone();
            async move {
                let mut seen = recorder.lock().unwrap();
                seen.authorization = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                seen.body = body.to_vec();
                (status, [(header::CONTENT_TYPE, "application/json")], reply)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/mcp",
        listener.local_addr().unwrap().port()
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, seen)
}

#[tokio::test]
async fn forwards_to_the_plugin_endpoint_verbatim_and_falls_back_when_it_is_gone() {
    let Some(fixture) = Fixture::new("gw-forward").await else {
        return;
    };
    let reply = r#"{"jsonrpc":"2.0","id":7,"result":{"forwarded":true}}"#;
    let (endpoint, seen) = fake_endpoint(StatusCode::OK, reply).await;
    fixture.write_endpoint(&endpoint, "plugin-token");

    let raw =
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#;
    let response = fixture
        .post("demo", "srv")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(raw)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.text().await.unwrap(),
        reply,
        "response body is passed through"
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.authorization.as_deref(), Some("Bearer plugin-token"));
        assert_eq!(seen.body, raw.as_bytes(), "request body is passed through");
    }
    // 转发之前插件已经经宿主拉起（入口文件属于在跑的那个进程）。
    assert_eq!(events(&fixture.log, "spawned").len(), 1);

    // 入口连不上（进程换了、端口没了）：重读一次仍不行就走 stdio 中转。
    let closed = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    fixture.write_endpoint(&format!("http://127.0.0.1:{closed}/mcp"), "stale");
    let (_, relayed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": {"name": "echo", "arguments": {"via": "stdio"}}}))
        .await;
    assert!(
        relayed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("stdio")
    );

    // 入口不认 token（文件是另一个进程留下的）：同样退回中转。
    let (rejecting, _) =
        fake_endpoint(StatusCode::UNAUTHORIZED, r#"{"error":"Unauthorized."}"#).await;
    fixture.write_endpoint(&rejecting, "other");
    let (_, relayed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "echo", "arguments": {"via": "fallback"}}}))
        .await;
    assert!(
        relayed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fallback")
    );

    // 入口回 404（旧端口上换了别的服务、路径不对）：同样算没送到。
    let (missing, _) = fake_endpoint(StatusCode::NOT_FOUND, r#"{"error":"Not found."}"#).await;
    fixture.write_endpoint(&missing, "other");
    let (_, relayed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": {"name": "echo", "arguments": {"via": "not-found"}}}))
        .await;
    assert!(
        relayed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not-found")
    );

    // 入口不是回环地址：不认，不带着插件 token 发出去。
    fixture.write_endpoint("http://example.com:80/mcp", "leak");
    let (_, relayed) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": {"name": "echo", "arguments": {"via": "guarded"}}}))
        .await;
    assert!(
        relayed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("guarded")
    );
}

/// 请求可能已经在插件里执行了：不重发、不中转，回 -32603。出图、提交视频不是
/// 幂等的，重发会执行两遍。
#[tokio::test]
async fn failures_after_delivery_are_reported_not_retried() {
    let Some(fixture) = Fixture::new("gw-no-resend").await else {
        return;
    };
    let echo_calls = |fixture: &Fixture| {
        events(&fixture.log, "tools_call")
            .into_iter()
            .filter(|event| event["name"] == "echo")
            .count()
    };

    // 其他非 2xx：-32603 带上状态码与正文，不中转。
    let (broken, seen) =
        fake_endpoint(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"boom"}"#).await;
    fixture.write_endpoint(&broken, "t");
    let (status, answer) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call",
            "params": {"name": "echo", "arguments": {}}}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["id"], 21);
    assert_eq!(answer["error"]["code"], -32603);
    let message = answer["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("HTTP 500") && message.contains("boom"),
        "{message}"
    );
    assert!(
        !seen.lock().unwrap().body.is_empty(),
        "it was delivered once"
    );
    assert_eq!(echo_calls(&fixture), 0, "and never relayed over stdio");

    // 连接中途断开：插件可能已经开始执行，同样不重发。
    let dropper = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let drop_url = format!(
        "http://127.0.0.1:{}/mcp",
        dropper.local_addr().unwrap().port()
    );
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        while let Ok((mut socket, _)) = dropper.accept().await {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            drop(socket);
        }
    });
    fixture.write_endpoint(&drop_url, "t");
    let (_, answer) = fixture
        .rpc(json!({"jsonrpc": "2.0", "id": 22, "method": "tools/call",
            "params": {"name": "echo", "arguments": {}}}))
        .await;
    assert_eq!(answer["error"]["code"], -32603, "{answer}");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Plugin HTTP endpoint failed")
    );
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "not resent"
    );
    assert_eq!(echo_calls(&fixture), 0, "not relayed");
}

/// 聊天 harness 在另一个进程：它经网关调用插件，和页面共用 Web 进程里那一个
/// 插件进程，而不是自己再拉起一份。
#[tokio::test]
async fn chat_tools_go_through_the_gateway_and_share_one_plugin_process() {
    use willdeep_core::mcp::LazyToolSource;
    let Some(fixture) = Fixture::new("gw-chat").await else {
        return;
    };
    let chat = willdeep_core::plugin::PluginChatTools::new(&fixture.home);
    assert!(chat.available());
    chat.refresh_missing().await;
    assert!(
        chat.definitions()
            .iter()
            .any(|definition| definition.name == "mcp__srv__echo")
    );
    let output = chat
        .call("mcp__srv__echo", json!({"from": "chat"}))
        .await
        .expect("call");
    assert!(output.contains("chat"), "{output}");
    assert_eq!(
        events(&fixture.log, "spawned").len(),
        1,
        "the chat side must reuse the gateway's plugin process"
    );
}

#[test]
fn constant_time_comparison_matches_equality() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
}
