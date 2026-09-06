use reqwest::{Client, RequestBuilder, StatusCode, Url};
use std::time::{Duration, SystemTime};

use super::{ProviderConfig, ProviderError, ProviderKind};
use crate::{CLIENT_NAME, CLIENT_USER_AGENT};

const ERROR_BODY_LIMIT: usize = 8 * 1024;
const SUCCESS_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Some compatible servers ignore stream=true and return a JSON completion.
/// Decode the existing response without issuing a second generation request.
pub async fn emit_buffered_completion(
    completion: crate::types::Completion,
    events: &dyn super::ProviderEventSink,
    deadline: tokio::time::Instant,
) -> Result<crate::types::Completion, ProviderError> {
    tokio::time::timeout_at(deadline, async {
        if !completion.content.is_empty() {
            events
                .emit(super::ProviderEvent::TextDelta(completion.content.clone()))
                .await;
        }
        if let Some(usage) = &completion.usage {
            events
                .emit(super::ProviderEvent::Usage(usage.clone()))
                .await;
        }
    })
    .await
    .map_err(|_| ProviderError::DeadlineExceeded)?;
    Ok(completion)
}

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
/// 429 和 5xx 遵守 Retry-After；无有效头时使用退避。其他 4xx 不重试。
/// 所有尝试及等待共享一个请求期限，等待被取消后不会再发送请求。
pub async fn send_retrying(
    request: RequestBuilder,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    let deadline = request_deadline(config)?;
    let response =
        send_open_retrying(request, config, &super::NoopProviderEvents, deadline).await?;
    tokio::time::timeout_at(deadline, decode_success(response, config))
        .await
        .map_err(|_| ProviderError::DeadlineExceeded)?
}

pub fn request_deadline(config: &ProviderConfig) -> Result<tokio::time::Instant, ProviderError> {
    tokio::time::Instant::now()
        .checked_add(Duration::from_secs(config.request_timeout_secs))
        .ok_or(ProviderError::DeadlineExceeded)
}

/// Retry only before a successful response is delivered. Body/stream failures
/// are not replayed: the provider may already have generated billable content.
pub async fn send_open_retrying(
    request: RequestBuilder,
    config: &ProviderConfig,
    events: &dyn super::ProviderEventSink,
    deadline: tokio::time::Instant,
) -> Result<reqwest::Response, ProviderError> {
    for attempt in 1..MAX_ATTEMPTS {
        // 拿不到副本说明 body 不可重放（流式请求），那就没有重发一说，
        // 直接跳出去把原件发掉。
        let Some(candidate) = request.try_clone() else {
            break;
        };
        match tokio::time::timeout_at(deadline, send_once(candidate, config))
            .await
            .map_err(|_| ProviderError::DeadlineExceeded)?
        {
            Ok(bytes) => return Ok(bytes),
            Err(error) if is_retryable(&error) => {
                let delay = retry_delay(&error, attempt);
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if delay >= remaining {
                    return Err(ProviderError::RetryDeferred {
                        retry_after_secs: delay.as_secs(),
                        source: Box::new(error),
                    });
                }
                tokio::time::timeout_at(deadline, async {
                    events
                        .emit(super::ProviderEvent::RetryWait { attempt, delay })
                        .await;
                    tokio::time::sleep(delay).await;
                    events
                        .emit(super::ProviderEvent::RetryStarted { attempt })
                        .await;
                })
                .await
                .map_err(|_| ProviderError::DeadlineExceeded)?;
            }
            Err(error) => return Err(error),
        }
    }
    // 最后一遍：用掉原始 request，错误照原样往上抛，不再包一层重试的说辞。
    tokio::time::timeout_at(deadline, send_once(request, config))
        .await
        .map_err(|_| ProviderError::DeadlineExceeded)?
}

fn retry_delay(error: &ProviderError, attempt: u32) -> Duration {
    let fallback = Duration::from_millis(RETRY_BASE_DELAY_MS << (attempt - 1));
    match error {
        ProviderError::Http {
            retry_after: Some(delay),
            ..
        } => *delay,
        ProviderError::Http {
            status: StatusCode::TOO_MANY_REQUESTS,
            ..
        } => fallback.max(Duration::from_secs(1)),
        _ => fallback,
    }
}

