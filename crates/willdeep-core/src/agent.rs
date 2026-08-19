use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;

use crate::background::BackgroundTaskRegistry;
use crate::goal::{
    ContinuationDecision, ContinuationRung, GoalContinuation, RoundObservation, SoftStopReason,
};
use crate::provider::{Provider, ProviderError};
use crate::subagent::{SpawnAgentArgs, SubagentCatalog};
use crate::tools::{ToolError, ToolRegistry};
use crate::types::{Message, ToolCall, Usage};

/// 在途自动压缩的触发水位：请求估算达到窗口的这个百分比即开始摘要旧历史。
const AUTO_COMPRESSION_TRIGGER_PERCENT: u64 = 75;
/// 逃生水位。越过它说明下一次请求随时可能被 Provider 拒收，此时无视
/// `AUTO_COMPRESSION_MIN_MESSAGES`，哪怕只有几条消息也要压——少数几条
/// 巨型工具输出就能撑爆窗口，而它们恰恰凑不够常规条数门槛。
const AUTO_COMPRESSION_ESCAPE_PERCENT: u64 = 90;
/// 摘要之后仍越过这条天花板，就从保留区头部继续丢，直到降下来。
const AUTO_COMPRESSION_CEILING_PERCENT: u64 = 95;
/// 单条消息允许占用的窗口比例。超过就地裁掉中段——超大消息通常是刚读进来
/// 的文件或工具输出，正躺在摘要够不着的保留区里，只摘要旧历史治不了它。
const OVERSIZED_MESSAGE_PERCENT: u64 = 25;
/// 自动压缩保留在摘要之后的最近消息条数。
const AUTO_COMPRESSION_KEEP_RECENT: usize = 10;
/// 自动压缩要求的最小消息条数。低于该值时，可摘要区不足 5 条，
/// 摘要省下的 token 抵不过一次 Provider 调用。
const AUTO_COMPRESSION_MIN_MESSAGES: usize = 16;
/// 兜底丢弃时必须保住的尾部消息条数：再挤也要留下最近一轮问答。
const AUTO_COMPRESSION_MIN_TAIL: usize = 2;
/// 裁剪超大消息时保留在头部的比例，其余额度留给尾部——报错和断言通常在末尾。
const OVERSIZED_HEAD_PERCENT: usize = 60;
/// token 粗估用的字符密度。真实分词器另说，这里只需要一个稳定的保守刻度。
const CHARS_PER_TOKEN: u64 = 4;

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
        /// 摘要之后仍超窗时，从保留区头部丢掉的消息条数。丢弃只影响本次
        /// 请求视图，会话存档不受影响，但用户有权知道模型少看了几条。
        dropped_messages: usize,
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
    /// What a delegated run actually proved, emitted once per run whether it
    /// passed, failed or had no verifier at all.
    ///
    /// This is the only place `verified` is a fact rather than a claim: the
    /// verdict comes from the verifier's exit code, so a report that reads
    /// like success but never passed a check cannot be counted as one. With
    /// `repo_commit` for the tree the run started from, one record is a
    /// complete replay case — initial state, task, verdict.
    SubagentVerdict {
        id: uuid::Uuid,
        repo_commit: Option<String>,
        verifier_command: Option<String>,
        /// `None` when the run had no verifier: unverified, not failed.
        verifier_passed: Option<bool>,
        attempts: usize,
        /// Citations the runtime could check in a report-only run: file paths,
        /// line numbers and commit hashes the worker named. Zero means the
        /// report cited nothing checkable, which is not the same as a report
        /// whose every citation held up.
        claims_checked: usize,
        /// Cited locations that do not exist. A read-only trade has no exit
        /// code to judge it, and this is the one thing a program can still
        /// verify about its answer.
        claims_unverifiable: usize,
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
    provider: RwLock<Arc<dyn Provider>>,
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
            provider: RwLock::new(provider),
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

    /// Switch future completions to another model without rebuilding the
    /// Agent's tools, approvals, subagents, or event sinks.
    pub fn set_model(&self, model: &str) -> Result<(), ProviderError> {
        let configured = self
            .provider
            .read()
            .map_err(|_| ProviderError::InvalidResponse("provider lock poisoned".to_owned()))?
            .with_model(model)?;
        *self
            .provider
            .write()
            .map_err(|_| ProviderError::InvalidResponse("provider lock poisoned".to_owned()))? =
            configured;
        Ok(())
    }

    fn provider(&self) -> Result<Arc<dyn Provider>, ProviderError> {
        self.provider
            .read()
            .map(|provider| provider.clone())
            .map_err(|_| ProviderError::InvalidResponse("provider lock poisoned".to_owned()))
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
                .provider()?
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
            let scope = catalog.write_scope(args.profile.as_deref());
            let approved_targets = if scope.writes() {
                let requested = args.requested_write_targets(scope);
                if requested.is_empty() {
                    return Err(ToolError::OutsideWorkspace(
                        "a writing profile needs its files declared up front: target_file for editor, task.relevant_files for implementer, test_fixer or build_fixer".to_owned(),
                    ));
                }
                Some(self.tools.approve_subagent_write_set(&requested).await?)
            } else {
                None
            };
            catalog
                .run(args, approved_targets)
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
        let summary = self.provider()?.complete(&[request], &[]).await?.content;
        let recent = history.split_off(split);
        let mut compressed = vec![Message::user(format!(
            "<context-summary>\n{summary}\n</context-summary>"
        ))];
        compressed.extend(recent);
        self.sink
            .emit(AgentEvent::CompressionCompleted {
                estimated_tokens: estimate_tokens(&compressed),
                dropped_messages: 0,
            })
            .await;
        Ok(compressed)
    }

    async fn request_messages(
        &self,
        messages: &[Message],
        cache: &mut Option<(usize, String)>,
    ) -> Result<Vec<Message>, AgentError> {
        let window = self.config.context_window;
        // 先做不花模型钱的裁剪。裁完往往就落回水位以下，连摘要都省了。
        let clamped = clamp_oversized_messages(messages, window);
        let messages: &[Message] = clamped.as_deref().unwrap_or(messages);
        let estimated = estimate_tokens(messages);
        if estimated < window.saturating_mul(AUTO_COMPRESSION_TRIGGER_PERCENT) / 100 {
            return Ok(messages.to_vec());
        }
        let urgent = estimated >= window.saturating_mul(AUTO_COMPRESSION_ESCAPE_PERCENT) / 100;
        if !urgent && messages.len() < AUTO_COMPRESSION_MIN_MESSAGES {
            return Ok(messages.to_vec());
        }
        // 逃生状态下按历史长度收缩保留区，否则 `len - 10` 会退化成 0，
        // 切不出摘要区，压缩等于没发生。
        let keep = if urgent {
            AUTO_COMPRESSION_KEEP_RECENT
                .min(messages.len().saturating_sub(AUTO_COMPRESSION_MIN_TAIL + 1))
        } else {
            AUTO_COMPRESSION_KEEP_RECENT
        };
        let mut split = messages.len().saturating_sub(keep);
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
            let summary = self.provider()?.complete(&[request], &[]).await?.content;
            *cache = Some((split, summary));
        }
        let mut result = vec![messages[0].clone()];
        result.push(Message::user(format!(
            "<context-summary>\n{}\n</context-summary>",
            cache.as_ref().unwrap().1
        )));
        result.extend_from_slice(&messages[split..]);
        // 摘要只吃 `[1..split]`。保留区自己就超窗时，摘要救不了场，
        // 与其把一个必被拒收的请求发出去，不如从保留区头部继续丢。
        let dropped_messages = drop_until_under_ceiling(&mut result, window);
        self.sink
            .emit(AgentEvent::CompressionCompleted {
                estimated_tokens: estimate_tokens(&result),
                dropped_messages,
            })
            .await;
        Ok(result)
    }
}

