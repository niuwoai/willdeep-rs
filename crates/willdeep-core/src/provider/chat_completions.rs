use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::common::{client, decode_success, endpoint, openai_auth};
use super::{Provider, ProviderConfig, ProviderError};
use crate::types::{Completion, Message, MessageAttachment, Role, ToolCall, ToolDefinition, Usage};

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
        let wire_messages = messages.iter().map(WireMessage::from).collect::<Vec<_>>();
        let wire_tools = tools.iter().map(WireTool::from).collect::<Vec<_>>();
        let body = ChatRequest {
            model: &self.config.model,
            messages: &wire_messages,
            tools: &wire_tools,
            tool_choice: (!tools.is_empty()).then_some("auto"),
            stream: false,
        };
        let request =
            openai_auth(self.client.post(self.endpoint.clone()), &self.config).json(&body);
        let bytes = decode_success(request.send().await?, &self.config).await?;
        let response: ChatResponse = serde_json::from_slice(&bytes)
            .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::EmptyResponse)?;
        Ok(Completion {
            content: choice.message.content.unwrap_or_default(),
            tool_calls: choice
                .message
                .tool_calls
                .into_iter()
                .map(|call| ToolCall {
                    id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                })
                .collect(),
            finish_reason: choice.finish_reason,
            usage: response.usage.map(|usage| Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            }),
        })
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
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: serde_json::Value,
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
            tool_call_id: &message.tool_call_id,
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| WireOutgoingToolCall {
                    id: &call.id,
                    kind: "function",
                    function: WireOutgoingFunction {
                        name: &call.name,
                        arguments: &call.arguments,
                    },
                })
                .collect(),
        }
    }
}

fn chat_content(message: &Message) -> serde_json::Value {
    if message.attachments.is_empty() {
        return serde_json::Value::String(message.content.clone());
    }
    let mut parts = vec![serde_json::json!({"type":"text","text":message.content})];
    for attachment in &message.attachments {
        match attachment {
            MessageAttachment::Text { name, content } => parts.push(serde_json::json!({"type":"text","text":format!("[Pasted text: {name}]\n{content}")})),
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
    arguments: &'a str,
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
}
