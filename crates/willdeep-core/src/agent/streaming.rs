use super::*;
use crate::checkpoint::CheckpointRecorder;
use crate::provider::{ProviderEvent, ProviderEventSink};
use std::sync::Mutex;

#[derive(Default)]
struct State {
    text: String,
    first_text_at: Option<std::time::Instant>,
    usage: Option<Usage>,
    failure: Option<String>,
}

pub(super) struct StreamEvents<'a, 'b> {
    checkpoint: Mutex<&'a mut CheckpointRecorder<'b>>,
    state: Mutex<State>,
    sink: &'a dyn EventSink,
    turn: usize,
    input: u64,
    output: u64,
}

impl<'a, 'b> StreamEvents<'a, 'b> {
    pub(super) fn new(
        checkpoint: &'a mut CheckpointRecorder<'b>,
        sink: &'a dyn EventSink,
        turn: usize,
        input: u64,
        output: u64,
    ) -> Self {
        Self {
            checkpoint: Mutex::new(checkpoint),
            state: Mutex::new(State::default()),
            sink,
            turn,
            input,
            output,
        }
    }

    pub(super) fn partial(&self) -> Result<crate::types::Completion, AgentError> {
        self.flush_checkpoint()?;
        let state = self
            .state
            .lock()
            .map_err(|_| AgentError::Checkpoint("stream state lock poisoned".into()))?;
        if let Some(error) = &state.failure {
            return Err(AgentError::Checkpoint(error.clone()));
        }
        Ok(crate::types::Completion {
            content: state.text.clone(),
            tool_calls: Vec::new(),
            usage: state.usage.clone(),
            finish_reason: Some("incomplete".into()),
        })
    }

    pub(super) fn first_text_at(&self) -> Option<std::time::Instant> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .first_text_at
    }

    pub(super) async fn wait<T>(
        &self,
        response: impl std::future::Future<Output = T>,
    ) -> Result<T, AgentError> {
        tokio::pin!(response);
        let mut interval = tokio::time::interval(crate::checkpoint::STREAM_CHECKPOINT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                value = &mut response => return Ok(value),
                _ = interval.tick() => self.flush_checkpoint()?,
            }
        }
    }

    fn flush_checkpoint(&self) -> Result<(), AgentError> {
        if let Some(error) = &self
            .state
            .lock()
            .map_err(|_| AgentError::Checkpoint("stream state lock poisoned".into()))?
            .failure
        {
            return Err(AgentError::Checkpoint(error.clone()));
        }
        self.checkpoint
            .lock()
            .map_err(|_| AgentError::Checkpoint("stream checkpoint lock poisoned".into()))?
            .flush_stream()
    }

    fn persist(&self, event: &ProviderEvent) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match event {
            ProviderEvent::TextDelta(text) => {
                if !text.is_empty() {
                    state
                        .first_text_at
                        .get_or_insert_with(std::time::Instant::now);
                }
                state.text.push_str(text);
            }
            ProviderEvent::Usage(usage) => state.usage = Some(usage.clone()),
            ProviderEvent::RetryWait { .. } | ProviderEvent::RetryStarted { .. } => return,
        }
        if state.failure.is_some() {
            return;
        }
        let usage = state.usage.as_ref().cloned().unwrap_or_default();
        let result = self
            .checkpoint
            .lock()
            .map_err(|_| "stream checkpoint lock poisoned".to_owned())
            .and_then(|mut checkpoint| {
                checkpoint
                    .record_stream(
                        match event {
                            ProviderEvent::TextDelta(delta) => Some(delta.as_str()),
                            _ => None,
                        },
                        self.turn,
                        self.input.saturating_add(usage.input_tokens.unwrap_or(0)),
                        self.output.saturating_add(usage.output_tokens.unwrap_or(0)),
                    )
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            state.failure = Some(error);
        }
    }
}

