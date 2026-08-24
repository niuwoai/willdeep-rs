//! 生命周期挂钩：审计留痕与门禁拦截。
//!
//! # 和通知 webhook 不是一件事
//!
//! `[notifications]` 那条 webhook 是**事后的礼貌性通知**：跑在关键路径外、
//! detached、端点挂了最多浪费一次超时，永远不会让一个轮次失败。这是对的——
//! 没人希望因为通知服务器宕机就干不了活。
//!
//! hook 正好相反，是**事中的裁决**：跑在关键路径上，阻塞式 hook 非零退出会
//! 真的把动作拦下来。一个能被静默丢弃的门禁不是门禁。
//!
//! 两者共存，各司其职。别把审计需求接到 webhook 上——那条路会丢事件。
//!
//! # 契约
//!
//! 每个 hook 是一条 shell 命令。触发时：
//!
//! 1. 事件 JSON 从 **stdin** 喂进去（一行，UTF-8）。
//! 2. **退出码 0** = 放行；阻塞式 hook 的**非零退出 = 拦截**，stderr 前若干
//!    字节作为拒绝理由回给模型和用户。
//! 3. 超时按 [`HookFailure`] 处置。
//!
//! 非阻塞 hook 的退出码只进日志，不影响流程——它就是来留痕的。
//!
//! # 失败时默认拦，不默认放
//!
//! 阻塞式 hook 超时或起不来时，默认 [`HookFailure::Deny`]。理由是这类 hook 的
//! 用途是合规门禁，而**一个坏掉就自动放行的门禁，等于没有门禁**——它恰好会在
//! 出事的时候失效。代价是 hook 写坏会把 agent 卡住，所以拒绝理由里必须点名是
//! 哪个 hook、为什么，让人五秒钟内能定位到该改哪一行配置。
//!
//! 明确不想要这个语义的，把 `on_error` 设成 `ignore`。
//!
//! # hook 自己不进沙箱
//!
//! [`crate::sandbox`] 罩的是模型请求执行的命令。hook 是**操作者自己配的代码**，
//! 信任级别与操作者的 shell 相同：审计 hook 往 `/var/log` 写、门禁 hook 去问
//! 公司的策略服务，都是它的本职。把它关进工作区围栏只会让这两件事都干不成。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 触发点。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// 工具即将执行。阻塞式 hook 在这里拦截。
    PreTool,
    /// 工具执行完毕。审计用，拦不住已经发生的事。
    PostTool,
    /// 一次审批有了结果（人批的或 judge 批的）。审计用。
    ApprovalResolved,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::PreTool => "pre_tool",
            HookEvent::PostTool => "post_tool",
            HookEvent::ApprovalResolved => "approval_resolved",
        }
    }

    /// 这个事件上的 hook 有没有可能拦住什么。`PostTool` 发生在事后，
    /// 让它"拦截"只会制造一种拦得住的错觉。
    pub fn can_block(self) -> bool {
        matches!(self, HookEvent::PreTool)
    }
}

/// hook 自己出问题（超时、起不来、shell 找不到）时怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFailure {
    /// 拦下来。阻塞式 hook 的默认值：坏掉就放行的门禁等于没有门禁。
    #[default]
    Deny,
    /// 放过去，只记一笔。
    Ignore,
}

/// 一条 hook 配置。
#[derive(Clone, Debug)]
pub struct Hook {
    pub name: String,
    pub event: HookEvent,
    pub command: String,
    /// 非零退出是否拦截。只有 [`HookEvent::can_block`] 为真的事件上才有意义。
    pub blocking: bool,
    pub timeout: Duration,
    pub on_error: HookFailure,
}

impl Hook {
    /// 这条 hook 实际上会不会拦人。`PostTool` 上配 `blocking = true` 不是错误，
    /// 但也不会生效——事情已经发生了。
    pub fn blocks(&self) -> bool {
        self.blocking && self.event.can_block()
    }
}

