//! `send_agent_message` / `stop_agent`：主 Agent 指挥自己起的后台子 Agent。
//!
//! 不弹审批（等同于在自己的子任务里改主意），但每次调用都写审批审计。子 Agent
//! 没有子 Agent 目录，调这两个工具只会得到 unknown tool。

use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    agent_id: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopArgs {
    agent_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AwaitArgs {
    #[serde(default)]
    agent_ids: Vec<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn parse<T: serde::de::DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArguments {
        tool: call.name.clone(),
        source,
    })
}

impl Agent {
    /// `await_agents`：等本会话的后台子 Agent，把报告一次交回。
    pub(super) async fn execute_await_agents(&self, call: &ToolCall) -> Result<String, ToolError> {
        let Some(catalog) = &self.subagents else {
            return Err(ToolError::UnknownTool(call.name.clone()));
        };
        let args = parse::<AwaitArgs>(call)?;
        let seconds = args
            .timeout_seconds
            .unwrap_or(crate::tools::DEFAULT_AWAIT_SECONDS)
            .clamp(1, crate::tools::MAX_AWAIT_SECONDS);
        catalog
            .await_agents(&args.agent_ids, std::time::Duration::from_secs(seconds))
            .await
            .map_err(|message| ToolError::InvalidArguments {
                tool: call.name.clone(),
                source: <serde_json::Error as serde::de::Error>::custom(message),
            })
    }

    pub(super) fn execute_agent_control_tool(
        &self,
        call: &ToolCall,
    ) -> Option<Result<String, ToolError>> {
        if !matches!(call.name.as_str(), "send_agent_message" | "stop_agent") {
            return None;
        }
        let Some(catalog) = &self.subagents else {
            return Some(Err(ToolError::UnknownTool(call.name.clone())));
        };
        Some(if call.name == "send_agent_message" {
            parse::<SendArgs>(call).and_then(|args| {
                let chars = args.message.chars().count();
                let outcome = catalog.send_agent_message(&args.agent_id, &args.message);
                self.audit(
                    &call.name,
                    &args.agent_id,
                    &outcome,
                    &format!("{chars} chars"),
                );
                outcome
                    .map(|agent_id| {
                        format!(
                            "Message queued for background agent {agent_id}; it takes effect at the child's next turn boundary."
                        )
                    })
                    .map_err(ToolError::Network)
            })
        } else {
            parse::<StopArgs>(call).and_then(|args| {
                let outcome = catalog.stop_agent(&args.agent_id);
                self.audit(&call.name, &args.agent_id, &outcome, "stop requested");
                outcome
                    .map(|agent_id| {
                        format!(
                            "Stop requested for background agent {agent_id}; its report will still be delivered with status killed."
                        )
                    })
                    .map_err(ToolError::Network)
            })
        })
    }

