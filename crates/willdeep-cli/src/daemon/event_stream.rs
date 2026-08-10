use std::collections::VecDeque;
use std::convert::Infallible;

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
        read_events(&self.path, after, limit)
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<RuntimeEvent> {
        self.live.subscribe()
    }

    pub(super) fn latest_sequence(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.next_sequence.saturating_sub(1))
            .unwrap_or_default()
    }
}

struct LiveEventState {
    backlog: VecDeque<RuntimeEvent>,
    receiver: tokio::sync::broadcast::Receiver<RuntimeEvent>,
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

async fn next_event(state: &mut LiveEventState) -> Option<RuntimeEvent> {
    loop {
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
        match state.receiver.recv().await {
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
        let mut state = LiveEventState {
            backlog: log.read_after(0, 1).unwrap().into(),
            receiver,
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
}
