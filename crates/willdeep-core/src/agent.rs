use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;

use crate::background::BackgroundTaskRegistry;
use crate::goal::{
    ContinuationDecision, ContinuationRung, GoalBudget, GoalContinuation, RoundObservation,
    SoftStopReason,
};
use crate::provider::{Provider, ProviderError};
use crate::routing::{RoutingGuard, RoutingTier};
use crate::subagent::{SpawnAgentArgs, SubagentCatalog};
use crate::tools::{ToolError, ToolRegistry};
use crate::types::{Message, ToolCall, Usage, sanitize_tool_history};

mod context;
mod parallel;
mod progress;
mod recovery;
mod streaming;
mod uncertain;

const MAX_INCOMPLETE_RESPONSES: usize = 3;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseOutcome {
    /// 事件已经进到会留下来的对话里。
    Delivered,
    /// 这一轮整个作废，事件没被任何人看过。
    Failed,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    /// 宿主签发的抢占事件取消了这一轮的 provider 请求。转录与工具记录都还在，
    /// 下一轮从保留的上下文继续——用户看到的应该是「插了一件急事」，不是
    /// 「刚才那步白做了」。
    TurnPreempted {
        turn: usize,
    },
    AssistantText(String),
    ProviderProgress(crate::provider::ProviderEvent),
    ToolRequested(ToolCall),
    ToolCompleted {
        call: ToolCall,
        output: String,
        is_error: bool,
    },
    Usage(Usage),
    /// Deterministic routing decision made by the runtime, before provider
    /// prompting. Deep events are emitted only after admission succeeds.
    RouteDecided {
        tier: RoutingTier,
        profile: Option<String>,
        confidence: u8,
        auto_dispatched: bool,
        reason: String,
    },
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
    SubagentRetryWait {
        id: uuid::Uuid,
        attempt: u32,
        delay: std::time::Duration,
    },
    SubagentRetryStarted {
        id: uuid::Uuid,
        attempt: u32,
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
    Partial,
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
    /// 轮次上限用尽，模型没来得及给终稿。`final_text` 是它最后一段可见文字，
    /// 只是部分结果；改好的文件、跑过的命令都还在，只是没收敛。
    MaxTurns,
    /// The provider stopped mid-response or filtered output; never a verified final answer.
    Incomplete,
    /// Workspace changes have no passing verification for their current snapshot.
    Unverified,
}

impl AgentStopReason {
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Finished | Self::GoalComplete)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::GoalComplete => "goal_complete",
            Self::BudgetLimited => "budget_limited",
            Self::MaxTurns => "max_turns",
            Self::Incomplete => "incomplete",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug)]
pub struct AgentOutcome {
    pub final_text: String,
    pub turns: usize,
    pub messages: Vec<Message>,
    pub stop_reason: AgentStopReason,
    /// 本次运行累计的 token 用量（Provider 报了多少算多少）。
    /// 调用方拿它做用量展示与遥测，不必自己去挂 `AgentEvent::Usage` 收集器。
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 从运行开始到首次可用响应的毫秒数：流式文本取首个非空文本增量，
    /// 非流式响应或纯工具调用取完整响应。重试提示与用量事件不算响应。
    /// 后续轮次不会覆盖该值；它包含首次请求前的上下文准备时间。
    pub first_response_millis: Option<u64>,
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
    #[error("subagent stopped with {reason:?} after {turns} rounds; partial result:\n{report}")]
    SubagentPartial {
        reason: AgentStopReason,
        turns: usize,
        report: String,
    },
    #[error("cannot persist agent execution checkpoint: {0}")]
    Checkpoint(String),
    #[error("cannot capture verification snapshot: {0}")]
    VerificationSnapshot(String),
    #[error(
        "protected instructions and task context need {estimated} tokens, but only {capacity} remain after reserving tool schemas and output; select a larger context window or explicitly narrow the task"
    )]
    ContextCapacity { estimated: u64, capacity: u64 },
}

