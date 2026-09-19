use super::*;
use crate::provider::common::{request_deadline, send_open_retrying};
use crate::provider::sse::{SseEvent, read_stream};
use crate::provider::{ProviderEvent, ProviderEventSink};
use std::collections::BTreeMap;

const MAX_TOOL_CALLS: u64 = 128;
#[cfg(test)]
mod tests;

pub(super) async fn complete(
    provider: &ChatCompletionsProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    events: &dyn ProviderEventSink,
) -> Result<Completion, ProviderError> {
    let wire_messages = wire_messages(messages);
    let wire_tools = tools.iter().map(WireTool::from).collect::<Vec<_>>();
    let body = ChatRequest {
        model: &provider.config.model,
        messages: &wire_messages,
        tools: &wire_tools,
        tool_choice: (!tools.is_empty()).then_some("auto"),
        stream: true,
        reasoning_effort: provider.is_auxiliary_or_loopback().then_some("none"),
        think: provider.is_auxiliary_or_loopback().then_some(false),
    };
    let mut body = serde_json::to_value(body).map_err(|error| invalid(error.to_string()))?;
    body["stream_options"] = serde_json::json!({"include_usage": true});
    let request = openai_auth(
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
            let (finished, updates) = result?;
            for update in updates {
                events.emit(update).await;
            }
            Ok(finished)
        }
    })
    .await;
    if let Err(source) = result {
        return Err(ProviderError::StreamInterrupted {
            source: Box::new(source),
            partial: Box::new(state.partial()),
        });
    }
    state
        .finish()
        .map_err(|source| ProviderError::StreamInterrupted {
            source: Box::new(source),
            partial: Box::new(state.partial()),
        })
}

/// 思维链攒到这么多字就冲刷一条 `ReasoningDelta`。思考型模型一步能吐几千字，
/// 逐块外发会把守护进程的事件日志刷爆；界面只要「它此刻在想什么」，按段够用。
const REASONING_DELTA_CHARS: usize = 120;

#[derive(Default)]
struct State {
    text: String,
    /// 思维链原文。只累积、不当作正文外发：它既不是模型给用户的回答，
    /// 但下一轮请求又必须原样带回去。
    reasoning: String,
    /// 还没外发的那截思维链；攒够 [`REASONING_DELTA_CHARS`]、遇到换行、正文或
    /// 工具调用开始、收尾时冲刷。
    pending_reasoning: String,
    calls: BTreeMap<u64, ToolCall>,
    reason: Option<String>,
    usage: Option<Usage>,
}

