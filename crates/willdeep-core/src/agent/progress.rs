use crate::types::ToolCall;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Novel successful observations are evidence; another attempt at the same error is not.
#[derive(Default)]
pub(super) struct ProgressTracker {
    seen: HashSet<[u8; 32]>,
}

impl ProgressTracker {
    pub fn observe(&mut self, call: &ToolCall, output: &str, is_error: bool) -> bool {
        if is_error {
            return false;
        }
        if call.name == "run_command"
            && output.starts_with("exit_code:")
            && !output.starts_with("exit_code: 0\n")
        {
            return false;
        }
        let arguments = call
            .parsed_arguments()
            .map(|value| value.to_string())
            .unwrap_or_else(|_| call.arguments.clone());
        let mut hash = Sha256::new();
        for part in [call.name.as_str(), arguments.as_str(), output] {
            hash.update(part.as_bytes());
            hash.update([0]);
        }
        self.seen.insert(hash.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentConfig, AgentEvent, EventSink};
    use crate::goal::{ContinuationRung, GoalBudget, GoalContinuation};
    use crate::provider::{Provider, ProviderError};
    use crate::types::{Completion, Message, ToolDefinition};
    use std::sync::{Arc, Mutex};

    struct RepeatingProvider {
        request: std::sync::atomic::AtomicUsize,
        path: &'static str,
    }

    #[async_trait::async_trait]
    impl Provider for RepeatingProvider {
        async fn complete(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            let request = self
                .request
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Completion {
                reasoning: None,
                content: "Work remains outstanding".into(),
                tool_calls: if request.is_multiple_of(2) {
                    vec![ToolCall {
                        id: format!("attempt-{request}"),
                        name: "read_file".into(),
                        arguments: serde_json::json!({"path": self.path}).to_string(),
                    }]
                } else {
                    Vec::new()
                },
                finish_reason: Some("stop".into()),
                usage: None,
            })
        }
    }

    #[derive(Default)]
    struct ProgressEvents(Mutex<Vec<AgentEvent>>);

    #[async_trait::async_trait]
    impl EventSink for ProgressEvents {
        async fn emit(&self, event: AgentEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn actual_goal_loop_escalates_repeated_errors_and_unchanged_successes() {
        for (path, is_error, reconcile_index) in
            [("missing.txt", true, 2), ("stable.txt", false, 3)]
        {
            let root = std::env::temp_dir().join(format!("progress-loop-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("stable.txt"), "unchanged evidence").unwrap();
            let goal = Arc::new(GoalContinuation::new());
            goal.activate(
                "Complete the outstanding task",
                GoalBudget {
                    wall_clock: None,
                    max_continuations: 20,
                },
            );
            let events = Arc::new(ProgressEvents::default());
            let provider = Arc::new(RepeatingProvider {
                request: 0.into(),
                path,
            });
            let agent = Agent::new(
                provider,
                crate::ToolRegistry::new(&root, crate::ApprovalMode::ReadOnly).unwrap(),
                AgentConfig {
                    max_turns: 12,
                    system_prompt: "test".into(),
                    context_window: 128_000,
                    token_budget: None,
                },
            )
            .with_goal_continuation(goal)
            .with_event_sink(events.clone());
            let outcome = agent.run("Continue working").await.unwrap();
            assert!(!outcome.stop_reason.is_complete());
            let events = events.0.lock().unwrap();
            let calls: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    if let AgentEvent::ToolCompleted { is_error, .. } = event {
                        Some(*is_error)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                calls,
                vec![is_error; 6],
                "actual tool executions must match the scenario"
            );
            let rungs: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    if let AgentEvent::GoalContinuationInjected { rung } = event {
                        Some(*rung)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(rungs.len(), 6);
            assert_eq!(rungs[0], ContinuationRung::Guidance);
            assert_eq!(rungs[reconcile_index], ContinuationRung::Reconcile);
            assert_eq!(rungs[5], ContinuationRung::Backoff);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn errors_and_repeated_successes_do_not_count_as_progress() {
        let mut tracker = ProgressTracker::default();
        let call = ToolCall {
            id: "first".to_owned(),
            name: "read_file".to_owned(),
            arguments: r#"{"path":"a"}"#.to_owned(),
        };
        assert!(!tracker.observe(&call, "not found", true));
        assert!(tracker.observe(&call, "content", false));
        assert!(!tracker.observe(
            &ToolCall {
                id: "second".to_owned(),
                ..call.clone()
            },
            "content",
            false
        ));
        assert!(tracker.observe(&call, "changed content", false));
        let command = ToolCall {
            name: "run_command".to_owned(),
            ..call
        };
        assert!(!tracker.observe(&command, "exit_code: 1\nstdout:\n", false));
        assert!(tracker.observe(&command, "exit_code: 0\nstdout:\n", false));
    }
}
