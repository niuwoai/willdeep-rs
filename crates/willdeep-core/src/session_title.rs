//! 会话标题的两级生成：先确定性派生，再一次性模型润色。
//!
//! 历史会话面板里 20 条 `New session` 等于没有列表——人只能靠 UUID 前八位
//! 认会话，而 UUID 不携带任何语义。macOS 版 Xedit 早就解决了这件事，做法是
//! 两级：
//!
//! * **L1 派生**（[`derive_from_prompt`]）——第一条用户消息的前若干字符。
//!   不花模型调用、不可能失败，提交那一刻列表里就有可读的标题。
//! * **L2 摘要**（[`summarize`]）——第一轮助手回复落地后，用便宜模型跑一次
//!   单发请求，把「这段对话在干什么」压成一行短句。
//!
//! 两级的分工是刻意的：L2 会因为没配模型、断网、模型抽风而拿不到结果，
//! 而那时列表里必须已经有 L1 的标题兜底。**只有 L2 失败会静默**，L1 不会。
//!
//! [`TitleSource`] 是这台状态机的唯一裁决位：
//!
//! ```text
//! Legacy ─┬─(标题是占位符)─→ Derived ─→ Summarized
//!         └─(标题不是占位符)─→ 不动（老会话的人工标题，见下）
//! 任何状态 ──/session rename──→ User（终态，自动流程永不覆盖）
//! ```
//!
//! `Legacy` 是这个字段出现之前就存在的会话。它们没有来源信息，只能看标题
//! 本身：`New session` 这种占位符可以接管，其它一律当人写的。**宁可漏改一个
//! 该改的，也不能改掉一个人自己起的名字**——前者是标题不够好看，后者是数据丢失。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::provider::Provider;
use crate::types::{Message, Role};

/// L1 派生标题的字符上限。列表一行放不下更多，再长只是把后面的元信息挤掉。
pub const MAX_DERIVED_TITLE_CHARS: usize = 80;

/// L2 摘要标题的字符上限。提示词要的是 12 字以内，这里是防模型不听话的硬闸。
pub const MAX_SUMMARY_TITLE_CHARS: usize = 24;

/// 送进摘要请求的单侧正文上限。标题调用要一直便宜，不能随对话长度增长。
const MAX_SUMMARY_INPUT_CHARS: usize = 800;

/// 没有可用素材时的占位标题。也是 [`is_placeholder`] 认得的名字之一。
pub const PLACEHOLDER_TITLE: &str = "New session";

/// 只有附件、没有正文时的占位标题。
const ATTACHMENT_TITLE: &str = "Attachment conversation";

/// 标题的来源，决定自动流程还能不能改它。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// 字段出现之前就存在的会话，或外部（Xedit）写入的会话。
    #[default]
    Legacy,
    /// L1：从第一条用户消息确定性派生。
    Derived,
    /// L2：模型摘要过。自动流程到此为止，不再自动改写。
    Summarized,
    /// 人显式命名（`/session rename`）。终态。
    User,
}

impl TitleSource {
    /// `title` 配上这个来源时，自动流程能不能接管。
    ///
    /// `Legacy` 要看标题本身：占位符可以接管，其它当人写的。
    pub fn allows_auto_title(self, title: &str) -> bool {
        match self {
            Self::Derived => true,
            Self::Legacy => is_placeholder(title),
            Self::Summarized | Self::User => false,
        }
    }
}

/// 这个标题是不是「等于没有标题」。
///
/// 名单里既有 rs 自己历史上用过的几种占位符，也有 Xedit 侧的中英文默认名——
/// 两端共享会话文件，一个 Xedit 建的 `新对话` 在 rs 这边同样该被接管。
pub fn is_placeholder(title: &str) -> bool {
    let normalized = title.trim().to_lowercase();
    if normalized.is_empty() {
        return true;
    }
    [
        "new session",
        "new runtime session",
        "swift session",
        "new chat",
        "chat",
        "untitled",
        "新对话",
        "新聊天",
        "新会话",
        "新的对话",
        "未命名",
    ]
    .contains(&normalized.as_str())
}

/// L1：把第一条用户提示词压成标题。
///
/// 提示词是不可信文本，可能整条就是一个密钥。命中凭据特征时**不截断、不脱敏、
/// 直接放弃**回到占位符——半截密钥仍然是密钥，而标题会出现在会话列表、
/// 通知载荷和导出文件里，交出去就收不回来了。
pub fn derive_from_prompt(prompt: &str, has_attachments: bool) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return if has_attachments {
            ATTACHMENT_TITLE.to_owned()
        } else {
            PLACEHOLDER_TITLE.to_owned()
        };
    }
    if looks_sensitive(&normalized) {
        return PLACEHOLDER_TITLE.to_owned();
    }
    clamp_title(&normalized, MAX_DERIVED_TITLE_CHARS)
}