/// 裁掉任何单条超过窗口 `OVERSIZED_MESSAGE_PERCENT` 的消息的中段，保留首尾。
/// 没有消息越界时返回 `None`，让调用方省掉一次整表克隆。
///
/// 纯字符串处理，不花 Provider 调用——这类消息几乎总是工具输出，
/// 摘要它们的成本比它们本身还贵。
fn clamp_oversized_messages(messages: &[Message], window: u64) -> Option<Vec<Message>> {
    let budget_chars = usize::try_from(
        window
            .saturating_mul(OVERSIZED_MESSAGE_PERCENT)
            .saturating_div(100)
            .saturating_mul(CHARS_PER_TOKEN),
    )
    .unwrap_or(usize::MAX);
    if budget_chars == 0 {
        return None;
    }
    if !messages
        .iter()
        .any(|message| message.content.chars().count() > budget_chars)
    {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|message| {
                let length = message.content.chars().count();
                if length <= budget_chars {
                    return message.clone();
                }
                let head = budget_chars * OVERSIZED_HEAD_PERCENT / 100;
                let tail = budget_chars.saturating_sub(head);
                let head_text: String = message.content.chars().take(head).collect();
                let tail_text: String = message.content.chars().skip(length - tail).collect();
                let elided = length - head - tail;
                let mut clamped = message.clone();
                clamped.content = format!(
                    "{head_text}\n… [{elided} chars elided by context compaction] …\n{tail_text}"
                );
                clamped
            })
            .collect(),
    )
}

