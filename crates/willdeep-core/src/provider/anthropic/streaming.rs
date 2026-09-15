use super::*;
use crate::provider::common::{request_deadline, send_open_retrying};
use crate::provider::sse::{SseEvent, read_stream};
use crate::provider::{ProviderEvent, ProviderEventSink};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
mod tests;

pub(super) async fn complete(
    provider: &AnthropicMessagesProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    events: &dyn ProviderEventSink,
) -> Result<Completion, ProviderError> {
    let (system, messages) = encode_messages(messages);
    let body = AnthropicRequest {
        model: &provider.config.model,
        max_tokens: max_output_tokens(&provider.config.model, provider.config.max_output_tokens),
        system,
        messages,
        tools: tools.iter().map(AnthropicTool::from).collect(),
        stream: true,
    };
    let request = anthropic_auth(
        provider.client.post(provider.endpoint.clone()),
        &provider.config,
    )
    .json(&body);
    let deadline = request_deadline(&provider.config)?;
    let response = send_open_retrying(request, &provider.config, events, deadline).await?;
    if !crate::provider::sse::is_event_stream(&response) {
        let bytes = tokio::time::timeout_at(
            deadline,
            crate::provider::common::decode_success(response, &provider.config),
        )
        .await
        .map_err(|_| ProviderError::DeadlineExceeded)??;
        let completion = super::decode_completion(&bytes)?;
        return crate::provider::common::emit_buffered_completion(completion, events, deadline)
            .await;
    }
    let mut state = State::default();
    let result = read_stream(response, deadline, |event| {
        let result = state.push(event);
        async move {
            let (done, updates) = result?;
            for update in updates {
                events.emit(update).await;
            }
            Ok(done)
        }
    })
    .await;
    result
        .and_then(|()| state.finish())
        .map_err(|source| ProviderError::StreamInterrupted {
            source: Box::new(source),
            partial: Box::new(state.partial()),
        })
}

enum Content {
    Text(String),
    Tool {
        id: String,
        name: String,
        initial: Value,
        json: String,
    },
    Opaque,
}
struct Block {
    content: Content,
    closed: bool,
}
#[derive(Default)]
struct State {
    started: bool,
    blocks: BTreeMap<u64, Block>,
    reason: Option<String>,
    usage: Option<Usage>,
}

