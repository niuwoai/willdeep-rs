use super::*;
use crate::provider::stream_test_support::{Events, frame, server};
use serde_json::json;

fn push(state: &mut State, value: Value) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
    state.push(SseEvent {
        kind: "message".into(),
        data: value.to_string(),
    })
}
fn start() -> Value {
    json!({"type":"message_start","message":{"id":"m1","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}})
}
fn block(index: u64, content: Value) -> Value {
    json!({"type":"content_block_start","index":index,"content_block":content})
}
fn stop(index: u64) -> Value {
    json!({"type":"content_block_stop","index":index})
}
fn reason(value: &str, tokens: u64) -> Value {
    json!({"type":"message_delta","delta":{"stop_reason":value},"usage":{"output_tokens":tokens}})
}

#[test]
fn tool_fragments_and_cumulative_usage_are_assembled_once() {
    let mut state = State::default();
    push(&mut state, start()).unwrap();
    push(
        &mut state,
        block(
            0,
            json!({"type":"tool_use","id":"t1","name":"read_file","input":{}}),
        ),
    )
    .unwrap();
    for part in ["{\"path\":", "\"a\"}"] {
        push(&mut state, json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":part}})).unwrap();
    }
    push(&mut state, stop(0)).unwrap();
    push(&mut state, reason("tool_use", 4)).unwrap();
    push(&mut state, reason("tool_use", 7)).unwrap();
    assert!(push(&mut state, json!({"type":"message_stop"})).unwrap().0);
    let result = state.finish().unwrap();
    assert_eq!(result.tool_calls[0].arguments, "{\"path\":\"a\"}");
    assert_eq!(result.usage.unwrap().total_tokens, Some(17));
}

#[test]
fn truncation_keeps_partial_usage_but_drops_tools() {
    let mut state = State::default();
    push(&mut state, start()).unwrap();
    push(
        &mut state,
        block(
            0,
            json!({"type":"tool_use","id":"t1","name":"write_file","input":{}}),
        ),
    )
    .unwrap();
    push(&mut state, json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}})).unwrap();
    push(&mut state, stop(0)).unwrap();
    push(&mut state, reason("max_tokens", 20)).unwrap();
    let result = state.finish().unwrap();
    assert!(result.is_incomplete());
    assert!(result.tool_calls.is_empty());
    assert_eq!(result.usage.unwrap().output_tokens, Some(20));
}

#[test]
fn lifecycle_errors_never_produce_completed_messages() {
    assert!(push(&mut State::default(), json!({"type":"message_stop"})).is_err());
    let mut state = State::default();
    push(&mut state, start()).unwrap();
    push(&mut state, block(0, json!({"type":"text","text":""}))).unwrap();
    assert!(push(&mut state, reason("end_turn", 3)).is_err());
    push(&mut state, stop(0)).unwrap();
    assert!(push(&mut state, json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"late"}})).is_err());
}

#[test]
fn thinking_blocks_are_not_exposed_as_assistant_text() {
    let mut state = State::default();
    push(&mut state, start()).unwrap();
    assert!(
        push(
            &mut state,
            block(0, json!({"type":"thinking","thinking":"private"}))
        )
        .unwrap()
        .1
        .is_empty()
    );
    assert!(push(&mut state, json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"more private"}})).unwrap().1.is_empty());
    push(&mut state, stop(0)).unwrap();
    push(&mut state, block(1, json!({"type":"text","text":"answer"}))).unwrap();
    push(&mut state, stop(1)).unwrap();
    push(&mut state, reason("end_turn", 9)).unwrap();
    assert_eq!(state.finish().unwrap().content, "answer");
}

#[tokio::test]
async fn real_anthropic_stream_delivers_text_and_cumulative_usage() {
    let payload = frame(start())
        + &frame(block(0, json!({"type":"text","text":""})))
        + &frame(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}),
        )
        + &frame(stop(0))
        + &frame(reason("end_turn", 5))
        + &frame(json!({"type":"message_stop"}));
    let (url, requests, task) = server(payload).await;
    let provider = AnthropicMessagesProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::Anthropic,
        crate::provider::ApiDialect::AnthropicMessages,
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
    let result = result.unwrap();
    assert_eq!(result.content, "你好");
    assert_eq!(result.usage.unwrap().total_tokens, Some(15));
    assert_eq!(requests.lock().unwrap()[0]["stream"], true);
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, ProviderEvent::TextDelta(text) if text == "你好"))
    );
}

#[tokio::test]
async fn interrupted_anthropic_stream_keeps_partial_text_and_usage_without_retry() {
    let payload = frame(start()) + &frame(block(0, json!({"type":"text","text":"saved"})));
    let (url, requests, task) = server(payload).await;
    let provider = AnthropicMessagesProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::Anthropic,
        crate::provider::ApiDialect::AnthropicMessages,
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
        panic!("partial required")
    };
    assert_eq!(partial.content, "saved");
    assert_eq!(partial.usage.unwrap().input_tokens, Some(10));
    assert!(partial.tool_calls.is_empty());
    assert_eq!(requests.lock().unwrap().len(), 1);
}
