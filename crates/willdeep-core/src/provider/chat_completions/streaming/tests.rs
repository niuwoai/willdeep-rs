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

#[test]
fn reasoning_deltas_are_kept_for_replay_without_leaking_into_the_answer() {
    let mut state = State::default();
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"reasoning_content":"先看"}}]}),
    )
    .unwrap();
    // 有的网关把同一份思维链平铺成 reasoning，不能两个字段都收，否则重复。
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"reasoning_content":"日志","reasoning":"日志"}}]}),
    )
    .unwrap();
    push(
        &mut state,
        json!({"choices":[{"index":0,"delta":{"content":"好的"},"finish_reason":"stop"}]}),
    )
    .unwrap();
    let result = state.finish().unwrap();
    assert_eq!(result.content, "好的");
    assert_eq!(result.reasoning.as_deref(), Some("先看日志"));
}

use crate::provider::stream_test_support::{Events, frame, server};

/// DeepSeek 一类 thinking 模型要求上一轮的思维链原样回传，少了这条，只要历史里
/// 出现过工具调用，整条请求就是 400；`arguments` 则必须是合法 JSON 对象，否则
/// 另一批上游会以 `function.arguments must be valid JSON` 拒掉同一条历史。
#[tokio::test]
async fn replayed_history_carries_reasoning_and_repairs_broken_tool_arguments() {
    let payload =
        frame(json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}))
            + "data: [DONE]\n\n";
    let (url, requests, task) = server(payload).await;
    let provider = ChatCompletionsProvider::new(ProviderConfig::new(
        crate::provider::ProviderKind::OpenAiCompatible,
        crate::provider::ApiDialect::ChatCompletions,
        url,
        "test-key",
        "test-model",
    ))
    .unwrap();
    let call = ToolCall {
        id: "call-1".into(),
        name: "read_file".into(),
        arguments: "{\"path\": ".into(),
    };
    let history = vec![
        Message::user("看下日志"),
        Message::assistant("", vec![call.clone()]).with_reasoning(Some("先读文件".into())),
        Message::tool(&call, "contents"),
    ];
    let result = provider
        .complete_with_events(&history, &[], &Events::default())
        .await;
    task.abort();
    assert_eq!(result.unwrap().content, "ok");
    let requests = requests.lock().unwrap();
    let assistant = &requests[0]["messages"][1];
    assert_eq!(assistant["reasoning_content"], "先读文件");
    let arguments = assistant["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments stay a string");
    let parsed: serde_json::Value =
        serde_json::from_str(arguments).expect("outgoing arguments parse as JSON");
    assert_eq!(parsed["_raw_arguments"], "{\"path\": ");
    assert!(
        requests[0]["messages"][0]
            .get("reasoning_content")
            .is_none()
    );
}

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

/// 线上会话里 67 条带工具调用的 assistant 有 10 条没吐思维链（`git commit`、
/// `git switch` 这类直给的步骤），压缩器插入的归档引用/摘要也没有。thinking
/// 模式带 tools 时每条 assistant 都得带 `reasoning_content`，缺一条就 400。
#[test]
fn thinking_history_backfills_empty_reasoning_on_every_assistant_message() {
    let first = ToolCall {
        id: "call-1".into(),
        name: "run".into(),
        arguments: "{\"cmd\":\"git status\"}".into(),
    };
    let second = ToolCall {
        id: "call-2".into(),
        name: "run".into(),
        arguments: "{\"cmd\":\"git commit\"}".into(),
    };
    let history = vec![
        Message::system("sys"),
        Message::user("提交一下"),
        Message::assistant("", vec![first.clone()]).with_reasoning(Some("先看状态".into())),
        Message::tool(&first, "clean"),
        Message::assistant("", vec![second.clone()]),
        Message::tool(&second, "done"),
        Message::assistant("<context-summary>…</context-summary>", Vec::new()),
    ];
    let wire = serde_json::to_value(wire_messages(&history)).unwrap();
    assert_eq!(wire[2]["reasoning_content"], "先看状态");
    assert_eq!(wire[4]["reasoning_content"], "");
    assert_eq!(wire[6]["reasoning_content"], "");
    for index in [0, 1, 3, 5] {
        assert!(wire[index].get("reasoning_content").is_none(), "{index}");
    }
}

/// 没见过思维链的端点不能凭空多出字段：严格校验的上游会拒未知字段。
#[test]
fn history_without_reasoning_sends_no_reasoning_field() {
    let call = ToolCall {
        id: "call-1".into(),
        name: "run".into(),
        arguments: "{}".into(),
    };
    let history = vec![
        Message::user("hi"),
        Message::assistant("", vec![call.clone()]),
        Message::tool(&call, "ok"),
        Message::assistant("done", Vec::new()),
    ];
    let wire = serde_json::to_value(wire_messages(&history)).unwrap();
    for message in wire.as_array().unwrap() {
        assert!(message.get("reasoning_content").is_none());
    }
}
