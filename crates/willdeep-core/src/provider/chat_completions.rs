use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::common::{client, endpoint, openai_auth, send_retrying};
use super::{Provider, ProviderConfig, ProviderError};
use crate::types::{Completion, Message, MessageAttachment, Role, ToolCall, ToolDefinition, Usage};
mod streaming;

pub struct ChatCompletionsProvider {
    config: ProviderConfig,
    client: Client,
    endpoint: reqwest::Url,
}

impl ChatCompletionsProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            endpoint: endpoint(&config.base_url, "chat/completions")?,
            client: client(&config)?,
            config,
        })
    }
}

#[async_trait]
impl Provider for ChatCompletionsProvider {
    async fn complete_with_events(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        events: &dyn super::ProviderEventSink,
    ) -> Result<Completion, ProviderError> {
        streaming::complete(self, messages, tools, events).await
    }

    fn with_model(&self, model: &str) -> Result<std::sync::Arc<dyn Provider>, ProviderError> {
        let mut config = self.config.clone();
        config.model = model.to_owned();
        super::build_provider(config)
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let wire_messages = wire_messages(messages);
        let wire_tools = tools.iter().map(WireTool::from).collect::<Vec<_>>();
        let body = ChatRequest {
            model: &self.config.model,
            messages: &wire_messages,
            tools: &wire_tools,
            tool_choice: (!tools.is_empty()).then_some("auto"),
            stream: false,
            reasoning_effort: self.is_auxiliary_or_loopback().then_some("none"),
            think: self.is_auxiliary_or_loopback().then_some(false),
        };
        let request =
            openai_auth(self.client.post(self.endpoint.clone()), &self.config).json(&body);
        let bytes = send_retrying(request, &self.config).await?;
        decode_completion(&bytes)
    }
}

impl ChatCompletionsProvider {
    fn is_auxiliary_or_loopback(&self) -> bool {
        self.config.allow_unauthenticated
            || reqwest::Url::parse(&self.config.base_url)
                .ok()
                .is_some_and(|url| super::is_loopback_base_url(&url))
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [WireMessage<'a>],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: &'a Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: serde_json::Value,
    /// DeepSeek 等 thinking 模型要求上一轮的思维链原样回传，少了这一条，
    /// 只要历史里出现过工具调用，整条请求就是 400。
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireOutgoingToolCall<'a>>,
}

impl<'a> From<&'a Message> for WireMessage<'a> {
    fn from(message: &'a Message) -> Self {
        Self {
            role: match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            },
            content: chat_content(message),
            reasoning_content: message.reasoning.as_deref(),
            tool_call_id: &message.tool_call_id,
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| WireOutgoingToolCall {
                    id: &call.id,
                    kind: "function",
                    function: WireOutgoingFunction {
                        name: &call.name,
                        // 最后一道闸：历史可能来自非流式解码或旧会话导入，
                        // 那些路径没经过流式解码器的 JSON 对象校验。
                        arguments: call.normalized_arguments(),
                    },
                })
                .collect(),
        }
    }
}

/// 带 tools 的 thinking 模式下，上游要求**每一条** assistant 消息都带
/// `reasoning_content`：模型某一步没吐思维链（常见于 `git commit` 这类直给的
/// 工具调用），或是压缩器插入的归档引用/摘要，缺了这个字段整条请求照样 400。
/// 只有历史里真出现过思维链，才能确认对端认这个字段，这时给缺的补空串；
/// 没有证据的端点保持不发，免得严格校验的上游拒绝未知字段。
fn wire_messages(messages: &[Message]) -> Vec<WireMessage<'_>> {
    let backfill = messages.iter().any(|message| message.reasoning.is_some());
    messages
        .iter()
        .map(|message| {
            let mut wire = WireMessage::from(message);
            if backfill && message.role == Role::Assistant && wire.reasoning_content.is_none() {
                wire.reasoning_content = Some("");
            }
            wire
        })
        .collect()
}

fn chat_content(message: &Message) -> serde_json::Value {
    if message.attachments.is_empty() {
        return serde_json::Value::String(message.content.clone());
    }
    let mut parts = vec![serde_json::json!({"type":"text","text":message.content})];
    for attachment in &message.attachments {
        match attachment {
            MessageAttachment::Text { name, content } => parts.push(serde_json::json!({"type":"text","text":super::common::pasted_text_block(name, content)})),
            MessageAttachment::Image { media_type, data, .. } => parts.push(serde_json::json!({"type":"image_url","image_url":{"url":format!("data:{media_type};base64,{data}")}})),
        }
    }
    serde_json::Value::Array(parts)
}

