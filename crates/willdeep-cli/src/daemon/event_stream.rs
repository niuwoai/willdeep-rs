use std::collections::VecDeque;
use std::convert::Infallible;

use axum::body::{Body, Bytes};
use axum::http::header::CONTENT_TYPE;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream;

use super::*;

const LIVE_BACKLOG_LIMIT: usize = 1_000;

pub(super) struct EventLog {
    path: PathBuf,
    state: Mutex<EventLogState>,
    live: tokio::sync::broadcast::Sender<RuntimeEvent>,
}

struct EventLogState {
    next_sequence: u64,
}

impl EventLog {
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        let next_sequence = read_events(&path, 0, usize::MAX)?
            .last()
            .map_or(1, |event| event.sequence.saturating_add(1));
        let (live, _) = tokio::sync::broadcast::channel(512);
        Ok(Self {
            path,
            state: Mutex::new(EventLogState { next_sequence }),
            live,
        })
    }

    pub(super) fn append(
        &self,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<RuntimeEvent> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime event log lock poisoned"))?;
        let event = RuntimeEvent {
            sequence: state.next_sequence,
            timestamp: now(),
            kind: kind.into(),
            message: message.into(),
        };
        let mut file = append_log(&self.path)?;
        serde_json::to_writer(&mut file, &event)?;
        std::io::Write::write_all(&mut file, b"\n")?;
        file.sync_data()?;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let _ = self.live.send(event.clone());
        Ok(event)
    }

    pub(super) fn read_after(&self, after: u64, limit: usize) -> Result<Vec<RuntimeEvent>> {
        // Append writes a JSON object and its trailing newline in separate I/O
        // operations. Hold the same lock while reading so an active client can
        // never observe the durable log between those operations.
        let _state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime event log lock poisoned"))?;
        read_events(&self.path, after, limit)
    }

    pub(super) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<RuntimeEvent> {
        self.live.subscribe()
    }

    pub(super) fn latest_sequence(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.next_sequence.saturating_sub(1))
            .unwrap_or_default()
    }
}

pub(super) fn public_event(mut event: RuntimeEvent) -> RuntimeEvent {
    if event.kind == "task.output" {
        event.message = redact_task_output(&event.message);
    }
    event.message = redact_suffix(&event.message, " root=");
    event.message = replace_error_with_reason(&event.message);
    event
}

/// 失败原因的公共分类。
///
/// 起因是一次真实故障：会话历史涨到 94 万 token，压缩撞上 provider 的 1M 上限，
/// 而 TUI 吃的是公共事件流——`error=` 整段被剥掉之后，用户手上只剩
/// `failure_domain=provider` 和两串 UUID，看不出是自己的上下文炸了。
///
/// 原文不能放行：它带着本机路径、命令行，还可能带凭据，而 Web 桥接和手机中继
/// 都吃这条流。折中是只放行一个**闭集合**里的静态标签——能说清「为什么失败」，
/// 又在构造上不可能夹带任何现场内容。完整错误照旧只留在本机 `task.diagnostics`。
fn failure_reason(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| error.contains(needle));
    if has(&[
        "maximum context length",
        "context_length_exceeded",
        "context window",
        "reduce the length of the messages",
        "too many tokens",
    ]) {
        "context_overflow"
    } else if has(&["429", "rate limit", "rate_limit", "too many requests"]) {
        "rate_limited"
    } else if has(&[
        "401",
        "403",
        "invalid_api_key",
        "invalid api key",
        "unauthorized",
        "authentication",
    ]) {
        "auth"
    } else if has(&["insufficient", "quota", "balance", "payment", "402"]) {
        "quota"
    } else if has(&["timeout", "timed out", "deadline"]) {
        "timeout"
    } else if has(&[
        "tls",
        "handshake",
        "connection",
        "dns",
        "unexpected eof",
        "connect error",
    ]) {
        "network"
    } else if has(&["500", "502", "503", "504", "overloaded", "server error"]) {
        "provider_unavailable"
    } else {
        "unclassified"
    }
}