/// 喂给 hook 的事件。字段刻意扁平：hook 多半是三行 shell + `jq`，
/// 嵌套结构会让最常见的用法难写。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookPayload {
    pub event: String,
    pub session_id: Option<String>,
    pub workspace: Option<String>,
    /// 工具名，或审批事件里的动作名。
    pub tool: Option<String>,
    /// 命令原文或工具参数，**已按审批日志同一套规则脱敏**。
    pub detail: Option<String>,
    /// 审批结果 / 工具结果。
    pub outcome: Option<String>,
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl HookPayload {
    pub fn new(event: HookEvent) -> Self {
        Self {
            event: event.as_str().to_owned(),
            session_id: None,
            workspace: None,
            tool: None,
            detail: None,
            outcome: None,
            exit_code: None,
            extra: BTreeMap::new(),
        }
    }

    /// 带上工具名与细节。细节在这里就脱敏，不指望每个 hook 作者自己记得——
    /// 凭据一旦交出进程，就再也收不回来了。
    pub fn with_tool(mut self, tool: impl Into<String>, detail: &str) -> Self {
        self.tool = Some(tool.into());
        self.detail = Some(crate::judge::redact_credentials(detail));
        self
    }

    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    pub fn with_exit_code(mut self, code: Option<i32>) -> Self {
        self.exit_code = code;
        self
    }

    pub fn with_session(mut self, session: Option<String>, workspace: Option<String>) -> Self {
        self.session_id = session;
        self.workspace = workspace;
        self
    }
}

/// 一次触发的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOutcome {
    Allow,
    /// 被拦下。`hook` 是哪条 hook 拦的，`reason` 是它的 stderr（或超时说明）。
    Deny {
        hook: String,
        reason: String,
    },
}

impl HookOutcome {
    pub fn is_denied(&self) -> bool {
        matches!(self, HookOutcome::Deny { .. })
    }

    /// 回给模型和用户的话。点名到具体 hook——不点名的话，一次拦截等于让人
    /// 去翻整份配置猜是谁干的。
    pub fn message(&self) -> Option<String> {
        match self {
            HookOutcome::Allow => None,
            HookOutcome::Deny { hook, reason } => Some(format!(
                "<hook-denied hook=\"{hook}\">\n{}\n</hook-denied>",
                if reason.trim().is_empty() {
                    "（该 hook 未给出理由）"
                } else {
                    reason.trim()
                }
            )),
        }
    }
}

/// 喂给 hook 的 JSON 上限。一次 300KB 的工具输出灌进 stdin，慢的是 agent 不是
/// hook；审计要的是"谁在什么时候做了什么"，不是完整副本。
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
/// 拒绝理由上限。它会进模型上下文，不能让一条 hook 的调试输出吃掉整个窗口。
const MAX_REASON_BYTES: usize = 2 * 1024;

/// 已注册的 hook。空注册表的 [`HookRegistry::fire`] 是一次无分配的快速返回——
/// 绝大多数用户没配 hook，这条路径不该有成本。
#[derive(Clone, Debug, Default)]
pub struct HookRegistry {
    hooks: Vec<Hook>,
}

impl HookRegistry {
    pub fn new(hooks: Vec<Hook>) -> Self {
        Self { hooks }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// 触发某个事件上的全部 hook，按配置顺序。
    ///
    /// 第一条拦截即短路：后面的 hook 不再跑。动作已经不会发生了，再跑下去只是
    /// 在为一件不存在的事收集审计记录，还平白多花时间。
    pub async fn fire(&self, event: HookEvent, payload: &HookPayload) -> HookOutcome {
        for hook in self.hooks.iter().filter(|hook| hook.event == event) {
            let result = run_hook(hook, payload).await;
            if let HookOutcome::Deny { .. } = result {
                return result;
            }
        }
        HookOutcome::Allow
    }
}

async fn run_hook(hook: &Hook, payload: &HookPayload) -> HookOutcome {
    let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned());
    let body = truncate_utf8(body, MAX_PAYLOAD_BYTES);

    match tokio::time::timeout(hook.timeout, spawn_hook(hook, body)).await {
        Ok(Ok(output)) => {
            if output.status.success() || !hook.blocks() {
                HookOutcome::Allow
            } else {
                HookOutcome::Deny {
                    hook: hook.name.clone(),
                    reason: truncate_utf8(
                        String::from_utf8_lossy(&output.stderr).into_owned(),
                        MAX_REASON_BYTES,
                    ),
                }
            }
        }
        // 起不来和超时走同一条处置：两种情况下 hook 都没能给出判断，而
        // 「没能给出判断」正是 on_error 要回答的问题。
        Ok(Err(error)) => hook_failed(hook, &format!("hook 无法执行：{error}")),
        Err(_) => hook_failed(
            hook,
            &format!("hook 超时（{} 秒内没有返回）", hook.timeout.as_secs()),
        ),
    }
}

