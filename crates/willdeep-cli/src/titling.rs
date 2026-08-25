//! 会话标题的接管点：谁在什么时候把标题从占位符换成人话。
//!
//! 判定规则全在 [`willdeep_core::session_title`]，这里只负责「在哪一刻调用它」。
//! 三个前端（TUI 本地轮次、Runtime Harness、headless run）各有自己的轮次循环，
//! 但都必须落在同一对钩子上，否则同一个会话在不同入口下会得到不同的标题策略：
//!
//! * [`apply_derived_title`] —— 提交提示词那一刻，同步、不花模型调用。
//! * [`apply_summarized_title`] —— 第一轮助手回复落地之后，一次性摘要。
//!
//! 两个函数都返回「标题是否变了」，让调用方决定要不要落盘、要不要发事件。
//! 它们自己不写存储：TUI 与 Harness 的保存时机不同，多写一次 `save` 会把
//! `updated_at` 顶掉，进而打乱历史列表按最近使用的排序。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use willdeep_core::session_title::{self, TitleSource};
use willdeep_core::{Agent, Message, Role, Session};

/// 本进程内已经试过摘要的会话。
///
/// 摘要成功会落 [`TitleSource::Summarized`]，那是持久的终态；**失败没有终态**，
/// 而失败可能每轮复现（模型名写错、网关一直 4xx、模型每次都吐垃圾）。没有这个
/// 集合的话，一条治不好的会话会在每一轮都白花一次调用，而且因为失败是静默的，
/// 没人会发现。进程重启即清空：换了配置该给它新的机会。
fn attempted() -> &'static Mutex<HashSet<uuid::Uuid>> {
    static ATTEMPTED: OnceLock<Mutex<HashSet<uuid::Uuid>>> = OnceLock::new();
    ATTEMPTED.get_or_init(Mutex::default)
}

/// L1：会话还挂着占位标题时，用第一条用户输入把它换掉。
///
/// 优先用**历史里第一条**用户消息而不是当前这轮的提示词。老会话补标题时，
/// 当前这轮多半是「继续」「嗯」这种没有信息量的话，而第一条消息才是这段
/// 对话真正在说的事。
pub(crate) fn apply_derived_title(
    session: &mut Session,
    prompt: &str,
    has_attachments: bool,
) -> bool {
    if !session.title_source.allows_auto_title(&session.title)
        || !session_title::is_placeholder(&session.title)
    {
        return false;
    }
    let derived = session_title::derive_from_messages(&session.messages)
        .unwrap_or_else(|| session_title::derive_from_prompt(prompt, has_attachments));
    if session_title::is_placeholder(&derived) || derived == session.title {
        return false;
    }
    session.title = derived;
    session.title_source = TitleSource::Derived;
    true
}

/// 这条会话现在该不该跑 L2。**有副作用**：返回 `true` 的同时把它记进
/// [`attempted`]，所以同一个进程里对同一条会话只会返回一次 `true`。
pub(crate) fn claim_summary_attempt(session: &Session) -> bool {
    if session.title_source != TitleSource::Derived {
        return false;
    }
    if !has_summarizable_exchange(session) {
        return false;
    }
    attempted()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(session.id)
}

/// 跑摘要本身。纯计算 + 一次网络往返，不碰会话。
///
/// TUI 需要它单独存在：摘要在事件循环里 `await` 会让界面在每轮收尾时僵住，
/// 所以那边把这一步扔进后台任务，只把结果送回主循环。
pub(crate) async fn summarized_title(agent: &Agent, messages: &[Message]) -> Option<String> {
    let (first_user, last_assistant) = summarizable_exchange(messages)?;
    agent.summarize_title(&first_user, &last_assistant).await
}

/// 把摘要结果写回会话并落锁。`None`（失败）不落锁：这条会话在下次进程启动
/// 时还能再试一次，而 `Summarized` 是「真的摘要过」的意思，不能拿失败冒充。
pub(crate) fn adopt_summarized_title(session: &mut Session, title: Option<String>) -> bool {
    let Some(title) = title else {
        return false;
    };
    session.title_source = TitleSource::Summarized;
    if title == session.title {
        return false;
    }
    session.title = title;
    true
}

/// Harness 侧的合并写法：判定、摘要、写回一步到位。
pub(crate) async fn apply_summarized_title(agent: &Agent, session: &mut Session) -> bool {
    if !claim_summary_attempt(session) {
        return false;
    }
    let title = summarized_title(agent, &session.messages).await;
    adopt_summarized_title(session, title)
}

fn has_summarizable_exchange(session: &Session) -> bool {
    summarizable_exchange(&session.messages).is_some()
}

