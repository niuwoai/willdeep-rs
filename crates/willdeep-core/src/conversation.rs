//! Human-facing projection. Never mutate model history or use display progress
//! as evidence that a task has actually completed.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::types::{Message, MessageSource, Role};

/// 一次工具调用在回放里的样子：名字、脱敏摘要、有没有结果。原始参数不出投影——
/// Web 端的会话详情是公共投影，路径与凭据不能跟着走。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolTrace {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 历史里有没有这条调用的结果；没有说明轮次在这里被打断了。
    pub completed: bool,
}

/// 工具调用的一句话摘要：够回答「执行了什么」，又不把路径和凭据带出去。
///
/// - 命令只取前三个词。整条命令行常带路径，而「路径不下发浏览器」是既有承诺；
/// - 看起来像路径的词换成 `…`，包括 `/abs`、`~/x` 与 `--flag=/abs` 这种；
/// - 按命令审批同一套规则打码，再按**字符**截断——用户完全可能把 token 粘进
///   命令行。
///
/// 认不出的工具返回 `None`：宁可不显示，也不要把一段不知道含什么的参数漏出去。
pub fn tool_detail(name: &str, arguments: &str) -> Option<String> {
    const MAX_DETAIL_CHARS: usize = 60;
    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok()?;
    let detail = match name {
        "run_command" => {
            let command = parsed.get("command")?.as_str()?;
            command
                .split_whitespace()
                .take(3)
                .map(elide_path_like)
                .collect::<Vec<_>>()
                .join(" ")
        }
        "spawn_agent" => {
            let profile = parsed
                .get("profile")
                .and_then(|value| value.as_str())
                .unwrap_or("generalist");
            match parsed.get("label").and_then(|value| value.as_str()) {
                Some(label) if !label.trim().is_empty() => {
                    format!("{profile} · {}", label.trim())
                }
                _ => profile.to_owned(),
            }
        }
        _ => return None,
    };
    let redacted = crate::judge::redact_credentials(&detail);
    if redacted.trim().is_empty() {
        return None;
    }
    let mut chars = redacted.chars();
    let excerpt: String = chars.by_ref().take(MAX_DETAIL_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    })
}

/// 看起来像路径的词换成省略号。
///
/// 判定放宽到 `--flag=/abs` 这种形式：把路径藏在等号后面是最常见的漏法。
fn elide_path_like(token: &str) -> String {
    let candidate = token.split_once('=').map_or(token, |(_, value)| value);
    if candidate.starts_with('/') || candidate.starts_with('~') || candidate.starts_with("./") {
        return match token.split_once('=') {
            Some((flag, _)) => format!("{flag}=…"),
            None => "…".to_owned(),
        };
    }
    token.to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Skipped,
    Failed,
}

