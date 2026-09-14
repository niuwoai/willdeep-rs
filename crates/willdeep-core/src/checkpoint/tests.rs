use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use super::*;
use crate::provider::{Provider, ProviderError};
use crate::types::{Completion, ToolCall, ToolDefinition, Usage};
use crate::{Agent, AgentConfig, ApprovalMode, Session, SessionStore, ToolRegistry};

struct WriteThenStop {
    requests: AtomicUsize,
    hang: bool,
}

#[test]
fn streaming_reuses_history_and_appends_only_the_current_assistant_message() {
    #[derive(Default)]
    struct Inspect(std::sync::Mutex<Vec<(usize, Vec<String>, u64)>>);
    impl CheckpointSink for Inspect {
        fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
            self.0.lock().unwrap().push((
                checkpoint.messages[0].content.as_ptr() as usize,
                checkpoint
                    .messages
                    .iter()
                    .skip(1)
                    .map(|message| message.content.clone())
                    .collect(),
                checkpoint.metadata.output_tokens,
            ));
            Ok(())
        }
    }
    let sink = Inspect::default();
    let mut recorder = CheckpointRecorder::new(Some(&sink));
    let history = vec![Message::user("large history ".repeat(10000))];
    recorder.record(&history, 1, 0, 0).unwrap();
    recorder.record_stream(Some("第一段"), 1, 10, 1).unwrap();
    recorder.record_stream(Some("，第二段"), 1, 10, 2).unwrap();
    recorder.record_stream(None, 1, 10, 3).unwrap();
    let snapshots = sink.0.lock().unwrap();
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.0 == snapshots[0].0),
        "unchanged history must not be reallocated on each delta"
    );
    assert_eq!(snapshots.last().unwrap().1, vec!["第一段，第二段"]);
    assert_eq!(snapshots.last().unwrap().2, 3);
}

#[test]
fn claimed_sink_holds_ownership_between_runs_and_rejects_concurrent_reuse() {
    let (_, sink, session) = fixture(false);
    let competitor = SessionCheckpointSink {
        store: sink.store.clone(),
        session_id: session.id,
    };
    let claimed = sink.claim().unwrap();
    assert!(competitor.acquire_run().is_err());
    let first = claimed.acquire_run().unwrap();
    assert!(claimed.acquire_run().is_err());
    drop(first);
    assert!(competitor.acquire_run().is_err());
    let second = claimed.acquire_run().unwrap();
    drop(second);
    drop(claimed);
    assert!(competitor.acquire_run().unwrap().is_some());
}

#[test]
#[ignore = "subprocess entry point invoked by execution_lock_is_shared_across_processes"]
fn execution_lock_probe() {
    let home = std::env::var_os("WILLDEEP_TEST_LOCK_HOME").unwrap();
    let id = std::env::var("WILLDEEP_TEST_LOCK_ID")
        .unwrap()
        .parse()
        .unwrap();
    let expected = std::env::var("WILLDEEP_TEST_LOCK_AVAILABLE").unwrap() == "true";
    let sink = SessionCheckpointSink {
        store: SessionStore::new(home),
        session_id: id,
    };
    assert_eq!(sink.acquire_run().is_ok(), expected);
}

#[test]
fn execution_lock_is_shared_across_processes() {
    let (_, sink, session) = fixture(false);
    let guard = sink.acquire_run().unwrap();
    let probe = |available: bool| {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "checkpoint::tests::execution_lock_probe",
                "--ignored",
            ])
            .env(
                "WILLDEEP_TEST_LOCK_HOME",
                session.workspace.parent().unwrap(),
            )
            .env("WILLDEEP_TEST_LOCK_ID", session.id.to_string())
            .env("WILLDEEP_TEST_LOCK_AVAILABLE", available.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    };
    probe(false);
    drop(guard);
    probe(true);
}

#[tokio::test]
async fn a_second_run_is_rejected_before_provider_or_tool_execution() {
    let (agent, sink, session) = fixture(false);
    let guard = sink.acquire_run().unwrap().unwrap();
    let second = SessionCheckpointSink {
        store: sink.store.clone(),
        session_id: session.id,
    };
    let result = agent
        .run_checkpointed(
            session.messages.clone(),
            Message::user("write file"),
            Some(&second),
        )
        .await;
    assert!(
        matches!(result, Err(AgentError::Checkpoint(message)) if message.contains("active execution"))
    );
    assert!(!session.workspace.join("result.txt").exists());
    drop(guard);
    assert!(second.acquire_run().unwrap().is_some());
}

