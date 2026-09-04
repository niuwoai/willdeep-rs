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
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MessageAttachment>,
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
        Self::plain(Role::User, content)
    }

    pub fn user_with_attachments(
        content: impl Into<String>,
        attachments: Vec<MessageAttachment>,
    ) -> Self {
        let mut message = Self::plain(Role::User, content);
        message.attachments = attachments;
        message
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls,
            attachments: Vec::new(),
        }
    }

    pub fn tool(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call.id.clone()),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
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
}

/// Remove incomplete tool round trips before replaying persisted history.
///
/// Older desktop-session imports kept `role=tool` but discarded the matching
/// camelCase tool metadata. OpenAI-compatible providers reject that history
/// before model execution because a tool message without `tool_call_id` is not
/// a valid protocol item. Complete pairs are preserved; orphan results and
/// calls without a persisted result are omitted from the replay.
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

#[derive(Clone, Debug)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
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
                content: "legacy orphan".to_owned(),
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
}