/// 把 `error=<原文>` 换成 `reason=<分类>`，而不是整段删掉。
fn replace_error_with_reason(message: &str) -> String {
    let marker = " error=";
    let Some(index) = message.find(marker) else {
        return message.to_owned();
    };
    let error = message[index + marker.len()..].trim();
    if error.is_empty() {
        return message[..index].to_owned();
    }
    format!("{} reason={}", &message[..index], failure_reason(error))
}

fn redact_task_output(message: &str) -> String {
    let Some((prefix, payload)) = message.split_once(' ') else {
        return message.to_owned();
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return prefix.to_owned();
    };
    if let Some(object) = value.as_object_mut() {
        for field in [
            "arguments",
            "output",
            "report",
            "workspace",
            "root_workspace",
        ] {
            object.remove(field);
        }
    }
    format!(
        "{prefix} {}",
        serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned())
    )
}

fn redact_suffix(message: &str, marker: &str) -> String {
    message
        .find(marker)
        .map_or_else(|| message.to_owned(), |index| message[..index].to_owned())
}

struct LiveEventState {
    backlog: VecDeque<RuntimeEvent>,
    receiver: tokio::sync::broadcast::Receiver<RuntimeEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    log: Arc<EventLog>,
    cursor: u64,
}

pub(super) async fn events_stream_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    // Subscribe before reading the durable backlog. Events appended between
    // these operations can appear in both sources and are removed by cursor.
    let receiver = state.events.subscribe();
    let backlog = state
        .events
        .read_after(query.after, query.limit.clamp(1, LIVE_BACKLOG_LIMIT))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into();
    let live = LiveEventState {
        backlog,
        receiver,
        shutdown: state.shutdown.subscribe(),
        log: state.events.clone(),
        cursor: query.after,
    };
    let events = stream::unfold(live, |mut state| async move {
        next_event(&mut state)
            .await
            .map(|event| (Ok::<Event, Infallible>(sse_event(event)), state))
    });
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response())
}

pub(super) async fn events_ndjson_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<uuid::Uuid>().ok());
    let receiver = state.events.subscribe();
    let backlog = state
        .events
        .read_after(query.after, query.limit.clamp(1, LIVE_BACKLOG_LIMIT))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into();
    let live = LiveEventState {
        backlog,
        receiver,
        shutdown: state.shutdown.subscribe(),
        log: state.events.clone(),
        cursor: query.after,
    };
    let events = stream::unfold(live, move |mut state| async move {
        next_event(&mut state).await.map(|event| {
            let line = Ok::<Bytes, Infallible>(ndjson_event(public_event(event), request_id));
            (line, state)
        })
    });
    Ok(Response::builder()
        .header(CONTENT_TYPE, "application/x-ndjson; charset=utf-8")
        .body(Body::from_stream(events))
        .expect("valid NDJSON response"))
}

fn ndjson_event(event: RuntimeEvent, request_id: Option<uuid::Uuid>) -> Bytes {
    let response =
        willdeep_runtime_protocol::ApiResponse::ok(event, willdeep_core::VERSION, request_id);
    let mut value =
        serde_json::to_vec(&response).expect("Runtime event response is JSON serializable");
    value.push(b'\n');
    Bytes::from(value)
}

