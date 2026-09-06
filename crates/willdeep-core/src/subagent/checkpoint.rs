use std::path::Path;

use crate::{Agent, AgentError, AgentOutcome, Message, Session, SessionStore};

fn persistence_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Checkpoint(error.to_string())
}

pub(super) async fn run(
    agent: &Agent,
    home: &Path,
    workspace: &Path,
    id: uuid::Uuid,
    profile: &str,
    brief: String,
) -> Result<AgentOutcome, AgentError> {
    std::fs::create_dir_all(home).map_err(persistence_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))
            .map_err(persistence_error)?;
    }
    let store = SessionStore::new(home);
    match store.load(id) {
        Ok(_) => {}
        Err(crate::session::SessionError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            let mut session = Session::new(workspace.to_path_buf(), Some(profile.into()), &brief);
            session.id = id;
            store.save(&mut session).map_err(persistence_error)?;
        }
        Err(error) => return Err(persistence_error(error)),
    }
    let checkpoint = crate::checkpoint::SessionCheckpointSink {
        store: store.clone(),
        session_id: id,
    }
    .claim()
    .map_err(persistence_error)?;
    let mut session = store.load(id).map_err(persistence_error)?;
    if session.workspace != workspace {
        return Err(AgentError::Checkpoint(
            "worker checkpoint workspace changed".into(),
        ));
    }
    if let Some(notice) = session
        .execution_checkpoint
        .as_ref()
        .and_then(crate::checkpoint::CheckpointMetadata::recovery_notice)
    {
        session.messages.push(notice);
    }
    agent
        .run_checkpointed(session.messages, Message::user(brief), Some(&checkpoint))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct WaitingProvider(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);
    #[async_trait::async_trait]
    impl crate::provider::Provider for WaitingProvider {
        async fn complete(
            &self,
            _: &[Message],
            _: &[crate::types::ToolDefinition],
        ) -> Result<crate::types::Completion, crate::provider::ProviderError> {
            if let Some(started) = self.0.lock().unwrap().take() {
                let _ = started.send(());
            }
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn cancelled_worker_releases_ownership_and_preserves_its_submitted_message() {
        let root = std::env::temp_dir().join(format!("worker-cancel-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let home = root.join("workers");
        std::fs::create_dir_all(&workspace).unwrap();
        let id = uuid::Uuid::new_v4();
        let (started, observed) = tokio::sync::oneshot::channel();
        let agent = Agent::new(
            Arc::new(WaitingProvider(std::sync::Mutex::new(Some(started)))),
            crate::ToolRegistry::new(&workspace, crate::ApprovalMode::ReadOnly).unwrap(),
            crate::AgentConfig {
                max_turns: 1,
                system_prompt: String::new(),
                context_window: 32000,
                token_budget: None,
            },
        );
        let mut execution = Box::pin(run(
            &agent,
            &home,
            &workspace,
            id,
            "reader",
            "inspect safely".into(),
        ));
        tokio::select! {
            _ = &mut execution => panic!("worker finished before cancellation"),
            result = tokio::time::timeout(std::time::Duration::from_secs(3), observed) => { result.unwrap().unwrap(); }
        }
        drop(execution);
        let store = SessionStore::new(&home);
        let session = store.load(id).unwrap();
        assert!(session.messages.iter().any(
            |message| message.role == crate::Role::User && message.content == "inspect safely"
        ));
        assert_eq!(
            session.execution_checkpoint.unwrap().status,
            crate::checkpoint::CheckpointStatus::Running
        );
        let ownership = crate::checkpoint::SessionCheckpointSink {
            store,
            session_id: id,
        }
        .claim()
        .unwrap();
        drop(ownership);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Probe {
        resume: bool,
        calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl crate::provider::Provider for Probe {
        async fn complete(
            &self,
            messages: &[Message],
            _: &[crate::types::ToolDefinition],
        ) -> Result<crate::types::Completion, crate::provider::ProviderError> {
            let turn = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.resume {
                assert_eq!(messages.iter().filter(|message| message.tool_call_id.as_deref() == Some("persisted-write")).count(), 1);
                assert!(messages.iter().any(|message| {
                    message
                        .tool_calls
                        .iter()
                        .any(|call| call.id == "persisted-write")
                }));
            } else if turn > 0 {
                return Err(crate::provider::ProviderError::DeadlineExceeded);
            }
            Ok(crate::types::Completion {
                content: "report".into(),
                usage: None,
                finish_reason: Some("stop".into()),
                tool_calls: if self.resume {
                    Vec::new()
                } else {
                    vec![crate::types::ToolCall {
                        id: "persisted-write".into(),
                        name: "create_file".into(),
                        arguments: serde_json::json!({"path":"result.txt","content":"one write"})
                            .to_string(),
                    }]
                },
            })
        }
    }

    #[tokio::test]
    async fn recreated_worker_receives_completed_tools_from_its_durable_history() {
        let root = std::env::temp_dir().join(format!("worker-resume-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let home = root.join("workers");
        std::fs::create_dir_all(&workspace).unwrap();
        let id = uuid::Uuid::new_v4();
        let agent = |resume| {
            Agent::new(
                Arc::new(Probe {
                    resume,
                    calls: AtomicUsize::new(0),
                }),
                crate::ToolRegistry::new(&workspace, crate::ApprovalMode::WorkspaceAccess).unwrap(),
                crate::AgentConfig {
                    max_turns: 3,
                    system_prompt: String::new(),
                    context_window: 32000,
                    token_budget: None,
                },
            )
        };
        assert!(
            run(
                &agent(false),
                &home,
                &workspace,
                id,
                "implementer",
                "write".into()
            )
            .await
            .is_err()
        );
        let saved = SessionStore::new(&home).load(id).unwrap();
        assert_eq!(
            saved.execution_checkpoint.unwrap().status,
            crate::checkpoint::CheckpointStatus::Failed
        );
        let first_modified = std::fs::metadata(workspace.join("result.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let outcome = run(
            &agent(true),
            &home,
            &workspace,
            id,
            "implementer",
            "continue".into(),
        )
        .await
        .unwrap();
        assert!(outcome.stop_reason.is_complete());
        assert_eq!(
            std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
            "one write"
        );
        assert_eq!(
            std::fs::metadata(workspace.join("result.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            first_modified
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