#[test]
fn execution_guards_are_session_scoped_and_leave_metadata_editable() {
    let (_, sink, session) = fixture(false);
    let guard = sink.acquire_run().unwrap();
    sink.store.set_pinned(session.id, true).unwrap();
    let mut other = Session::new(session.workspace.clone(), None, "another task");
    sink.store.save(&mut other).unwrap();
    let other_sink = SessionCheckpointSink {
        store: sink.store.clone(),
        session_id: other.id,
    };
    assert!(other_sink.acquire_run().unwrap().is_some());
    assert!(sink.acquire_run().is_err());
    drop(guard);
    assert!(sink.acquire_run().unwrap().is_some());
}

#[tokio::test]
async fn unreadable_recovery_state_prevents_execution() {
    let (agent, mut sink, session) = fixture(false);
    sink.session_id = uuid::Uuid::new_v4();
    let result = agent
        .run_checkpointed(session.messages, Message::user("write file"), Some(&sink))
        .await;
    assert!(matches!(result, Err(crate::AgentError::Checkpoint(_))));
    assert!(!session.workspace.join("result.txt").exists());
}

#[tokio::test]
async fn restarting_an_unverified_run_preserves_its_original_baseline() {
    struct ClaimComplete(std::path::PathBuf);
    #[async_trait]
    impl Provider for ClaimComplete {
        async fn complete(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            std::fs::write(self.0.join("revision"), "changed").unwrap();
            Ok(Completion {
                content: "finished".into(),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".into()),
            })
        }
    }
    let root = std::env::temp_dir().join(format!(
        "willdeep-resume-verification-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("revision"), "initial").unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "finish work");
    store.save(&mut session).unwrap();
    for _ in 0..2 {
        // Recreate the agent, store and sink: no in-memory baseline survives.
        let captured = root.clone();
        let agent = Agent::new(
            Arc::new(ClaimComplete(root.clone())),
            ToolRegistry::new(&root, ApprovalMode::ReadOnly)
                .unwrap()
                .with_verification_snapshot(move || {
                    std::fs::read_to_string(captured.join("revision")).ok()
                })
                .with_verification_reporter(|_| {}),
            AgentConfig {
                max_turns: 5,
                system_prompt: "test".into(),
                context_window: 128_000,
                token_budget: None,
            },
        );
        let sink = SessionCheckpointSink {
            store: SessionStore::new(&root),
            session_id: session.id,
        };
        let saved = sink.store.load(session.id).unwrap();
        let result = agent
            .run_checkpointed(saved.messages, Message::user("continue"), Some(&sink))
            .await
            .unwrap();
        assert_eq!(result.stop_reason, crate::AgentStopReason::Unverified);
        let saved = sink.store.load(session.id).unwrap();
        let checkpoint = saved.execution_checkpoint.unwrap();
        assert_eq!(checkpoint.status, CheckpointStatus::Partial);
        assert_eq!(checkpoint.verification_baseline.as_deref(), Some("initial"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[async_trait]
impl Provider for WriteThenStop {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        if self.requests.fetch_add(1, Ordering::SeqCst) > 0 {
            if self.hang {
                std::future::pending::<()>().await;
            }
            return Err(ProviderError::EmptyResponse);
        }
        Ok(Completion {
            content: "Create the requested file".to_owned(),
            tool_calls: vec![ToolCall {
                id: "write-once".to_owned(),
                name: "create_file".to_owned(),
                arguments: r#"{"path":"result.txt","content":"completed write"}"#.to_owned(),
            }],
            finish_reason: Some("tool_calls".to_owned()),
            usage: Some(Usage {
                input_tokens: Some(40),
                output_tokens: Some(20),
                ..Usage::default()
            }),
        })
    }
}

fn fixture(hang: bool) -> (Agent, SessionCheckpointSink, Session) {
    let home = std::env::temp_dir().join(format!("willdeep-checkpoint-{}", uuid::Uuid::new_v4()));
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = SessionStore::new(&home);
    let mut session = Session::new(workspace.clone(), None, "write file");
    store.save(&mut session).unwrap();
    let sink = SessionCheckpointSink {
        store,
        session_id: session.id,
    };
    let agent = Agent::new(
        Arc::new(WriteThenStop {
            requests: AtomicUsize::new(0),
            hang,
        }),
        ToolRegistry::new(workspace, ApprovalMode::WorkspaceAccess).unwrap(),
        AgentConfig {
            max_turns: 3,
            system_prompt: "test".to_owned(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    (agent, sink, session)
}

fn assert_saved_write(sink: &SessionCheckpointSink, status: CheckpointStatus) {
    let saved = sink.store.load(sink.session_id).unwrap();
    assert_eq!(
        std::fs::read_to_string(saved.workspace.join("result.txt")).unwrap(),
        "completed write"
    );
    assert!(
        saved
            .messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("write-once"))
    );
    let checkpoint = saved.execution_checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.status, status);
    assert_eq!(checkpoint.input_tokens, 40);
    assert_eq!(checkpoint.output_tokens, 20);
    assert!(checkpoint.pending_call_ids.is_empty());
    assert!(
        checkpoint
            .recovery_notice()
            .unwrap()
            .content
            .contains("do not repeat successful writes")
    );
}

#[tokio::test]
async fn provider_failure_keeps_real_writes_and_their_tool_history() {
    let (agent, sink, _) = fixture(false);
    assert!(
        agent
            .run_checkpointed(Vec::new(), Message::user("write file"), Some(&sink))
            .await
            .is_err()
    );
    assert_saved_write(&sink, CheckpointStatus::Failed);
    assert!(sink.acquire_run().unwrap().is_some());
}

#[tokio::test]
async fn cancellation_keeps_the_last_durable_boundary() {
    struct NextTurn(Arc<tokio::sync::Notify>);
    #[async_trait]
    impl crate::EventSink for NextTurn {
        async fn emit(&self, event: crate::AgentEvent) {
            if matches!(event, crate::AgentEvent::TurnStarted { turn: 2 }) {
                self.0.notify_one();
            }
        }
    }
    let (agent, sink, _) = fixture(true);
    let next_turn = Arc::new(tokio::sync::Notify::new());
    let agent = agent.with_event_sink(Arc::new(NextTurn(next_turn.clone())));
    let mut execution =
        Box::pin(agent.run_checkpointed(Vec::new(), Message::user("write file"), Some(&sink)));
    tokio::select! {
        _ = &mut execution => panic!("run finished before cancellation"),
        result = tokio::time::timeout(std::time::Duration::from_secs(10), next_turn.notified()) => result.unwrap(),
    }
    drop(execution);
    assert_saved_write(&sink, CheckpointStatus::Running);
    assert!(sink.acquire_run().unwrap().is_some());
}

struct RejectPending;
struct StreamThenStop {
    hang: bool,
}

struct StreamThenWrite;
#[async_trait]
impl Provider for StreamThenWrite {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        unreachable!()
    }
    async fn complete_with_events(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        events: &dyn crate::provider::ProviderEventSink,
    ) -> Result<Completion, ProviderError> {
        events
            .emit(crate::provider::ProviderEvent::TextDelta(
                "reject this checkpoint".into(),
            ))
            .await;
        Ok(Completion {
            content: "write".into(),
            finish_reason: Some("tool_calls".into()),
            usage: None,
            tool_calls: vec![ToolCall {
                id: "write".into(),
                name: "create_file".into(),
                arguments: r#"{"path":"forbidden.txt","content":"must not run"}"#.into(),
            }],
        })
    }
}

#[tokio::test]
async fn failed_stream_checkpoint_prevents_completed_response_tools() {
    struct RejectStream;
    impl CheckpointSink for RejectStream {
        fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
            if checkpoint
                .messages
                .iter()
                .any(|message| message.content == "reject this checkpoint")
            {
                Err("disk full".into())
            } else {
                Ok(())
            }
        }
    }
    let (_, _, session) = fixture(false);
    let agent = Agent::new(
        Arc::new(StreamThenWrite),
        ToolRegistry::new(&session.workspace, ApprovalMode::WorkspaceAccess).unwrap(),
        AgentConfig {
            max_turns: 1,
            system_prompt: "test".into(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    let result = agent
        .run_checkpointed(Vec::new(), Message::user("work"), Some(&RejectStream))
        .await;
    assert!(matches!(result, Err(AgentError::Checkpoint(_))));
    assert!(!session.workspace.join("forbidden.txt").exists());
}

#[async_trait]
impl Provider for StreamThenStop {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        panic!("Agent must use the event-aware provider entry");
    }

    async fn complete_with_events(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        events: &dyn crate::provider::ProviderEventSink,
    ) -> Result<Completion, ProviderError> {
        use crate::provider::ProviderEvent;
        events
            .emit(ProviderEvent::TextDelta("streamed ".into()))
            .await;
        for output in [2, 5] {
            events
                .emit(ProviderEvent::Usage(Usage {
                    input_tokens: Some(4),
                    output_tokens: Some(output),
                    ..Usage::default()
                }))
                .await;
        }
        events.emit(ProviderEvent::TextDelta("text".into())).await;
        if self.hang {
            std::future::pending::<()>().await;
        }
        Err(ProviderError::StreamInterrupted {
            source: Box::new(ProviderError::EmptyResponse),
            partial: Box::new(Completion {
                content: "streamed text".into(),
                tool_calls: Vec::new(),
                finish_reason: Some("incomplete".into()),
                usage: Some(Usage {
                    input_tokens: Some(4),
                    output_tokens: Some(5),
                    ..Usage::default()
                }),
            }),
        })
    }
}

fn stream_fixture(hang: bool) -> (Agent, SessionCheckpointSink) {
    let (_, sink, session) = fixture(false);
    let agent = Agent::new(
        Arc::new(StreamThenStop { hang }),
        ToolRegistry::new(session.workspace, ApprovalMode::WorkspaceAccess).unwrap(),
        AgentConfig {
            max_turns: 3,
            system_prompt: "test".into(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    (agent, sink)
}

fn assert_stream_saved(sink: &SessionCheckpointSink, expected: CheckpointStatus) {
    let saved = sink.store.load(sink.session_id).unwrap();
    assert_eq!(saved.messages.last().unwrap().content, "streamed text");
    assert!(saved.messages.last().unwrap().tool_calls.is_empty());
    let metadata = saved.execution_checkpoint.unwrap();
    assert_eq!(metadata.status, expected);
    assert_eq!(metadata.input_tokens, 4);
    assert_eq!(
        metadata.output_tokens, 5,
        "cumulative snapshots must not be added together"
    );
}

#[tokio::test]
async fn interrupted_stream_persists_text_and_latest_usage() {
    let (agent, sink) = stream_fixture(false);
    assert!(
        agent
            .run_checkpointed(Vec::new(), Message::user("work"), Some(&sink))
            .await
            .is_err()
    );
    assert_stream_saved(&sink, CheckpointStatus::Failed);
}

#[tokio::test]
async fn cancelled_stream_persists_received_events_before_completion() {
    let (agent, sink) = stream_fixture(true);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            agent.run_checkpointed(Vec::new(), Message::user("work"), Some(&sink))
        )
        .await
        .is_err()
    );
    assert_stream_saved(&sink, CheckpointStatus::Running);
}

impl CheckpointSink for RejectPending {
    fn save(&self, checkpoint: &RunCheckpoint) -> Result<(), String> {
        if checkpoint.metadata.pending_call_ids.is_empty() {
            Ok(())
        } else {
            Err("disk unavailable".to_owned())
        }
    }
}

#[tokio::test]
async fn checkpoint_failure_prevents_unjournaled_side_effects() {
    let (agent, _, session) = fixture(false);
    assert!(matches!(
        agent
            .run_checkpointed(
                Vec::new(),
                Message::user("write file"),
                Some(&RejectPending)
            )
            .await,
        Err(AgentError::Checkpoint(_))
    ));
    assert!(!session.workspace.join("result.txt").exists());
}

#[test]
fn unresolved_calls_are_uncertain_and_completed_siblings_are_preserved() {
    let (_, sink, _) = fixture(false);
    let first = ToolCall {
        id: "done".to_owned(),
        name: "create_file".to_owned(),
        arguments: "{}".to_owned(),
    };
    let second = ToolCall {
        id: "uncertain".to_owned(),
        ..first.clone()
    };
    let messages = vec![
        Message::assistant("", vec![first.clone(), second]),
        Message::tool(&first, "saved"),
    ];
    CheckpointRecorder::new(Some(&sink))
        .record(&messages, 2, 11, 12)
        .unwrap();
    let saved = sink.store.load(sink.session_id).unwrap();
    assert_eq!(
        saved
            .execution_checkpoint
            .as_ref()
            .unwrap()
            .pending_call_ids,
        ["uncertain"]
    );
    assert!(
        saved
            .execution_checkpoint
            .unwrap()
            .recovery_notice()
            .unwrap()
            .content
            .contains("UNKNOWN")
    );
    let mut history = saved.messages;
    crate::types::sanitize_tool_history(&mut history);
    assert_eq!(history[0].tool_calls.len(), 1);
    assert_eq!(history[0].tool_calls[0].id, "done");
    assert_eq!(history[1].content, "saved");
}
