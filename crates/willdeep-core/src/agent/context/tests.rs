use super::*;
use std::sync::Mutex;

mod usage_tests;

#[tokio::test]
async fn compression_preserves_unknown_effects_and_their_replay_guard() {
    let (agent, _) = agent(8192);
    let pending = ToolCall {
        id: "uncertain-write".into(),
        name: "run_command".into(),
        arguments: r#"{"command":"external-operation"}"#.into(),
    };
    let mut original = history();
    original.insert(2, Message::assistant("interrupted", vec![pending.clone()]));
    // Compress directly after interruption, before run_inner has repaired pairs.
    let mut manual = agent.compress_history(original).await.unwrap();
    assert_pairs(&manual);
    let guard = uncertain::UncertainCalls::recover(&mut manual);
    assert!(guard.needs_approval(&pending));
    assert!(manual.iter().any(|m| {
        m.tool_calls
            .iter()
            .any(|call| call.id == pending.id && call.arguments == pending.arguments)
    }));

    // Even the last-resort archive pass cannot remove recovery constraints.
    agent.archive_old_batches(&mut manual, 1).unwrap();
    assert_pairs(&manual);
    let guard = uncertain::UncertainCalls::recover(&mut manual);
    assert!(guard.needs_approval(&pending));
    assert!(uncertain::has_unknown_effect(&manual));
}

#[tokio::test]
async fn automatic_compression_keeps_unknown_call_parameters() {
    let (agent, _) = agent(8192);
    let pending = ToolCall {
        id: "unknown-command".into(),
        name: "run_command".into(),
        arguments: r#"{"command":"external-operation"}"#.into(),
    };
    let mut messages = history();
    messages.insert(2, Message::assistant("interrupted", vec![pending.clone()]));
    uncertain::UncertainCalls::recover(&mut messages);
    let mut compressed = agent.request_messages(&messages, &mut None).await.unwrap();
    assert_pairs(&compressed);
    assert!(uncertain::UncertainCalls::recover(&mut compressed).needs_approval(&pending));
}

struct CaptureProvider {
    requests: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
    ) -> Result<crate::types::Completion, ProviderError> {
        self.requests.lock().unwrap().push(messages.to_vec());
        Ok(crate::types::Completion {
            reasoning: None,
            content: "OBJECTIVE: repair bug. USER CONSTRAINTS: preserve dirty files. COMPLETED: inspected. REMAINING: validate.".to_owned(),
            tool_calls: Vec::new(), finish_reason: Some("stop".to_owned()), usage: None,
        })
    }
}