#[derive(Serialize)]
struct WireOutgoingToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireOutgoingFunction<'a>,
}

#[derive(Serialize)]
struct WireOutgoingFunction<'a> {
    name: &'a str,
    arguments: std::borrow::Cow<'a, str>,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunctionDefinition<'a>,
}

impl<'a> From<&'a ToolDefinition> for WireTool<'a> {
    fn from(tool: &'a ToolDefinition) -> Self {
        Self {
            kind: "function",
            function: WireFunctionDefinition {
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.parameters,
            },
        }
    }
}

#[derive(Serialize)]
struct WireFunctionDefinition<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatAssistantMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatAssistantMessage {
    content: Option<String>,
    /// 各家字段名不统一：DeepSeek/GLM/vLLM 用 `reasoning_content`，
    /// 另有网关平铺成 `reasoning`。两个都收，回传时统一成前者。
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireIncomingToolCall>,
}

#[derive(Deserialize)]
struct WireIncomingToolCall {
    id: String,
    function: WireIncomingFunction,
}

#[derive(Deserialize)]
struct WireIncomingFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    /// OpenAI 与多数兼容网关走这里。
    prompt_tokens_details: Option<PromptTokensDetails>,
    /// DeepSeek 等网关把命中数平铺在顶层，字段名也不一样。
    prompt_cache_hit_tokens: Option<u64>,
}

impl ChatUsage {
    fn cache_read_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .or(self.prompt_cache_hit_tokens)
    }
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn image_attachment_becomes_data_url_part() {
        let message = Message::user_with_attachments(
            "look",
            vec![MessageAttachment::Image {
                name: "a.png".into(),
                media_type: "image/png".into(),
                data: "YWJj".into(),
                width: 1,
                height: 1,
            }],
        );
        let value = chat_content(&message);
        assert_eq!(value[1]["type"], "image_url");
        assert_eq!(value[1]["image_url"]["url"], "data:image/png;base64,YWJj");
    }

    /// 粘贴文本必须连正文一起到达，并且说清楚它不是工作区里的文件。
    #[test]
    fn text_attachment_carries_its_content_and_denies_being_a_file() {
        let message = Message::user_with_attachments(
            "see below",
            vec![MessageAttachment::Text {
                name: "paste-1.txt".into(),
                content: "the code word is ZEBRA".into(),
            }],
        );
        let value = chat_content(&message);
        assert_eq!(value[0]["text"], "see below");
        let text = value[1]["text"].as_str().unwrap();
        assert!(text.contains("paste-1.txt"));
        assert!(text.contains("the code word is ZEBRA"));
        assert!(text.contains("not a file in the workspace"));
        assert!(text.ends_with("[End of pasted text \"paste-1.txt\"]"));
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    /// OpenAI 兼容网关各报各的缓存字段，两种写法都得认。
    #[test]
    fn cache_hits_are_read_from_either_wire_shape() {
        let openai: ChatUsage = serde_json::from_str(
            r#"{"prompt_tokens":1000,"completion_tokens":20,"total_tokens":1020,
                "prompt_tokens_details":{"cached_tokens":768}}"#,
        )
        .expect("openai usage");
        assert_eq!(openai.cache_read_tokens(), Some(768));

        let deepseek: ChatUsage = serde_json::from_str(
            r#"{"prompt_tokens":1000,"completion_tokens":20,"total_tokens":1020,
                "prompt_cache_hit_tokens":512}"#,
        )
        .expect("deepseek usage");
        assert_eq!(deepseek.cache_read_tokens(), Some(512));

        let silent: ChatUsage =
            serde_json::from_str(r#"{"prompt_tokens":1000,"completion_tokens":20}"#)
                .expect("silent usage");
        assert_eq!(
            silent.cache_read_tokens(),
            None,
            "a gateway that says nothing must not be reported as a cache miss"
        );
    }
}

fn decode_completion(bytes: &[u8]) -> Result<Completion, ProviderError> {
    let response: ChatResponse = serde_json::from_slice(bytes)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(ProviderError::EmptyResponse)?;
    Ok(Completion {
        content: choice.message.content.unwrap_or_default(),
        reasoning: choice
            .message
            .reasoning_content
            .or(choice.message.reasoning)
            .filter(|value| !value.is_empty()),
        tool_calls: choice
            .message
            .tool_calls
            .into_iter()
            .map(|call| {
                let mut call = ToolCall {
                    id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                };
                // 非流式分支没有流式解码器那道校验，这里补上。
                call.normalize_arguments();
                call
            })
            .collect(),
        finish_reason: choice.finish_reason,
        usage: response.usage.map(|usage| Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cache_read_tokens: usage.cache_read_tokens(),
        }),
    })
}