/// 从保留区头部丢消息，直到估算降到窗口 `AUTO_COMPRESSION_CEILING_PERCENT`
/// 以下。首条消息与摘要永远保留，尾部至少留 `AUTO_COMPRESSION_MIN_TAIL` 条。
/// 返回实际丢弃的条数。
fn drop_until_under_ceiling(messages: &mut Vec<Message>, window: u64) -> usize {
    let ceiling = window.saturating_mul(AUTO_COMPRESSION_CEILING_PERCENT) / 100;
    // 索引 0 是首条消息，索引 1 是摘要；保留区从 2 开始。
    let floor = 2 + AUTO_COMPRESSION_MIN_TAIL;
    let mut dropped = 0;
    while messages.len() > floor && estimate_tokens(messages) > ceiling {
        messages.remove(2);
        dropped += 1;
    }
    dropped
}

fn estimate_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|message| {
            message.content.chars().count() as u64 / CHARS_PER_TOKEN
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
                    cache_read_tokens: None,
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

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEvent>>,
    }

    #[async_trait]
    impl EventSink for RecordingSink {
        async fn emit(&self, event: AgentEvent) {
            self.events.lock().expect("events").push(event);
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

    /// 锁定 75% 触发线：构造一段估算落在窗口 75%~80% 之间的历史，
    /// 它在旧的 80% 水位下不会压缩，在当前水位下必须压缩。
    #[tokio::test]
    async fn compression_triggers_at_seventy_five_percent_of_window() {
        let provider = RecordingProvider::new(&["compact summary", "final"]);
        let window = 1_000_u64;
        let agent = Agent::new(
            provider.clone(),
            registry("compression-threshold"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: window,
                token_budget: None,
            },
        );
        let history = (0..18)
            .map(|index| {
                let body = format!("{}{index:03}", "x".repeat(137));
                if index % 2 == 0 {
                    Message::user(body)
                } else {
                    Message::assistant(body, Vec::new())
                }
            })
            .collect::<Vec<_>>();

        let mut request_view = history.clone();
        request_view.push(Message::user("continue"));
        let estimated = estimate_tokens(&request_view);
        assert!(
            estimated >= window * 75 / 100 && estimated < window * 80 / 100,
            "fixture must sit between the old and new trigger, got {estimated}"
        );

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

    /// 少数几条巨型消息凑不够 16 条门槛，但已经贴着窗口。逃生水位必须让
    /// 压缩照常发生，否则这一轮请求直接被 Provider 拒收。
    #[tokio::test]
    async fn a_short_but_nearly_full_history_still_compresses() {
        let provider = RecordingProvider::new(&["compact summary", "final"]);
        let window = 1_000_u64;
        let agent = Agent::new(
            provider.clone(),
            registry("compression-escape"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: window,
                token_budget: None,
            },
        );
        let history = (0..8)
            .map(|index| {
                let body = format!("{}{index:03}", "y".repeat(417));
                if index % 2 == 0 {
                    Message::user(body)
                } else {
                    Message::assistant(body, Vec::new())
                }
            })
            .collect::<Vec<_>>();

        let mut request_view = history.clone();
        request_view.push(Message::user("continue"));
        assert!(
            request_view.len() < AUTO_COMPRESSION_MIN_MESSAGES,
            "fixture must stay under the regular message-count gate"
        );
        assert!(
            estimate_tokens(&request_view) >= window * AUTO_COMPRESSION_ESCAPE_PERCENT / 100,
            "fixture must sit above the escape watermark"
        );

        agent
            .run_with_history(history, "continue")
            .await
            .expect("run");
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 2, "the summary request must have happened");
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("<context-summary>"))
        );
    }

    /// 超大消息通常是刚读进来的文件，就躺在摘要够不着的保留区里。
    /// 它必须被就地裁剪，而且不能白白烧一次 Provider 调用。
    #[tokio::test]
    async fn an_oversized_message_is_clamped_in_place_without_a_summary_call() {
        let provider = RecordingProvider::new(&["final"]);
        let window = 1_000_u64;
        let agent = Agent::new(
            provider.clone(),
            registry("compression-oversized"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: window,
                token_budget: None,
            },
        );
        let history = vec![Message::user(format!("HEAD{}TAIL", "z".repeat(5_000)))];

        agent
            .run_with_history(history, "continue")
            .await
            .expect("run");
        let requests = provider.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1, "clamping must not cost a summary call");
        let clamped = requests[0]
            .iter()
            .find(|message| message.content.contains("elided by context compaction"))
            .expect("oversized message must be clamped");
        assert!(clamped.content.starts_with("HEAD"));
        assert!(clamped.content.ends_with("TAIL"));
        assert!(
            estimate_tokens(&requests[0]) < window * AUTO_COMPRESSION_TRIGGER_PERCENT / 100,
            "clamping alone should bring the request back under the trigger"
        );
    }

    /// 保留区自己就撑爆窗口时，摘要救不了场：必须继续丢，并且如实汇报丢了几条。
    #[tokio::test]
    async fn an_oversized_keep_window_drops_messages_and_reports_how_many() {
        let provider = RecordingProvider::new(&["compact summary", "final"]);
        let sink = Arc::new(RecordingSink::default());
        let window = 1_000_u64;
        let agent = Agent::new(
            provider.clone(),
            registry("compression-ceiling"),
            AgentConfig {
                max_turns: 2,
                system_prompt: "system".to_owned(),
                context_window: window,
                token_budget: None,
            },
        )
        .with_event_sink(sink.clone());
        let history = (0..20)
            .map(|index| {
                let body = format!("{}{index:03}", "w".repeat(797));
                if index % 2 == 0 {
                    Message::user(body)
                } else {
                    Message::assistant(body, Vec::new())
                }
            })
            .collect::<Vec<_>>();

        agent
            .run_with_history(history, "continue")
            .await
            .expect("run");

        let dropped = sink
            .events
            .lock()
            .expect("events")
            .iter()
            .find_map(|event| match event {
                AgentEvent::CompressionCompleted {
                    dropped_messages, ..
                } => Some(*dropped_messages),
                _ => None,
            })
            .expect("a compression must have completed");
        assert!(dropped > 0, "an over-full keep window must shed messages");

        let requests = provider.requests.lock().expect("requests");
        let sent = &requests[1];
        assert!(
            sent.len() >= 2 + AUTO_COMPRESSION_MIN_TAIL,
            "the first message, the summary, and the last exchange must survive"
        );
        assert!(
            estimate_tokens(sent) <= window * AUTO_COMPRESSION_CEILING_PERCENT / 100,
            "the request must end up under the ceiling"
        );
        assert!(
            sent.iter()
                .any(|message| message.content.contains("<context-summary>"))
        );
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