    /// 审计里只记动作、目标与结果，不记消息正文。
    fn audit(
        &self,
        tool: &str,
        agent_ref: &str,
        outcome: &Result<uuid::Uuid, String>,
        detail: &str,
    ) {
        let result = match outcome {
            Ok(_) => "delivered".to_owned(),
            Err(error) => format!("refused: {error}"),
        };
        self.tools.audit_agent_control(
            &format!("{tool} {}", agent_ref.trim()),
            format!("no approval required for this session's own background subagent; {detail}; {result}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::BackgroundTaskStatus;
    use crate::provider::ProviderError;
    use crate::tools::{ApprovalMode, ApprovalSource, ApprovalTrace};
    use crate::types::{Completion, ToolDefinition};

    /// 子 Agent 的模型：第一轮等父 Agent 放行后要一次工具，第二轮把看到的
    /// 对话记下来收尾。
    struct GatedChild {
        release: Arc<AtomicBool>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Provider for GatedChild {
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            let transcript = messages
                .iter()
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let first = {
                let mut seen = self.seen.lock().unwrap();
                seen.push(transcript);
                seen.len() == 1
            };
            if first {
                while !self.release.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                return Ok(Completion {
                    content: String::new(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "look".into(),
                        name: "list_directory".into(),
                        arguments: "{}".into(),
                    }],
                    finish_reason: Some("tool_calls".into()),
                    usage: None,
                });
            }
            Ok(Completion {
                content: "child report".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: Some("stop".into()),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn a_message_reaches_the_childs_next_round_and_is_audited() {
        let root =
            std::env::temp_dir().join(format!("willdeep-messaging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let child: Arc<dyn Provider> = Arc::new(GatedChild {
            release: release.clone(),
            seen: seen.clone(),
        });
        let background = Arc::new(BackgroundTaskRegistry::default());
        let mut reports = background.subscribe();
        let catalog = SubagentCatalog::new(
            &root,
            crate::subagent::builtin_profiles(child.clone()),
            background.clone(),
        );
        let traces = Arc::new(Mutex::new(Vec::<ApprovalTrace>::new()));
        let recorded = traces.clone();
        let tools = ToolRegistry::new(&root, ApprovalMode::Strict)
            .unwrap()
            .with_background_tasks(background.clone())
            .with_approval_reporter(move |trace| recorded.lock().unwrap().push(trace));
        let agent = Agent::new(
            child,
            tools,
            AgentConfig {
                max_turns: 4,
                system_prompt: "parent".into(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_subagents(Arc::new(catalog));

        let started = agent
            .execute_tool(&ToolCall {
                id: "spawn".into(),
                name: "spawn_agent".into(),
                arguments: serde_json::json!({"prompt":"inspect the tree","profile":"scout","run_in_background":true}).to_string(),
            })
            .await
            .expect("spawn background child");
        let agent_id = started
            .split("agent_id=")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .expect("agent id")
            .to_owned();
        // 等子 Agent 进入第一轮请求再发，消息才一定落在「下一轮」。
        while seen.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let send = |message: String| ToolCall {
            id: "msg".into(),
            name: "send_agent_message".into(),
            arguments: serde_json::json!({"agent_id": agent_id, "message": message}).to_string(),
        };
        let too_long = agent
            .execute_tool(&send("x".repeat(crate::tools::MAX_AGENT_MESSAGE_CHARS + 1)))
            .await
            .unwrap_err();
        assert!(too_long.to_string().contains("never truncated"));
        agent
            .execute_tool(&send("PARENT-SAYS: also read Cargo.toml".into()))
            .await
            .expect("message queued");
        release.store(true, Ordering::SeqCst);

        let report = reports.recv().await.unwrap();
        assert_eq!(report.snapshot.status, BackgroundTaskStatus::Completed);
        let seen = seen.lock().unwrap().clone();
        assert!(!seen[0].contains("PARENT-SAYS"));
        assert!(
            seen[1].contains("Additional instructions from the parent Agent:\n\nPARENT-SAYS: also read Cargo.toml"),
            "{seen:#?}"
        );

        let stop = agent
            .execute_tool(&ToolCall {
                id: "stop".into(),
                name: "stop_agent".into(),
                arguments: serde_json::json!({"agent_id": agent_id}).to_string(),
            })
            .await
            .unwrap_err();
        assert!(stop.to_string().contains("already finished"));

        let traces = traces.lock().unwrap().clone();
        let audited = traces
            .iter()
            .filter(|trace| trace.source == ApprovalSource::NotRequired)
            .collect::<Vec<_>>();
        assert_eq!(audited.len(), 3, "{traces:#?}");
        assert!(audited[1].detail.ends_with("delivered"));
        assert!(
            !audited
                .iter()
                .any(|trace| trace.detail.contains("PARENT-SAYS"))
        );
        assert!(audited[2].command.starts_with("stop_agent "));
        std::fs::remove_dir_all(root).ok();
    }

    /// 所有子 Agent 都卡在第一轮，直到放行后一起交报告。
    struct HeldChildren {
        release: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Provider for HeldChildren {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            while !self.release.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            Ok(Completion {
                content: "CONCLUSION: looked around".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: Some("stop".into()),
                usage: None,
            })
        }
    }

    /// 并行派工的汇合：两个后台 Worker 同时跑，`await_agents` 超时时如实列出
    /// 还在跑的；放行后一次拿回两份报告（带运行时尾注），且这两份报告不再
    /// 作为完成通知投第二遍。
    #[tokio::test]
    async fn await_agents_joins_parallel_background_workers() {
        let root = std::env::temp_dir().join(format!("willdeep-await-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let child: Arc<dyn Provider> = Arc::new(HeldChildren {
            release: release.clone(),
        });
        let background = Arc::new(BackgroundTaskRegistry::default());
        let catalog = SubagentCatalog::new(
            &root,
            crate::subagent::builtin_profiles(child.clone()),
            background.clone(),
        );
        let tools = ToolRegistry::new(&root, ApprovalMode::Strict)
            .unwrap()
            .with_background_tasks(background.clone());
        let agent = Agent::new(
            child,
            tools,
            AgentConfig {
                max_turns: 4,
                system_prompt: "parent".into(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_subagents(Arc::new(catalog));
        let call = |name: &str, arguments: serde_json::Value| ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            arguments: arguments.to_string(),
        };
        let mut agent_ids = Vec::new();
        for prompt in ["inspect src", "inspect docs"] {
            let started = agent
                .execute_tool(&call(
                    "spawn_agent",
                    serde_json::json!({"prompt": prompt, "profile": "scout", "run_in_background": true}),
                ))
                .await
                .expect("spawn background child");
            agent_ids.push(
                started
                    .split("agent_id=")
                    .nth(1)
                    .and_then(|rest| rest.split(',').next())
                    .expect("agent id")
                    .to_owned(),
            );
        }

        let waited = agent
            .execute_tool(&call(
                "await_agents",
                serde_json::json!({"timeout_seconds": 1}),
            ))
            .await
            .expect("await with timeout");
        assert!(waited.contains("Timed out"), "{waited}");
        for id in &agent_ids {
            assert!(waited.contains(id.as_str()), "{waited}");
        }

        release.store(true, Ordering::SeqCst);
        let joined = agent
            .execute_tool(&call(
                "await_agents",
                serde_json::json!({"agent_ids": agent_ids, "timeout_seconds": 30}),
            ))
            .await
            .expect("join");
        assert_eq!(joined.matches("<agent-report").count(), 2, "{joined}");
        assert_eq!(joined.matches("CONCLUSION: looked around").count(), 2);
        assert_eq!(joined.matches("<worker-facts").count(), 2);
        assert!(!joined.contains("Timed out"));
        assert!(
            background.drain_pending().is_empty(),
            "reports handed over by await_agents are not delivered again"
        );

        let nothing = agent
            .execute_tool(&call("await_agents", serde_json::json!({})))
            .await
            .expect("nothing running");
        assert!(nothing.contains("nothing to wait for"), "{nothing}");
        assert!(
            agent
                .execute_tool(&call(
                    "await_agents",
                    serde_json::json!({"agent_ids": ["agent_zzzzzz"]})
                ))
                .await
                .is_err(),
            "ids this session did not start are not found"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 第一个 Worker 往黑板写一条发现；第二个 Worker 的任务简报里就有它。
    struct BoardChildren {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Provider for BoardChildren {
        async fn complete(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
        ) -> Result<Completion, ProviderError> {
            let transcript = messages
                .iter()
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let offered = tools.iter().any(|tool| tool.name == "board_post");
            self.seen
                .lock()
                .unwrap()
                .push(format!("offered={offered}\n{transcript}"));
            let posted = messages
                .iter()
                .any(|message| message.content.starts_with("Posted to the shared board"));
            if transcript.contains("map the config") && !posted {
                return Ok(Completion {
                    content: String::new(),
                    reasoning: None,
                    tool_calls: vec![ToolCall {
                        id: "post".into(),
                        name: "board_post".into(),
                        arguments:
                            r#"{"kind":"fact","text":"config is loaded in src/config.rs:42"}"#
                                .into(),
                    }],
                    finish_reason: Some("tool_calls".into()),
                    usage: None,
                });
            }
            Ok(Completion {
                content: "CONCLUSION: done".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: Some("stop".into()),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn workers_share_findings_through_the_board() {
        let root = std::env::temp_dir().join(format!("willdeep-board-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let child: Arc<dyn Provider> = Arc::new(BoardChildren { seen: seen.clone() });
        let board = Arc::new(crate::board::Board::in_memory());
        let background = Arc::new(BackgroundTaskRegistry::default());
        let catalog = SubagentCatalog::new(
            &root,
            crate::subagent::builtin_profiles(child.clone()),
            background.clone(),
        )
        .with_board(board.clone());
        let tools = ToolRegistry::new(&root, ApprovalMode::Strict)
            .unwrap()
            .with_background_tasks(background.clone())
            .with_board(board.clone(), "parent");
        let agent = Agent::new(
            child,
            tools,
            AgentConfig {
                max_turns: 4,
                system_prompt: "parent".into(),
                context_window: 128_000,
                token_budget: None,
            },
        )
        .with_subagents(Arc::new(catalog));
        let call = |name: &str, arguments: serde_json::Value| ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            arguments: arguments.to_string(),
        };

        let first = agent
            .execute_tool(&call(
                "spawn_agent",
                serde_json::json!({"prompt": "map the config", "profile": "scout", "run_in_background": true}),
            ))
            .await
            .expect("spawn first");
        assert!(first.contains("agent_id="));
        let joined = agent
            .execute_tool(&call(
                "await_agents",
                serde_json::json!({"timeout_seconds": 30}),
            ))
            .await
            .expect("join first");
        assert!(joined.contains("New notes on the shared board"), "{joined}");
        assert!(joined.contains("[fact] worker:scout:"), "{joined}");
        assert!(joined.contains("src/config.rs:42"));

        agent
            .execute_tool(&call(
                "spawn_agent",
                serde_json::json!({"prompt": "fix the loader", "profile": "scout"}),
            ))
            .await
            .expect("second worker");
        {
            let transcripts = seen.lock().unwrap();
            let second = transcripts
                .iter()
                .find(|text| text.contains("fix the loader"))
                .expect("second worker ran");
            assert!(
                second.starts_with("offered=true"),
                "workers get the board tools"
            );
            assert!(
                second.contains("<board note="),
                "the brief carries the board"
            );
            assert!(second.contains("src/config.rs:42"));
        }

        let read = agent
            .execute_tool(&call("board_read", serde_json::json!({"since_seq": 0})))
            .await
            .expect("parent reads");
        assert!(read.contains("src/config.rs:42"));
        let plain = ToolRegistry::new(&root, ApprovalMode::Strict).unwrap();
        assert!(
            !plain
                .definitions()
                .iter()
                .any(|tool| tool.name == "board_post"),
            "no board, no board tools"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
