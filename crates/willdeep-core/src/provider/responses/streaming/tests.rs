use super::*;
use crate::provider::stream_test_support::{Events, frame, server};
use serde_json::json;

fn push(state: &mut State, value: Value) -> Result<(bool, Vec<ProviderEvent>), ProviderError> {
    state.push(SseEvent {
        kind: "message".into(),
        data: value.to_string(),
    })
}
fn message(text: &str) -> Value {
    json!({"type":"message","content":[{"type":"output_text","text":text}]})
}
fn terminal(status: &str, output: Vec<Value>) -> Value {
    json!({"type":format!("response.{status}"),"response":{"id":"r1","status":status,"output":output,
        "usage":{"input_tokens":10,"output_tokens":5,"input_tokens_details":{"cached_tokens":3}}}})
}

#[test]
fn final_response_confirms_streamed_text_and_arguments() {
    let mut state = State::default();
    push(&mut state, json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello"})).unwrap();
    push(&mut state, json!({"type":"response.function_call_arguments.delta","item_id":"item1","delta":"{\"path\":"})).unwrap();
    push(
        &mut state,
        json!({"type":"response.function_call_arguments.delta","item_id":"item1","delta":"\"a\"}"}),
    )
    .unwrap();
    let (done, updates) = push(&mut state, terminal("completed", vec![message("hello"),
        json!({"type":"function_call","id":"item1","call_id":"call1","name":"read_file","arguments":"{\"path\":\"a\"}"})])).unwrap();
    assert!(done);
    assert_eq!(
        updates.len(),
        1,
        "final snapshot must not duplicate text deltas"
    );
    let result = state.completed.unwrap();
    assert_eq!(result.tool_calls[0].id, "call1");
    assert_eq!(result.usage.unwrap().cache_read_tokens, Some(3));
}

#[test]
fn contradictory_final_text_does_not_become_a_success() {
    let mut state = State::default();
    push(&mut state, json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"observed"})).unwrap();
    assert!(
        push(
            &mut state,
            terminal("completed", vec![message("different")])
        )
        .is_err()
    );
    assert_eq!(state.partial().content, "observed");
    assert!(state.completed.is_none());
}

#[test]
fn incomplete_terminal_keeps_text_and_usage_but_no_tools() {
    let mut state = State::default();
    push(&mut state, json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"partial"})).unwrap();
    push(
        &mut state,
        terminal(
            "incomplete",
            vec![json!({"type":"function_call","arguments":"{"})],
        ),
    )
    .unwrap();
    let result = state.completed.unwrap();
    assert!(result.is_incomplete());
    assert!(result.tool_calls.is_empty());
    assert_eq!(result.content, "partial");
    assert_eq!(result.usage.unwrap().total_tokens, Some(15));
}

#[test]
fn repeated_sequence_and_terminal_status_mismatch_are_rejected() {
    let mut state = State::default();
    let event = json!({"type":"response.created","sequence_number":1,"response":{"id":"r1"}});
    push(&mut state, event.clone()).unwrap();
    assert!(push(&mut state, event).is_err());
    assert!(
        push(
            &mut State::default(),
            json!({"type":"response.completed","response":{"status":"in_progress"}})
        )
        .is_err()
    );
}

#[tokio::test]
async fn real_responses_request_delivers_text_and_authoritative_usage() {
    let payload = frame(
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"你好"}),
    ) + &frame(terminal("completed", vec![message("你好")]));
    let (url, requests, task) = server(payload).await;
    let provider = ResponsesProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::OpenAiCompatible,
        crate::provider::ApiDialect::Responses,
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
    assert_eq!(result.unwrap().content, "你好");
    assert_eq!(requests.lock().unwrap()[0]["stream"], true);
    assert_eq!(requests.lock().unwrap()[0]["store"], false);
    assert_eq!(events.0.lock().unwrap().len(), 2);
}

#[test]
fn final_function_identity_cannot_change_or_disappear() {
    let mut state = State::default();
    push(
        &mut state,
        json!({"type":"response.output_item.added","item":{
            "type":"function_call","id":"item1","call_id":"call1","name":"read_file"
        }}),
    )
    .unwrap();
    assert!(push(&mut state, terminal("completed", vec![message("done")])).is_err());
    assert!(push(&mut state, terminal("completed", vec![json!({
        "type":"function_call","id":"item1","call_id":"call1","name":"write_file","arguments":"{}"
    })])).is_err());
    assert!(state.completed.is_none());
}

#[tokio::test]
async fn responses_eof_keeps_partial_text_and_does_not_retry() {
    let payload = frame(
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"saved"}),
    );
    let (url, requests, task) = server(payload).await;
    let provider = ResponsesProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::OpenAiCompatible,
        crate::provider::ApiDialect::Responses,
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
        panic!("partial error required")
    };
    assert_eq!(partial.content, "saved");
    assert!(partial.tool_calls.is_empty());
    assert_eq!(requests.lock().unwrap().len(), 1);
}