pub struct Agent {
    provider: RwLock<Arc<dyn Provider>>,
    tools: ToolRegistry,
    config: AgentConfig,
    sink: Arc<dyn EventSink>,
    image_fallback: Option<(Arc<dyn Provider>, String)>,
    /// 上下文压缩的专用 Provider。bool 为真表示压缩指令由网关托管
    /// （some.im 的 someim-32b-compressor，服务端 replace 注入），
    /// 摘要请求只发裸转录；为假时仍携带行内压缩指令。
    compressors: Vec<(Arc<dyn Provider>, bool)>,
    /// 会话标题摘要的专用 Provider。没绑就没有 L2 润色，列表停在 L1 派生标题。
    titlers: Vec<Arc<dyn Provider>>,
    subagents: Option<Arc<SubagentCatalog>>,
    instruction_inbox: Option<Arc<AgentInstructionInbox>>,
    goal_continuation: Option<Arc<GoalContinuation>>,
    background_tasks: Option<Arc<BackgroundTaskRegistry>>,
    routing: Option<Arc<RoutingGuard>>,
    /// 宿主事件内核。不挂等于本改动前的行为：没有边界投递，也没有抢占。
    kernel: Option<crate::kernel::EventKernel>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, tools: ToolRegistry, config: AgentConfig) -> Self {
        Self {
            provider: RwLock::new(provider),
            tools,
            config,
            sink: Arc::new(NoopSink),
            image_fallback: None,
            compressors: Vec::new(),
            titlers: Vec::new(),
            subagents: None,
            instruction_inbox: None,
            goal_continuation: None,
            background_tasks: None,
            routing: None,
            kernel: None,
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

    /// Bind a dedicated provider for context-compression summaries. With
    /// `hosted_prompt` the fixed compression instruction lives on the some.im
    /// gateway (`someim-32b-compressor`, injected in replace mode) and the
    /// client sends only the bare transcript; without it the inline
    /// instruction is kept so a custom model never loses its task description.
    pub fn with_compressor(mut self, provider: Arc<dyn Provider>, hosted_prompt: bool) -> Self {
        self.compressors.push((provider, hosted_prompt));
        self
    }

    /// Bind context-compression candidates in preference order. A failed or
    /// empty local summary falls through to the hosted/session candidate.
    pub fn with_compressors(mut self, providers: Vec<(Arc<dyn Provider>, bool)>) -> Self {
        self.compressors = providers;
        self
    }

    /// Bind the provider used for the one-shot session-title summary. Left
    /// unbound the session keeps its deterministic first-prompt title — a
    /// missing title model degrades the label, never the conversation.
    pub fn with_titler(mut self, provider: Arc<dyn Provider>) -> Self {
        self.titlers.push(provider);
        self
    }

    /// Bind title candidates in preference order. Titling is decorative, so
    /// every failed candidate is skipped without affecting the conversation.
    pub fn with_titlers(mut self, providers: Vec<Arc<dyn Provider>>) -> Self {
        self.titlers = providers;
        self
    }

    /// 把第一轮问答压成一行短标题。没绑标题 Provider、调用失败或模型返回
    /// 垃圾时一律 `None`——标题是装饰，不值得为它中断任何东西。
    pub async fn summarize_title(&self, first_user: &str, first_assistant: &str) -> Option<String> {
        for provider in &self.titlers {
            if let Some(title) =
                crate::session_title::summarize(provider.clone(), first_user, first_assistant).await
            {
                return Some(title);
            }
        }
        None
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

    /// 挂上宿主事件内核。
    ///
    /// 挂上之后，待投递事件会在**模型与工具的步骤边界**进入对话，宿主签发的
    /// critical 事件还能取消正在进行的 provider 步骤。不挂就完全不影响原有
    /// 行为——内核是加在主循环旁边的一层，不是替换。
    pub fn with_event_kernel(mut self, kernel: crate::kernel::EventKernel) -> Self {
        self.kernel = Some(kernel);
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

    /// Attach deterministic small-model routing and deep-tier admission.
    pub fn with_routing_guard(mut self, routing: Arc<RoutingGuard>) -> Self {
        self.routing = Some(routing);
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
        messages: Vec<Message>,
        user_message: Message,
    ) -> Result<AgentOutcome, AgentError> {
        self.run_checkpointed(messages, user_message, None).await
    }

    pub async fn run_checkpointed(
        &self,
        messages: Vec<Message>,
        user_message: Message,
        sink: Option<&dyn crate::checkpoint::CheckpointSink>,
    ) -> Result<AgentOutcome, AgentError> {
        let _execution_guard = sink
            .map(|sink| sink.acquire_run())
            .transpose()
            .map_err(AgentError::Checkpoint)?;
        let mut recorder = crate::checkpoint::CheckpointRecorder::new(sink);
        recorder.initialize_evidence(&self.tools)?;
        recorder.initialize_verification(
            self.tools
                .try_verification_baseline()
                .map_err(AgentError::VerificationSnapshot)?,
        )?;
        let result = self.run_inner(messages, user_message, &mut recorder).await;
        recorder.finish(&result)?;
        result
    }

    async fn run_inner(
        &self,
        mut messages: Vec<Message>,
        mut user_message: Message,
        checkpoint: &mut crate::checkpoint::CheckpointRecorder<'_>,
    ) -> Result<AgentOutcome, AgentError> {
        // Persisted history can come from older desktop bridges that retained
        // `role=tool` while losing the protocol IDs. Never let one malformed
        // historical item make every future turn fail at the Provider boundary.
        let uncertain_calls = uncertain::UncertainCalls::recover(&mut messages);
        sanitize_tool_history(&mut messages);
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
        if let Some(goal) = goal_from_message(&user_message.content)
            && let Some(continuation) = &self.goal_continuation
        {
            continuation.activate(goal, GoalBudget::default());
        }
        self.apply_runtime_route(&mut user_message).await;
        messages.retain(|message| message.role != crate::types::Role::System);
        messages.insert(0, Message::system(&self.config.system_prompt));
        let mut rules = crate::project_rules::ProjectRules::new(self.tools.workspace())
            .map_err(ToolError::Io)?;
        // The approval judge reads this as inert context: it decides whether
        // a bounded action is relevant to the current goal, never whether a
        // destructive one is permitted.
        self.tools.set_task_context(&user_message.content);
        messages.push(user_message);
        let definitions = self.tools.definitions();
        let mut compressed: Option<(usize, String)> = None;
        let mut used_tokens = 0_u64;
        // 分别累计输入/输出，供 AgentOutcome 上报——`used_tokens` 是预算判定用的
        // 合计值，两者语义不同，不能互相顶替。
        let mut input_tokens = 0_u64;
        let mut output_tokens = 0_u64;
        // 自上次续推判定以来成功发起的工具调用数——续推判定的「进展证据」。
        let mut tools_since_check = 0_usize;
        let mut progress = progress::ProgressTracker::default();
        let mut incomplete_responses = 0_usize;
        let verification_baseline = checkpoint.verification_baseline().map(str::to_owned);
        let mut unverified_stops = 0_usize;
        // 流式文本在增量到达时计时；无文本增量的响应在完整返回时计时。
        let mut first_response_millis: Option<u64> = None;
        let run_started = std::time::Instant::now();
        for turn in 1..=self.config.max_turns {
            rules.refresh().map_err(ToolError::Io)?;
            messages[0].content = format!(
                "{}\n\n{}\n\n{}",
                self.config.system_prompt,
                rules.render(),
                self.tools.required_verification_prompt()
            );
            self.append_pending_instructions(&mut messages);
            checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
            // 事件在这里进对话，而不是在工具与工具之间：一次 assistant 的
            // tool_calls 必须紧跟它那批 tool 结果，中间插一条用户消息会把这个
            // 配对拆散。turn 顶部就是「当前模型或工具步骤已经结束」那个边界。
            let leases = self.append_kernel_events(&mut messages);
            if !leases.is_empty()
                && let Err(error) = checkpoint.record(&messages, turn, input_tokens, output_tokens)
            {
                self.settle_leases(&leases, LeaseOutcome::Failed);
                return Err(error);
            }
            self.sink.emit(AgentEvent::TurnStarted { turn }).await;
            let prepared = self
                .request_messages_accounted(&messages, &mut compressed, &mut |usage| {
                    input_tokens = input_tokens.saturating_add(usage.input_tokens.unwrap_or(0));
                    output_tokens = output_tokens.saturating_add(usage.output_tokens.unwrap_or(0));
                    used_tokens =
                        used_tokens.saturating_add(usage.total_tokens.unwrap_or_else(|| {
                            usage
                                .input_tokens
                                .unwrap_or(0)
                                .saturating_add(usage.output_tokens.unwrap_or(0))
                        }));
                    checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
                    if let Some(budget) = self.config.token_budget
                        && used_tokens >= budget
                    {
                        return Err(AgentError::TokenBudgetExceeded {
                            budget,
                            used: used_tokens,
                        });
                    }
                    Ok(())
                })
                .await;
            let request_messages = match prepared {
                Ok(messages) => messages,
                Err(error) => {
                    self.settle_leases(&leases, LeaseOutcome::Failed);
                    return Err(error);
                }
            };
            let stream = streaming::StreamEvents::new(
                checkpoint,
                self.sink.as_ref(),
                turn,
                input_tokens,
                output_tokens,
            );
            let response = match stream
                .wait(self.complete_or_preempt(&request_messages, &definitions, &stream))
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    drop(stream);
                    self.settle_leases(&leases, LeaseOutcome::Failed);
                    return Err(error);
                }
            };
            if first_response_millis.is_none() {
                first_response_millis = stream
                    .first_text_at()
                    .map(|at| at.saturating_duration_since(run_started).as_millis() as u64);
            }
            let streamed_partial = match stream.partial() {
                Ok(partial) => partial,
                Err(error) => {
                    drop(stream);
                    self.settle_leases(&leases, LeaseOutcome::Failed);
                    return Err(error);
                }
            };
            drop(stream);
            let partial = match &response {
                Err(ProviderError::StreamInterrupted { partial, .. }) => partial.as_ref(),
                _ => &streamed_partial,
            };
            if !matches!(&response, Ok(Some(_))) {
                if !partial.content.is_empty() {
                    messages.push(Message::assistant(&partial.content, Vec::new()));
                }
                if let Some(usage) = &partial.usage {
                    input_tokens = input_tokens.saturating_add(usage.input_tokens.unwrap_or(0));
                    output_tokens = output_tokens.saturating_add(usage.output_tokens.unwrap_or(0));
                    used_tokens =
                        used_tokens.saturating_add(usage.total_tokens.unwrap_or_else(|| {
                            usage
                                .input_tokens
                                .unwrap_or(0)
                                .saturating_add(usage.output_tokens.unwrap_or(0))
                        }));
                    self.sink.emit(AgentEvent::Usage(usage.clone())).await;
                }
                checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
            }
            let completion = match response {
                Ok(Some(completion)) => {
                    self.settle_leases(&leases, LeaseOutcome::Delivered);
                    first_response_millis
                        .get_or_insert_with(|| run_started.elapsed().as_millis() as u64);
                    completion
                }
                Ok(None) => {
                    if let Some(budget) = self.config.token_budget
                        && used_tokens >= budget
                    {
                        self.settle_leases(&leases, LeaseOutcome::Delivered);
                        return Err(AgentError::TokenBudgetExceeded {
                            budget,
                            used: used_tokens,
                        });
                    }
                    // 被抢占：请求丢掉了，但事件文本已经在 transcript 里，
                    // 下一轮模型照样看得到，所以算投递成功。放回 pending 只会
                    // 让同一批事件再讲一遍。
                    self.settle_leases(&leases, LeaseOutcome::Delivered);
                    self.sink.emit(AgentEvent::TurnPreempted { turn }).await;
                    continue;
                }
                Err(error) => {
                    // 这一轮的 messages 会随着错误一起丢掉，事件也就没人看过。
                    self.settle_leases(&leases, LeaseOutcome::Failed);
                    return Err(error.into());
                }
            };
            let response_incomplete = completion.is_incomplete();
            if let Some(usage) = completion.usage {
                input_tokens = input_tokens.saturating_add(usage.input_tokens.unwrap_or(0));
                output_tokens = output_tokens.saturating_add(usage.output_tokens.unwrap_or(0));
                used_tokens = used_tokens.saturating_add(usage.total_tokens.unwrap_or_else(|| {
                    usage
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.output_tokens.unwrap_or(0))
                }));
                self.sink.emit(AgentEvent::Usage(usage)).await;
                let mut partial_history = messages.clone();
                if !completion.content.is_empty() {
                    partial_history.push(Message::assistant(&completion.content, Vec::new()));
                }
                checkpoint.record(&partial_history, turn, input_tokens, output_tokens)?;
                if let Some(budget) = self.config.token_budget
                    && used_tokens >= budget
                {
                    messages.push(Message::assistant(&completion.content, Vec::new()));
                    messages.push(Message::user("[budget-limited] The token budget was exhausted after this provider response. None of the tool calls proposed in that response were executed. Resume from the saved results and outstanding work when more budget is available."));
                    checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
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
            if response_incomplete {
                incomplete_responses += 1;
                // Truncated tool calls are not executable, even if their JSON happens to parse.
                messages.push(Message::assistant(&content, Vec::new()));
                if incomplete_responses >= MAX_INCOMPLETE_RESPONSES
                    || completion.finish_reason.as_deref() == Some("content_filter")
                {
                    return Ok(AgentOutcome {
                        final_text: content,
                        turns: turn,
                        messages,
                        stop_reason: AgentStopReason::Incomplete,
                        input_tokens,
                        output_tokens,
                        first_response_millis,
                    });
                }
                messages.push(Message::user("[response-incomplete] The provider ended the previous response before completion. No tool calls from that incomplete response were executed. Continue the outstanding work, using smaller complete steps. Do not treat the partial output as task completion."));
                continue;
            }
            incomplete_responses = 0;
            if completion.tool_calls.is_empty() {
                if content.is_empty() {
                    return Err(AgentError::EmptyResponse);
                }
                messages.push(Message::assistant(&content, Vec::new()));
                if self.append_pending_instructions(&mut messages) {
                    continue;
                }
                let wrapping_up = self
                    .goal_continuation
                    .as_ref()
                    .is_some_and(|goal| goal.wrap_up_pending());
                if !wrapping_up
                    && let Some(feedback) = self
                        .tools
                        .completion_verification_feedback(verification_baseline.as_deref())
                {
                    unverified_stops += 1;
                    if unverified_stops >= MAX_INCOMPLETE_RESPONSES {
                        return Ok(AgentOutcome {
                            final_text: content,
                            turns: turn,
                            messages,
                            stop_reason: AgentStopReason::Unverified,
                            input_tokens,
                            output_tokens,
                            first_response_millis,
                        });
                    }
                    messages.push(Message::user(format!("[completion-verification-required] {feedback} Continue the outstanding task. Do not weaken checks or claim completion. If verification is unavailable, explain the limitation; the runtime will retain a partial result.")));
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
                                input_tokens,
                                output_tokens,
                                first_response_millis,
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
                    input_tokens,
                    output_tokens,
                    first_response_millis,
                });
            }
            messages.push(Message::assistant(content, completion.tool_calls.clone()));
            checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
            let mut rules_changed = false;
            let mut parallel_results = self
                .parallel_reads(&completion.tool_calls, &mut rules)
                .await;
            for call in completion.tool_calls {
                if parallel_results.is_none() {
                    self.sink
                        .emit(AgentEvent::ToolRequested(call.clone()))
                        .await;
                }
                let result = if let Some(results) = parallel_results.as_mut() {
                    results.pop_front().expect("one result per parallel read")
                } else {
                    match rules.before_call(&call) {
                        Ok(changed) => {
                            rules_changed |= changed;
                            if rules_changed {
                                Err(ToolError::HookDenied("Applicable project instructions changed or a new directory scope was discovered. No action was executed. Read the refreshed system instructions and reissue the appropriate tool call on the next round.".to_owned()))
                            } else {
                                if uncertain_calls.needs_approval(&call) {
                                    match self.tools.approve_uncertain_replay(&call).await {
                                        Ok(()) => self.execute_tool(&call).await,
                                        Err(error) => Err(error),
                                    }
                                } else {
                                    self.execute_tool(&call).await
                                }
                            }
                        }
                        Err(error) => Err(ToolError::Io(error)),
                    }
                };
                let (output, is_error) = match result {
                    Ok(output) => (output, false),
                    Err(error) => (format!("tool error: {error}"), true),
                };
                if progress.observe(&call, &output, is_error) {
                    tools_since_check = tools_since_check.saturating_add(1);
                }
                self.sink
                    .emit(AgentEvent::ToolCompleted {
                        call: call.clone(),
                        output: output.clone(),
                        is_error,
                    })
                    .await;
                messages.push(Message::tool(&call, output));
                checkpoint.record(&messages, turn, input_tokens, output_tokens)?;
            }
        }
        // 触顶不再判失败。此前这里返回 `AgentError::MaxTurns`，整轮标成失败，
        // 模型改好的十个文件、写到一半的结论一句都不给看，用户只看到一行
        // 「reached the maximum of N turns」。现在把最后一段可见文字当部分结果
        // 交出去，`stop_reason` 标成 `MaxTurns`，由展示层说明它没收敛。
        let final_text = messages
            .iter()
            .rev()
            .find(|message| {
                message.role == crate::types::Role::Assistant && !message.content.trim().is_empty()
            })
            .map(|message| message.content.clone())
            .unwrap_or_default();
        Ok(AgentOutcome {
            final_text,
            turns: self.config.max_turns,
            messages,
            stop_reason: AgentStopReason::MaxTurns,
            input_tokens,
            output_tokens,
            first_response_millis,
        })
    }

    /// 取一批待投递事件，渲染进对话，返回它们的 lease。
    ///
    /// 一次最多取这么多条：事件是通知不是正文，一口气灌几十条只会把这一轮的
    /// 注意力全占掉，剩下的下一轮再来。
    fn append_kernel_events(&self, messages: &mut Vec<Message>) -> Vec<uuid::Uuid> {
        const BATCH: usize = 8;
        let Some(kernel) = &self.kernel else {
            return Vec::new();
        };
        let batch = kernel.take_for_model(BATCH);
        if batch.is_empty() {
            return Vec::new();
        }
        let events: Vec<_> = batch.iter().map(|leased| leased.event.clone()).collect();
        let Some(text) = crate::kernel::render_for_model(&events) else {
            // 渲染不出内容就别占着 lease。
            let leases: Vec<uuid::Uuid> = batch.iter().map(|leased| leased.lease_id).collect();
            kernel.release(&leases);
            return Vec::new();
        };
        messages.push(Message::user(text));
        batch.iter().map(|leased| leased.lease_id).collect()
    }

    /// 一批 lease 该按投递成功还是投递失败结账。
    ///
    /// 分界不是「请求成不成功」而是「事件有没有进到留得下来的 transcript」：
    /// 被抢占的那一轮请求虽然废了，事件文本却留在对话里，模型下一轮照样看得
    /// 到；而请求报错时整轮 messages 一起丢，事件就等于没人看过。
    fn settle_leases(&self, leases: &[uuid::Uuid], outcome: LeaseOutcome) {
        if leases.is_empty() {
            return;
        }
        let Some(kernel) = &self.kernel else {
            return;
        };
        match outcome {
            LeaseOutcome::Delivered => kernel.ack(leases),
            LeaseOutcome::Failed => kernel.release(leases),
        };
    }

    /// 发一次 provider 请求，除非中途被宿主签发的抢占事件打断。
    ///
    /// 返回 `Ok(None)` 表示被抢占。取消的只有这一次请求：transcript、工具
    /// 记录和事件队列都原样留着，下一轮从保留的上下文继续。
    async fn complete_or_preempt(
        &self,
        messages: &[Message],
        definitions: &[crate::types::ToolDefinition],
        events: &dyn crate::provider::ProviderEventSink,
    ) -> Result<Option<crate::types::Completion>, ProviderError> {
        let provider = self.provider()?;
        let Some(kernel) = self.kernel.clone() else {
            return provider
                .complete_with_events(messages, definitions, events)
                .await
                .map(Some);
        };
        tokio::select! {
            completion = provider.complete_with_events(messages, definitions, events) => completion.map(Some),
            () = kernel.preempted() => Ok(None),
        }
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
            if let Some(result) = self.execute_recovery_tool(call).await {
                return result;
            }
            if call.name != "spawn_agent" {
                let result = self.tools.execute(call).await;
                if result.is_ok()
                    && let Some(routing) = &self.routing
                {
                    routing.record_tool_success(&call.name);
                }
                return result;
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
            let profile = args
                .profile
                .as_deref()
                .unwrap_or("generalist")
                .trim()
                .to_ascii_lowercase();
            if !catalog.has_profile(&profile) {
                return Err(ToolError::Network(format!(
                    "subagent profile not found: {profile}"
                )));
            }
            let approved_command = match args.target_command.as_deref() {
                Some(_) if profile != "ops_runner" => {
                    return Err(ToolError::ApprovalDenied(
                        "target_command may only be used with profile=\"ops_runner\"".to_owned(),
                    ));
                }
                Some(command) => Some(self.tools.approve_subagent_command(command).await?),
                None => None,
            };
            // 准入绑的是**档位**，不是工种名。`deep` 从工种正交化成
            // WorkerTier::Expert 之后，这道闸必须跟着搬——否则 `profile="deep"`
            // 只是换了个名字就绕过了票据。
            let requested_tier = args
                .worker_tier
                .as_deref()
                .and_then(crate::WorkerTier::parse)
                .or_else(|| crate::WorkerTier::parse(&profile))
                .unwrap_or_default();
            if let Some(routing) = &self.routing
                && requested_tier.requires_admission()
            {
                routing
                    .authorize_deep(args.escalation.as_ref())
                    .map_err(ToolError::Network)?;
                self.sink
                    .emit(AgentEvent::RouteDecided {
                        tier: RoutingTier::Deep,
                        profile: Some(profile.clone()),
                        confidence: 100,
                        auto_dispatched: false,
                        reason: "runtime-validated escalation ticket".to_owned(),
                    })
                    .await;
            }
            let scope = catalog.write_scope(Some(&profile));
            let approved_targets = if scope.writes() {
                let requested = args.requested_write_targets(scope);
                if requested.is_empty() {
                    return Err(ToolError::OutsideWorkspace(
                        "a writing profile needs its files declared up front: target_file for legacy editor, task.write_files for implementer, test_fixer or build_fixer".to_owned(),
                    ));
                }
                Some(self.tools.approve_subagent_write_set(&requested).await?)
            } else {
                None
            };
            // 专家档自己不算「低档尝试」，别让它给自己攒证据。
            if !requested_tier.requires_admission()
                && let Some(routing) = &self.routing
            {
                // Only count a lower-tier attempt after its task packet and
                // write authority have passed validation. A rejected packet
                // must not become evidence that unlocks Deep.
                routing.record_profile_attempt(&profile);
            }
            catalog
                .run_authorized(args, approved_targets, approved_command)
                .await
                .map_err(|error| ToolError::Network(error.to_string()))
        })
    }

    fn apply_runtime_route<'a>(
        &'a self,
        user_message: &'a mut Message,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        // A delegated run owns another Agent. Type-erasing this edge keeps
        // the parent's async state from recursively embedding the child's.
        Box::pin(async move {
            let (Some(routing), Some(catalog)) = (&self.routing, &self.subagents) else {
                return;
            };
            let decision = routing
                .route(routing_request_from_message(&user_message.content))
                .await;
            self.sink
                .emit(AgentEvent::RouteDecided {
                    tier: decision.tier,
                    profile: decision.profile.map(str::to_owned),
                    confidence: decision.confidence,
                    auto_dispatched: decision.auto_dispatch_read_only,
                    reason: decision.reason.to_owned(),
                })
                .await;
            let Some(profile) = decision.profile else {
                return;
            };
            if !decision.auto_dispatch_read_only {
                if let Some(steering) = decision.steering() {
                    user_message.content.push_str("\n\n");
                    user_message.content.push_str(&steering);
                }
                return;
            }

            routing.record_profile_attempt(profile);
            let prompt = user_message.content.clone();
            let report = catalog
                .run(
                    SpawnAgentArgs {
                        prompt,
                        label: Some(format!("runtime preflight: {profile}")),
                        profile: Some(profile.to_owned()),
                        run_in_background: Some(false),
                        ..SpawnAgentArgs::default()
                    },
                    None,
                )
                .await;
            let report = match report {
                Ok(report) => report,
                Err(error) => format!(
                    "The `{profile}` preflight failed. Continue on the standard tier and do not treat this as evidence that deep is required. Error: {error}"
                ),
            };
            user_message.content.push_str(&format!(
                "\n\n<runtime-route tier=\"worker\" profile=\"{profile}\" confidence=\"{}\">\n\
The runtime dispatched this bounded read-only preflight before the standard model. Use the report as evidence, verify citations when needed, and answer without re-reading the same material unless it is incomplete.\n\
<worker-report>\n{report}\n</worker-report>\n\
</runtime-route>",
                decision.confidence
            ));
        })
    }
}

/// Frontends share the same explicit goal envelope. Parsing it in the core
/// keeps Web, TUI and headless Runtime behavior identical.
fn goal_from_message(content: &str) -> Option<String> {
    const OPEN: &str = "<goal>";
    const CLOSE: &str = "</goal>";
    let start = content.find(OPEN)? + OPEN.len();
    let rest = &content[start..];
    let end = rest.find(CLOSE)?;
    let goal = rest[..end].trim();
    if goal.is_empty() || goal.len() > 16 * 1024 {
        return None;
    }
    Some(goal.to_owned())
}

/// Goal frontends repeat a broad objective on every turn. Route the current
/// request, not that repeated envelope, or one word such as "implement" in
/// the goal would pin every later read-only question to the standard tier.
fn routing_request_from_message(content: &str) -> &str {
    let Some((_, tail)) = content.split_once("</goal>") else {
        return content;
    };
    let tail = tail.trim_start();
    let tail = tail
        .strip_prefix("Continue until this goal is genuinely complete.")
        .unwrap_or(tail)
        .trim_start();
    if tail.is_empty() { content } else { tail }
}

#[cfg(test)]
mod tests;
