//! Web 端撞上 Runtime 版本交接时的重试：旧 Runtime 排空期间拒绝新活，
//! 等替换实例起来后自动重发，前端只看到一行本地化的等待提示。

use super::*;

/// 等旧 Runtime 交接给替换实例的节奏：每秒探一次，最多等两分钟。
/// 排空要等手上的活干完，可能远超两分钟；那种情况让用户稍后重发，
/// 别让一个 SSE 连接无限期挂着。
#[derive(Clone, Copy)]
pub(super) struct HandoffRetry {
    pub(super) limit: Duration,
    pub(super) poll: Duration,
}

pub(super) const RUNTIME_HANDOFF_RETRY: HandoffRetry = HandoffRetry {
    limit: Duration::from_secs(120),
    poll: Duration::from_secs(1),
};

/// 跑 `attempt`；撞上版本交接就给前端发一条「正在等新 Runtime」，按节奏重试。
/// 其它错误原样返回。每次重试都用新的 request_id——被闸门拒掉的请求没进
/// 幂等缓存，重发不会重复执行。
pub(super) async fn retry_through_runtime_handoff<T, F, Fut>(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    language: Language,
    retry: HandoffRetry,
    mut attempt: F,
) -> Result<T, WebError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let deadline = tokio::time::Instant::now() + retry.limit;
    let mut announced = false;
    loop {
        let error = match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if !crate::daemon::is_runtime_handoff(&error) || tx.is_closed() {
            return Err(WebError::from_anyhow(error));
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "warning: web turn gave up after {}s waiting for the replacement Runtime: {error}",
                retry.limit.as_secs()
            );
            return Err(WebError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: language
                    .text(
                        "Runtime 正在升级，新版本还没就绪，请稍后重新发送",
                        "WillDeep Runtime is still upgrading. Send your message again in a moment",
                        "Runtime を更新中です。しばらくしてからもう一度送信してください",
                    )
                    .to_owned(),
            });
        }
        if !announced {
            announced = true;
            send_event(
                tx,
                serde_json::json!({
                    "type":"runtime_handoff_wait",
                    "label":language.text(
                        "Runtime 正在升级，等待新版本就绪",
                        "Runtime is upgrading; waiting for the new version",
                        "Runtime を更新中、新しいバージョンを待機中",
                    ),
                }),
            )
            .await;
        }
        tokio::time::sleep(retry.poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_error(code: willdeep_runtime_protocol::ErrorCode, retryable: bool) -> anyhow::Error {
        crate::daemon::tui_bridge::RuntimeApiError {
            code,
            message: "Runtime is draining for version handoff".to_owned(),
            retryable,
        }
        .into()
    }

    const FAST_HANDOFF: HandoffRetry = HandoffRetry {
        limit: Duration::from_secs(5),
        poll: Duration::from_millis(1),
    };

    async fn drain_events(mut rx: mpsc::Receiver<Result<Event, Infallible>>) -> usize {
        rx.close();
        let mut count = 0;
        while rx.recv().await.is_some() {
            count += 1;
        }
        count
    }

    #[tokio::test]
    async fn a_draining_runtime_is_retried_until_the_replacement_answers() {
        let (tx, rx) = mpsc::channel(8);
        let mut calls = 0;
        let value = retry_through_runtime_handoff(&tx, Language::ZhCn, FAST_HANDOFF, || {
            calls += 1;
            let succeed = calls >= 3;
            async move {
                if succeed {
                    Ok("turn")
                } else {
                    Err(runtime_error(
                        willdeep_runtime_protocol::ErrorCode::Unavailable,
                        true,
                    ))
                }
            }
        })
        .await
        .unwrap_or_else(|error| panic!("retry should succeed: {}", error.message));
        assert_eq!(value, "turn");
        assert_eq!(calls, 3);
        drop(tx);
        // 等待提示只发一次，不随每次重试刷屏。
        assert_eq!(drain_events(rx).await, 1);
    }

    #[tokio::test]
    async fn other_runtime_errors_are_not_retried() {
        let (tx, rx) = mpsc::channel(8);
        for (code, retryable) in [
            (willdeep_runtime_protocol::ErrorCode::Unavailable, false),
            (willdeep_runtime_protocol::ErrorCode::Conflict, true),
        ] {
            let mut calls = 0;
            let result: Result<(), WebError> =
                retry_through_runtime_handoff(&tx, Language::En, FAST_HANDOFF, || {
                    calls += 1;
                    async move { Err(runtime_error(code, retryable)) }
                })
                .await;
            assert!(result.is_err());
            assert_eq!(calls, 1);
        }
        let mut calls = 0;
        let result: Result<(), WebError> =
            retry_through_runtime_handoff(&tx, Language::En, FAST_HANDOFF, || {
                calls += 1;
                async { Err(anyhow::anyhow!("connection refused")) }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
        drop(tx);
        assert_eq!(drain_events(rx).await, 0);
    }

    #[tokio::test]
    async fn a_handoff_that_outlasts_the_wait_ends_with_a_localized_message() {
        let (tx, _rx) = mpsc::channel(8);
        let retry = HandoffRetry {
            limit: Duration::ZERO,
            poll: Duration::from_millis(1),
        };
        let result: Result<(), WebError> =
            retry_through_runtime_handoff(&tx, Language::ZhCn, retry, || async {
                Err(runtime_error(
                    willdeep_runtime_protocol::ErrorCode::Unavailable,
                    true,
                ))
            })
            .await;
        let error = result.expect_err("handoff wait should time out");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("Runtime 正在升级"));
        assert!(!error.message.contains("draining"));
    }

    #[tokio::test]
    async fn a_closed_browser_stream_stops_waiting_for_the_handoff() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx);
        let mut calls = 0;
        let result: Result<(), WebError> =
            retry_through_runtime_handoff(&tx, Language::En, FAST_HANDOFF, || {
                calls += 1;
                async {
                    Err(runtime_error(
                        willdeep_runtime_protocol::ErrorCode::Unavailable,
                        true,
                    ))
                }
            })
            .await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
