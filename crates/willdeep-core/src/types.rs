use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Display provenance, independent of the provider-facing role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MessageSource>,
    pub content: String,
    /// 思考型模型（DeepSeek、GLM 等）在 thinking 模式下要求把上一轮的
    /// `reasoning_content` 原样回传，否则整条历史会被上游判为非法。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageSource {
    OperatorInput,
    HostInstruction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageAttachment {
    Text {
        name: String,
        content: String,
    },
    Image {
        name: String,
        media_type: String,
        data: String,
        width: u32,
        height: u32,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        let mut message = Self::plain(Role::User, content);
        message.source = Some(MessageSource::OperatorInput);
        message
    }

    pub fn host_instruction(content: impl Into<String>) -> Self {
        let mut message = Self::plain(Role::User, content);
        message.source = Some(MessageSource::HostInstruction);
        message
    }

    pub fn user_with_attachments(
        content: impl Into<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Self {
        let mut message = Self::user(content);
        message.attachments = attachments;
        message
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            source: None,
            content: content.into(),
            reasoning: None,
            tool_call_id: None,
            tool_calls,
            attachments: Vec::new(),
        }
    }

    /// 带上模型这一轮的思维链原文。空串视为「没有」，不占用协议字段。
    pub fn with_reasoning(mut self, reasoning: Option<String>) -> Self {
        self.reasoning = reasoning.filter(|value| !value.is_empty());
        self
    }

    pub fn tool(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            source: None,
            content: content.into(),
            reasoning: None,
            tool_call_id: Some(call.id.clone()),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            source: None,
            content: content.into(),
            reasoning: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    pub fn parsed_arguments(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }

    /// 出站前必须成立的不变量：`function.arguments` 是一个合法 JSON **对象**的字符串。
    ///
    /// 上游对这条要求的严格程度各不相同——同一条截断的 `{"path": ` 在一家是 200，
    /// 在另一家就是 `Assistant tool call function.arguments must be valid JSON.` 的
    /// 400，而且整条历史一起被拒，会话从此卡死。与其把脏数据发出去赌运气，不如在
    /// 本地补成对象：空串补 `{}`，非对象的原文装进 `_raw_arguments` 保留证据。
    pub fn normalized_arguments(&self) -> std::borrow::Cow<'_, str> {
        if matches!(
            serde_json::from_str::<Value>(&self.arguments),
            Ok(Value::Object(_))
        ) {
            return std::borrow::Cow::Borrowed(&self.arguments);
        }
        if self.arguments.trim().is_empty() {
            return std::borrow::Cow::Owned("{}".to_owned());
        }
        std::borrow::Cow::Owned(serde_json::json!({ "_raw_arguments": self.arguments }).to_string())
    }

    /// 就地修复 `arguments`，用于回放持久化历史前的清洗。
    pub fn normalize_arguments(&mut self) {
        if let std::borrow::Cow::Owned(normalized) = self.normalized_arguments() {
            self.arguments = normalized;
        }
    }
}