async fn next_event(state: &mut LiveEventState) -> Option<RuntimeEvent> {
    loop {
        if *state.shutdown.borrow() {
            return None;
        }
        while let Some(event) = state.backlog.pop_front() {
            if event.sequence > state.cursor {
                state.cursor = event.sequence;
                return Some(event);
            }
        }
        if state.log.latest_sequence() > state.cursor {
            state.backlog = state
                .log
                .read_after(state.cursor, LIVE_BACKLOG_LIMIT)
                .unwrap_or_default()
                .into();
            if !state.backlog.is_empty() {
                continue;
            }
        }
        let received = tokio::select! {
            changed = state.shutdown.changed() => {
                if changed.is_err() || *state.shutdown.borrow() {
                    return None;
                }
                continue;
            }
            received = state.receiver.recv() => received,
        };
        match received {
            Ok(event) if event.sequence > state.cursor => {
                state.cursor = event.sequence;
                return Some(event);
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                state.backlog = state
                    .log
                    .read_after(state.cursor, LIVE_BACKLOG_LIMIT)
                    .unwrap_or_default()
                    .into();
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        }
    }
}

fn sse_event(event: RuntimeEvent) -> Event {
    Event::default()
        .id(event.sequence.to_string())
        .event(event.kind.clone())
        .data(serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn backlog_and_live_broadcast_resume_without_duplicates() {
        let root =
            std::env::temp_dir().join(format!("willdeep-live-events-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let log = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        log.append("first", "one").unwrap();
        let receiver = log.subscribe();
        log.append("second", "two").unwrap();
        let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
        let mut state = LiveEventState {
            backlog: log.read_after(0, 1).unwrap().into(),
            receiver,
            shutdown,
            log: log.clone(),
            cursor: 0,
        };

        assert_eq!(next_event(&mut state).await.unwrap().sequence, 1);
        assert_eq!(next_event(&mut state).await.unwrap().sequence, 2);
        log.append("third", "three").unwrap();
        // The queued broadcast copy of event 2 is skipped by the cursor.
        assert_eq!(next_event(&mut state).await.unwrap().sequence, 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn idle_event_stream_ends_when_runtime_shutdown_starts() {
        let root =
            std::env::temp_dir().join(format!("willdeep-event-shutdown-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let log = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let mut state = LiveEventState {
            backlog: VecDeque::new(),
            receiver: log.subscribe(),
            shutdown: receiver,
            log,
            cursor: 0,
        };
        let waiting = tokio::spawn(async move { next_event(&mut state).await });
        tokio::task::yield_now().await;
        shutdown.send(true).unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(100), waiting)
                .await
                .expect("event stream must not block Runtime shutdown")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_retry_wait_survives_event_log_reopen_and_public_projection() {
        let root =
            std::env::temp_dir().join(format!("willdeep-child-retry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("events.ndjson");
        let id = uuid::Uuid::new_v4();
        let payload = crate::agent_event_json(willdeep_core::AgentEvent::SubagentRetryWait {
            id,
            attempt: 2,
            delay: Duration::from_millis(1500),
        });
        let log = EventLog::open(path.clone()).unwrap();
        log.append(
            "task.output",
            format!("task_id={} {payload}", uuid::Uuid::new_v4()),
        )
        .unwrap();
        drop(log);
        let reopened = EventLog::open(path).unwrap();
        let events = reopened.read_after(0, 10).unwrap();
        assert_eq!(events.len(), 1);
        let public = public_event(events[0].clone());
        let (_, body) = public.message.split_once(' ').unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(body).unwrap(),
            payload
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn readers_wait_until_an_event_line_is_complete() {
        let root =
            std::env::temp_dir().join(format!("willdeep-event-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let log = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        log.append("first", "one").unwrap();
        let write_guard = log.state.lock().unwrap();
        let event = RuntimeEvent {
            sequence: 2,
            timestamp: now(),
            kind: "second".to_owned(),
            message: "two".to_owned(),
        };
        let line = serde_json::to_vec(&event).unwrap();
        let split = line.len() / 2;
        let mut file = append_log(&log.path).unwrap();
        file.write_all(&line[..split]).unwrap();
        file.flush().unwrap();

        let reader_log = Arc::clone(&log);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            tx.send(reader_log.read_after(0, 10)).unwrap();
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            rx.try_recv().is_err(),
            "reader must not parse an event while append owns the log lock"
        );

        file.write_all(&line[split..]).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_data().unwrap();
        drop(write_guard);
        let events = rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
        reader.join().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "first");
        assert_eq!(events[1], event);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ndjson_event_is_one_complete_envelope_line() {
        let request_id = uuid::Uuid::new_v4();
        let line = ndjson_event(
            RuntimeEvent {
                sequence: 42,
                timestamp: 123,
                kind: "turn.completed".to_owned(),
                message: "done".to_owned(),
            },
            Some(request_id),
        );
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(line.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["data"]["sequence"], 42);
        assert_eq!(value["meta"]["request_id"], request_id.to_string());
    }

    #[test]
    fn public_events_remove_tool_payloads_reports_paths_and_errors() {
        let event = public_event(RuntimeEvent {
            sequence: 1,
            timestamp: 2,
            kind: "task.output".to_owned(),
            message: concat!(
                "task_id=abc ",
                r#"{"type":"tool_requested","name":"run_command","arguments":{"command":"secret"},"output":"private","report":"private","workspace":"/private/path","root_workspace":"/root"}"#
            )
            .to_owned(),
        });
        assert!(event.message.contains("run_command"));
        for secret in ["secret", "private", "/private/path", "/root"] {
            assert!(!event.message.contains(secret));
        }
        let event = public_event(RuntimeEvent {
            sequence: 2,
            timestamp: 3,
            kind: "task.failed".to_owned(),
            message: "task_id=abc exit_code=1 error=/private/path token=secret".to_owned(),
        });
        assert_eq!(event.message, "task_id=abc exit_code=1 reason=unclassified");
    }

    /// 失败原因必须说得出口，又不能夹带现场：放行的只有闭集合里的静态标签。
    #[test]
    fn public_failures_carry_a_classified_reason_but_never_the_error_text() {
        let event = public_event(RuntimeEvent {
            sequence: 4,
            timestamp: 5,
            kind: "task.failed".to_owned(),
            message: concat!(
                "task_id=abc session_id=def exit_code=none failure_domain=provider ",
                r#"error=provider returned HTTP 400 Bad Request: {"error":{"message":"#,
                r#""This model's maximum context length is 1048576 tokens. However, you "#,
                r#"requested 1305317 tokens","param":null}} at /Users/someone/secret-repo"#
            )
            .to_owned(),
        });
        assert_eq!(
            event.message,
            "task_id=abc session_id=def exit_code=none failure_domain=provider \
             reason=context_overflow"
        );
        for leaked in ["1305317", "/Users/someone", "Bad Request"] {
            assert!(!event.message.contains(leaked), "leaked {leaked}");
        }
    }

    #[test]
    fn failure_reasons_cover_the_common_provider_faults() {
        for (error, expected) in [
            ("HTTP 429 Too Many Requests", "rate_limited"),
            ("HTTP 401 invalid_api_key", "auth"),
            ("tls handshake eof", "network"),
            ("request timed out after 60s", "timeout"),
            ("HTTP 503 upstream overloaded", "provider_unavailable"),
            ("insufficient balance", "quota"),
            ("something nobody predicted", "unclassified"),
        ] {
            assert_eq!(failure_reason(error), expected, "for {error}");
        }
    }

    /// 失败的工具事件现在会带上参数和输出，供本机 `task.diagnostics` 排查。
    /// 这条边界不能因此松动：公共事件流（Web 桥接、手机中继都吃它）必须照旧剥干净。
    #[test]
    fn failed_tool_payloads_still_never_reach_public_consumers() {
        let event = public_event(RuntimeEvent {
            sequence: 3,
            timestamp: 4,
            kind: "task.output".to_owned(),
            message: concat!(
                "task_id=abc ",
                r#"{"type":"tool_completed","name":"run_command","is_error":true,"#,
                r#""arguments":"{\"command\":\"deploy --token hunter2\"}","output":"stderr secret"}"#
            )
            .to_owned(),
        });

        assert!(event.message.contains("run_command"));
        assert!(event.message.contains("is_error"));
        for secret in ["deploy", "hunter2", "stderr secret"] {
            assert!(!event.message.contains(secret), "{}", event.message);
        }
    }
}
