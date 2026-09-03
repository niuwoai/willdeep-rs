use reqwest::{Client, RequestBuilder, StatusCode, Url};

use super::{ProviderConfig, ProviderError, ProviderKind};
use crate::{CLIENT_NAME, CLIENT_USER_AGENT};

const ERROR_BODY_LIMIT: usize = 8 * 1024;

/// 一次 Provider 请求最多发几遍（含第一遍）。
const MAX_ATTEMPTS: u32 = 3;

/// 首次退避时长，此后逐次翻倍：250ms、500ms。三次尝试最多多等 750ms，
/// 换掉的是「握手抖一下整个 task 就死」。
const RETRY_BASE_DELAY_MS: u64 = 250;

/// 粘贴文本附件送给模型时的包装。
///
/// 此前只有一行 `[Pasted text: paste-1.txt]` 打头。`.txt` 后缀让模型把它当成工作区
/// 里的一个文件，转头去 `search_files`、`ls tmp` 找，找不到就宣布「内容我看不到」，
/// 把轮次全耗在找一个不存在的文件上——而正文明明就在下一行。现在把「这是用户
/// 贴进聊天的文本、不是文件、全文就在这里」说清楚，并用结束标记圈住正文，
/// 免得正文里的内容与后面的消息混在一起。
pub fn pasted_text_block(name: &str, content: &str) -> String {
    format!(
        "[Pasted text attachment \"{name}\": the user pasted this text directly into the chat. It is not a file in the workspace; do not search for it. Its full content follows.]\n{content}\n[End of pasted text \"{name}\"]"
    )
}

pub fn client(config: &ProviderConfig) -> Result<Client, ProviderError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
        .user_agent(CLIENT_USER_AGENT)
        .build()
        .map_err(|error| ProviderError::Client(error.to_string()))
}

pub fn endpoint(base_url: &str, suffix: &str) -> Result<Url, ProviderError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    if trimmed.to_ascii_lowercase().ends_with(suffix) {
        return Url::parse(trimmed)
            .map_err(|error| ProviderError::InvalidBaseUrl(error.to_string()));
    }
    Url::parse(&format!("{trimmed}/{suffix}"))
        .map_err(|error| ProviderError::InvalidBaseUrl(error.to_string()))
}

pub fn anthropic_endpoint(base_url: &str) -> Result<Url, ProviderError> {
    let mut trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed = trimmed.trim_end_matches("/v1");
    }
    endpoint(trimmed, "v1/messages")
}

pub fn openai_auth(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    let request = apply_client_headers(request);
    let request = if config.api_key.trim().is_empty() {
        request
    } else {
        request.bearer_auth(config.api_key.trim())
    };
    apply_some_im_headers(request, config)
}

pub fn anthropic_auth(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    let request = apply_client_headers(request);
    let request = if config.kind == ProviderKind::SomeIm {
        request.bearer_auth(config.api_key.trim())
    } else {
        request.header("x-api-key", config.api_key.trim())
    };
    let request = request.header("anthropic-version", "2023-06-01");
    apply_some_im_headers(request, config)
}

fn apply_client_headers(request: RequestBuilder) -> RequestBuilder {
    request
        .header("x-client-name", CLIENT_NAME)
        .header("x-client-version", crate::VERSION)
}

fn apply_some_im_headers(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    if config.kind != ProviderKind::SomeIm {
        return request;
    }
    request
        .header("x-willdeep-session-id", &config.session_id)
        // The relay's usage ledger reads `X-Playground-Session-ID` and ignores
        // our own session header: sending only the latter leaves every usage
        // record's session_id empty, and worker requests can no longer be
        // attributed to the chat that spawned them — which is exactly the
        // number the Skill Worker economics rest on. Same opaque UUID, sent
        // alongside, as the macOS app does.
        .header("X-Playground-Session-ID", &config.session_id)
        .header("x-willdeep-workspace-id", &config.workspace_id)
}

