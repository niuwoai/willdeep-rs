use super::*;
use crate::provider::common::{request_deadline, send_open_retrying};
use crate::provider::sse::{SseEvent, read_stream};
use crate::provider::{ProviderEvent, ProviderEventSink};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

pub(super) async fn complete(
    provider: &ResponsesProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    events: &dyn ProviderEventSink,
) -> Result<Completion, ProviderError> {
    let (instructions, input) = encode_input(messages);
    let body = ResponsesRequest {
        model: &provider.config.model,
        instructions: (!instructions.is_empty()).then_some(instructions),
        input,
        tools: tools.iter().map(ResponseTool::from).collect(),
        stream: true,
        store: false,
    };
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
            let (done, updates) = result?;
            for update in updates {
                events.emit(update).await;
            }
            Ok(done)
        }
    })
    .await;
    match result {
        Ok(()) => state.completed.take().ok_or(ProviderError::EmptyResponse),
        Err(source) => Err(ProviderError::StreamInterrupted {
            source: Box::new(source),
            partial: Box::new(state.partial()),
        }),
    }
}

#[derive(Default)]
struct State {
    text: BTreeMap<(u64, u64), String>,
    arguments: BTreeMap<String, String>,
    call_identity: BTreeMap<String, (String, String)>,
    sequence: Option<u64>,
    response_id: Option<String>,
    usage: Option<Usage>,
    completed: Option<Completion>,
}