/// 发一遍并读出 body。连接层的失败在这里就被收进 [`ProviderError::Request`]，
/// 好让重试判定跟 HTTP 状态码那一路走同一个出口——否则 `?` 会让它绕过重试，
/// 而连接失败恰恰是最该重发的那一类。
async fn send_once(
    request: RequestBuilder,
    config: &ProviderConfig,
) -> Result<reqwest::Response, ProviderError> {
    let response = request.send().await?;
    if response.status().is_success() {
        return Ok(response);
    }
    match decode_success(response, config).await {
        Err(error) => Err(error),
        Ok(_) => Err(ProviderError::InvalidResponse(
            "unexpected HTTP status transition".to_owned(),
        )),
    }
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
        ProviderError::Http { status, .. } => {
            status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
        }
        _ => false,
    }
}

pub async fn decode_success(
    mut response: reqwest::Response,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_retry_after(value, SystemTime::now()));
    let limit = if status.is_success() {
        SUCCESS_BODY_LIMIT
    } else {
        ERROR_BODY_LIMIT
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() > limit {
            if status.is_success() {
                return Err(ProviderError::InvalidResponse(
                    "provider response exceeds the bounded body limit".to_owned(),
                ));
            }
            break;
        }
    }
    if status.is_success() {
        return Ok(bytes);
    }
    Err(ProviderError::Http {
        status,
        body: safe_error_body(status, &bytes, Some(config.api_key.trim())),
        retry_after,
    })
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        // A syntactically valid but enormous delay must not become a fast retry.
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|time| time.duration_since(now).unwrap_or_default())
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
        scripted_server_with_retry_after(script, None).await
    }

    async fn scripted_server_with_retry_after(
        script: Vec<Option<u16>>,
        retry_after: Option<&'static str>,
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
                        let header = retry_after
                            .map(|value| format!("retry-after: {value}\r\n"))
                            .unwrap_or_default();
                        let response = format!(
                            "HTTP/1.1 {status} X\r\n{header}content-length: {}\r\nconnection: close\r\n\r\n{body}",
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

    #[test]
    fn retry_after_parses_seconds_dates_and_overflow_without_early_retry() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now + Duration::from_secs(30)), now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(30)), now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_retry_after("999999999999999999999999", now),
            Some(Duration::from_secs(u64::MAX))
        );
        assert_eq!(parse_retry_after("-1", now), None);
        assert_eq!(parse_retry_after("invalid", now), None);
    }

    #[tokio::test]
    async fn rate_limit_waits_for_retry_after_before_sending_again() {
        #[derive(Default)]
        struct Events(std::sync::Mutex<Vec<super::super::ProviderEvent>>);
        #[async_trait::async_trait]
        impl super::super::ProviderEventSink for Events {
            async fn emit(&self, event: super::super::ProviderEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let (url, hits, server) =
            scripted_server_with_retry_after(vec![Some(429)], Some("1")).await;
        let config = probe_config(&url);
        let started = std::time::Instant::now();
        let events = Events::default();
        send_open_retrying(
            client(&config).unwrap().get(&url),
            &config,
            &events,
            tokio::time::Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let recorded = events.0.lock().unwrap();
        assert!(matches!(
            recorded.as_slice(),
            [
                super::super::ProviderEvent::RetryWait { attempt: 1, .. },
                super::super::ProviderEvent::RetryStarted { attempt: 1 }
            ]
        ));
        server.abort();
    }

    #[tokio::test]
    async fn excessive_retry_after_returns_without_sending_early() {
        let (url, hits, server) =
            scripted_server_with_retry_after(vec![Some(503)], Some("120")).await;
        let mut config = probe_config(&url);
        config.request_timeout_secs = 1;
        let error = send_retrying(client(&config).unwrap().get(&url), &config)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::RetryDeferred {
                retry_after_secs: 120,
                ..
            }
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn cancelling_retry_wait_never_sends_another_request() {
        let (url, hits, server) =
            scripted_server_with_retry_after(vec![Some(429)], Some("1")).await;
        let task = tokio::spawn(async move {
            let config = probe_config(&url);
            send_retrying(client(&config).unwrap().get(&url), &config).await
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while hits.load(Ordering::SeqCst) == 0 {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn error_body_is_bounded_before_the_server_finishes_sending() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 1000000000\r\n\r\n")
                .await
                .unwrap();
            stream
                .write_all(&vec![b'x'; ERROR_BODY_LIMIT + 1])
                .await
                .unwrap();
            stream.flush().await.unwrap();
            std::future::pending::<()>().await;
        });
        let config = probe_config(&url);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            send_retrying(client(&config).unwrap().get(&url), &config),
        )
        .await;
        server.abort();
        let error = result
            .expect("must not wait for the remaining gigabyte")
            .unwrap_err();
        let ProviderError::Http { body, .. } = error else {
            panic!("unexpected error: {error}")
        };
        assert!(body.ends_with("[truncated]"));
        assert!(body.len() < ERROR_BODY_LIMIT + 32);
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