/// L1 的历史版本：从已有消息里找第一条用户输入来派生。
///
/// 用在「这条会话早就存在、标题却还是占位符」的补救路径上。此时用**当前这轮**
/// 的提示词派生会得到一个断章取义的标题（第 40 轮的「继续」），而第一条用户
/// 消息才是这段对话真正在说的事。
pub fn derive_from_messages(messages: &[Message]) -> Option<String> {
    let first_user = messages
        .iter()
        .find(|message| message.role == Role::User && !message.content.trim().is_empty())?;
    let title = derive_from_prompt(&first_user.content, !first_user.attachments.is_empty());
    (!is_placeholder(&title)).then_some(title)
}

/// L2：一次性摘要调用。失败一律返回 `None`，调用方保留 L1 标题。
///
/// 只发第一轮的一问一答，且两侧都截断——标题调用的成本必须与对话长度无关，
/// 否则它会在最该便宜的地方（长会话）变得最贵。
pub async fn summarize(
    provider: Arc<dyn Provider>,
    first_user: &str,
    first_assistant: &str,
) -> Option<String> {
    let user_clip = clip(first_user, MAX_SUMMARY_INPUT_CHARS);
    let assistant_clip = clip(first_assistant, MAX_SUMMARY_INPUT_CHARS);
    if user_clip.trim().is_empty() {
        return None;
    }
    let messages = [
        Message::system(SUMMARY_SYSTEM_PROMPT),
        Message::user(format!(
            "USER:\n{user_clip}\n\nASSISTANT:\n{assistant_clip}"
        )),
    ];
    let completion = provider.complete(&messages, &[]).await.ok()?;
    sanitize_summary(&completion.content)
}

const SUMMARY_SYSTEM_PROMPT: &str = "\
你是一个把对话浓缩成短标题的工具。
读取 USER 和 ASSISTANT 两段，输出**单行**标题。
硬性要求：
- 不超过 12 个字符（中文按字数算，英文按词数算且不超过 6 个英文词）；
- 用户用中文则输出中文，用户用英文则输出英文；
- 不要任何引号、前后缀、标点结尾、解释、emoji、\"标题：\" 之类的导言；
- 直接输出标题本身，纯文本；
- 抓住核心动作或主题，例如 \"排查 CPU 负载\"、\"配置 SSH 登录\"、\"修复登录 bug\"，\
避免笼统词如 \"技术问题\" / \"帮助\"。";

/// 清洗模型返回的标题。模型会加引号、加「标题：」、多写一行解释、超长，
/// 提示词说了不要也照样会——所以这里逐条拆掉，而不是相信提示词。
///
/// 同样跑一次凭据检查：摘要的输入里有用户提示词，模型完全可能把密钥抄进标题。
pub fn sanitize_summary(raw: &str) -> Option<String> {
    let mut title = raw.trim().to_owned();
    // 只取第一行：模型偶尔会补一行「（基于……）」。
    if let Some(line) = title.lines().next() {
        title = line.trim().to_owned();
    }
    let lowercase = title.to_lowercase();
    for prefix in ["title:", "title：", "标题:", "标题：", "title ", "标题 "] {
        if lowercase.starts_with(prefix) {
            title = title[prefix.len()..].trim().to_owned();
            break;
        }
    }
    // 剥到不再变化为止：`**加粗**` 是模型最常见的越界方式，只剥一层会留下
    // 一对孤零零的星号。上限防的是理论上的病态输入，不是现实。
    for _ in 0..4 {
        let stripped = strip_wrapping_pair(&title);
        if stripped == title {
            break;
        }
        title = stripped;
    }
    while title
        .chars()
        .next_back()
        .is_some_and(|character| "。.!?！？,，;；:：".contains(character))
    {
        title.pop();
    }
    let title = title.trim();
    if title.is_empty() || is_placeholder(title) || looks_sensitive(title) {
        return None;
    }
    Some(clamp_title(title, MAX_SUMMARY_TITLE_CHARS))
}

/// 成对的包裹符号剥一层。调用方负责循环。
fn strip_wrapping_pair(title: &str) -> String {
    const PAIRS: [(char, char); 10] = [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('“', '”'),
        ('‘', '’'),
        ('「', '」'),
        ('『', '』'),
        ('【', '】'),
        ('*', '*'),
        ('_', '_'),
    ];
    let mut characters = title.chars();
    let (Some(first), Some(last)) = (characters.next(), characters.next_back()) else {
        return title.to_owned();
    };
    if title.chars().count() < 2 {
        return title.to_owned();
    }
    for (open, close) in PAIRS {
        if first == open && last == close {
            return title
                .chars()
                .skip(1)
                .take(title.chars().count() - 2)
                .collect::<String>()
                .trim()
                .to_owned();
        }
    }
    title.to_owned()
}

/// 凭据特征检查。宁可错杀一个标题，不可漏放一个密钥。
fn looks_sensitive(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    let marker = [
        "api_key",
        "api key",
        "password",
        "passwd",
        "secret",
        "authorization:",
        "bearer ",
        "private key",
        "access_token",
        "refresh_token",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "akia",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker));
    marker || text.split_whitespace().any(is_high_entropy_token)
}