/// Remove incomplete tool round trips before replaying persisted history.
///
/// Older desktop-session imports kept `role=tool` but discarded the matching
/// camelCase tool metadata. OpenAI-compatible providers reject that history
/// before model execution because a tool message without `tool_call_id` is not
/// a valid protocol item. Complete pairs are preserved; orphan results and
/// calls without a persisted result are omitted from the replay.
///
/// 同时修复 `function.arguments`：历史可能来自非流式解码、旧版桌面会话导入或
/// 其它 Provider 分支，这些路径都没有流式解码器那道 JSON 对象校验。
pub fn sanitize_tool_history(messages: &mut Vec<Message>) {
    let mut pending = HashSet::new();
    let mut matched = HashSet::new();
    let mut valid_tool_messages = HashSet::new();

    for (index, message) in messages.iter().enumerate() {
        match message.role {
            Role::Assistant => {
                for call in &message.tool_calls {
                    if !call.id.trim().is_empty() && !call.name.trim().is_empty() {
                        pending.insert(call.id.clone());
                    }
                }
            }
            Role::Tool => {
                let Some(call_id) = message
                    .tool_call_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                if pending.remove(call_id) {
                    matched.insert(call_id.to_owned());
                    valid_tool_messages.insert(index);
                }
            }
            Role::System | Role::User => {}
        }
    }

    let mut index = 0_usize;
    messages.retain_mut(|message| {
        let keep = match message.role {
            Role::Assistant => {
                message.tool_calls.retain(|call| matched.contains(&call.id));
                for call in &mut message.tool_calls {
                    call.normalize_arguments();
                }
                true
            }
            Role::Tool => valid_tool_messages.contains(&index),
            Role::System | Role::User => true,
        };
        index += 1;
        keep
    });
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub content: String,
    /// 思考型模型这一轮的 `reasoning_content`，需要随 assistant 消息回传给上游。
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

impl Completion {
    pub fn is_incomplete(&self) -> bool {
        self.finish_reason.as_deref().is_some_and(|reason| {
            matches!(
                reason,
                "length"
                    | "max_tokens"
                    | "incomplete"
                    | "content_filter"
                    | "pause_turn"
                    | "model_context_window_exceeded"
            )
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// 命中提示词缓存的输入 token。`None` 是「Provider 没报」，`Some(0)` 是
    /// 「确实一条没命中」——两者不是一回事，界面只在知道时展示命中率。
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_incomplete_tool_history_without_touching_complete_pairs() {
        let complete = ToolCall {
            id: "call-complete".to_owned(),
            name: "read_file".to_owned(),
            arguments: "{}".to_owned(),
        };
        let incomplete = ToolCall {
            id: "call-incomplete".to_owned(),
            name: "run_command".to_owned(),
            arguments: "{}".to_owned(),
        };
        let mut messages = vec![
            Message::user("inspect"),
            Message::assistant("", vec![complete.clone(), incomplete]),
            Message::tool(&complete, "contents"),
            Message {
                role: Role::Tool,
                source: None,
                content: "legacy orphan".to_owned(),
                reasoning: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            },
            Message::assistant("done", Vec::new()),
        ];

        sanitize_tool_history(&mut messages);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].tool_calls.len(), 1);
        assert_eq!(messages[1].tool_calls[0].id, complete.id);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call-complete"));
        assert_eq!(messages[3].content, "done");
    }

    #[test]
    fn tool_arguments_are_repaired_into_json_objects_before_replay() {
        let object = ToolCall {
            id: "a".to_owned(),
            name: "read_file".to_owned(),
            arguments: r#"{"path":"a.txt"}"#.to_owned(),
        };
        // 原样通过，不重新序列化，免得把上游给的格式改了。
        assert_eq!(object.normalized_arguments(), r#"{"path":"a.txt"}"#);

        let blank = ToolCall {
            arguments: "   ".to_owned(),
            ..object.clone()
        };
        assert_eq!(blank.normalized_arguments(), "{}");

        // 截断和非对象都不能直接出站：证据留在 _raw_arguments 里。
        for broken in [r#"{"path": "#, "null", "[1,2]", "None"] {
            let call = ToolCall {
                arguments: broken.to_owned(),
                ..object.clone()
            };
            let normalized = call.normalized_arguments();
            let parsed: Value =
                serde_json::from_str(&normalized).expect("normalized arguments parse");
            assert_eq!(parsed["_raw_arguments"], broken);
        }
    }

    #[test]
    fn replayed_history_repairs_arguments_it_never_validated_on_the_way_in() {
        let broken = ToolCall {
            id: "call-broken".to_owned(),
            name: "read_file".to_owned(),
            arguments: "{\"path\": ".to_owned(),
        };
        let mut messages = vec![
            Message::user("inspect"),
            Message::assistant("", vec![broken.clone()]),
            Message::tool(&broken, "contents"),
        ];

        sanitize_tool_history(&mut messages);

        let arguments = &messages[1].tool_calls[0].arguments;
        serde_json::from_str::<Value>(arguments).expect("sanitized history is replayable");
    }
}
