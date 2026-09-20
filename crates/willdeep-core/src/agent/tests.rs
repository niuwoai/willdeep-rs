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

#[tokio::test]
async fn first_response_latency_is_captured_before_stream_completion() {
    struct DelayedStream;
    #[async_trait]
    impl Provider for DelayedStream {
        async fn complete(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            unreachable!("streaming entry point required")
        }
        async fn complete_with_events(
            &self,
            _: &[Message],
            _: &[ToolDefinition],
            events: &dyn crate::provider::ProviderEventSink,
        ) -> Result<Completion, ProviderError> {
            events
                .emit(crate::provider::ProviderEvent::TextDelta("ready".into()))
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok(Completion {
                reasoning: None,
                content: "ready".into(),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".into()),
            })
        }
    }
    let root =
        std::env::temp_dir().join(format!("willdeep-first-response-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let agent = Agent::new(
        Arc::new(DelayedStream),
        ToolRegistry::new(&root, ApprovalMode::ReadOnly).unwrap(),
        AgentConfig {
            max_turns: 1,
            system_prompt: String::new(),
            context_window: 32000,
            token_budget: None,
        },
    );
    let started = std::time::Instant::now();
    let result = agent.run("say ready").await.unwrap();
    let elapsed = started.elapsed().as_millis() as u64;
    assert!(elapsed.saturating_sub(result.first_response_millis.unwrap()) >= 95);
    assert_eq!(result.final_text, "ready");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn configured_verification_reaches_the_model_and_blocks_unchecked_completion() {
    struct ClaimsDone;
    #[async_trait]
    impl Provider for ClaimsDone {
        async fn complete(
            &self,
            messages: &[Message],
            _: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            assert!(messages.iter().any(|message| {
                message.content.contains("[required-verification]")
                    && message.content.contains("cargo test --workspace")
            }));
            Ok(Completion {
                reasoning: None,
                content: "done".into(),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".into()),
            })
        }
    }
    let root = std::env::temp_dir().join(format!(
        "willdeep-explicit-contract-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let tools = ToolRegistry::new(&root, ApprovalMode::ReadOnly)
        .unwrap()
        .with_verification_snapshot(|| Some("unchanged".into()));
    tools
        .require_verifications(&["cargo test --workspace".into()])
        .unwrap();
    let agent = Agent::new(
        Arc::new(ClaimsDone),
        tools,
        AgentConfig {
            max_turns: 4,
            system_prompt: "test".into(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    let outcome = agent.run("finish the task").await.unwrap();
    assert_eq!(outcome.stop_reason, AgentStopReason::Unverified);
    assert_eq!(outcome.turns, 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn changed_workspace_cannot_finish_without_current_verification() {
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
                reasoning: None,
                content: "<goal-status>complete</goal-status> Everything is complete".into(),
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: Some("stop".into()),
            })
        }
    }
    let root =
        std::env::temp_dir().join(format!("willdeep-completion-gate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("revision"), "initial").unwrap();
    let captured = root.clone();
    let tools = ToolRegistry::new(&root, ApprovalMode::ReadOnly)
        .unwrap()
        .with_verification_snapshot(move || std::fs::read_to_string(captured.join("revision")).ok())
        .with_verification_reporter(|_| {});
    let continuation = Arc::new(GoalContinuation::new());
    continuation.activate("finish the task", crate::goal::GoalBudget::default());
    let agent = Agent::new(
        Arc::new(ClaimComplete(root.clone())),
        tools,
        AgentConfig {
            max_turns: 5,
            system_prompt: "test".into(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_goal_continuation(continuation);
    let result = agent.run("finish the task").await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::Unverified);
    assert_eq!(result.turns, 3);
    assert!(result.messages.iter().any(|message| {
        message.content.contains("completion-verification-required")
            && message
                .content
                .contains("no passing verification for the current snapshot")
    }));
    std::fs::remove_dir_all(root).unwrap();
}

struct ConcurrentReadSink {
    barrier: tokio::sync::Barrier,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl EventSink for ConcurrentReadSink {
    async fn emit(&self, event: AgentEvent) {
        use std::sync::atomic::Ordering::SeqCst;
        if matches!(event, AgentEvent::ToolRequested(_)) {
            let active = self.active.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(active, SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, SeqCst);
        }
    }
}

#[tokio::test]
async fn independent_reads_overlap_but_mixed_batches_stay_serial() {
    let root = std::env::temp_dir().join(format!("willdeep-parallel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut calls = Vec::new();
    for index in 0..8 {
        let path = format!("{index}.txt");
        std::fs::write(root.join(&path), format!("value-{index}")).unwrap();
        calls.push(ToolCall {
            id: index.to_string(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":path}).to_string(),
        });
    }
    let sink = Arc::new(ConcurrentReadSink {
        barrier: tokio::sync::Barrier::new(4),
        active: 0.into(),
        peak: 0.into(),
    });
    let agent = Agent::new(
        RecordingProvider::new(&[]),
        ToolRegistry::new(&root, ApprovalMode::Strict).unwrap(),
        AgentConfig {
            max_turns: 2,
            system_prompt: "test".into(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_event_sink(sink.clone());
    let mut rules = crate::project_rules::ProjectRules::new(&root).unwrap();
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        agent.parallel_reads(&calls, &mut rules),
    )
    .await
    .expect("four independent reads must start without waiting for the first to finish")
    .unwrap();
    for (index, result) in results.into_iter().enumerate() {
        assert!(
            result.unwrap().contains(&format!("value-{index}")),
            "results preserve provider order"
        );
    }
    assert_eq!(sink.peak.load(std::sync::atomic::Ordering::SeqCst), 4);
    calls[1].name = "create_file".into();
    assert!(agent.parallel_reads(&calls, &mut rules).await.is_none());
    calls[1].name = "read_file".into();
    std::fs::write(
        root.join("AGENTS.md"),
        "Do not inspect files until these rules are read.",
    )
    .unwrap();
    let results = agent.parallel_reads(&calls, &mut rules).await.unwrap();
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(ToolError::HookDenied(_))))
    );
    let mut agent = agent;
    agent.tools =
        agent
            .tools
            .with_hooks(crate::hooks::HookRegistry::new(vec![crate::hooks::Hook {
                name: "serial audit".into(),
                event: crate::hooks::HookEvent::PreTool,
                command: "exit 99".into(),
                blocking: true,
                timeout: std::time::Duration::from_secs(1),
                on_error: Default::default(),
            }]));
    assert!(
        agent.parallel_reads(&calls, &mut rules).await.is_none(),
        "hooks may have side effects and cannot overlap"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn auxiliary_title_and_compression_fall_back_after_empty_local_reply() {
    let local_title = RecordingProvider::new(&[""]);
    let remote_title = RecordingProvider::new(&["修复登录 bug"]);
    let local_compressor = RecordingProvider::new(&[""]);
    let remote_compressor = RecordingProvider::new(&["preserved summary"]);
    let agent = Agent::new(
        RecordingProvider::new(&[]),
        registry("auxiliary-fallback"),
        AgentConfig {
            max_turns: 2,
            system_prompt: "test".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_titlers(vec![local_title.clone(), remote_title.clone()])
    .with_compressors(vec![
        (local_compressor.clone(), false),
        (remote_compressor.clone(), false),
    ]);
    assert_eq!(
        agent
            .summarize_title("Fix login", "Updated handler")
            .await
            .as_deref(),
        Some("修复登录 bug")
    );
    let history = (0..16)
        .map(|index| {
            if index % 2 == 0 {
                Message::user(format!("constraint {index}"))
            } else {
                Message::assistant(format!("observed {index}"), Vec::new())
            }
        })
        .collect();
    let compressed = agent.compress_history(history).await.unwrap();
    assert!(
        compressed
            .iter()
            .any(|message| message.content.contains("preserved summary"))
    );
    for provider in [
        local_title,
        remote_title,
        local_compressor,
        remote_compressor,
    ] {
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
    }
}

struct TruncatedProvider {
    calls: std::sync::atomic::AtomicUsize,
    forever: bool,
}

#[async_trait]
impl Provider for TruncatedProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let incomplete = call == 0 || self.forever;
        if call > 0 {
            assert!(
                messages
                    .iter()
                    .any(|message| message.content.contains("[response-incomplete]"))
            );
        }
        Ok(Completion {
            reasoning: None,
            content: if incomplete {
                "partial"
            } else {
                "actual final answer"
            }
            .to_owned(),
            tool_calls: if incomplete {
                vec![ToolCall {
                    id: format!("partial-{call}"),
                    name: "create_file".to_owned(),
                    arguments: r#"{"path":"must-not-exist.txt","content":"truncated action"}"#
                        .to_owned(),
                }]
            } else {
                Vec::new()
            },
            finish_reason: Some(if incomplete { "length" } else { "stop" }.to_owned()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn truncated_responses_continue_without_executing_partial_tool_calls() {
    let tools = registry("truncated");
    let path = tools.workspace().join("must-not-exist.txt");
    let agent = Agent::new(
        Arc::new(TruncatedProvider {
            calls: 0.into(),
            forever: false,
        }),
        tools,
        AgentConfig {
            max_turns: 4,
            system_prompt: "test".to_owned(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    let result = agent.run("do the task").await.unwrap();
    assert_eq!(result.final_text, "actual final answer");
    assert_eq!(result.turns, 2);
    assert!(!path.exists());
    assert!(
        result
            .messages
            .iter()
            .all(|message| message.tool_calls.is_empty())
    );
}

#[tokio::test]
async fn repeated_truncation_is_partial_instead_of_success_or_an_infinite_loop() {
    let agent = Agent::new(
        Arc::new(TruncatedProvider {
            calls: 0.into(),
            forever: true,
        }),
        registry("truncated-limit"),
        AgentConfig {
            max_turns: 10,
            system_prompt: "test".to_owned(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    let result = agent.run("do the task").await.unwrap();
    assert_eq!(result.stop_reason, AgentStopReason::Incomplete);
    assert_eq!(result.turns, MAX_INCOMPLETE_RESPONSES);
}

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
            reasoning: None,
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
            reasoning: None,
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
            reasoning: None,
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
    let root = std::env::temp_dir().join(format!("willdeep-agent-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("workspace");
    ToolRegistry::new(root, ApprovalMode::Strict).expect("registry")
}

/// 每一轮都只发工具调用、永远不给终稿的模型。
struct EndlessToolProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for EndlessToolProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(Completion {
            reasoning: None,
            content: format!("still working, step {call}"),
            tool_calls: vec![crate::types::ToolCall {
                id: format!("call-{call}"),
                name: "list_directory".to_owned(),
                arguments: r#"{"path":"."}"#.to_owned(),
            }],
            finish_reason: Some("tool_calls".to_owned()),
            usage: None,
        })
    }
}

/// 轮次用尽不再是错误：交出最后一段可见文字，停机原因标 `MaxTurns`，
/// 历史里的工具往返一条不少——改动都在，只是没收敛。
#[tokio::test]
async fn exhausting_turns_returns_the_partial_result_instead_of_failing() {
    let agent = Agent::new(
        Arc::new(EndlessToolProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }),
        registry("max-turns-partial"),
        AgentConfig {
            max_turns: 3,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    );

    let outcome = agent
        .run("keep going")
        .await
        .expect("hitting the turn limit must not be an error");

    assert_eq!(outcome.stop_reason, AgentStopReason::MaxTurns);
    assert_eq!(outcome.turns, 3);
    assert_eq!(outcome.final_text, "still working, step 3");
    let tool_results = outcome
        .messages
        .iter()
        .filter(|message| message.role == crate::types::Role::Tool)
        .count();
    assert_eq!(
        tool_results, 3,
        "every tool round trip stays in the history"
    );
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

struct OperatorSteerProvider {
    calls: std::sync::atomic::AtomicUsize,
    /// 根 Agent 自带的收件箱，Agent 建好之后才拿得到，所以放在槽位里。
    inbox: std::sync::Mutex<Option<Arc<AgentInstructionInbox>>>,
}

#[async_trait]
impl Provider for OperatorSteerProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            let inbox = self
                .inbox
                .lock()
                .unwrap()
                .clone()
                .expect("inbox slot filled");
            assert!(inbox.push_operator("先别删，只改 handler".to_owned()));
        } else {
            // 用户插话以用户身份进对话，不裹「parent Agent」那层宿主口吻。
            let steer = messages
                .iter()
                .find(|message| message.content == "先别删，只改 handler")
                .expect("operator steering reaches the next model call");
            assert_eq!(steer.role, crate::types::Role::User);
            assert_eq!(
                steer.source,
                Some(crate::types::MessageSource::OperatorInput)
            );
            assert!(
                !messages
                    .iter()
                    .any(|message| message.content.contains("Additional instructions")),
                "no parent-agent wrapper for operator steering"
            );
        }
        Ok(Completion {
            reasoning: None,
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

/// 根 Agent 自带收件箱：用户在本轮进行中说的话在下一次调模型前以用户身份注入，
/// 模型刚要收尾也会因此再跑一轮。
#[tokio::test]
async fn operator_steering_is_injected_as_a_user_message_before_the_next_call() {
    let provider = Arc::new(OperatorSteerProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
        inbox: std::sync::Mutex::new(None),
    });
    let agent = Agent::new(
        provider.clone(),
        registry("steering"),
        AgentConfig {
            max_turns: 3,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    );
    let inbox = agent
        .instruction_inbox()
        .expect("root agents carry an inbox");
    *provider.inbox.lock().unwrap() = Some(inbox);

    let outcome = agent
        .run("delete the upload module")
        .await
        .expect("continued run");
    assert_eq!(outcome.final_text, "revised answer");
    assert_eq!(outcome.turns, 2);
    assert!(outcome.messages.iter().any(|message| {
        message.content == "先别删，只改 handler"
            && message.source == Some(crate::types::MessageSource::OperatorInput)
    }));
}

fn kernel_event(
    interrupt: crate::kernel::InterruptPolicy,
    title: &str,
) -> willdeep_runtime_protocol::KernelEvent {
    crate::kernel::host_event(
        uuid::Uuid::nil(),
        willdeep_runtime_protocol::EventSource::Worker,
        "worker.completed",
        willdeep_runtime_protocol::EventPriority::Normal,
        interrupt,
        title,
        None,
        Some(title.to_owned()),
        false,
    )
}

fn kernel_agent(provider: Arc<dyn Provider>, kernel: crate::kernel::EventKernel) -> Agent {
    Agent::new(
        provider,
        registry("kernel"),
        AgentConfig {
            max_turns: 4,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_event_kernel(kernel)
}

/// 待投递事件在 turn 边界进入对话，作为用户消息而不是系统提示词。
#[tokio::test]
async fn kernel_events_reach_the_model_as_user_material() {
    let kernel = crate::kernel::EventKernel::new();
    kernel.publish(
        kernel_event(
            crate::kernel::InterruptPolicy::YieldAtBoundary,
            "tests green",
        ),
        crate::kernel::DedupPolicy::Once,
    );
    let provider = RecordingProvider::new(&["done"]);
    let agent = kernel_agent(provider.clone(), kernel.clone());

    let outcome = agent.run("carry on").await.expect("run");
    assert_eq!(outcome.final_text, "done");

    let requests = provider.requests.lock().expect("requests");
    let delivered = requests[0]
        .iter()
        .find(|message| message.content.contains("tests green"))
        .expect("event text must reach the model");
    assert_eq!(
        delivered.role,
        crate::types::Role::User,
        "事件是材料不是系统指令"
    );
    // 请求成功之后才算处理完。
    assert!(kernel.take_for_model(4).is_empty());
    assert_eq!(
        kernel.snapshot()[0].delivery.state,
        willdeep_runtime_protocol::DeliveryState::Handled
    );
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        Err(ProviderError::InvalidResponse(
            "upstream is down".to_owned(),
        ))
    }
}

/// 请求失败时这一轮的 messages 全丢，所以事件必须回到待投递。
///
/// 这是 lease 的全部意义：提前 ack 的话，这条事件就在一次谁也没看见的
/// 请求里永久消失了。
#[tokio::test]
async fn a_failed_request_puts_its_events_back() {
    let kernel = crate::kernel::EventKernel::new();
    kernel.publish(
        kernel_event(
            crate::kernel::InterruptPolicy::YieldAtBoundary,
            "build broke",
        ),
        crate::kernel::DedupPolicy::Once,
    );
    let agent = kernel_agent(Arc::new(FailingProvider), kernel.clone());
    agent.run("carry on").await.expect_err("provider is down");

    assert_eq!(
        kernel.snapshot()[0].delivery.state,
        willdeep_runtime_protocol::DeliveryState::Pending
    );
    assert_eq!(kernel.take_for_model(4).len(), 1);
}

/// 宿主签发的抢占取消正在进行的请求，但转录留着，下一轮继续。
struct SlowThenFastProvider {
    kernel: crate::kernel::EventKernel,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for SlowThenFastProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            // 请求进行到一半，一条宿主 critical 事件到达。
            self.kernel.publish(
                kernel_event(crate::kernel::InterruptPolicy::Preempt, "context exhausted"),
                crate::kernel::DedupPolicy::Once,
            );
            // 抢占赢下 select 之前这次请求不该返回。
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            panic!("preemption must cancel this request");
        }
        Ok(Completion {
            reasoning: None,
            content: "picked up after the interrupt".to_owned(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".to_owned()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn host_preemption_cancels_the_request_and_keeps_the_transcript() {
    let kernel = crate::kernel::EventKernel::new();
    let provider = Arc::new(SlowThenFastProvider {
        kernel: kernel.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let sink = Arc::new(RecordingSink::default());
    let agent = kernel_agent(provider.clone(), kernel.clone()).with_event_sink(sink.clone());

    let outcome = agent.run("long job").await.expect("run continues");
    assert_eq!(outcome.final_text, "picked up after the interrupt");
    // 第一轮被取消，第二轮才出结果——转录没被回滚，只是那一步作废。
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let events = sink.events.lock().expect("events");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnPreempted { .. })),
        "抢占要让用户看见，否则界面上就是无故停顿"
    );
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
async fn snapshot_failure_stops_before_provider_but_unsupported_workspaces_can_answer() {
    let provider = RecordingProvider::new(&["answer"]);
    let config = AgentConfig {
        max_turns: 2,
        system_prompt: "system".into(),
        context_window: 128_000,
        token_budget: None,
    };
    let agent = Agent::new(
        provider.clone(),
        registry("snapshot-error")
            .with_fallible_verification_snapshot(|| Err("unreadable repository".into())),
        config.clone(),
    );
    assert!(matches!(
        agent.run("inspect").await,
        Err(AgentError::VerificationSnapshot(_))
    ));
    assert!(provider.requests.lock().unwrap().is_empty());
    let agent = Agent::new(
        provider.clone(),
        registry("snapshot-unsupported").with_fallible_verification_snapshot(|| Ok(None)),
        config,
    );
    let outcome = agent.run("answer a question").await.unwrap();
    assert_eq!(outcome.stop_reason, AgentStopReason::Finished);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
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
async fn goal_envelope_activates_continuation_in_the_core_runtime() {
    let provider = RecordingProvider::new(&[
        "I have only inspected the first part.",
        "<goal-status>complete</goal-status> Everything is verified.",
    ]);
    let continuation = Arc::new(GoalContinuation::new());
    let agent = Agent::new(
        provider.clone(),
        registry("goal-envelope"),
        AgentConfig {
            max_turns: 3,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_goal_continuation(continuation.clone());

    let outcome = agent
        .run("<goal>\nship the runtime\n</goal>\nContinue until this goal is genuinely complete.\n\ninspect the queue")
        .await
        .expect("goal should continue until the marker");

    assert_eq!(outcome.turns, 2);
    assert_eq!(outcome.stop_reason, AgentStopReason::GoalComplete);
    assert!(!continuation.is_active());
    assert_eq!(provider.requests.lock().expect("requests").len(), 2);
}

#[tokio::test]
async fn deep_spawn_is_refused_before_provider_work_without_a_ticket() {
    let provider = RecordingProvider::new(&["unused"]);
    let root = std::env::temp_dir().join(format!("willdeep-deep-gate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("workspace");
    let catalog = Arc::new(SubagentCatalog::new(
        &root,
        crate::subagent::builtin_profiles(provider.clone()),
        Arc::new(BackgroundTaskRegistry::default()),
    ));
    let agent = Agent::new(
        provider,
        registry("deep-gate"),
        AgentConfig {
            max_turns: 2,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_subagents(catalog)
    .with_routing_guard(Arc::new(RoutingGuard::new(Default::default())));
    let call = ToolCall {
        id: "deep-1".to_owned(),
        name: "spawn_agent".to_owned(),
        arguments: serde_json::json!({
            "profile": "deep",
            "prompt": "inspect everything"
        })
        .to_string(),
    };

    let error = agent
        .execute_tool(&call)
        .await
        .expect_err("deep without a ticket must be rejected");

    assert!(error.to_string().contains("deep requires escalation"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn rejected_write_packet_does_not_unlock_deep() {
    let provider = RecordingProvider::new(&["unused"]);
    let root = std::env::temp_dir().join(format!(
        "willdeep-invalid-packet-deep-gate-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).expect("workspace");
    let catalog = Arc::new(SubagentCatalog::new(
        &root,
        crate::subagent::builtin_profiles(provider.clone()),
        Arc::new(BackgroundTaskRegistry::default()),
    ));
    let agent = Agent::new(
        provider,
        registry("invalid-packet-deep-gate"),
        AgentConfig {
            max_turns: 2,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    )
    .with_subagents(catalog)
    .with_routing_guard(Arc::new(RoutingGuard::new(Default::default())));
    let invalid_implementer = ToolCall {
        id: "implementer-invalid".to_owned(),
        name: "spawn_agent".to_owned(),
        arguments: serde_json::json!({
            "profile": "implementer",
            "prompt": "change the implementation"
        })
        .to_string(),
    };
    let deep = ToolCall {
        id: "deep-after-invalid".to_owned(),
        name: "spawn_agent".to_owned(),
        arguments: serde_json::json!({
            "profile": "deep",
            "prompt": "inspect everything",
            "escalation": {
                "reason": "cross-module invariants still conflict",
                "attempted_profiles": ["implementer"],
                "context_evidence": "twenty modules remain coupled after slicing",
                "why_not_decompose": "the same invariant must be proven across every module"
            }
        })
        .to_string(),
    };

    let packet_error = agent
        .execute_tool(&invalid_implementer)
        .await
        .expect_err("writing profile without targets must be rejected");
    assert!(packet_error.to_string().contains("files declared up front"));
    let deep_error = agent
        .execute_tool(&deep)
        .await
        .expect_err("rejected task packet must not count as lower-tier evidence");
    assert!(
        deep_error
            .to_string()
            .contains("runtime observed neither a lower-tier worker attempt")
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn repeated_goal_text_does_not_poison_the_current_route() {
    let message = "<goal>\nimplement the entire product\n</goal>\nContinue until this goal is genuinely complete.\n\n定位登录检查在哪个文件";
    assert_eq!(
        routing_request_from_message(message),
        "定位登录检查在哪个文件"
    );
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
async fn persisted_orphan_tool_results_are_removed_before_provider_replay() {
    let provider = RecordingProvider::new(&["recovered"]);
    let agent = Agent::new(
        provider.clone(),
        registry("orphan-tool-history"),
        AgentConfig {
            max_turns: 1,
            system_prompt: "system".to_owned(),
            context_window: 128_000,
            token_budget: None,
        },
    );
    let history = vec![
        Message::user("old request"),
        Message::assistant("", Vec::new()),
        Message {
            role: crate::types::Role::Tool,
            source: None,
            content: "legacy output".to_owned(),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        },
        Message::assistant("old answer", Vec::new()),
    ];

    let outcome = agent
        .run_with_history(history, "continue")
        .await
        .expect("malformed persisted history should recover");

    let requests = provider.requests.lock().expect("requests");
    assert!(
        requests[0]
            .iter()
            .all(|message| message.role != crate::types::Role::Tool)
    );
    assert!(
        outcome
            .messages
            .iter()
            .all(|message| message.role != crate::types::Role::Tool)
    );
    assert_eq!(outcome.final_text, "recovered");
}