impl State {
    fn push(&mut self, event: SseEvent) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
        let value: Value =
            serde_json::from_str(&event.data).map_err(|error| invalid(error.to_string()))?;
        let kind = string(&value, "type")?;
        if event.kind != "message" && event.kind != kind {
            return Err(invalid("SSE event type mismatch"));
        }
        if let Some(sequence) = value["sequence_number"].as_u64() {
            if self.sequence.is_some_and(|old| sequence <= old) {
                return Err(invalid("Responses sequence did not advance"));
            }
            self.sequence = Some(sequence);
        }
        if let Some(id) = value["response"]["id"].as_str() {
            if self.response_id.as_deref().is_some_and(|old| old != id) {
                return Err(invalid("Responses identity changed"));
            }
            self.response_id = Some(id.to_owned());
        }
        match kind {
            "response.output_item.added" if value["item"]["type"] == "function_call" => {
                let item = &value["item"];
                let id = string(item, "id")?.to_owned();
                let identity = (
                    string(item, "call_id")?.to_owned(),
                    string(item, "name")?.to_owned(),
                );
                if self.call_identity.insert(id, identity).is_some() {
                    return Err(invalid("duplicate Responses function item"));
                }
                Ok((false, Vec::new()))
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let key = (
                    index(&value, "output_index")?,
                    index(&value, "content_index")?,
                );
                let delta = string(&value, "delta")?;
                self.text.entry(key).or_default().push_str(delta);
                Ok((false, vec![ProviderEvent::TextDelta(delta.to_owned())]))
            }
            "response.function_call_arguments.delta" => {
                let id = string(&value, "item_id")?.to_owned();
                self.arguments
                    .entry(id)
                    .or_default()
                    .push_str(string(&value, "delta")?);
                Ok((false, Vec::new()))
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                self.terminal(kind, &value["response"])
            }
            "error" => Err(invalid("Responses stream emitted an error")),
            _ => Ok((false, Vec::new())),
        }
    }

    fn terminal(
        &mut self,
        kind: &str,
        response: &Value,
    ) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
        let status = string(response, "status")?;
        if kind.strip_prefix("response.") != Some(status) {
            return Err(invalid("terminal event and response status disagree"));
        }
        let mut updates = Vec::new();
        if let Some(usage) = response.get("usage").filter(|value| !value.is_null()) {
            let usage: ResponseUsage = serde_json::from_value(usage.clone())
                .map_err(|error| invalid(error.to_string()))?;
            let usage = Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage
                    .input_tokens
                    .zip(usage.output_tokens)
                    .and_then(|(a, b)| a.checked_add(b)),
                cache_read_tokens: usage
                    .input_tokens_details
                    .and_then(|details| details.cached_tokens),
            };
            self.usage = Some(usage.clone());
            updates.push(ProviderEvent::Usage(usage));
        }
        if status == "failed" {
            return Err(invalid("Responses generation failed"));
        }
        let output = response["output"]
            .as_array()
            .ok_or_else(|| invalid("terminal response lacks output"))?;
        let mut final_text = BTreeMap::new();
        let mut calls = Vec::new();
        let mut seen_arguments = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for (output_index, item) in output.iter().enumerate() {
            if status == "completed"
                && item["status"]
                    .as_str()
                    .is_some_and(|status| status != "completed")
            {
                return Err(invalid("completed response contains an unfinished item"));
            }
            match item["type"].as_str() {
                Some("message") => {
                    let content = item["content"]
                        .as_array()
                        .ok_or_else(|| invalid("message lacks content"))?;
                    for (content_index, part) in content.iter().enumerate() {
                        let field = match part["type"].as_str() {
                            Some("output_text" | "text") => "text",
                            Some("refusal") => "refusal",
                            _ => continue,
                        };
                        final_text.insert(
                            (output_index as u64, content_index as u64),
                            string(part, field)?.to_owned(),
                        );
                    }
                }
                Some("function_call") if status == "completed" => {
                    let id = string(item, "id")?;
                    let arguments = string(item, "arguments")?;
                    if self.arguments.get(id).is_some_and(|seen| seen != arguments) {
                        return Err(invalid(
                            "final tool arguments differ from streamed arguments",
                        ));
                    }
                    seen_arguments.insert(id.to_owned());
                    let call_id = string(item, "call_id")?;
                    let name = string(item, "name")?;
                    if self
                        .call_identity
                        .get(id)
                        .is_some_and(|(seen_id, seen_name)| seen_id != call_id || seen_name != name)
                    {
                        return Err(invalid(
                            "final function identity differs from streamed item",
                        ));
                    }
                    const MAX_TOOL_CALLS: usize = 128;
                    if calls.len() >= MAX_TOOL_CALLS {
                        return Err(invalid("too many Responses function calls"));
                    }
                    if call_id.is_empty()
                        || name.is_empty()
                        || !identities.insert(call_id)
                        || !serde_json::from_str::<Value>(arguments)
                            .is_ok_and(|value| value.is_object())
                    {
                        return Err(invalid("invalid completed function call"));
                    }
                    calls.push(ToolCall {
                        id: call_id.to_owned(),
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    });
                }
                _ => {}
            }
        }
        if status == "completed" {
            if self.arguments.keys().any(|id| !seen_arguments.contains(id))
                || self
                    .call_identity
                    .keys()
                    .any(|id| !seen_arguments.contains(id))
            {
                return Err(invalid(
                    "streamed function call missing from final response",
                ));
            }
            for (key, text) in &self.text {
                if final_text.get(key) != Some(text) {
                    return Err(invalid("final text differs from streamed text"));
                }
            }
        }
        for (key, text) in &final_text {
            if !self.text.contains_key(key) {
                updates.push(ProviderEvent::TextDelta(text.clone()));
            }
        }
        if status == "incomplete" {
            for (key, text) in &self.text {
                if final_text
                    .get(key)
                    .is_some_and(|final_value| final_value != text)
                {
                    return Err(invalid("incomplete response contradicts observed text"));
                }
                final_text.entry(*key).or_insert_with(|| text.clone());
            }
        }
        self.text = final_text;
        let content = self.text.values().cloned().collect::<String>();
        if content.is_empty() && calls.is_empty() && status == "completed" {
            return Err(ProviderError::EmptyResponse);
        }
        self.completed = Some(Completion {
            content,
            reasoning: None,
            tool_calls: calls,
            finish_reason: Some(status.to_owned()),
            usage: self.usage.clone(),
        });
        Ok((true, updates))
    }

    fn partial(&self) -> Completion {
        Completion {
            reasoning: None,
            content: self.text.values().cloned().collect(),
            tool_calls: Vec::new(),
            finish_reason: Some("incomplete".to_owned()),
            usage: self.usage.clone(),
        }
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value[key]
        .as_str()
        .ok_or_else(|| invalid(format!("Responses event lacks {key}")))
}
fn index(value: &Value, key: &str) -> Result<u64, ProviderError> {
    const MAX_PART_INDEX: u64 = 1024;
    value[key]
        .as_u64()
        .filter(|index| *index < MAX_PART_INDEX)
        .ok_or_else(|| invalid("invalid Responses part index"))
}
fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidResponse(message.into())
}
