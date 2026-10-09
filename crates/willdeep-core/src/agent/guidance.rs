use crate::types::ToolCall;

const STAGNANT_READ_ROUNDS: usize = 3;
const EXPLORATION_CHECKPOINT_ROUNDS: usize = 6;

#[derive(Default)]
pub(super) struct ExplorationGuidance {
    read_rounds: usize,
    stagnant_rounds: usize,
}

impl ExplorationGuidance {
    pub fn finish_round(&mut self, all_reads: bool, novel_read: bool) -> Option<&'static str> {
        if !all_reads {
            self.read_rounds = 0;
            self.stagnant_rounds = 0;
            return None;
        }
        self.read_rounds = self.read_rounds.saturating_add(1);
        self.stagnant_rounds = if novel_read {
            0
        } else {
            self.stagnant_rounds.saturating_add(1)
        };
        if self.stagnant_rounds >= STAGNANT_READ_ROUNDS {
            self.read_rounds = 0;
            self.stagnant_rounds = 0;
            return Some(
                "[exploration-checkpoint] Recent read-only rounds produced no new successful tool evidence. Summarize established facts and choose the next bounded action or delegation. If more exploration is necessary, identify the unresolved question and a targeted check instead of repeating unchanged reads. Do not force a write, bypass permissions, or claim completion without verification.",
            );
        }
        if self.read_rounds >= EXPLORATION_CHECKPOINT_ROUNDS {
            self.read_rounds = 0;
            return Some(
                "[exploration-checkpoint] Several consecutive rounds have only inspected the workspace. This does not prove a stall: new evidence may still be necessary. State what remains unknown; if the goal and affected files are bounded, proceed with the change or delegate now. Otherwise choose a targeted check that resolves the unknown. Preserve time for implementation and verification; honour read-only tasks and all write constraints.",
            );
        }
        None
    }
}

pub(super) fn is_exploration_read(call: &ToolCall) -> bool {
    matches!(
        call.name.as_str(),
        "read_file"
            | "grep_files"
            | "search_files"
            | "list_directory"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "git_blame"
    )
}

pub(super) fn budget_context(
    used: u64,
    budget: Option<u64>,
    turn: usize,
    max_turns: usize,
) -> String {
    let mut context = format!(
        "[execution-budget] Current provider round: {turn}/{max_turns}. Remaining rounds including this one: {}.",
        max_turns.saturating_sub(turn).saturating_add(1)
    );
    let token_pressure = if let Some(budget) = budget {
        context.push_str(&format!(
            " This Agent run's accounted provider/compression tokens: {used}/{budget}; remaining: {}. Separate child and auxiliary calls are not included in this counter; their limits may stop execution earlier.",
            budget.saturating_sub(used)
        ));
        u128::from(used) * 4 >= u128::from(budget) * 3
    } else {
        false
    };
    let turn_pressure = max_turns > 0 && (turn as u128) * 4 >= (max_turns as u128) * 3;
    if token_pressure || turn_pressure {
        context.push_str(" Budget is approaching its limit. Prioritize a bounded implementation or timely delegation and leave room for integration and verification. Avoid broad rediscovery. If the task is read-only, focus on the remaining evidence and answer. This is an advisory, not permission to weaken tests, skip verification, or exceed any limit.");
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentConfig};
    use crate::provider::{Provider, ProviderError};
    use crate::types::{Completion, Message, ToolDefinition, Usage};
    use std::sync::{Arc, Mutex};

    #[test]
    fn new_evidence_resets_stagnation_but_still_reaches_a_checkpoint() {
        let mut guidance = ExplorationGuidance::default();
        assert!(guidance.finish_round(true, false).is_none());
        assert!(guidance.finish_round(true, false).is_none());
        assert!(guidance.finish_round(true, true).is_none());
        assert!(guidance.finish_round(true, true).is_none());
        assert!(guidance.finish_round(true, true).is_none());
        let checkpoint = guidance.finish_round(true, true).unwrap();
        assert!(checkpoint.contains("does not prove a stall"));
    }

    #[test]
    fn action_resets_exploration_and_notices_are_not_emitted_every_round() {
        let mut guidance = ExplorationGuidance::default();
        for _ in 0..2 {
            assert!(guidance.finish_round(true, false).is_none());
        }
        assert!(guidance.finish_round(false, false).is_none());
        for _ in 0..2 {
            assert!(guidance.finish_round(true, false).is_none());
        }
        assert!(guidance.finish_round(true, false).is_some());
        assert!(guidance.finish_round(true, false).is_none());
    }

    #[test]
    fn counters_do_not_overflow_and_unknown_token_limits_are_not_invented() {
        let context = budget_context(u64::MAX, Some(u64::MAX), usize::MAX, usize::MAX);
        assert!(context.contains("remaining: 0"));
        assert!(context.contains("approaching its limit"));
        assert!(!budget_context(0, None, 1, 60).contains("tokens:"));
        assert!(!budget_context(749, Some(1000), 1, 60).contains("approaching its limit"));
        assert!(budget_context(750, Some(1000), 1, 60).contains("approaching its limit"));
    }

    #[derive(Default)]
    struct InspectingProvider {
        requests: Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl Provider for InspectingProvider {
        async fn complete(
            &self,
            messages: &[Message],
            _: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(messages.to_vec());
            let round = requests.len();
            Ok(Completion {
                content: if round <= 4 {
                    "Inspecting the file"
                } else {
                    "Read-only investigation complete"
                }
                .into(),
                reasoning: None,
                tool_calls: if round <= 4 {
                    vec![ToolCall {
                        id: format!("read-{round}"),
                        name: "read_file".into(),
                        arguments: r#"{"path":"stable.txt"}"#.into(),
                    }]
                } else {
                    Vec::new()
                },
                finish_reason: Some("stop".into()),
                usage: Some(Usage {
                    input_tokens: Some(180),
                    output_tokens: Some(20),
                    total_tokens: Some(200),
                    ..Default::default()
                }),
            })
        }
    }

    #[tokio::test]
    async fn normal_loop_delivers_guidance_after_complete_tool_batches_without_extra_requests() {
        let root =
            std::env::temp_dir().join(format!("exploration-guidance-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("stable.txt"), "unchanged").unwrap();
        let provider = Arc::new(InspectingProvider::default());
        let agent = Agent::new(
            provider.clone(),
            crate::ToolRegistry::new(&root, crate::ApprovalMode::ReadOnly).unwrap(),
            AgentConfig {
                max_turns: 8,
                context_window: 128_000,
                token_budget: Some(1100),
                system_prompt: "Read-only investigation".into(),
            },
        );
        let outcome = agent
            .run("Inspect the file without changing it")
            .await
            .unwrap();
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(outcome.input_tokens + outcome.output_tokens, 1000);
        assert!(requests[..4].iter().all(|r| {
            !r.iter()
                .any(|m| m.content.starts_with("[exploration-checkpoint]"))
        }));
        let final_request = &requests[4];
        let checkpoint = final_request
            .iter()
            .position(|m| m.content.starts_with("[exploration-checkpoint]"))
            .unwrap();
        assert_eq!(
            final_request[checkpoint - 1].tool_call_id.as_deref(),
            Some("read-4")
        );
        assert!(
            final_request[0]
                .content
                .contains("tokens: 800/1100; remaining: 300")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("stable.txt")).unwrap(),
            "unchanged"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
