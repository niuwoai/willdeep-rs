use super::*;
use crate::checkpoint::{CheckpointSink, RunCheckpoint};
use crate::types::Completion;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn manual_compression_bill_survives_reload_and_stale_metadata_save() {
    let (agent, _) = agent(8192);
    let home = std::env::temp_dir().join(format!("compression-ledger-{}", uuid::Uuid::new_v4()));
    let store = crate::SessionStore::new(&home);
    let mut session = crate::Session::new(home.clone(), None, "test");
    session.messages = history();
    store.save(&mut session).unwrap();
    let mut metadata_writer = store.load(session.id).unwrap();
    let agent = agent.with_compressors(vec![(BilledProvider::new("summary", 10), false)]);
    let compressed = agent
        .compress_history_recorded(session.messages.clone(), &mut |usage| {
            store
                .record_compression_usage(session.id, usage)
                .map_err(|e| AgentError::Checkpoint(e.to_string()))
        })
        .await
        .unwrap();
    // A stale title edit must merge the newer bill rather than overwrite it.
    metadata_writer.title = "renamed".into();
    store.save(&mut metadata_writer).unwrap();
    store.refresh_execution(&mut session).unwrap();
    session.replace_with_compressed_messages(compressed);
    store.save(&mut session).unwrap();
    store.save(&mut session).unwrap();
    let reloaded = crate::SessionStore::new(&home).load(session.id).unwrap();
    assert_eq!(reloaded.title, "renamed");
    assert_eq!(reloaded.manual_compression_usage.reported_calls, 1);
    assert_eq!(reloaded.manual_compression_usage.input_tokens, 10);
    assert_eq!(reloaded.manual_compression_usage.output_tokens, 2);
    assert_eq!(reloaded.compression_generation, 1);
    assert!(reloaded.execution_checkpoint.is_none());
    std::fs::remove_dir_all(home).unwrap();
}

#[tokio::test]
async fn failed_manual_compression_keeps_bill_and_original_history() {
    let (agent, _) = agent(8192);
    let home = std::env::temp_dir().join(format!(
        "compression-failed-ledger-{}",
        uuid::Uuid::new_v4()
    ));
    let store = crate::SessionStore::new(&home);
    let mut session = crate::Session::new(home.clone(), None, "test");
    session.messages = history();
    store.save(&mut session).unwrap();
    let agent = agent.with_compressors(vec![(BilledProvider::new("", 5), false)]);
    let result = agent
        .compress_history_recorded(session.messages.clone(), &mut |usage| {
            store
                .record_compression_usage(session.id, usage)
                .map_err(|e| AgentError::Checkpoint(e.to_string()))
        })
        .await;
    assert!(result.is_err());
    let reloaded = store.load(session.id).unwrap();
    assert_eq!(reloaded.manual_compression_usage.reported_calls, 1);
    assert_eq!(reloaded.manual_compression_usage.input_tokens, 5);
    assert_eq!(reloaded.messages.len(), session.messages.len());
    assert_eq!(reloaded.compression_generation, 0);
    std::fs::remove_dir_all(home).unwrap();
}

struct BilledProvider {
    calls: AtomicUsize,
    content: &'static str,
    input: u64,
    interrupted: bool,
}

impl BilledProvider {
    fn new(content: &'static str, input: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            content,
            input,
            interrupted: false,
        })
    }
}

#[async_trait]
impl Provider for BilledProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let completion = Completion {
            content: self.content.into(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".into()),
            usage: Some(Usage {
                input_tokens: Some(self.input),
                output_tokens: Some(2),
                total_tokens: Some(self.input + 2),
                ..Usage::default()
            }),
        };
        if self.interrupted {
            return Err(ProviderError::StreamInterrupted {
                source: Box::new(ProviderError::EmptyResponse),
                partial: Box::new(completion),
            });
        }
        Ok(completion)
    }
}

#[derive(Default)]
struct Ledger {
    checkpoints: Mutex<Vec<RunCheckpoint>>,
    usage: Mutex<Vec<Usage>>,
}

impl CheckpointSink for Ledger {
    fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
        self.checkpoints.lock().unwrap().push(checkpoint.clone());
        Ok(())
    }
}

#[async_trait]
impl EventSink for Ledger {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::Usage(usage) = event {
            self.usage.lock().unwrap().push(usage);
        }
    }
}

#[tokio::test]
async fn automatic_compression_counts_in_outcome_and_durable_usage() {
    let (mut agent, _) = agent(8192);
    let root = BilledProvider::new("done", 3);
    let compressor = BilledProvider::new("summary", 10);
    agent.provider = RwLock::new(root.clone());
    let ledger = Arc::new(Ledger::default());
    let agent = agent
        .with_compressors(vec![(compressor.clone(), false)])
        .with_event_sink(ledger.clone());
    let result = agent
        .run_checkpointed(history(), Message::user("continue"), Some(ledger.as_ref()))
        .await
        .unwrap();
    assert_eq!((result.input_tokens, result.output_tokens), (13, 4));
    assert_eq!(root.calls.load(Ordering::SeqCst), 1);
    assert_eq!(compressor.calls.load(Ordering::SeqCst), 1);
    let checkpoints = ledger.checkpoints.lock().unwrap();
    let last = &checkpoints.last().unwrap().metadata;
    assert_eq!((last.input_tokens, last.output_tokens), (13, 4));
    assert!(
        checkpoints
            .iter()
            .any(|c| c.metadata.input_tokens == 10 && c.metadata.output_tokens == 2)
    );
    assert_eq!(ledger.usage.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn compression_exhausting_budget_prevents_root_request_and_keeps_bill() {
    let (mut agent, _) = agent(8192);
    let root = BilledProvider::new("must not run", 3);
    agent.provider = RwLock::new(root.clone());
    agent.config.token_budget = Some(12);
    let ledger = Arc::new(Ledger::default());
    let agent = agent
        .with_compressors(vec![(BilledProvider::new("summary", 10), false)])
        .with_event_sink(ledger.clone());
    let result = agent
        .run_checkpointed(history(), Message::user("continue"), Some(ledger.as_ref()))
        .await;
    assert!(matches!(
        result,
        Err(AgentError::TokenBudgetExceeded {
            budget: 12,
            used: 12
        })
    ));
    assert_eq!(root.calls.load(Ordering::SeqCst), 0);
    let checkpoints = ledger.checkpoints.lock().unwrap();
    let last = &checkpoints.last().unwrap().metadata;
    assert_eq!((last.input_tokens, last.output_tokens), (10, 2));
    assert_eq!(ledger.usage.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn manual_compression_reports_unusable_and_interrupted_fallback_usage() {
    let (agent, _) = agent(8192);
    let interrupted = Arc::new(BilledProvider {
        calls: AtomicUsize::new(0),
        content: "partial",
        input: 7,
        interrupted: true,
    });
    let ledger = Arc::new(Ledger::default());
    let agent = agent
        .with_compressors(vec![
            (BilledProvider::new("", 5), false),
            (interrupted, false),
            (BilledProvider::new("summary", 10), false),
        ])
        .with_event_sink(ledger.clone());
    agent.compress_history(history()).await.unwrap();
    let usage = ledger.usage.lock().unwrap();
    assert_eq!(usage.len(), 3);
    assert_eq!(
        usage.iter().map(|u| u.input_tokens.unwrap()).sum::<u64>(),
        22
    );
}