impl State {
    fn push(&mut self, event: SseEvent) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
        if event.data.trim() == "[DONE]" {
            if self.reason.is_none() {
                return Err(invalid("stream completion lacks finish_reason"));
            }
            return Ok((true, self.flush_reasoning()));
        }
        let value: serde_json::Value =
            serde_json::from_str(&event.data).map_err(|error| invalid(error.to_string()))?;
        if value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(invalid("provider emitted a stream error"));
        }
        let mut updates = Vec::new();
        if let Some(usage) = value.get("usage").filter(|value| !value.is_null()) {
            let usage: ChatUsage = serde_json::from_value(usage.clone())
                .map_err(|error| invalid(error.to_string()))?;
            let usage = Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cache_read_tokens: usage.cache_read_tokens(),
            };
            self.usage = Some(usage.clone());
            updates.push(ProviderEvent::Usage(usage));
        }
        let choices = value
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("stream chunk lacks choices"))?;
        for choice in choices {
            if choice.get("index").and_then(serde_json::Value::as_u64) != Some(0) {
                continue;
            }
            let delta = &choice["delta"];
            for field in ["reasoning_content", "reasoning"] {
                if let Some(text) = delta[field].as_str().filter(|text| !text.is_empty()) {
                    self.reasoning.push_str(text);
                    self.pending_reasoning.push_str(text);
                    if text.contains('\n')
                        || self.pending_reasoning.chars().count() >= REASONING_DELTA_CHARS
                    {
                        updates.extend(self.flush_reasoning());
                    }
                    break;
                }
            }
            for field in ["content", "refusal"] {
                if let Some(text) = delta[field].as_str().filter(|text| !text.is_empty()) {
                    if self.reason.is_some() {
                        return Err(invalid("text delta after finish_reason"));
                    }
                    // 正文一开始，思维链就结束了：先把攒着的那截发掉，保持先后顺序。
                    updates.extend(self.flush_reasoning());
                    self.text.push_str(text);
                    updates.push(ProviderEvent::TextDelta(text.to_owned()));
                }
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                if self.reason.is_some() {
                    return Err(invalid("tool delta after finish_reason"));
                }
                updates.extend(self.flush_reasoning());
                for call in calls {
                    self.merge_call(call)?;
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if !matches!(reason, "stop" | "tool_calls" | "length" | "content_filter") {
                    return Err(invalid("unsupported streamed finish reason"));
                }
                if self
                    .reason
                    .as_deref()
                    .is_some_and(|previous| previous != reason)
                {
                    return Err(invalid("conflicting finish reasons"));
                }
                self.reason = Some(reason.to_owned());
                updates.extend(self.flush_reasoning());
            }
        }
        Ok((false, updates))
    }

    /// 把还没外发的思维链作一条 `ReasoningDelta` 交出去；没有就什么都不发。
    fn flush_reasoning(&mut self) -> Vec<ProviderEvent> {
        if self.pending_reasoning.is_empty() {
            return Vec::new();
        }
        vec![ProviderEvent::ReasoningDelta(std::mem::take(
            &mut self.pending_reasoning,
        ))]
    }

    fn merge_call(&mut self, value: &serde_json::Value) -> Result<(), ProviderError> {
        let index = value["index"]
            .as_u64()
            .filter(|index| *index < MAX_TOOL_CALLS)
            .ok_or_else(|| invalid("invalid streamed tool index"))?;
        let call = self.calls.entry(index).or_insert_with(|| ToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
        if let Some(id) = value["id"].as_str() {
            if !call.id.is_empty() && call.id != id {
                return Err(invalid("streamed tool identity changed"));
            }
            call.id = id.to_owned();
        }
        if let Some(name) = value["function"]["name"].as_str() {
            call.name.push_str(name);
        }
        if let Some(arguments) = value["function"]["arguments"].as_str() {
            call.arguments.push_str(arguments);
        }
        Ok(())
    }

    fn reasoning(&self) -> Option<String> {
        (!self.reasoning.is_empty()).then(|| self.reasoning.clone())
    }

    fn partial(&self) -> Completion {
        Completion {
            content: self.text.clone(),
            reasoning: self.reasoning(),
            tool_calls: Vec::new(),
            finish_reason: Some("incomplete".to_owned()),
            usage: self.usage.clone(),
        }
    }

    fn finish(&self) -> Result<Completion, ProviderError> {
        let mut completion = Completion {
            content: self.text.clone(),
            reasoning: self.reasoning(),
            tool_calls: self.calls.values().cloned().collect(),
            finish_reason: self.reason.clone(),
            usage: self.usage.clone(),
        };
        if completion.is_incomplete() {
            completion.tool_calls.clear();
            return Ok(completion);
        }
        if completion.tool_calls.is_empty() && completion.content.is_empty() {
            return Err(ProviderError::EmptyResponse);
        }
        if completion.finish_reason.as_deref() == Some("tool_calls")
            && completion.tool_calls.is_empty()
        {
            return Err(invalid("tool_calls finish reason without tool calls"));
        }
        let mut identities = std::collections::BTreeSet::new();
        if completion.finish_reason.as_deref() != Some("tool_calls")
            && !completion.tool_calls.is_empty()
        {
            return Err(invalid("tool calls without a tool_calls finish reason"));
        }
        for call in &completion.tool_calls {
            if !identities.insert(&call.id) {
                return Err(invalid("duplicate streamed tool identity"));
            }
            if call.id.is_empty()
                || call.name.is_empty()
                || !serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .is_ok_and(|value| value.is_object())
            {
                return Err(invalid("incomplete streamed tool call"));
            }
        }
        Ok(completion)
    }
}

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidResponse(message.into())
}