impl State {
    fn push(&mut self, event: SseEvent) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
        let value: Value =
            serde_json::from_str(&event.data).map_err(|error| invalid(error.to_string()))?;
        let kind = string(&value, "type")?;
        if event.kind != "message" && event.kind != kind {
            return Err(invalid("Anthropic SSE type mismatch"));
        }
        if kind == "ping" {
            return Ok((false, Vec::new()));
        }
        if kind == "error" {
            return Err(invalid("Anthropic stream emitted an error"));
        }
        if kind == "message_start" {
            if self.started {
                return Err(invalid("duplicate message_start"));
            }
            if value["message"]["role"] != "assistant" {
                return Err(invalid("message_start is not an assistant message"));
            }
            self.started = true;
            return Ok((false, self.update_usage(&value["message"]["usage"])?));
        }
        if !self.started {
            return Err(invalid("content arrived before message_start"));
        }
        match kind {
            "content_block_start" => Ok((false, self.start_block(&value)?)),
            "content_block_delta" => Ok((false, self.delta(&value)?)),
            "content_block_stop" => {
                let block = self
                    .blocks
                    .get_mut(&index(&value)?)
                    .ok_or_else(|| invalid("stop for unknown block"))?;
                if block.closed {
                    return Err(invalid("duplicate block stop"));
                }
                block.closed = true;
                Ok((false, Vec::new()))
            }
            "message_delta" => {
                if self.blocks.values().any(|block| !block.closed) {
                    return Err(invalid("message_delta before block stop"));
                }
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    if !matches!(
                        reason,
                        "end_turn"
                            | "stop_sequence"
                            | "tool_use"
                            | "max_tokens"
                            | "pause_turn"
                            | "refusal"
                            | "model_context_window_exceeded"
                    ) {
                        return Err(invalid("unsupported Anthropic stop reason"));
                    }
                    if self
                        .reason
                        .as_deref()
                        .is_some_and(|previous| previous != reason)
                    {
                        return Err(invalid("conflicting stop reasons"));
                    }
                    self.reason = Some(reason.to_owned());
                }
                Ok((false, self.update_usage(&value["usage"])?))
            }
            "message_stop" => {
                if self.reason.is_none() || self.blocks.values().any(|block| !block.closed) {
                    return Err(invalid(
                        "message stopped without closed blocks and stop reason",
                    ));
                }
                Ok((true, Vec::new()))
            }
            _ => Ok((false, Vec::new())),
        }
    }

    fn start_block(&mut self, value: &Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        if self.reason.is_some() {
            return Err(invalid("content after stop reason"));
        }
        let index = index(value)?;
        if self.blocks.contains_key(&index) {
            return Err(invalid("duplicate content block"));
        }
        let block = &value["content_block"];
        let mut updates = Vec::new();
        let content = match string(block, "type")? {
            "text" => {
                let text = string(block, "text")?.to_owned();
                if !text.is_empty() {
                    updates.push(ProviderEvent::TextDelta(text.clone()));
                }
                Content::Text(text)
            }
            "tool_use" => Content::Tool {
                id: string(block, "id")?.to_owned(),
                name: string(block, "name")?.to_owned(),
                initial: block["input"].clone(),
                json: String::new(),
            },
            _ => Content::Opaque,
        };
        self.blocks.insert(
            index,
            Block {
                content,
                closed: false,
            },
        );
        Ok(updates)
    }

    fn delta(&mut self, value: &Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        let block = self
            .blocks
            .get_mut(&index(value)?)
            .ok_or_else(|| invalid("delta for unknown block"))?;
        if block.closed {
            return Err(invalid("delta after content block stop"));
        }
        let delta = &value["delta"];
        match (string(delta, "type")?, &mut block.content) {
            ("text_delta", Content::Text(text)) => {
                let addition = string(delta, "text")?;
                text.push_str(addition);
                Ok(vec![ProviderEvent::TextDelta(addition.to_owned())])
            }
            ("input_json_delta", Content::Tool { initial, json, .. }) => {
                if initial.as_object().is_some_and(|object| !object.is_empty()) {
                    return Err(invalid("tool delta conflicts with initial input"));
                }
                json.push_str(string(delta, "partial_json")?);
                Ok(Vec::new())
            }
            ("text_delta" | "input_json_delta", _) => {
                Err(invalid("content delta has the wrong block type"))
            }
            _ => Ok(Vec::new()),
        }
    }

    fn update_usage(&mut self, value: &Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        if value.is_null() {
            return Ok(Vec::new());
        }
        let mut usage = self.usage.clone().unwrap_or_default();
        for (name, target) in [
            ("input_tokens", &mut usage.input_tokens),
            ("output_tokens", &mut usage.output_tokens),
            ("cache_read_input_tokens", &mut usage.cache_read_tokens),
        ] {
            if let Some(value) = value.get(name) {
                let count = value
                    .as_u64()
                    .ok_or_else(|| invalid("invalid Anthropic usage"))?;
                if target.is_some_and(|old| count < old) {
                    return Err(invalid("cumulative Anthropic usage decreased"));
                }
                *target = Some(count);
            }
        }
        usage.total_tokens = usage
            .input_tokens
            .zip(usage.output_tokens)
            .and_then(|(a, b)| a.checked_add(b));
        self.usage = Some(usage.clone());
        Ok(vec![ProviderEvent::Usage(usage)])
    }

    fn partial(&self) -> Completion {
        Completion {
            reasoning: None,
            content: self
                .blocks
                .values()
                .filter_map(|block| match &block.content {
                    Content::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            tool_calls: Vec::new(),
            finish_reason: Some("incomplete".into()),
            usage: self.usage.clone(),
        }
    }

    fn finish(&self) -> Result<Completion, ProviderError> {
        let mut result = self.partial();
        result.finish_reason = self.reason.clone();
        if result.is_incomplete() {
            return Ok(result);
        }
        let mut identities = BTreeSet::new();
        for block in self.blocks.values() {
            if let Content::Tool {
                id,
                name,
                initial,
                json,
            } = &block.content
            {
                let arguments = if json.is_empty() {
                    initial.clone()
                } else {
                    serde_json::from_str::<Value>(json)
                        .map_err(|_| invalid("invalid streamed tool JSON"))?
                };
                if id.is_empty()
                    || name.is_empty()
                    || !identities.insert(id)
                    || !arguments.is_object()
                {
                    return Err(invalid("invalid Anthropic tool call"));
                }
                result.tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.to_string(),
                });
            }
        }
        if (result.finish_reason.as_deref() == Some("tool_use")) == result.tool_calls.is_empty() {
            return Err(invalid("stop reason does not match tool calls"));
        }
        if result.content.is_empty() && result.tool_calls.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(result)
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value[key]
        .as_str()
        .ok_or_else(|| invalid(format!("Anthropic event lacks {key}")))
}
fn index(value: &Value) -> Result<u64, ProviderError> {
    const MAX_BLOCKS: u64 = 128;
    value["index"]
        .as_u64()
        .filter(|index| *index < MAX_BLOCKS)
        .ok_or_else(|| invalid("invalid Anthropic block index"))
}
fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidResponse(message.into())
}
