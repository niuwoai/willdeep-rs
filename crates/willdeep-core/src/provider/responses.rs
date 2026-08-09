use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::common::{client, decode_success, endpoint, openai_auth};
use super::{Provider, ProviderConfig, ProviderError};
use crate::types::{Completion, Message, Role, ToolCall, ToolDefinition, Usage};

pub struct ResponsesProvider {
    config: ProviderConfig,
    client: Client,
    endpoint: reqwest::Url,
}

impl ResponsesProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            endpoint: endpoint(&config.base_url, "responses")?,
            client: client(&config)?,
            config,
        })
    }
}

#[async_trait]
impl Provider for ResponsesProvider {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let (instructions, input) = encode_input(messages);
        let wire_tools = tools.iter().map(ResponseTool::from).collect::<Vec<_>>();
        let body = ResponsesRequest {
            model: &self.config.model,
            instructions: (!instructions.is_empty()).then_some(instructions),
            input,
            tools: wire_tools,
            stream: false,
            store: false,
        };
        let request =
            openai_auth(self.client.post(self.endpoint.clone()), &self.config).json(&body);
        let bytes = decode_success(request.send().await?, &self.config).await?;
        let response: ResponsesResponse = serde_json::from_slice(&bytes)
            .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;

        let mut text = Vec::new();
        let mut tool_calls = Vec::new();
        for item in response.output {
            match item.kind.as_str() {
                "message" => {
                    for content in item.content {
                        if matches!(content.kind.as_str(), "output_text" | "text")
                            && let Some(value) = content.text
                        {
                            text.push(value);
                        }
                    }
                }
                "function_call" => {
                    tool_calls.push(ToolCall {
                        id: item
                            .call_id
                            .or(item.id)
                            .unwrap_or_else(|| "call_unknown".to_owned()),
                        name: item.name.unwrap_or_default(),
                        arguments: item.arguments.unwrap_or_else(|| "{}".to_owned()),
                    });
                }
                _ => {}
            }
        }
        if text.is_empty() && tool_calls.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(Completion {
            content: text.join(""),
            tool_calls,
            finish_reason: response.status,
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

fn encode_input(messages: &[Message]) -> (String, Vec<ResponseInputItem>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::System => instructions.push(message.content.clone()),
            Role::User => input.push(ResponseInputItem::message("user", &message.content)),
            Role::Assistant => {
                if !message.content.is_empty() {
                    input.push(ResponseInputItem::message("assistant", &message.content));
                }
                input.extend(message.tool_calls.iter().map(|call| ResponseInputItem {
                    kind: "function_call",
                    role: None,
                    content: Vec::new(),
                    call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    arguments: Some(call.arguments.clone()),
                    output: None,
                }));
            }
            Role::Tool => input.push(ResponseInputItem {
                kind: "function_call_output",
                role: None,
                content: Vec::new(),
                call_id: message.tool_call_id.clone(),
                name: None,
                arguments: None,
                output: Some(message.content.clone()),
            }),
        }
    }
    (instructions.join("\n\n"), input)
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponseTool<'a>>,
    stream: bool,
    store: bool,
}

#[derive(Serialize)]
struct ResponseInputItem {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    content: Vec<ResponseContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

impl ResponseInputItem {
    fn message(role: &'static str, text: &str) -> Self {
        Self {
            kind: "message",
            role: Some(role),
            content: vec![ResponseContent {
                kind: if role == "assistant" {
                    "output_text"
                } else {
                    "input_text"
                },
                text: text.to_owned(),
            }],
            call_id: None,
            name: None,
            arguments: None,
            output: None,
        }
    }
}

#[derive(Serialize)]
struct ResponseContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Serialize)]
struct ResponseTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

impl<'a> From<&'a ToolDefinition> for ResponseTool<'a> {
    fn from(tool: &'a ToolDefinition) -> Self {
        Self {
            kind: "function",
            name: &tool.name,
            description: &tool.description,
            parameters: &tool.parameters,
        }
    }
}

#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponseOutputItem>,
    status: Option<String>,
    usage: Option<ResponseUsage>,
}

#[derive(Deserialize)]
struct ResponseOutputItem {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
    #[serde(default)]
    content: Vec<ResponseOutputContent>,
}

#[derive(Deserialize)]
struct ResponseOutputContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ResponseUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCall;

    #[test]
    fn input_preserves_function_call_pairing() {
        let call = ToolCall {
            id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        };
        let messages = vec![
            Message::system("rules"),
            Message::assistant("", vec![call.clone()]),
            Message::tool(&call, "contents"),
        ];
        let (instructions, input) = encode_input(&messages);
        assert_eq!(instructions, "rules");
        assert_eq!(input.len(), 2);
        assert_eq!(input[0].call_id.as_deref(), Some("call_1"));
        assert_eq!(input[1].call_id.as_deref(), Some("call_1"));
    }
}