fn hook_failed(hook: &Hook, reason: &str) -> HookOutcome {
    if hook.blocks() && hook.on_error == HookFailure::Deny {
        HookOutcome::Deny {
            hook: hook.name.clone(),
            reason: reason.to_owned(),
        }
    } else {
        HookOutcome::Allow
    }
}

async fn spawn_hook(hook: &Hook, body: String) -> std::io::Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;

    let mut command = crate::tools::platform_shell(&hook.command);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // 写失败不当错误：hook 完全可以不读 stdin 就退出（`exit 0` 是合法
        // 的审计 hook），那种情况下这里必然是 broken pipe。
        let _ = stdin.write_all(body.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    child.wait_with_output().await
}

/// 按字节截断但不切碎 UTF-8。切碎的话，接收端 `jq` 会直接吐一个解析错误，
/// 而那个错误看起来像 hook 自己写错了。
fn truncate_utf8(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(name: &str, event: HookEvent, command: &str) -> Hook {
        Hook {
            name: name.to_owned(),
            event,
            command: command.to_owned(),
            blocking: false,
            timeout: Duration::from_secs(5),
            on_error: HookFailure::Deny,
        }
    }

    fn blocking(name: &str, command: &str) -> Hook {
        Hook {
            blocking: true,
            ..hook(name, HookEvent::PreTool, command)
        }
    }

    fn payload() -> HookPayload {
        HookPayload::new(HookEvent::PreTool).with_tool("run_command", "cargo test")
    }

    #[tokio::test]
    async fn an_empty_registry_allows_everything() {
        let registry = HookRegistry::default();

        assert!(registry.is_empty());
        assert_eq!(
            registry.fire(HookEvent::PreTool, &payload()).await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn a_passing_blocking_hook_allows() {
        let registry = HookRegistry::new(vec![blocking("gate", "exit 0")]);

        assert_eq!(
            registry.fire(HookEvent::PreTool, &payload()).await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn a_failing_blocking_hook_denies_with_its_stderr() {
        let registry = HookRegistry::new(vec![blocking(
            "policy",
            "echo '未经变更单批准的命令' >&2; exit 3",
        )]);

        let outcome = registry.fire(HookEvent::PreTool, &payload()).await;

        let HookOutcome::Deny { hook, reason } = outcome else {
            panic!("应当被拦下");
        };
        assert_eq!(hook, "policy");
        assert!(reason.contains("未经变更单批准的命令"));
    }

    #[tokio::test]
    async fn a_failing_observer_hook_does_not_deny() {
        // 非阻塞 hook 是来留痕的。它自己挂了不该连累这次工具调用。
        let registry = HookRegistry::new(vec![hook("audit", HookEvent::PreTool, "exit 9")]);

        assert_eq!(
            registry.fire(HookEvent::PreTool, &payload()).await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn post_tool_cannot_block_even_if_configured_to() {
        // 事情已经发生了，让它"拦截"只会制造一种拦得住的错觉。
        let mut audit = hook("late", HookEvent::PostTool, "exit 1");
        audit.blocking = true;
        assert!(!audit.blocks());

        let registry = HookRegistry::new(vec![audit]);
        let payload = HookPayload::new(HookEvent::PostTool);

        assert_eq!(
            registry.fire(HookEvent::PostTool, &payload).await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn a_hook_for_another_event_does_not_fire() {
        let registry = HookRegistry::new(vec![blocking("gate", "exit 1")]);

        assert_eq!(
            registry
                .fire(
                    HookEvent::ApprovalResolved,
                    &HookPayload::new(HookEvent::ApprovalResolved)
                )
                .await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn a_timed_out_gate_denies_by_default() {
        // 坏掉就自动放行的门禁，恰好会在出事的时候失效。
        let mut gate = blocking("slow", "sleep 5");
        gate.timeout = Duration::from_millis(150);

        let outcome = HookRegistry::new(vec![gate])
            .fire(HookEvent::PreTool, &payload())
            .await;

        let HookOutcome::Deny { hook, reason } = outcome else {
            panic!("超时应当拦下");
        };
        assert_eq!(hook, "slow");
        assert!(reason.contains("超时"), "{reason}");
    }

    #[tokio::test]
    async fn a_timed_out_gate_can_be_told_to_let_things_through() {
        let mut gate = blocking("slow", "sleep 5");
        gate.timeout = Duration::from_millis(150);
        gate.on_error = HookFailure::Ignore;

        assert_eq!(
            HookRegistry::new(vec![gate])
                .fire(HookEvent::PreTool, &payload())
                .await,
            HookOutcome::Allow
        );
    }

    #[tokio::test]
    async fn the_first_denial_short_circuits_the_rest() {
        let root = std::env::temp_dir().join(format!("willdeep-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("scratch");
        let marker = root.join("second-ran");

        let registry = HookRegistry::new(vec![
            blocking("first", "exit 1"),
            blocking("second", &format!("touch {}", marker.display())),
        ]);

        let outcome = registry.fire(HookEvent::PreTool, &payload()).await;

        assert!(outcome.is_denied());
        assert!(!marker.exists(), "动作已经不会发生了，第二条不该再跑");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_payload_reaches_the_hook_on_stdin() {
        let root = std::env::temp_dir().join(format!("willdeep-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("scratch");
        let captured = root.join("payload.json");

        let registry = HookRegistry::new(vec![hook(
            "audit",
            HookEvent::PreTool,
            &format!("cat > {}", captured.display()),
        )]);
        registry.fire(HookEvent::PreTool, &payload()).await;

        let text = std::fs::read_to_string(&captured).expect("hook 应当收到 stdin");
        let parsed: HookPayload = serde_json::from_str(&text).expect("应当是合法 JSON");
        assert_eq!(parsed.event, "pre_tool");
        assert_eq!(parsed.tool.as_deref(), Some("run_command"));
        assert_eq!(parsed.detail.as_deref(), Some("cargo test"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn credentials_never_reach_the_hook() {
        // 凭据一旦交出进程就收不回来了，不指望每个 hook 作者自己记得脱敏。
        let root = std::env::temp_dir().join(format!("willdeep-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("scratch");
        let captured = root.join("payload.json");

        let secret = HookPayload::new(HookEvent::PreTool).with_tool(
            "run_command",
            "curl -H 'Authorization: Bearer sk-0123456789abcdef' https://x",
        );
        let registry = HookRegistry::new(vec![hook(
            "audit",
            HookEvent::PreTool,
            &format!("cat > {}", captured.display()),
        )]);
        registry.fire(HookEvent::PreTool, &secret).await;

        let text = std::fs::read_to_string(&captured).expect("hook 应当收到 stdin");
        assert!(
            !text.contains("sk-0123456789abcdef"),
            "凭据泄漏进了 hook：{text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_hook_that_ignores_stdin_is_not_an_error() {
        // `exit 0` 是一条合法的审计 hook。它不读 stdin，写入端必然 broken pipe，
        // 那不该被当成 hook 失败。
        let registry = HookRegistry::new(vec![blocking("quiet", "exit 0")]);

        assert_eq!(
            registry.fire(HookEvent::PreTool, &payload()).await,
            HookOutcome::Allow
        );
    }

    #[test]
    fn denial_messages_name_the_hook() {
        let outcome = HookOutcome::Deny {
            hook: "policy".to_owned(),
            reason: "需要变更单".to_owned(),
        };
        let message = outcome.message().expect("应当有话说");

        assert!(message.contains("policy"));
        assert!(message.contains("需要变更单"));
    }

    #[test]
    fn a_silent_denial_still_says_something() {
        let outcome = HookOutcome::Deny {
            hook: "policy".to_owned(),
            reason: "   ".to_owned(),
        };

        assert!(outcome.message().unwrap().contains("未给出理由"));
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        let text = "审计".repeat(100);
        let cut = truncate_utf8(text, 7);

        assert!(cut.len() <= 7);
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }
}
