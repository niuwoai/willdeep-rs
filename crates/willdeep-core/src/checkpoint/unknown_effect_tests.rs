use super::*;
use crate::provider::{Provider, ProviderError};
use crate::types::{Completion, ToolCall, ToolDefinition};
use crate::{
    Agent, AgentConfig, AgentEvent, ApprovalDecision, ApprovalMode, Approver, EventSink, Session,
    SessionStore, ToolRegistry,
};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn append_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(), name: "run_command".into(),
        arguments: serde_json::json!({"command": "ruby -e 'File.open(\"writes.log\", \"a\") { |f| f.write(\"once\\n\") }'"}).to_string(),
    }
}

struct AppendProvider {
    requests: AtomicUsize,
    recovering: bool,
}

#[async_trait]
impl Provider for AppendProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let first = self.requests.fetch_add(1, Ordering::SeqCst) == 0;
        if first && self.recovering {
            assert!(messages.iter().any(|message| {
                message
                    .content
                    .starts_with("[willdeep-unknown-tool-effect]")
            }));
            assert!(
                messages
                    .iter()
                    .flat_map(|message| &message.tool_calls)
                    .any(|call| call.id == "original-append"
                        && call.arguments == append_call("").arguments)
            );
        }
        Ok(Completion {
            content: "Report the current state".into(),
            tool_calls: if first {
                vec![append_call(if self.recovering {
                    "retried-append"
                } else {
                    "original-append"
                })]
            } else {
                Vec::new()
            },
            finish_reason: Some("stop".into()),
            usage: None,
        })
    }
}

struct Approval {
    recovering: bool,
    requests: AtomicUsize,
}
#[async_trait]
impl Approver for Approval {
    async fn approve(&self, description: &str, rememberable: bool) -> ApprovalDecision {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if self.recovering {
            assert!(description.contains("effects are unknown"));
            assert!(!rememberable);
            ApprovalDecision::AlwaysAllow
        } else {
            ApprovalDecision::AllowOnce
        }
    }
}

struct BeforeReceipt(Arc<tokio::sync::Notify>);
#[async_trait]
impl EventSink for BeforeReceipt {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::ToolCompleted { call, is_error, .. } = event
            && call.id == "original-append"
        {
            assert!(!is_error, "the write must have actually succeeded");
            self.0.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

fn agent(workspace: &std::path::Path, approval: Arc<Approval>) -> Agent {
    Agent::new(
        Arc::new(AppendProvider {
            requests: 0.into(),
            recovering: approval.recovering,
        }),
        ToolRegistry::new(workspace, ApprovalMode::WorkspaceAccess)
            .unwrap()
            .with_approver(approval),
        AgentConfig {
            max_turns: 3,
            system_prompt: "test".into(),
            context_window: 32_000,
            token_budget: None,
        },
    )
}

#[tokio::test]
async fn cancelled_after_real_write_before_receipt_cannot_automatically_replay() {
    let home = std::env::temp_dir().join(format!("unknown-receipt-{}", uuid::Uuid::new_v4()));
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = SessionStore::new(&home);
    let mut session = Session::new(workspace.clone(), None, "append once");
    store.save(&mut session).unwrap();
    let sink = SessionCheckpointSink {
        store,
        session_id: session.id,
    };
    let boundary = Arc::new(tokio::sync::Notify::new());
    let first = agent(
        &workspace,
        Arc::new(Approval {
            recovering: false,
            requests: 0.into(),
        }),
    )
    .with_event_sink(Arc::new(BeforeReceipt(boundary.clone())));
    let mut execution =
        Box::pin(first.run_checkpointed(Vec::new(), Message::user("append once"), Some(&sink)));
    tokio::select! {
        _ = &mut execution => panic!("execution finished before receipt boundary"),
        result = tokio::time::timeout(std::time::Duration::from_secs(10), boundary.notified()) => result.unwrap(),
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("writes.log")).unwrap(),
        "once\n"
    );
    drop(execution);
    drop(first);
    let saved = sink.store.load(session.id).unwrap();
    assert_eq!(
        saved
            .execution_checkpoint
            .as_ref()
            .unwrap()
            .pending_call_ids,
        vec!["original-append"]
    );
    assert!(
        !saved
            .messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("original-append"))
    );
    drop(sink);

    // New store and executor use only disk state. Broad automatic approval must
    // not substitute for the one-time approval required by an unknown effect.
    let restored_sink = SessionCheckpointSink {
        store: SessionStore::new(&home),
        session_id: session.id,
    };
    let approval = Arc::new(Approval {
        recovering: true,
        requests: 0.into(),
    });
    let restored = agent(&workspace, approval.clone());
    let outcome = restored
        .run_checkpointed(
            saved.messages,
            Message::user("continue"),
            Some(&restored_sink),
        )
        .await
        .unwrap();
    assert_eq!(approval.requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("writes.log")).unwrap(),
        "once\n"
    );
    assert!(outcome.messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("retried-append")
            && message
                .content
                .contains("uncertain replay requires one-time approval")
    }));
    let reloaded = restored_sink.store.load(session.id).unwrap();
    assert!(reloaded.messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("original-append")
            && message
                .content
                .starts_with("[willdeep-unknown-tool-effect]")
    }));
    std::fs::remove_dir_all(home).unwrap();
}
