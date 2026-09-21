//! 轮次结束后预测用户的下一句。
//!
//! 与 macOS 版（Xedit `AgentInputSuggestion`，1.385.0-rc1）同一套契约：轮次收尾后
//! 额外发**一次**小请求，用标题摘要那一档模型，只看最近两条用户原话和助手回复的
//! 尾部，让模型以**用户的口吻**写出最可能的下一条消息。宿主把它以灰字放在空输入框
//! 里，Tab 采用、打字或 Esc 放弃。预测只活在内存里：不落盘、不进 Runtime 协议，
//! 也不该为它中断任何东西——拿不到就当没这回事。
//!
//! 这里只有纯函数和一次 Provider 往返；什么时候发、结果晚到怎么丢，是宿主的事。

use std::sync::Arc;

use crate::provider::{Provider, ProviderError};
use crate::session_title::{clip, looks_sensitive, strip_wrapping_pair};
use crate::types::{Message, MessageSource, Role};

/// 只带最近这几条用户原话：预测的是「接下来一句」，不是整段对话的总结。
const RECENT_USER_MESSAGE_LIMIT: usize = 2;
/// 每条用户原话只取开头这么多字符。
const USER_MESSAGE_CLIP: usize = 400;
/// 助手回复取**尾部**：结论和「要不要我继续」都在末尾，开头往往是复述任务。
const ASSISTANT_TAIL_CLIP: usize = 1500;
/// 提示词要求 60 字以内；超过这个数就不是一句话而是一段话，直接丢。
pub const MAX_SUGGESTION_CHARS: usize = 120;

/// 固定英文；输出语言靠「跟用户走」这条规则，不另做 i18n——它是给模型看的。
const SYSTEM_PROMPT: &str = "\
You predict the USER's next message in a chat with a coding agent.
You are given the USER's most recent messages and the tail of the ASSISTANT's latest reply.
Write the single most likely message the USER would send next, in the USER's own voice.
Rules:
- One line, at most 60 characters.
- Use the same language the USER writes in.
- No quotes, no \"User:\" prefix, no explanation, no markdown.
- If the assistant asked a yes/no question or offered to continue, answer it the way this user would.
- Never speak as the assistant. Never start with \"Sure\", \"I'll\", \"Let me\" or the like.
- If there is no obvious next step, output exactly: NONE";

/// 模型返回的那一行被判定为「助手口吻」的开头。用户不会这样开口，模型会。
const ASSISTANT_VOICE_PREFIXES: &[&str] = &[
    "好的，我",
    "好的,我",
    "好的。我",
    "收到，我",
    "我来",
    "我会",
    "我将",
    "我这就",
    "我马上",
    "让我",
    "sure, i",
    "sure! i",
    "sure, let",
    "okay, i",
    "ok, i",
    "i'll",
    "i will",
    "i've",
    "let me",
    "here's",
    "here is",
    "はい、",
    "承知しました",
    "かしこまりました",
    "了解しました",
    "では、",
];

/// 模型爱加的导语。提示词说了不要，它照样加，所以逐个剥掉而不是相信提示词。
const LEADING_LABELS: &[&str] = &[
    "user:",
    "user：",
    "用户:",
    "用户：",
    "next:",
    "next：",
    "next message:",
    "下一句:",
    "下一句：",
    "建议:",
    "建议：",
    "suggestion:",
    "suggestion：",
];

/// 组预测请求的正文。缺任何一边都返回 `None`：没有用户原话预测不了口吻，
/// 助手还没说话则没什么可接的。宿主注入的指令不算用户原话。
///
/// 成本与对话长度无关：最多两条用户原话各 400 字，加助手回复尾部 1500 字。
pub fn payload(messages: &[Message]) -> Option<String> {
    let last_assistant = messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant && !message.content.trim().is_empty())?;
    let mut recent_users = messages
        .iter()
        .rev()
        .filter(|message| {
            message.role == Role::User
                && message.source != Some(MessageSource::HostInstruction)
                && !message.content.trim().is_empty()
        })
        .take(RECENT_USER_MESSAGE_LIMIT)
        .map(|message| message.content.trim())
        .collect::<Vec<_>>();
    if recent_users.is_empty() {
        return None;
    }
    recent_users.reverse();

    let mut body = String::new();
    for user in recent_users {
        body.push_str("USER:\n");
        body.push_str(clip(user, USER_MESSAGE_CLIP).trim());
        body.push_str("\n\n");
    }
    body.push_str("ASSISTANT (tail of the latest reply):\n");
    body.push_str(tail(last_assistant.content.trim(), ASSISTANT_TAIL_CLIP).trim());
    Some(body)
}

/// 发给模型的那两条消息。实弹评测要看清洗之前的原始输出，所以单独拿出来。
pub(crate) fn request_messages(payload: &str) -> [Message; 2] {
    [Message::system(SYSTEM_PROMPT), Message::user(payload)]
}