#[async_trait::async_trait]
impl ProviderEventSink for StreamEvents<'_, '_> {
    async fn emit(&self, event: ProviderEvent) {
        self.persist(&event);
        self.sink.emit(AgentEvent::ProviderProgress(event)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_event_save_failure_remains_terminal_even_if_a_retry_would_succeed() {
        struct FailOnce(std::sync::atomic::AtomicUsize);
        impl crate::checkpoint::CheckpointSink for FailOnce {
            fn save(&self, _: &crate::checkpoint::RunCheckpoint) -> Result<(), String> {
                if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                    Err("transient write failure".into())
                } else {
                    Ok(())
                }
            }
        }
        let sink = FailOnce(std::sync::atomic::AtomicUsize::new(0));
        let mut checkpoint = CheckpointRecorder::new(Some(&sink));
        checkpoint
            .record(&[Message::user("work")], 1, 0, 0)
            .unwrap();
        let events = StreamEvents::new(&mut checkpoint, &NoopSink, 1, 0, 0);
        let response = async {
            events.emit(ProviderEvent::TextDelta("first".into())).await;
            std::future::pending::<()>().await;
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), events.wait(response))
            .await
            .unwrap();
        assert!(matches!(result, Err(AgentError::Checkpoint(_))));
    }

    #[tokio::test]
    async fn periodic_checkpoint_failure_cancels_a_silent_provider_wait() {
        struct Failing(std::sync::atomic::AtomicUsize);
        impl crate::checkpoint::CheckpointSink for Failing {
            fn save(&self, _: &crate::checkpoint::RunCheckpoint) -> Result<(), String> {
                if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2 {
                    return Err("checkpoint write failed".into());
                }
                Ok(())
            }
        }
        let sink = Failing(std::sync::atomic::AtomicUsize::new(0));
        let mut checkpoint = CheckpointRecorder::new(Some(&sink));
        checkpoint
            .record(&[Message::user("work")], 1, 0, 0)
            .unwrap();
        let events = StreamEvents::new(&mut checkpoint, &NoopSink, 1, 0, 0);
        let response = async {
            events.emit(ProviderEvent::TextDelta("first".into())).await;
            events.emit(ProviderEvent::TextDelta(" tail".into())).await;
            std::future::pending::<()>().await;
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), events.wait(response))
            .await
            .unwrap();
        assert!(matches!(result, Err(AgentError::Checkpoint(_))));
    }

    #[tokio::test]
    async fn quiet_provider_does_not_leave_text_buffered_until_another_event() {
        struct Saved(Mutex<Vec<String>>);
        impl crate::checkpoint::CheckpointSink for Saved {
            fn save(&self, checkpoint: &crate::checkpoint::RunCheckpoint) -> Result<(), String> {
                self.0
                    .lock()
                    .unwrap()
                    .push(checkpoint.messages.last().unwrap().content.clone());
                Ok(())
            }
        }
        let saved = Saved(Mutex::new(Vec::new()));
        let mut checkpoint = CheckpointRecorder::new(Some(&saved));
        checkpoint
            .record(&[Message::user("work")], 1, 0, 0)
            .unwrap();
        let events = StreamEvents::new(&mut checkpoint, &NoopSink, 1, 0, 0);
        let response = async {
            // Let the initial interval tick pass before producing the burst.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            events.emit(ProviderEvent::TextDelta("first".into())).await;
            events.emit(ProviderEvent::TextDelta(" tail".into())).await;
            assert_eq!(saved.0.lock().unwrap().last().unwrap(), "first");
            while saved.0.lock().unwrap().last().unwrap() != "first tail" {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), events.wait(response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.0.lock().unwrap().last().unwrap(), "first tail");
    }

    #[tokio::test]
    async fn first_text_time_ignores_empty_text_and_usage_and_stays_fixed() {
        let mut checkpoint = CheckpointRecorder::new(None);
        let events = StreamEvents::new(&mut checkpoint, &NoopSink, 1, 0, 0);
        events.emit(ProviderEvent::TextDelta(String::new())).await;
        events.emit(ProviderEvent::Usage(Usage::default())).await;
        events
            .emit(ProviderEvent::RetryWait {
                attempt: 1,
                delay: std::time::Duration::from_secs(2),
            })
            .await;
        assert!(events.first_text_at().is_none());
        let before = std::time::Instant::now();
        events.emit(ProviderEvent::TextDelta("首字".into())).await;
        let first = events.first_text_at().unwrap();
        assert!(first >= before && first <= std::time::Instant::now());
        events.emit(ProviderEvent::TextDelta("后续".into())).await;
        assert_eq!(events.first_text_at(), Some(first));
    }
}