fn agent(window: u64) -> (Agent, Arc<CaptureProvider>) {
    let workspace = std::env::temp_dir().join(format!("context-tests-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let tools = ToolRegistry::new(workspace, crate::ApprovalMode::Strict)
        .unwrap()
        .with_allowed_tools(["read_file".to_owned()]);
    (
        Agent::new(
            provider.clone(),
            tools,
            AgentConfig {
                max_turns: 3,
                system_prompt: "system".to_owned(),
                context_window: window,
                token_budget: None,
            },
        ),
        provider,
    )
}

fn history() -> Vec<Message> {
    let mut messages = vec![
        Message::system("Never expose credentials"),
        Message::user("Fix login; preserve my dirty config; do not push"),
    ];
    for index in 0..20 {
        let call = ToolCall {
            id: format!("call-{index}"),
            name: "read_file".to_owned(),
            arguments: format!(r#"{{"path":"src/file-{index}.rs"}}"#),
        };
        messages.push(Message::assistant("inspect", vec![call.clone()]));
        messages.push(Message::tool(
            &call,
            format!("file-{index}: {}", "x".repeat(1_900)),
        ));
    }
    messages.push(Message::user("continue with the original constraints"));
    messages
}

fn assert_pairs(messages: &[Message]) {
    let mut pending = std::collections::HashSet::new();
    for message in messages {
        match message.role {
            Role::Assistant => {
                assert!(pending.is_empty());
                pending.extend(message.tool_calls.iter().map(|call| call.id.as_str()));
            }
            Role::Tool => {
                assert!(pending.remove(message.tool_call_id.as_deref().unwrap()));
            }
            _ => assert!(pending.is_empty()),
        }
    }
    assert!(pending.is_empty());
}

#[tokio::test]
async fn automatic_compression_preserves_constraints_and_tool_protocol() {
    let (agent, provider) = agent(8_192);
    let messages = history();
    let result = agent.request_messages(&messages, &mut None).await.unwrap();
    assert!(
        result
            .iter()
            .any(|message| message.content.contains("<context-summary"))
    );
    for protected in messages
        .iter()
        .filter(|message| matches!(message.role, Role::System | Role::User))
    {
        assert!(
            result
                .iter()
                .any(|message| message.role == protected.role
                    && message.content == protected.content)
        );
    }
    assert_pairs(&result);
    let requests = provider.requests.lock().unwrap();
    assert!(requests[0][0].content.contains("src/file-0.rs"));
    assert!(requests[0][0].content.contains("call-0"));
    assert!(requests[0][0].content.contains("UNVERIFIED CLAIMS"));
    assert_eq!(
        messages.len(),
        43,
        "automatic compression must not mutate durable history"
    );
}

#[tokio::test]
async fn oversized_instructions_are_rejected_instead_of_silently_cropped() {
    let (agent, provider) = agent(4_096);
    for role in [Role::System, Role::User] {
        let mut message = Message::user(format!(
            "HEAD{}MUST NOT DELETE CONFIG{}TAIL",
            "x".repeat(10_000),
            "y".repeat(10_000)
        ));
        message.role = role;
        assert!(matches!(
            agent.request_messages(&[message], &mut None).await,
            Err(AgentError::ContextCapacity { .. })
        ));
    }
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oversized_tool_middle_is_retrievable_without_a_summary_request() {
    let (agent, provider) = agent(8_192);
    let call = ToolCall {
        id: "call".to_owned(),
        name: "read_file".to_owned(),
        arguments: "{}".to_owned(),
    };
    let original = format!(
        "{}IMPORTANT-MIDDLE{}",
        "h".repeat(20_000),
        "t".repeat(20_000)
    );
    let messages = vec![
        Message::system("system"),
        Message::user("inspect"),
        Message::assistant("", vec![call.clone()]),
        Message::tool(&call, &original),
    ];
    let result = agent.request_messages(&messages, &mut None).await.unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
    let id = agent.tools.archive_output(&original).unwrap();
    assert!(result[3].content.contains(&id));
    let page = agent
        .tools
        .execute(&ToolCall {
            id: "page".to_owned(),
            name: "read_tool_output".to_owned(),
            arguments: serde_json::json!({"id":id,"offset":20_000,"limit":16}).to_string(),
        })
        .await
        .unwrap();
    assert!(page.ends_with("IMPORTANT-MIDDLE"));
    assert_pairs(&result);
}

#[tokio::test]
async fn manual_compression_keeps_user_instructions_and_whole_tool_batches() {
    let (agent, _) = agent(8_192);
    let original = history();
    let compressed = agent.compress_history(original.clone()).await.unwrap();
    assert!(compressed.len() < original.len());
    assert_pairs(&compressed);
    assert!(
        compressed
            .iter()
            .any(|message| message.content == original[1].content)
    );
    assert_eq!(
        compressed.last().unwrap().content,
        original.last().unwrap().content
    );
}

#[test]
fn estimates_include_tool_arguments_text_attachments_and_non_ascii() {
    let plain = Message::assistant("", Vec::new());
    let mut tool = plain.clone();
    tool.tool_calls.push(ToolCall {
        id: "x".to_owned(),
        name: "create_file".to_owned(),
        arguments: "x".repeat(4_000),
    });
    assert!(estimate_tokens(&[tool]) >= estimate_tokens(&[plain]) + 1_000);
    let attached = Message::user_with_attachments(
        "",
        vec![MessageAttachment::Text {
            name: "paste".to_owned(),
            content: "约束".repeat(1_000),
        }],
    );
    assert!(estimate_tokens(&[attached]) >= 4_000);
    let definition = ToolDefinition {
        name: "large".to_owned(),
        description: "x".repeat(8_000),
        parameters: serde_json::json!({}),
    };
    assert!(message_capacity(1_000, &[definition]).is_err());
}

#[tokio::test]
async fn compression_starts_at_the_reserved_capacity_watermark_even_for_short_histories() {
    let (agent, provider) = agent(8_192);
    let capacity =
        message_capacity(agent.config.context_window, &agent.tools.definitions()).unwrap();
    let payload_tokens = capacity * COMPRESSION_TRIGGER_PERCENT / 100;
    let messages = vec![
        Message::system("system"),
        Message::user("preserve constraints"),
        Message::assistant("x".repeat(payload_tokens as usize * 4), Vec::new()),
        Message::user("continue"),
    ];
    agent.request_messages(&messages, &mut None).await.unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let (agent, provider) = self::agent(8_192);
    agent
        .request_messages(
            &[Message::system("system"), Message::user("short question")],
            &mut None,
        )
        .await
        .unwrap();
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn urgent_recent_batches_are_archived_as_groups() {
    let (agent, _) = agent(4_096);
    let mut messages = history();
    let removed = agent.archive_old_batches(&mut messages, 600).unwrap();
    assert!(removed > 0);
    assert_pairs(&messages);
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("preserve my dirty config"))
    );
    assert_eq!(
        messages.last().unwrap().content,
        "continue with the original constraints"
    );
}

#[tokio::test]
async fn hosted_compressor_receives_structured_transcript_without_duplicate_prompt() {
    let (mut agent, _) = agent(8_192);
    let compressor = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    agent = agent.with_compressor(compressor.clone(), true);
    agent.request_messages(&history(), &mut None).await.unwrap();
    let requests = compressor.requests.lock().unwrap();
    assert!(!requests[0][0].content.contains("Summarize this older"));
    let transcript: Vec<Message> = serde_json::from_str(&requests[0][0].content).unwrap();
    assert!(
        transcript
            .iter()
            .any(|message| !message.tool_calls.is_empty())
    );
}
