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

fn parse<T: serde::de::DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_str(&call.arguments).map_err(|source| ToolError::InvalidArguments {
        tool: call.name.clone(),
        source,
    })
}

impl Agent {
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
}
