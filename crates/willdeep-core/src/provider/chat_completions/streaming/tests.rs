use super::*;
use serde_json::json;

fn push(
    state: &mut State,
    value: serde_json::Value,
) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
    state.push(SseEvent {
        kind: "message".into(),
        data: value.to_string(),
    })
}

#[test]
fn interleaved_calls_are_assembled_by_index_and_usage_is_preserved() {
    let mut state = State::default();
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":1,"id":"b","function":{"name":"read_file","arguments":"{\"path\":"}},
            {"index":0,"id":"a","function":{"name":"read_file","arguments":"{\"path\":"}}
        ]}}]}),
    )
    .unwrap();
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"tool_calls":[
        {"index":0,"function":{"arguments":"\"a\"}"}},
        {"index":1,"function":{"arguments":"\"b\"}"}}
    ]},"finish_reason":"tool_calls"}]}),
    )
    .unwrap();
    push(&mut state, json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30,"prompt_tokens_details":{"cached_tokens":5}}})).unwrap();
    assert!(
        state
            .push(SseEvent {
                kind: "message".into(),
                data: "[DONE]".into()
            })
            .unwrap()
            .0
    );
    let result = state.finish().unwrap();
    assert_eq!(result.tool_calls[0].id, "a");
    assert_eq!(result.tool_calls[1].arguments, "{\"path\":\"b\"}");
    assert_eq!(result.usage.unwrap().cache_read_tokens, Some(5));
}

#[test]
fn truncated_tool_arguments_never_escape_as_executable_calls() {
    let mut state = State::default();
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"content":"partial","tool_calls":[
        {"index":0,"id":"a","function":{"name":"write_file","arguments":"{"}}
    ]},"finish_reason":"length"}]}),
    )
    .unwrap();
    let result = state.finish().unwrap();
    assert!(result.is_incomplete());
    assert!(result.tool_calls.is_empty());
    assert_eq!(result.content, "partial");
}

#[test]
fn done_alone_and_changed_tool_identity_are_rejected() {
    assert!(
        State::default()
            .push(SseEvent {
                kind: "message".into(),
                data: "[DONE]".into()
            })
            .is_err()
    );
    let mut state = State::default();
    state.merge_call(&json!({"index":0,"id":"a"})).unwrap();
    assert!(state.merge_call(&json!({"index":0,"id":"b"})).is_err());
    assert!(state.merge_call(&json!({"index":128,"id":"x"})).is_err());
}

#[test]
fn finish_reason_must_match_the_actual_tool_calls() {
    let mut state = State {
        text: "claimed a tool".into(),
        reason: Some("tool_calls".into()),
        ..State::default()
    };
    assert!(state.finish().is_err());
    state
        .merge_call(&json!({"index":0,"id":"a","function":{"name":"read_file","arguments":"{}"}}))
        .unwrap();
    state.reason = Some("stop".into());
    assert!(state.finish().is_err());
    state.reason = Some("tool_calls".into());
    state
        .merge_call(&json!({"index":1,"id":"a","function":{"name":"read_file","arguments":"{}"}}))
        .unwrap();
    assert!(state.finish().is_err());
}

use crate::provider::stream_test_support::{Events, frame, server};

#[tokio::test]
async fn real_chat_provider_requests_stream_and_emits_deltas_and_usage() {
    let payload = frame(
        json!({"choices":[{"index":0,"delta":{"content":"你好"},"finish_reason":null}]}),
    ) + &frame(
        json!({"choices":[{"index":0,"delta":{"content":"世界"},"finish_reason":"stop"}]}),
    ) + &frame(
        json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}),
    ) + "data: [DONE]\n\n";
    let (url, requests, task) = server(payload).await;
    let provider = ChatCompletionsProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::OpenAiCompatible,
        crate::provider::ApiDialect::ChatCompletions,
        url,
        "test-key",
        "test-model",
    ))
    .unwrap();
    let events = Events::default();
    let result = provider
        .complete_with_events(&[Message::user("hello")], &[], &events)
        .await;
    task.abort();
    assert_eq!(result.unwrap().content, "你好世界");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(requests[0]["stream_options"]["include_usage"], true);
    let events = events.0.lock().unwrap();
    assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "你好"));
    assert!(matches!(&events[2], ProviderEvent::Usage(usage) if usage.total_tokens == Some(5)));
}

#[tokio::test]
async fn interrupted_chat_preserves_text_without_replaying_or_exposing_tools() {
    let payload = frame(
        json!({"choices":[{"index":0,"delta":{"content":"saved","tool_calls":[
            {"index":0,"id":"a","function":{"name":"write_file","arguments":"{"}}
        ]}}]}),
    );
    let (url, requests, task) = server(payload).await;
    let provider = ChatCompletionsProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::OpenAiCompatible,
        crate::provider::ApiDialect::ChatCompletions,
        url,
        "test-key",
        "test-model",
    ))
    .unwrap();
    let result = provider
        .complete_with_events(&[Message::user("hello")], &[], &Events::default())
        .await;
    task.abort();
    let ProviderError::StreamInterrupted { partial, .. } = result.unwrap_err() else {
        panic!("expected partial error")
    };
    assert_eq!(partial.content, "saved");
    assert!(partial.tool_calls.is_empty());
    assert!(partial.usage.is_none());
    assert_eq!(requests.lock().unwrap().len(), 1);
}