fn is_high_entropy_token(token: &str) -> bool {
    let token = token.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    token.len() >= 24
        && token.bytes().any(|byte| byte.is_ascii_lowercase())
        && token.bytes().any(|byte| byte.is_ascii_uppercase())
        && token.bytes().any(|byte| byte.is_ascii_digit())
}

fn clamp_title(title: &str, limit: usize) -> String {
    if title.chars().count() <= limit {
        return title.to_owned();
    }
    let mut clamped = title
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    clamped.push('…');
    clamped
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_titles_are_recognized_across_both_frontends() {
        for title in [
            "",
            "   ",
            "New session",
            "new runtime session",
            "Swift session",
            "新对话",
            "未命名",
        ] {
            assert!(is_placeholder(title), "{title:?} should be a placeholder");
        }
        for title in ["修复登录 bug", "New session plan", "chat with ops"] {
            assert!(!is_placeholder(title), "{title:?} is a real title");
        }
    }

    /// 人起的名字是终态。自动流程连看都不该看它一眼。
    #[test]
    fn user_and_summarized_titles_are_never_auto_replaced() {
        assert!(!TitleSource::User.allows_auto_title("New session"));
        assert!(!TitleSource::Summarized.allows_auto_title("New session"));
        assert!(TitleSource::Derived.allows_auto_title("修复登录 bug"));
    }

    /// 老会话没有来源信息，只能看标题：占位符接管，其它一律不碰。
    #[test]
    fn legacy_sessions_are_adopted_only_when_the_title_is_a_placeholder() {
        assert!(TitleSource::Legacy.allows_auto_title("New session"));
        assert!(!TitleSource::Legacy.allows_auto_title("排查 CPU 负载"));
    }

    #[test]
    fn derived_title_is_the_first_prompt_clamped() {
        assert_eq!(derive_from_prompt("修复登录 bug", false), "修复登录 bug");
        assert_eq!(
            derive_from_prompt("  多余   空白    要压平 ", false),
            "多余 空白 要压平"
        );
        let long = "x".repeat(200);
        let title = derive_from_prompt(&long, false);
        assert_eq!(title.chars().count(), MAX_DERIVED_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    /// 标题会进列表、通知和导出文件。带凭据的提示词一个字都不许留下。
    #[test]
    fn credential_shaped_prompts_fall_back_to_the_placeholder() {
        for prompt in [
            "export OPENAI_API_KEY=sk-abcdef0123456789",
            "把 password 改成 hunter2",
            "curl -H 'Authorization: Bearer eyJhbGciOi' https://x",
            "token aB3xY9zQ1mN7pL4kR8sT2vW6",
        ] {
            assert_eq!(
                derive_from_prompt(prompt, false),
                PLACEHOLDER_TITLE,
                "{prompt:?} leaked into the title"
            );
        }
    }

    #[test]
    fn empty_prompt_with_attachments_says_so() {
        assert_eq!(derive_from_prompt("", true), ATTACHMENT_TITLE);
        assert_eq!(derive_from_prompt("", false), PLACEHOLDER_TITLE);
    }

    /// 补救路径要用**第一条**用户消息，不是最近那条。
    #[test]
    fn messages_derive_from_the_first_user_turn_not_the_latest() {
        let messages = vec![
            Message::user("重构订单模块的库存扣减"),
            Message::assistant("好的", Vec::new()),
            Message::user("继续"),
        ];
        assert_eq!(
            derive_from_messages(&messages).as_deref(),
            Some("重构订单模块的库存扣减")
        );
        assert_eq!(derive_from_messages(&[]), None);
    }

    #[test]
    fn summary_is_stripped_of_everything_the_prompt_asked_the_model_not_to_add() {
        assert_eq!(
            sanitize_summary("标题：修复登录 bug").as_deref(),
            Some("修复登录 bug")
        );
        assert_eq!(
            sanitize_summary("「排查 CPU 负载」").as_deref(),
            Some("排查 CPU 负载")
        );
        assert_eq!(
            sanitize_summary("**配置 SSH**").as_deref(),
            Some("配置 SSH")
        );
        assert_eq!(
            sanitize_summary("修复登录 bug。").as_deref(),
            Some("修复登录 bug")
        );
        assert_eq!(
            sanitize_summary("排查 CPU 负载\n（基于第一轮对话）").as_deref(),
            Some("排查 CPU 负载")
        );
    }

    /// 提示词说 12 字，模型照样能写一整段。硬闸不能只写在提示词里。
    #[test]
    fn summary_longer_than_the_cap_is_clamped_not_rejected() {
        let title = sanitize_summary(&"标".repeat(100)).expect("clamped title");
        assert_eq!(title.chars().count(), MAX_SUMMARY_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    /// 摘要的输入里有用户提示词，模型完全可能把密钥抄进标题。
    #[test]
    fn summary_carrying_a_credential_is_rejected_outright() {
        assert_eq!(sanitize_summary("sk-abcdef0123456789 的用法"), None);
        assert_eq!(sanitize_summary(""), None);
        assert_eq!(sanitize_summary("新会话"), None);
    }
}
