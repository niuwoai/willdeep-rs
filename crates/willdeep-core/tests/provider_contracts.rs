use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use willdeep_core::prompt::build_system_prompt;
use willdeep_core::{
    Agent, AgentConfig, ApiDialect, ApprovalMode, ProviderConfig, ProviderKind, ToolRegistry,
    build_provider,
};

#[derive(Clone, Copy)]
enum MockDialect {
    Chat,
    Responses,
    Anthropic,
}

#[tokio::test]
async fn chat_completions_runs_a_tool_round_with_some_im_headers() {
    let (base, requests, server) = mock_server(MockDialect::Chat).await;
    run_agent(&base, ProviderKind::SomeIm, ApiDialect::ChatCompletions).await;
    server.await.expect("server task");
    let requests = requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("x-willdeep-session-id:")
    );
    assert!(requests[1].contains("tool_call_id"));
}

#[tokio::test]
async fn responses_runs_a_typed_function_call_round() {
    let (base, requests, server) = mock_server(MockDialect::Responses).await;
    run_agent(&base, ProviderKind::SomeIm, ApiDialect::Responses).await;
    server.await.expect("server task");
    let requests = requests.lock().expect("request lock");
    assert!(requests[0].contains("POST /v1/responses HTTP/1.1"));
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("call_readme"));
}

#[tokio::test]
async fn anthropic_messages_uses_native_auth_and_tool_result_blocks() {
    let (base, requests, server) = mock_server(MockDialect::Anthropic).await;
    run_agent(
        &base,
        ProviderKind::Anthropic,
        ApiDialect::AnthropicMessages,
    )
    .await;
    server.await.expect("server task");
    let requests = requests.lock().expect("request lock");
    let first = requests[0].to_ascii_lowercase();
    assert!(first.contains("post /v1/messages http/1.1"));
    assert!(first.contains("x-api-key: test-key"));
    assert!(first.contains("anthropic-version: 2023-06-01"));
    assert!(!first.contains("authorization:"));
    assert!(requests[1].contains("tool_result"));
    assert!(requests[1].contains("toolu_readme"));
}

async fn run_agent(base: &str, kind: ProviderKind, dialect: ApiDialect) {
    let workspace = temporary_workspace();
    std::fs::write(workspace.join("README.md"), "WillDeep fixture\n").expect("write fixture");
    let provider = build_provider(ProviderConfig::new(
        kind,
        dialect,
        base,
        "test-key",
        "test-model",
    ))
    .expect("provider");
    let tools = ToolRegistry::new(&workspace, ApprovalMode::WorkspaceAccess).expect("tools");
    let agent = Agent::new(
        provider,
        tools,
        AgentConfig {
            max_turns: 4,
            system_prompt: build_system_prompt(&workspace),
        },
    );
    let outcome = agent
        .run("Read README.md and report its first line")
        .await
        .expect("agent run");
    assert_eq!(outcome.final_text, "verified final answer");
    assert_eq!(outcome.turns, 2);
    std::fs::remove_dir_all(workspace).expect("cleanup");
}

async fn mock_server(
    dialect: MockDialect,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for round in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_http_request(&mut stream).await;
            captured.lock().expect("capture lock").push(request);
            let body = mock_response(dialect, round);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });
    (format!("http://{address}/v1"), requests, server)
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end;
    loop {
        let read = stream.read(&mut chunk).await.expect("read request");
        assert!(read > 0, "connection closed before headers");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_bytes(&bytes, b"\r\n\r\n") {
            header_end = position + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await.expect("read body");
        assert!(read > 0, "connection closed before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn mock_response(dialect: MockDialect, round: usize) -> &'static str {
    match (dialect, round) {
        (MockDialect::Chat, 0) => {
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call_readme","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}"#
        }
        (MockDialect::Chat, _) => {
            r#"{"choices":[{"message":{"content":"verified final answer","tool_calls":[]},"finish_reason":"stop"}]}"#
        }
        (MockDialect::Responses, 0) => {
            r#"{"status":"completed","output":[{"type":"function_call","id":"fc_1","call_id":"call_readme","name":"read_file","arguments":"{\"path\":\"README.md\"}"}],"usage":{"input_tokens":10,"output_tokens":4}}"#
        }
        (MockDialect::Responses, _) => {
            r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"verified final answer"}]}]}"#
        }
        (MockDialect::Anthropic, 0) => {
            r#"{"content":[{"type":"tool_use","id":"toolu_readme","name":"read_file","input":{"path":"README.md"}}],"stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":4}}"#
        }
        (MockDialect::Anthropic, _) => {
            r#"{"content":[{"type":"text","text":"verified final answer"}],"stop_reason":"end_turn"}"#
        }
    }
}

fn temporary_workspace() -> PathBuf {
    let path = std::env::temp_dir().join(format!("willdeep-contract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create workspace");
    path
}