/// 摘要要的一问一答。
///
/// 用户侧取**第一条**（这段对话在说什么由它定调），助手侧取**最后一条**非空
/// 回复——一轮里模型常常先发几条只带工具调用、正文为空的消息，取第一条会拿到
/// 空字符串，摘要就失去了对照面。
fn summarizable_exchange(messages: &[Message]) -> Option<(String, String)> {
    let first_user = messages
        .iter()
        .find(|message| message.role == Role::User && !message.content.trim().is_empty())?;
    let last_assistant = messages
        .iter()
        .rfind(|message| message.role == Role::Assistant && !message.content.trim().is_empty())?;
    Some((first_user.content.clone(), last_assistant.content.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session_with(title: &str, source: TitleSource) -> Session {
        let mut session = Session::new(PathBuf::from("/tmp"), None, "");
        session.title = title.to_owned();
        session.title_source = source;
        session
    }

    fn exchanged(title: &str, source: TitleSource) -> Session {
        let mut session = session_with(title, source);
        session.messages.push(Message::user("修复登录 bug"));
        session
            .messages
            .push(Message::assistant("已定位到会话中间件", Vec::new()));
        session
    }

    #[test]
    fn placeholder_session_takes_the_prompt() {
        let mut session = session_with("New session", TitleSource::Legacy);
        assert!(apply_derived_title(&mut session, "修复登录 bug", false));
        assert_eq!(session.title, "修复登录 bug");
        assert_eq!(session.title_source, TitleSource::Derived);
    }

    /// 老会话补标题时，当前这轮多半是「继续」。第一条消息才有信息量。
    #[test]
    fn existing_history_wins_over_the_current_prompt() {
        let mut session = session_with("New session", TitleSource::Legacy);
        session.messages.push(Message::user("重构订单模块"));
        session
            .messages
            .push(Message::assistant("好的", Vec::new()));
        assert!(apply_derived_title(&mut session, "继续", false));
        assert_eq!(session.title, "重构订单模块");
    }

    #[test]
    fn a_human_named_session_is_left_alone() {
        let mut session = session_with("发布 0.43 的检查清单", TitleSource::User);
        assert!(!apply_derived_title(&mut session, "随便什么提示词", false));
        assert_eq!(session.title, "发布 0.43 的检查清单");
    }

    /// 摘要过一次就到终点，不能每轮都重算——那是每轮一次额外调用。
    #[test]
    fn a_locked_title_never_claims_another_attempt() {
        assert!(!claim_summary_attempt(&exchanged(
            "排查 CPU 负载",
            TitleSource::Summarized
        )));
        assert!(!claim_summary_attempt(&exchanged(
            "发布检查清单",
            TitleSource::User
        )));
    }

    /// 还没有助手回复时不开火：拿半轮对话摘出来的标题是猜的。
    #[test]
    fn an_unanswered_turn_is_not_summarizable_yet() {
        let mut session = session_with("修复登录 bug", TitleSource::Derived);
        session.messages.push(Message::user("修复登录 bug"));
        assert!(!claim_summary_attempt(&session));
    }

    /// 失败会静默，所以必须有别的东西阻止它每轮重试。
    #[test]
    fn one_attempt_per_session_per_process() {
        let session = exchanged("修复登录 bug", TitleSource::Derived);
        assert!(claim_summary_attempt(&session));
        assert!(!claim_summary_attempt(&session));
    }

    /// 摘要失败不能冒充成「摘要过」，否则一次网络抖动就永久剥夺了这条会话
    /// 的标题——重启后本该还有一次机会。
    #[test]
    fn a_failed_summary_does_not_lock_the_title() {
        let mut session = exchanged("修复登录 bug", TitleSource::Derived);
        assert!(!adopt_summarized_title(&mut session, None));
        assert_eq!(session.title_source, TitleSource::Derived);
        assert_eq!(session.title, "修复登录 bug");
    }

    /// 摘出来和原来一样也要落锁，否则下一轮为同一个结果再花一次调用。
    #[test]
    fn an_identical_summary_still_locks() {
        let mut session = exchanged("修复登录 bug", TitleSource::Derived);
        assert!(!adopt_summarized_title(
            &mut session,
            Some("修复登录 bug".to_owned())
        ));
        assert_eq!(session.title_source, TitleSource::Summarized);
    }

    /// 助手那侧要取最后一条**非空**回复：只带工具调用的空消息不是回复。
    #[test]
    fn tool_only_assistant_messages_are_not_the_exchange() {
        let messages = vec![
            Message::user("修复登录 bug"),
            Message::assistant("", Vec::new()),
            Message::assistant("已定位到会话中间件", Vec::new()),
        ];
        let (user, assistant) = summarizable_exchange(&messages).expect("exchange");
        assert_eq!(user, "修复登录 bug");
        assert_eq!(assistant, "已定位到会话中间件");
    }
}