/// 发一次 Provider 请求，连接层抖动和 5xx 自动重发。
///
/// 起因是一次真实故障：`tls handshake eof` 让任务在 5 秒内以 `failure_domain=provider`
/// 收场，而链路本身几秒后就恢复了——握手掉一次，整轮对话连同上下文一起丢掉，
/// 重来的成本远高于等那 250 毫秒。
///
/// 只重发本来就该重发的：连接建立失败、读写超时、以及 5xx。4xx 一律直接抛——
/// 密钥错了、模型名写错了、请求体不合法，重发多少遍都是同一个答案。429 也不在
/// 重发之列：限流要照 `Retry-After` 的节奏来，用这里这套几百毫秒的退避去顶，
/// 只会把限流顶得更死。
pub async fn send_retrying(
    request: RequestBuilder,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    for attempt in 1..MAX_ATTEMPTS {
        // 拿不到副本说明 body 不可重放（流式请求），那就没有重发一说，
        // 直接跳出去把原件发掉。
        let Some(candidate) = request.try_clone() else {
            break;
        };
        match send_once(candidate, config).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) if is_retryable(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(
                    RETRY_BASE_DELAY_MS << (attempt - 1),
                ))
                .await;
            }
            Err(error) => return Err(error),
        }
    }
    // 最后一遍：用掉原始 request，错误照原样往上抛，不再包一层重试的说辞。
    send_once(request, config).await
}

/// 发一遍并读出 body。连接层的失败在这里就被收进 [`ProviderError::Request`]，
/// 好让重试判定跟 HTTP 状态码那一路走同一个出口——否则 `?` 会让它绕过重试，
/// 而连接失败恰恰是最该重发的那一类。
async fn send_once(
    request: RequestBuilder,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    decode_success(request.send().await?, config).await
}

fn is_retryable(error: &ProviderError) -> bool {
    match error {
        // 三者分工：`is_connect` 是连不上（`tls handshake eof` 落在这儿），
        // `is_timeout` 是等太久，`is_request` 是连上了但半路断（hyper 报
        // `IncompleteMessage`）。少了最后一条就漏掉「握手过了才掉线」那一类，
        // 而它和前两类一样，重发一次通常就好了。
        //
        // 请求发出去、响应回来的路上断了，重发确实可能让对面多算一次用量。
        // 这是重试换可用性的固有代价：整轮对话连同上下文丢掉要贵得多。
        ProviderError::Request(error) => {
            error.is_connect() || error.is_timeout() || error.is_request()
        }
        ProviderError::Http { status, .. } => status.is_server_error(),
        _ => false,
    }
}

pub async fn decode_success(
    response: reqwest::Response,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    let status = response.status();
    let bytes = response.bytes().await?.to_vec();
    if status.is_success() {
        return Ok(bytes);
    }
    Err(ProviderError::Http {
        status,
        body: safe_error_body(status, &bytes, Some(config.api_key.trim())),
    })
}