/// 一次往返。`Ok(None)` 是「请求成功但没有可用预测」（模型说 NONE、或输出没过清洗），
/// `Err` 才是请求本身失败——调用方靠这个区分「换下一家再问」和「就此作罢」。
pub async fn predict(
    provider: Arc<dyn Provider>,
    payload: &str,
) -> Result<Option<String>, ProviderError> {
    let completion = provider.complete(&request_messages(payload), &[]).await?;
    Ok(sanitize(&completion.content))
}

/// 按顺序问一组候选 Provider：前一家请求失败才换下一家；有一家答了（哪怕是
/// `None`）就到此为止。TUI 的 Agent 与 Web 端点共用这一段，候选顺序由宿主定。
pub async fn predict_first(providers: &[Arc<dyn Provider>], payload: &str) -> Option<String> {
    for provider in providers {
        if let Ok(suggestion) = predict(provider.clone(), payload).await {
            return suggestion;
        }
    }
    None
}

/// 清洗模型返回的那一行。空、`NONE`、超长、助手口吻、疑似凭据一律 `None`。
pub fn sanitize(raw: &str) -> Option<String> {
    let mut text = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .to_owned();
    // 剥导语：`User: …`、`下一句：…`。剥到不再变化为止，模型偶尔会叠两层。
    loop {
        let lowercase = text.to_lowercase();
        let Some(label) = LEADING_LABELS
            .iter()
            .find(|label| lowercase.starts_with(*label))
        else {
            break;
        };
        text = text
            .chars()
            .skip(label.chars().count())
            .collect::<String>()
            .trim()
            .to_owned();
    }
    // 剥包裹：引号、书名号、`**加粗**`。上限防的是病态输入，不是现实。
    for _ in 0..4 {
        let stripped = strip_wrapping_pair(&text);
        if stripped == text {
            break;
        }
        text = stripped;
    }
    let text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let lowercase = text.to_lowercase();
    if lowercase.trim_end_matches(['.', '。', '!', '！']) == "none" {
        return None;
    }
    if text.chars().count() > MAX_SUGGESTION_CHARS {
        return None;
    }
    if ASSISTANT_VOICE_PREFIXES
        .iter()
        .any(|prefix| lowercase.starts_with(prefix))
    {
        return None;
    }
    // 输入里有用户提示词和助手回复，模型完全可能把里面的密钥抄回来当「下一句」。
    if looks_sensitive(&text) {
        return None;
    }
    Some(text)
}

fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant(text, Vec::new())
    }

    #[test]
    fn payload_uses_recent_context_only() {
        let long_tail = "x".repeat(2_000);
        let messages = [
            user("first request"),
            assistant("a1"),
            user("second request"),
            assistant("a2"),
            user("third request"),
            assistant(&format!("{long_tail}END")),
        ];
        let payload = payload(&messages).expect("payload");
        assert!(
            !payload.contains("first request"),
            "only the last two user messages"
        );
        assert!(payload.contains("second request"));
        assert!(payload.contains("third request"));
        assert!(
            payload.ends_with("END"),
            "assistant reply is taken from the tail"
        );
        assert!(payload.chars().count() < ASSISTANT_TAIL_CLIP + 2 * USER_MESSAGE_CLIP + 120);
    }

    #[test]
    fn payload_requires_both_sides() {
        assert!(payload(&[user("hi")]).is_none());
        assert!(payload(&[assistant("hello")]).is_none());
        assert!(payload(&[user("hi"), assistant("   \n")]).is_none());
        let mut host = user("do it");
        host.source = Some(MessageSource::HostInstruction);
        assert!(
            payload(&[host, assistant("ok")]).is_none(),
            "host instructions are not the user's words"
        );
    }

    #[test]
    fn sanitize_cleans_output() {
        assert_eq!(
            sanitize("\"Run the tests again\"\nbecause the assistant offered to").as_deref(),
            Some("Run the tests again")
        );
        assert_eq!(
            sanitize("User: 提交并合回 develop").as_deref(),
            Some("提交并合回 develop")
        );
        assert_eq!(sanitize("用户：「继续」").as_deref(), Some("继续"));
        assert_eq!(
            sanitize("**yes, go ahead**").as_deref(),
            Some("yes, go ahead")
        );
        assert_eq!(
            sanitize("\n\n  是的，继续  \n").as_deref(),
            Some("是的，继续")
        );
    }

    #[test]
    fn sanitize_rejects_non_suggestions() {
        assert!(sanitize("NONE").is_none());
        assert!(sanitize("none.").is_none());
        assert!(sanitize("\"NONE\"").is_none());
        assert!(sanitize("   \n  ").is_none());
        assert!(sanitize(&"很".repeat(MAX_SUGGESTION_CHARS + 1)).is_none());
        assert!(sanitize("好的，我来修复这个问题").is_none());
        assert!(sanitize("Sure, I'll run the tests").is_none());
        assert!(sanitize("Let me check the logs").is_none());
        assert!(sanitize("承知しました。すぐ直します").is_none());
        assert!(sanitize("use api_key sk-abcdef123456").is_none());
    }
}