impl StepStatus {
    pub fn token(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: StepStatus,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub summary: String,
    pub steps: Vec<PlanStep>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationItem {
    pub role: &'static str,
    pub content: String,
    pub attachment_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    pub details: Vec<String>,
    /// 这条 assistant 消息发起的工具调用，按发起顺序；回放时逐条落成工具行。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolTrace>,
}

/// Both surfaces use exactly this projection when reopening or refreshing a
/// session. Only explicitly sourced host messages are reclassified.
pub fn project(messages: &[Message], current_plan: Option<&Plan>) -> Vec<ConversationItem> {
    let mut items: Vec<ConversationItem> = Vec::new();
    let mut active: Option<usize> = None;
    // 哪些工具调用在历史里有结果：没有的那些，轮次就是在那里被打断的。
    let answered: HashSet<&str> = messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    for message in messages {
        let role = match message.role {
            Role::User if message.source == Some(MessageSource::HostInstruction) => "system",
            Role::User => "user",
            // 只带工具调用、正文为空的 assistant 消息也要进投影：工具行回放靠它。
            Role::Assistant
                if !message.content.trim().is_empty() || !message.tool_calls.is_empty() =>
            {
                "assistant"
            }
            _ => continue,
        };
        if role == "assistant" {
            let existing = active.and_then(|index| items[index].plan.as_ref());
            if let Some((plan, is_new)) = parse_plan_reply(&message.content, existing) {
                let index = if !is_new && let Some(index) = active {
                    index
                } else {
                    items.push(item("plan", String::new(), 0));
                    items.len() - 1
                };
                items[index].plan = Some(plan);
                if !items[index].details.contains(&message.content) {
                    items[index].details.push(message.content.clone());
                }
                active = Some(index);
                continue;
            }
        }
        let mut entry = item(role, message.content.clone(), message.attachments.len());
        if role == "user"
            && let Some(authored) = user_authored_text(&message.content)
        {
            entry.content = authored.to_owned();
            // A context-only turn has no operator bubble to display.
            if entry.content.trim().is_empty() && entry.attachment_count == 0 {
                continue;
            }
        }
        if role == "system" {
            entry.details.push(std::mem::take(&mut entry.content));
        }
        if role == "assistant" {
            entry.tools = message
                .tool_calls
                .iter()
                .map(|call| ToolTrace {
                    name: call.name.clone(),
                    detail: tool_detail(&call.name, &call.arguments),
                    completed: answered.contains(call.id.as_str()),
                })
                .collect();
        }
        items.push(entry);
    }
    // Persisted host state wins over model self-reports. Do not apply a newer
    // phase's state to an unrelated historical plan.
    if let Some(plan) = current_plan.filter(|plan| !plan.steps.is_empty()) {
        let matching = items.iter().rposition(|item| {
            item.plan.as_ref().is_some_and(|p| {
                p.steps.len() == plan.steps.len()
                    && (p
                        .steps
                        .iter()
                        .zip(&plan.steps)
                        .all(|(a, b)| a.text == b.text)
                        || p.steps.iter().all(|step| {
                            step.text == step.id
                                && !step.id.chars().all(|c| c.is_ascii_digit())
                                && step_index(plan, &step.id).is_some()
                        }))
            })
        });
        if let Some(index) = matching {
            items[index].plan = Some(plan.clone());
        } else {
            let mut entry = item("plan", String::new(), 0);
            entry.plan = Some(plan.clone());
            items.push(entry);
        }
    }
    items
}

const ATTACHED_CONTEXT_HEADER: &str = "The user attached the following context for this turn — treat it as task setup, not verbatim user text:";
const USER_TEXT_BOUNDARY: &str = "<<<willdeep:user-message:v1>>>";

/// The desktop composer escapes this boundary inside attachments. Require the
/// exact protocol header and a standalone boundary, never guess from skill names
/// or an ordinary sentence that happens to mention the protocol.
pub fn user_authored_text(content: &str) -> Option<&str> {
    let normalized = content.trim_start();
    let (header, _) = normalized.split_once('\n')?;
    if header.trim_end_matches('\r') != ATTACHED_CONTEXT_HEADER {
        return None;
    }
    let mut offset = 0;
    for line in normalized.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == USER_TEXT_BOUNDARY {
            return Some(normalized[offset..].trim());
        }
    }
    None
}

fn item(role: &'static str, content: String, attachment_count: usize) -> ConversationItem {
    ConversationItem {
        role,
        content,
        attachment_count,
        plan: None,
        details: Vec::new(),
        tools: Vec::new(),
    }
}

/// Parse only complete, explicitly typed fences. Ordinary prose, user-pasted
/// examples, unclosed streams and unknown statuses are never consumed.
pub fn parse_plan_reply(content: &str, existing: Option<&Plan>) -> Option<(Plan, bool)> {
    let mut plan = existing.cloned();
    let mut new_plan = false;
    let mut found = false;
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if !trimmed.starts_with("```") {
            continue;
        }
        let kind = trimmed.strip_prefix("```")?;
        if kind != "plan" && kind != "progress" {
            return None;
        }
        let mut body = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim() == "```" {
                closed = true;
                break;
            }
            if !line.trim().is_empty() {
                body.push(line.trim());
            }
        }
        if !closed || body.is_empty() {
            return None;
        }
        if kind == "plan" {
            let steps = body
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let (number, text) = line.split_once(". ")?;
                    if number.parse::<usize>().ok()? != index + 1 || text.trim().is_empty() {
                        return None;
                    }
                    Some(PlanStep {
                        id: number.to_owned(),
                        text: text.to_owned(),
                        status: StepStatus::Pending,
                        detail: None,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            plan = Some(Plan {
                summary: String::new(),
                steps,
            });
            new_plan = true;
        } else {
            let updates = body
                .iter()
                .map(|line| parse_update(line))
                .collect::<Option<Vec<_>>>()?;
            if let Some(plan) = plan.as_mut() {
                let indices = updates
                    .iter()
                    .map(|(key, _)| step_index(plan, key))
                    .collect::<Option<Vec<_>>>()?;
                for ((_, status), index) in updates.into_iter().zip(indices) {
                    plan.steps[index].status = status;
                }
            } else {
                plan = Some(Plan {
                    summary: String::new(),
                    steps: updates
                        .into_iter()
                        .map(|(id, status)| PlanStep {
                            text: id.clone(),
                            id,
                            status,
                            detail: None,
                        })
                        .collect(),
                });
            }
        }
        found = true;
    }
    found.then_some((plan?, new_plan))
}

fn parse_update(line: &str) -> Option<(String, StepStatus)> {
    let (id, status) = line.split_once(':').or_else(|| line.split_once(". "))?;
    let id = id.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let status =
        serde_json::from_value(serde_json::Value::String(status.trim().to_owned())).ok()?;
    Some((id.to_owned(), status))
}

fn step_index(plan: &Plan, key: &str) -> Option<usize> {
    plan.steps
        .iter()
        .position(|step| step.id == key)
        .or_else(|| {
            key.parse::<usize>()
                .ok()
                .filter(|index| *index > 0 && *index <= plan.steps.len())
                .map(|index| index - 1)
        })
        .or_else(|| {
            plan.steps.iter().position(|step| {
                step.text
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                    .any(|word| word == key)
            })
        })
}

#[cfg(test)]
mod tests;