fn safe_error_body(status: StatusCode, bytes: &[u8], api_key: Option<&str>) -> String {
    let length = bytes.len().min(ERROR_BODY_LIMIT);
    let mut body = String::from_utf8_lossy(&bytes[..length]).into_owned();
    for marker in ["Bearer ", "sk-", "api_key\":\""] {
        body = body.replace(marker, "[REDACTED]");
    }
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        body = body.replace(api_key, "[REDACTED]");
    }
    if bytes.len() > length {
        body.push_str(" [truncated]");
    }
    if body.trim().is_empty() {
        format!("{status} with empty response body")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ApiDialect, ProviderConfig};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 一个只够用来数「客户端敲了几次门」的 HTTP 服务端。
    ///
    /// `script` 按顺序决定每一次连接怎么收场：`None` 表示接了就把连接掐掉
    /// （不回一个字节），这是离 `tls handshake eof` 最近的可复现形态；
    /// `Some(status)` 表示回一个该状态码的空响应。脚本用完之后一律回 200。
    async fn scripted_server(
        script: Vec<Option<u16>>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let hits = Arc::new(AtomicUsize::new(0));
        let served = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let index = served.fetch_add(1, Ordering::SeqCst);
                // 把请求读掉一截，免得客户端还在写就看到连接关闭。
                let mut scratch = [0_u8; 1024];
                let _ = stream.read(&mut scratch).await;
                match script.get(index).copied().flatten() {
                    None if index < script.len() => drop(stream),
                    status => {
                        let status = status.unwrap_or(200);
                        let body = if status == 200 { "{\"data\":[]}" } else { "{}" };
                        let response = format!(
                            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.flush().await;
                    }
                }
            }
        });
        (base_url, hits, handle)
    }

    fn probe_config(base_url: &str) -> ProviderConfig {
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            ApiDialect::ChatCompletions,
            base_url,
            "test-key",
            "test-model",
        )
    }

    /// 掉一次握手不该让整轮对话陪葬——这正是 0.54.0-rc2 之前那次
    /// `failure_domain=provider` 的真实成因。
    #[tokio::test]
    async fn dropped_connection_is_retried_until_it_lands() {
        let (base_url, hits, server) = scripted_server(vec![None, None]).await;
        let config = probe_config(&base_url);
        let request = client(&config).expect("client").get(&base_url);

        let bytes = send_retrying(request, &config).await.expect("retried");

        assert_eq!(bytes, b"{\"data\":[]}");
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn server_errors_are_retried() {
        let (base_url, hits, server) = scripted_server(vec![Some(503)]).await;
        let config = probe_config(&base_url);
        let request = client(&config).expect("client").get(&base_url);

        send_retrying(request, &config).await.expect("retried");

        assert_eq!(hits.load(Ordering::SeqCst), 2);
        server.abort();
    }

    /// 4xx 重发多少遍都是同一个答案，多敲一次门只是白等。
    #[tokio::test]
    async fn client_errors_fail_on_the_first_try() {
        let (base_url, hits, server) = scripted_server(vec![Some(401)]).await;
        let config = probe_config(&base_url);
        let request = client(&config).expect("client").get(&base_url);

        let error = send_retrying(request, &config).await.expect_err("401");

        assert!(matches!(
            error,
            ProviderError::Http {
                status: StatusCode::UNAUTHORIZED,
                ..
            }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }

    /// 重试有上限：一直连不上就得如实报错，不能无限重发把任务吊在那儿。
    #[tokio::test]
    async fn retries_give_up_after_the_attempt_budget() {
        let (base_url, hits, server) = scripted_server(vec![Some(500), Some(500), Some(500)]).await;
        let config = probe_config(&base_url);
        let request = client(&config).expect("client").get(&base_url);

        let error = send_retrying(request, &config).await.expect_err("5xx");

        assert!(matches!(error, ProviderError::Http { .. }));
        assert_eq!(hits.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);
        server.abort();
    }

    #[test]
    fn every_provider_request_includes_client_identity() {
        for (kind, base_url) in [
            (ProviderKind::SomeIm, "https://some.im/v1"),
            (
                ProviderKind::OpenAiCompatible,
                "https://provider.example/v1",
            ),
            (ProviderKind::Anthropic, "https://api.anthropic.com"),
        ] {
            let config = ProviderConfig::new(
                kind,
                ApiDialect::Responses,
                base_url,
                "test-key",
                "test-model",
            );
            let request = client(&config)
                .expect("client")
                .get(format!("{base_url}/models"));
            let request = if kind == ProviderKind::Anthropic {
                anthropic_auth(request, &config)
            } else {
                openai_auth(request, &config)
            }
            .build()
            .expect("request");

            assert_eq!(
                request
                    .headers()
                    .get("x-client-name")
                    .and_then(|value| value.to_str().ok()),
                Some(CLIENT_NAME)
            );
            assert_eq!(
                request
                    .headers()
                    .get("x-client-version")
                    .and_then(|value| value.to_str().ok()),
                Some(crate::VERSION)
            );
        }
    }

    #[test]
    fn credential_free_local_request_omits_authorization() {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            ApiDialect::ChatCompletions,
            "http://127.0.0.1:11434/v1",
            "",
            "gemma4:e4b-it-qat",
        );
        let request = openai_auth(
            client(&config)
                .expect("client")
                .post("http://127.0.0.1:11434/v1/chat/completions"),
            &config,
        )
        .build()
        .expect("request");

        assert!(
            !request
                .headers()
                .contains_key(reqwest::header::AUTHORIZATION)
        );
    }
}
