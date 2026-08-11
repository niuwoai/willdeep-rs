use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::common::{anthropic_auth, anthropic_endpoint, client, decode_success};
use super::{Provider, ProviderConfig, ProviderError};
use crate::types::{Completion, Message, MessageAttachment, Role, ToolCall, ToolDefinition, Usage};

pub struct AnthropicMessagesProvider {
    config: ProviderConfig,
    client: Client,
    endpoint: reqwest::Url,
}

impl AnthropicMessagesProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            endpoint: anthropic_endpoint(&config.base_url)?,
            client: client(&config)?,
            config,
        })
    }
}

#[async_trait]
impl Provider for AnthropicMessagesProvider {
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
        let (system, wire_messages) = encode_messages(messages);
        let wire_tools = tools.iter().map(AnthropicTool::from).collect::<Vec<_>>();
        let body = AnthropicRequest {
            model: &self.config.model,
            max_tokens: max_output_tokens(&self.config.model, self.config.max_output_tokens),
            system,
            messages: wire_messages,
            tools: wire_tools,
            stream: false,
        };
        let request =
            anthropic_auth(self.client.post(self.endpoint.clone()), &self.config).json(&body);
        let bytes = decode_success(request.send().await?, &self.config).await?;
        let response: AnthropicResponse = serde_json::from_slice(&bytes)
            .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
        let mut text = Vec::new();
        let mut tool_calls = Vec::new();
        for block in response.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(value) = block.text {
                        text.push(value);
                    }
                }
                "tool_use" => tool_calls.push(ToolCall {
                    id: block.id.unwrap_or_else(|| "tool_unknown".to_owned()),
                    name: block.name.unwrap_or_default(),
                    arguments: serde_json::to_string(&block.input.unwrap_or_default())
                        .unwrap_or_else(|_| "{}".to_owned()),
                }),
                _ => {}
            }
        }
        if text.is_empty() && tool_calls.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(Completion {
            content: text.join(""),
            tool_calls,
            finish_reason: response.stop_reason,
            usage: response.usage.map(|usage| Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: match (usage.input_tokens, usage.output_tokens) {
                    (Some(input), Some(output)) => Some(input + output),
                    _ => None,
                },
            }),
        })
    }
}

fn max_output_tokens(model: &str, configured: u32) -> u32 {
    let model = model.to_ascii_lowercase();
    if model.contains("claude-3-opus")
        || model.contains("claude-3-haiku")
        || model.contains("claude-3-sonnet")
    {
        configured.min(4_096)
    } else if model.contains("claude-3-5")
        || model.contains("claude-3.5")
        || model.contains("claude-3-7")
    {
        configured.min(8_192)
    } else {
        configured
    }
}

fn encode_messages(messages: &[Message]) -> (String, Vec<AnthropicMessage>) {
    let mut system = Vec::new();
    let mut output = Vec::new();
    let mut pending_tool_results = Vec::new();
    for message in messages {
        match message.role {
            Role::System => system.push(message.content.clone()),
            Role::Tool => pending_tool_results.push(AnthropicContent::ToolResult {
                tool_use_id: message.tool_call_id.clone().unwrap_or_default(),
                content: message.content.clone(),
            }),
            Role::User => {
                let mut content = std::mem::take(&mut pending_tool_results);
                content.push(AnthropicContent::Text {
                    text: message.content.clone(),
                });
                content.extend(
                    message
                        .attachments
                        .iter()
                        .map(|attachment| match attachment {
                            MessageAttachment::Text { name, content } => AnthropicContent::Text {
                                text: format!("[Pasted text: {name}]\n{content}"),
                            },
                            MessageAttachment::Image {
                                media_type, data, ..
                            } => AnthropicContent::Image {
                                source: AnthropicImageSource {
                                    kind: "base64",
                                    media_type: media_type.clone(),
                                    data: data.clone(),
                                },
                            },
                        }),
                );
                output.push(AnthropicMessage {
                    role: "user",
                    content,
                });
            }
            Role::Assistant => {
                flush_tool_results(&mut output, &mut pending_tool_results);
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(AnthropicContent::Text {
                        text: message.content.clone(),
                    });
                }
                content.extend(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| AnthropicContent::ToolUse {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            input: serde_json::from_str(&call.arguments).unwrap_or_default(),
                        }),
                );
                output.push(AnthropicMessage {
                    role: "assistant",
                    content,
                });
            }
        }
    }
    flush_tool_results(&mut output, &mut pending_tool_results);
    (system.join("\n\n"), output)
}

fn flush_tool_results(messages: &mut Vec<AnthropicMessage>, pending: &mut Vec<AnthropicContent>) {
    if pending.is_empty() {
        return;
    }
    messages.push(AnthropicMessage {
        role: "user",
        content: std::mem::take(pending),
    });
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool<'a>>,
    stream: bool,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<AnthropicContent>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

impl<'a> From<&'a ToolDefinition> for AnthropicTool<'a> {
    fn from(tool: &'a ToolDefinition) -> Self {
        Self {
            name: &tool.name,
            description: &tool.description,
            input_schema: &tool.parameters,
        }
    }
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicResponseBlock>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicResponseBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_are_replayed_as_user_blocks() {
        let call = ToolCall {
            id: "tool_1".to_owned(),
            name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        };
        let messages = vec![
            Message::system("system"),
            Message::assistant("", vec![call.clone()]),
            Message::tool(&call, "result"),
        ];
        let (system, encoded) = encode_messages(&messages);
        assert_eq!(system, "system");
        assert_eq!(encoded.len(), 2);
        assert_eq!(encoded[1].role, "user");
    }

    #[test]
    fn legacy_claude_caps_are_preserved() {
        assert_eq!(max_output_tokens("claude-3-opus", 16_384), 4_096);
        assert_eq!(max_output_tokens("claude-3-5-sonnet", 16_384), 8_192);
        assert_eq!(max_output_tokens("claude-sonnet-4", 16_384), 16_384);
    }

    #[test]
    fn image_attachment_becomes_native_image_block() {
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
        let (_, encoded) = encode_messages(&[message]);
        let value = serde_json::to_value(&encoded[0]).unwrap();
        assert_eq!(value["content"][1]["type"], "image");
        assert_eq!(value["content"][1]["source"]["data"], "YWJj");
    }
}
