use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::background::BackgroundTaskRegistry;
use crate::goal::{
    ContinuationDecision, ContinuationRung, GoalContinuation, RoundObservation, SoftStopReason,
};
use crate::provider::{Provider, ProviderError};
use crate::subagent::{SpawnAgentArgs, SubagentCatalog};
use crate::tools::{ToolError, ToolRegistry};
use crate::types::{Message, ToolCall, Usage};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub system_prompt: String,
    pub context_window: u64,
    pub token_budget: Option<u64>,
}

#[derive(Default)]
pub struct AgentInstructionInbox {
    pending: Mutex<VecDeque<String>>,
}

impl AgentInstructionInbox {
    pub fn push(&self, instruction: String) -> bool {
        let instruction = instruction.trim();
        if instruction.is_empty() || instruction.len() > 16 * 1024 {
            return false;
        }
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        pending.push_back(instruction.to_owned());
        true
    }

    fn drain(&self) -> Vec<String> {
        self.pending
            .lock()
            .map(|mut pending| pending.drain(..).collect())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    AssistantText(String),
    ToolRequested(ToolCall),
    ToolCompleted {
        call: ToolCall,
        output: String,
        is_error: bool,
    },
    Usage(Usage),
    CompressionStarted {
        estimated_tokens: u64,
    },
    CompressionCompleted {
        estimated_tokens: u64,
    },
    BackgroundShellStarted {
        id: String,
    },
    BackgroundShellCompleted {
        id: String,
        status: crate::BackgroundTaskStatus,
        exit_code: Option<i32>,
        elapsed_millis: u64,
        output_bytes: usize,
    },
    SubagentStarted {
        id: uuid::Uuid,
        profile: String,
        model: Option<String>,
        label: String,
        background: bool,
        max_turns: usize,
        token_budget: Option<u64>,
        timeout_seconds: Option<u64>,
        workspace: std::path::PathBuf,
        root_workspace: std::path::PathBuf,
        worktree_branch: Option<String>,
        dedicated_worktree: bool,
    },
    SubagentCompleted {
        id: uuid::Uuid,
        status: SubagentLifecycleStatus,
        report: Option<String>,
    },
    SubagentTurnStarted {
        id: uuid::Uuid,
        turn: usize,
    },
    SubagentToolRequested {
        id: uuid::Uuid,
        name: String,
    },
    SubagentToolCompleted {
        id: uuid::Uuid,
        name: String,
        is_error: bool,
    },
    SubagentUsage {
        id: uuid::Uuid,
        usage: Usage,
    },
    /// 目标未达，宿主拒绝了一次隐式收口并注入续推引导。
    GoalContinuationInjected {
        rung: ContinuationRung,
    },
    /// 预算耗尽，转入有序收尾。
    GoalBudgetLimited {
        reason: SoftStopReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentLifecycleStatus {
    Completed,
    Blocked,
    Cancelled,
    Failed,
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

struct NoopSink;

#[async_trait]
impl EventSink for NoopSink {
    async fn emit(&self, _event: AgentEvent) {}
}

/// 一次 run 为什么停下来。长程模式下「停下来」有多种含义，调用方需要能区分。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentStopReason {
    /// 模型给出终稿且没有激活的目标——原有语义。
    #[default]
    Finished,
    /// 目标激活期间，模型显式声明目标达成。
    GoalComplete,
    /// 目标未达但预算耗尽，已按收尾引导产出交接快照。
    BudgetLimited,
}

#[derive(Debug)]
pub struct AgentOutcome {
    pub final_text: String,
    pub turns: usize,
    pub messages: Vec<Message>,
    pub stop_reason: AgentStopReason,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("provider returned neither text nor tool calls")]
    EmptyResponse,
    #[error("agent reached the maximum of {0} turns before producing a final answer")]
    MaxTurns(usize),
    #[error("agent exhausted its token budget of {budget} tokens (used {used})")]
    TokenBudgetExceeded { budget: u64, used: u64 },
    #[error("subagent failed: {0}")]
    Subagent(String),
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    config: AgentConfig,
    sink: Arc<dyn EventSink>,
    image_fallback: Option<(Arc<dyn Provider>, String)>,
    subagents: Option<Arc<SubagentCatalog>>,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
    goal_continuation: Option<Arc<GoalContinuation>>,
    background_tasks: Option<Arc<BackgroundTaskRegistry>>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, tools: ToolRegistry, config: AgentConfig) -> Self {
        Self {
            provider,
            tools,
            config,
            sink: Arc::new(NoopSink),
            image_fallback: None,
            subagents: None,
            instruction_inbox: None,
            goal_continuation: None,
            background_tasks: None,
        }
    }

    pub fn with_image_fallback(
        mut self,
        provider: Arc<dyn Provider>,
        label: impl Into<String>,
    ) -> Self {
        self.image_fallback = Some((provider, label.into()));
        self
    }

    pub fn with_subagents(mut self, catalog: Arc<SubagentCatalog>) -> Self {
        self.subagents = Some(catalog);
        self
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn with_instruction_inbox(mut self, inbox: Arc<AgentInstructionInbox>) -> Self {
        self.instruction_inbox = Some(inbox);
        self
    }

    /// 挂上跨 turn 共享的 Goal 续推句柄（long-horizon.v1 RA1）。
    ///
    /// 不挂等于关闭长程续推，`run_*` 的行为与本改动前完全一致。
    pub fn with_goal_continuation(mut self, continuation: Arc<GoalContinuation>) -> Self {
        self.goal_continuation = Some(continuation);
        self
    }

    /// 前端据此在 `/goal` 变更时同步激活状态，无需额外穿参。
    pub fn goal_continuation(&self) -> Option<&Arc<GoalContinuation>> {
        self.goal_continuation.as_ref()
    }

    /// 让续推判定能看见后台任务：仍有后台任务在跑时，「本轮没调工具」不算卡死。
    pub fn with_background_tasks(mut self, tasks: Arc<BackgroundTaskRegistry>) -> Self {
        self.background_tasks = Some(tasks);
        self
    }

    fn background_active(&self) -> bool {
        self.background_tasks
            .as_ref()
            .map(|tasks| {
                tasks
                    .snapshots()
                    .iter()
                    .any(|task| task.status == crate::BackgroundTaskStatus::Running)
            })
            .unwrap_or(false)
    }

    pub async fn run(&self, prompt: impl Into<String>) -> Result<AgentOutcome, AgentError> {
        self.run_with_history(Vec::new(), prompt).await
    }

    pub async fn run_with_history(
        &self,
        messages: Vec<Message>,
        prompt: impl Into<String>,
    ) -> Result<AgentOutcome, AgentError> {
        self.run_with_history_message(messages, Message::user(prompt))
            .await
    }

    pub async fn run_with_history_message(
        &self,
        mut messages: Vec<Message>,
        mut user_message: Message,
    ) -> Result<AgentOutcome, AgentError> {
        if let Some((provider, label)) = &self.image_fallback {
            let image_count = user_message
                .attachments
                .iter()
                .filter(|value| matches!(value, crate::types::MessageAttachment::Image { .. }))
                .count();
            if image_count > 0 {
                let vision_prompt = Message::user_with_attachments(
                    "Describe each attached image for a coding agent that cannot see images directly. Include visible UI text, errors, file names, code, controls, and layout.",
                    user_message.attachments.clone(),
                );
                let description = provider.complete(&[vision_prompt], &[]).await?.content;
                user_message.content.push_str(&format!("\n\n[Image description generated by {label} for {image_count} attached image(s)]\n{description}"));
                user_message.attachments.retain(|value| {
                    !matches!(value, crate::types::MessageAttachment::Image { .. })
                });
            }
        }
        messages.retain(|message| message.role != crate::types::Role::System);
        messages.insert(0, Message::system(&self.config.system_prompt));
        // The approval judge reads this as inert context: it decides whether
        // a bounded action is relevant to the current goal, never whether a
        // destructive one is permitted.
        self.tools.set_task_context(&user_message.content);
        messages.push(user_message);
        let definitions = self.tools.definitions();
        let mut compressed: Option<(usize, String)> = None;
        let mut used_tokens = 0_u64;
        // 自上次续推判定以来成功发起的工具调用数——续推判定的「进展证据」。
        let mut tools_since_check = 0_usize;
        for turn in 1..=self.config.max_turns {
            self.append_pending_instructions(&mut messages);
            self.sink.emit(AgentEvent::TurnStarted { turn }).await;
            let request_messages = self.request_messages(&messages, &mut compressed).await?;
            let completion = self
                .provider
                .complete(&request_messages, &definitions)
                .await?;
            if let Some(usage) = completion.usage {
                used_tokens = used_tokens.saturating_add(usage.total_tokens.unwrap_or_else(|| {
                    usage
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.output_tokens.unwrap_or(0))
                }));
                self.sink.emit(AgentEvent::Usage(usage)).await;
                if let Some(budget) = self.config.token_budget
                    && used_tokens >= budget
                {
                    return Err(AgentError::TokenBudgetExceeded {
                        budget,
                        used: used_tokens,
                    });
                }
            }
            let content = completion.content.trim().to_owned();
            if !content.is_empty() {
                self.sink
                    .emit(AgentEvent::AssistantText(content.clone()))
                    .await;
            }
            if completion.tool_calls.is_empty() {
                if content.is_empty() {
                    return Err(AgentError::EmptyResponse);
                }
                messages.push(Message::assistant(&content, Vec::new()));
                if self.append_pending_instructions(&mut messages) {
                    continue;
                }
                // 长程续推：目标未达且预算未尽时，这里不是终点。
                if let Some(continuation) = self.goal_continuation.clone() {
                    let observation = RoundObservation {
                        tools_executed: tools_since_check,
                        background_active: self.background_active(),
                    };
                    let was_wrapping_up = continuation.wrap_up_pending();
                    match continuation.evaluate(&content, observation) {
                        Some(ContinuationDecision::Continue { steering, rung }) => {
                            self.sink
                                .emit(AgentEvent::GoalContinuationInjected { rung })
                                .await;
                            messages.push(Message::user(steering));
                            tools_since_check = 0;
                            continue;
                        }
                        Some(ContinuationDecision::SoftStop { steering, reason }) => {
                            self.sink
                                .emit(AgentEvent::GoalBudgetLimited { reason })
                                .await;
                            messages.push(Message::user(steering));
                            tools_since_check = 0;
                            continue;
                        }
                        Some(ContinuationDecision::Complete) => {
                            return Ok(AgentOutcome {
                                final_text: content,
                                turns: turn,
                                messages,
                                stop_reason: if was_wrapping_up {
                                    AgentStopReason::BudgetLimited
                                } else {
                                    AgentStopReason::GoalComplete
                                },
                            });
                        }
                        None => {}
                    }
                }
                return Ok(AgentOutcome {
                    final_text: content,
                    turns: turn,
                    messages,
                    stop_reason: AgentStopReason::Finished,
                });
            }
            tools_since_check = tools_since_check.saturating_add(completion.tool_calls.len());
            messages.push(Message::assistant(content, completion.tool_calls.clone()));
            for call in completion.tool_calls {
                self.sink
                    .emit(AgentEvent::ToolRequested(call.clone()))
                    .await;
                let result = self.execute_tool(&call).await;
                let (output, is_error) = match result {
                    Ok(output) => (output, false),
                    Err(error) => (format!("tool error: {error}"), true),
                };
                self.sink
                    .emit(AgentEvent::ToolCompleted {
                        call: call.clone(),
                        output: output.clone(),
                        is_error,
                    })
                    .await;
                messages.push(Message::tool(&call, output));
            }
        }
        Err(AgentError::MaxTurns(self.config.max_turns))
    }

    fn append_pending_instructions(&self, messages: &mut Vec<Message>) -> bool {
        let instructions = self
            .instruction_inbox
            .as_ref()
            .map(|inbox| inbox.drain())
            .unwrap_or_default();
        if instructions.is_empty() {
            return false;
        }
        messages.push(Message::user(format!(
            "Additional instructions from the parent Agent:\n\n{}",
            instructions.join("\n\n")
        )));
        true
    }

    fn execute_tool<'a>(
        &'a self,
        call: &'a ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ToolError>> + Send + 'a>>
    {
        Box::pin(async move {
            if call.name != "spawn_agent" {
                return self.tools.execute(call).await;
            }
            let catalog = self
                .subagents
                .as_ref()
                .ok_or_else(|| ToolError::UnknownTool(call.name.clone()))?;
            let args: SpawnAgentArgs =
                serde_json::from_value(call.parsed_arguments().map_err(|source| {
                    ToolError::InvalidArguments {
                        tool: call.name.clone(),
                        source,
                    }
                })?)
                .map_err(|source| ToolError::InvalidArguments {
                    tool: call.name.clone(),
                    source,
                })?;
            let approved_target = if catalog.needs_write_approval(args.profile.as_deref()) {
                let requested = args.target_file.as_deref().ok_or_else(|| {
                    ToolError::OutsideWorkspace("editor profile requires target_file".to_owned())
                })?;
                Some(self.tools.approve_subagent_editor(requested).await?)
            } else {
                None
            };
            catalog
                .run(args, approved_target)
                .await
                .map_err(|error| ToolError::Network(error.to_string()))
        })
    }

    pub async fn compress_history(
        &self,
        messages: Vec<Message>,
    ) -> Result<Vec<Message>, AgentError> {
        let mut history = messages
            .into_iter()
            .filter(|message| message.role != crate::types::Role::System)
            .collect::<Vec<_>>();
        if history.len() < 8 {
            return Ok(history);
        }
        let split = history.len().saturating_sub(6);
        let estimated = estimate_tokens(&history);
        self.sink
            .emit(AgentEvent::CompressionStarted {
                estimated_tokens: estimated,
            })
            .await;
        let source = history[..split]
            .iter()
            .map(|message| format!("{:?}: {}", message.role, message.content))
            .collect::<Vec<_>>()
            .join("\n");
        let request = Message::user(format!(
            "Summarize this older coding-agent conversation compactly. Preserve decisions, constraints, changed files, commands, failures, unresolved work, and exact identifiers.\n\n{source}"
        ));
        let summary = self.provider.complete(&[request], &[]).await?.content;
        let recent = history.split_off(split);
        let mut compressed = vec![Message::user(format!(
            "<context-summary>\n{summary}\n</context-summary>"
        ))];
        compressed.extend(recent);
        self.sink
            .emit(AgentEvent::CompressionCompleted {
                estimated_tokens: estimate_tokens(&compressed),
            })
            .await;
        Ok(compressed)
    }

    async fn request_messages(
        &self,
        messages: &[Message],
        cache: &mut Option<(usize, String)>,
    ) -> Result<Vec<Message>, AgentError> {
        let estimated = estimate_tokens(messages);
        if estimated < self.config.context_window.saturating_mul(80) / 100 || messages.len() < 16 {
            return Ok(messages.to_vec());
        }
        let mut split = messages.len().saturating_sub(10);
        while split < messages.len() && messages[split].role != crate::types::Role::User {
            split += 1;
        }
        if split <= 1 || split >= messages.len() {
            return Ok(messages.to_vec());
        }
        if cache.as_ref().is_none_or(|(through, _)| *through != split) {
            self.sink
                .emit(AgentEvent::CompressionStarted {
                    estimated_tokens: estimated,
                })
                .await;
            let source = messages[1..split]
                .iter()
                .map(|message| format!("{:?}: {}", message.role, message.content))
                .collect::<Vec<_>>()
                .join("\n");
            let request = Message::user(format!(
                "Summarize this older coding-agent conversation compactly. Preserve decisions, constraints, changed files, commands, failures, unresolved work, and exact identifiers.\n\n{source}"
            ));
            let summary = self.provider.complete(&[request], &[]).await?.content;
            *cache = Some((split, summary));
        }
        let mut result = vec![messages[0].clone()];
        result.push(Message::user(format!(
            "<context-summary>\n{}\n</context-summary>",
            cache.as_ref().unwrap().1
        )));
        result.extend_from_slice(&messages[split..]);
        self.sink
            .emit(AgentEvent::CompressionCompleted {
                estimated_tokens: estimate_tokens(&result),
            })
            .await;
        Ok(result)
    }
}

fn estimate_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|message| {
            message.content.chars().count() as u64 / 4
                + 8
                + message
                    .attachments
                    .iter()
                    .filter(|value| matches!(value, crate::types::MessageAttachment::Image { .. }))
                    .count() as u64
                    * 1_024
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::provider::Provider;
    use crate::tools::ApprovalMode;
    use crate::types::{Completion, MessageAttachment, ToolDefinition};

    struct RecordingProvider {
        replies: Mutex<VecDeque<String>>,
        requests: Mutex<Vec<Vec<Message>>>,
    }

    struct UsageProvider;

    struct InstructionProvider {
        calls: std::sync::atomic::AtomicUsize,
        inbox: Arc<AgentInstructionInbox>,
    }

    #[async_trait]
    impl Provider for InstructionProvider {
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                assert!(self.inbox.push("also inspect tests".to_owned()));
            } else {
                assert!(messages.iter().any(|message| {
                    message.content.contains("also inspect tests")
                        && message.role == crate::types::Role::User
                }));
            }
            Ok(Completion {
                content: if call == 0 {
                    "first answer"
                } else {
                    "revised answer"
                }
                .to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: None,
            })
        }
    }

    #[async_trait]
    impl Provider for UsageProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            Ok(Completion {
                content: "would otherwise finish".to_owned(),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: Some(crate::types::Usage {
                    input_tokens: Some(800),
                    output_tokens: Some(300),
                    total_tokens: Some(1_100),
                }),
            })
        }
    }

    impl RecordingProvider {
        fn new(replies: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.iter().map(|value| (*value).to_owned()).collect()),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            self.requests
                .lock()
                .expect("requests")
                .push(messages.to_vec());
            Ok(Completion {
                content: self
                    .replies
                    .lock()
                    .expect("replies")
                    .pop_front()
                    .expect("reply"),
                tool_calls: Vec::new(),
                finish_reason: Some("stop".to_owned()),
                usage: None,
            })
        }
    }

    fn registry(name: &str) -> ToolRegistry {
        let root =
            std::env::temp_dir().join(format!("willdeep-agent-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        ToolRegistry::new(root, ApprovalMode::Strict).expect("registry")
    }

    #[tokio::test]
    async fn stops_before_returning_when_token_budget_is_exhausted() {
        let agent = Agent::new(
            Arc::new(UsageProvider),
            registry("token-budget"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: Some(1_000),
            },
        );

        let error = agent.run("work").await.expect_err("budget must stop run");
        assert!(matches!(
            error,
            AgentError::TokenBudgetExceeded {
                budget: 1_000,
                used: 1_100
            }
        ));
    }

    #[tokio::test]
    async fn parent_instruction_prevents_early_finish_and_continues_next_turn() {
        let inbox = Arc::new(AgentInstructionInbox::default());
        let provider = Arc::new(InstructionProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
            inbox: inbox.clone(),
        });
        let agent = Agent::new(
            provider.clone(),
            registry("instructions"),
            AgentConfig {
                max_turns: 3,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_instruction_inbox(inbox);

        let outcome = agent.run("inspect source").await.expect("continued run");
        assert_eq!(outcome.final_text, "revised answer");
        assert_eq!(outcome.turns, 2);
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    fn goal_agent(provider: Arc<RecordingProvider>, budget: crate::goal::GoalBudget) -> Agent {
        let continuation = Arc::new(GoalContinuation::new());
        continuation.activate("ship rc7", budget);
        Agent::new(
            provider,
            registry("goal"),
            AgentConfig {
                max_turns: 12,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_goal_continuation(continuation)
    }

    #[tokio::test]
    async fn without_a_goal_a_plain_reply_still_finishes_immediately() {
        let provider = RecordingProvider::new(&["done", "should never be requested"]);
        let agent = Agent::new(
            provider.clone(),
            registry("no-goal"),
            AgentConfig {
                max_turns: 4,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: None,
            },
        );

        let outcome = agent.run("do the thing").await.expect("run");

        assert_eq!(outcome.final_text, "done");
        assert_eq!(outcome.stop_reason, AgentStopReason::Finished);
        assert_eq!(provider.requests.lock().expect("requests").len(), 1);
    }

    #[tokio::test]
    async fn active_goal_refuses_implicit_stop_until_the_marker_appears() {
        let provider = RecordingProvider::new(&[
            "I finished the first part.",
            "Here is a summary of what I did.",
            "<goal-status>complete</goal-status> rc7 shipped and verified.",
        ]);
        let agent = goal_agent(provider.clone(), crate::goal::GoalBudget::default());

        let outcome = agent.run("ship it").await.expect("run");

        assert_eq!(outcome.stop_reason, AgentStopReason::GoalComplete);
        assert!(outcome.final_text.contains("rc7 shipped"));
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(
            requests.len(),
            3,
            "harness should refuse the first two stops"
        );
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("[goal-continuation]")),
            "the second request must carry the injected steering"
        );
    }

    #[tokio::test]
    async fn exhausted_budget_wraps_up_instead_of_looping_forever() {
        let provider = RecordingProvider::new(&[
            "still working",
            "another round without finishing",
            "STATE: branch feat/x · REMAINING: finish tests · BLOCKERS: none",
            "should never be requested",
        ]);
        let agent = goal_agent(
            provider.clone(),
            crate::goal::GoalBudget {
                wall_clock: None,
                max_continuations: 1,
            },
        );

        let outcome = agent.run("ship it").await.expect("run");

        // 一次续推 → 预算耗尽转收尾 → 收尾快照单独占一轮，然后才停。
        assert_eq!(outcome.stop_reason, AgentStopReason::BudgetLimited);
        assert!(outcome.final_text.contains("REMAINING"));
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 3, "budget must not silently loop forever");
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("[goal-continuation]")),
            "the first refusal is a normal continuation"
        );
        assert!(
            requests[2]
                .iter()
                .any(|message| message.content.contains("[goal-budget-limited]")),
            "the wrap-up turn must carry the handover steering"
        );
    }

    #[tokio::test]
    async fn vision_fallback_sends_image_only_to_vision_provider() {
        let main = RecordingProvider::new(&["done"]);
        let vision = RecordingProvider::new(&["a terminal showing an error"]);
        let agent = Agent::new(
            main.clone(),
            registry("vision"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_image_fallback(vision.clone(), "some.im / qwen3-vl-plus");
        let message = Message::user_with_attachments(
            "fix this",
            vec![MessageAttachment::Image {
                name: "shot.png".to_owned(),
                media_type: "image/png".to_owned(),
                data: "AA==".to_owned(),
                width: 1,
                height: 1,
            }],
        );

        let outcome = agent
            .run_with_history_message(Vec::new(), message)
            .await
            .expect("run");
        let vision_requests = vision.requests.lock().expect("vision requests");
        assert_eq!(vision_requests[0][0].attachments.len(), 1);
        let main_requests = main.requests.lock().expect("main requests");
        let user = main_requests[0]
            .iter()
            .find(|message| message.role == crate::types::Role::User)
            .expect("user");
        assert!(user.attachments.is_empty());
        assert!(user.content.contains("a terminal showing an error"));
        assert!(
            outcome
                .messages
                .iter()
                .any(|message| message.content.contains("qwen3-vl-plus"))
        );
    }

    #[tokio::test]
    async fn compression_uses_temporary_summary_but_preserves_history() {
        let provider = RecordingProvider::new(&["compact summary", "final"]);
        let agent = Agent::new(
            provider.clone(),
            registry("compression"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: 200,
                token_budget: None,
            },
        );
        let history = (0..18)
            .map(|index| {
                if index % 2 == 0 {
                    Message::user(format!("older user message {index} with enough detail"))
                } else {
                    Message::assistant(
                        format!("older assistant message {index} with enough detail"),
                        Vec::new(),
                    )
                }
            })
            .collect::<Vec<_>>();

        let outcome = agent
            .run_with_history(history, "continue")
            .await
            .expect("run");
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("<context-summary>"))
        );
        assert!(outcome.messages.len() >= 21);
    }

    #[tokio::test]
    async fn manual_compression_replaces_old_history_with_summary() {
        let provider = RecordingProvider::new(&["manual summary"]);
        let agent = Agent::new(
            provider,
            registry("manual-compression"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: 128_000,
                token_budget: None,
            },
        );
        let history = (0..12)
            .map(|index| Message::user(format!("message {index}")))
            .collect::<Vec<_>>();

        let compressed = agent.compress_history(history).await.expect("compress");

        assert_eq!(compressed.len(), 7);
        assert!(compressed[0].content.contains("manual summary"));
        assert_eq!(compressed.last().expect("last").content, "message 11");
    }
}
