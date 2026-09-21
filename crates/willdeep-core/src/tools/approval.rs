//! 审批档位与审批闸门。
//!
//! 档位与 macOS 版 WillDeep（Xedit `AgentApprovalMode`）逐档对齐，差别只在
//! 命名上保留了 CLI 已经发布过的配置写法：
//!
//! | 档位 | 文件写入（工作区内） | Shell | 网络 POST / MCP / 工作区外 |
//! |---|---|---|---|
//! | `read-only` | 拒绝 | 拒绝 | 拒绝 |
//! | `strict` | 每次问 | 每次问 | 每次问 |
//! | `smart` | 放行 | 静态规则 → AI 审核 → 问 | 问 |
//! | `workspace-write` | 放行 | 静态规则 → 围栏内且不出工作区则放行 → 问（不过 AI） | 问 |
//! | `full-access` | 放行 | 放行，破坏性形态照样问 | 放行 |
//!
//! 档位在会话中途可以换：[`SharedApprovalMode`] 是一个原子量，TUI、Runtime
//! 与注册表持有同一份，切换后下一次工具调用就按新档位判。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use super::{
    ApprovalDecision, ToolError, ToolRegistry, WEB_POST_SIGNATURE_PREFIX, command_signature,
    registrable_domain,
};
use crate::judge::{JudgeRequest, JudgeVerdict};
use crate::safety::CommandSafety;
use crate::sandbox::{SandboxPolicy, SandboxSpec};
use crate::types::ToolCall;

/// Why a command ran without an approval card — or why it needed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalSource {
    /// The static classifier proved the command read-only or bounded.
    StaticAllowlist,
    /// The AI judge returned YES for this exact action.
    Judge,
    /// A rule the operator previously chose to always allow.
    AlwaysAllowList,
    /// The user was asked.
    User,
    /// No approval is required by design; the trace is the audit record.
    NotRequired,
    /// Workspace-write mode: the OS write fence contains the command and it
    /// does not visibly reach another host.
    WorkspaceAccess,
    /// Full-access mode: the operator chose to skip review.
    FullAccess,
    /// The operator switched the approval mode. `command` holds the new mode.
    ModeChange,
}

impl ApprovalSource {
    /// `approvals.jsonl` 里的来源名。与 Xedit 审计里的来源名保持同一套拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaticAllowlist => "static",
            Self::Judge => "judge",
            Self::AlwaysAllowList => "always-allow",
            Self::User => "user",
            Self::NotRequired => "not-required",
            Self::WorkspaceAccess => "workspace-access",
            Self::FullAccess => "full-access",
            Self::ModeChange => "mode-change",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalTrace {
    pub command: String,
    pub source: ApprovalSource,
    /// Short, user-facing explanation ("static allowlist: read-only",
    /// "judge unavailable: connection refused").
    pub detail: String,
}

pub(super) type ApprovalReporter = Arc<dyn Fn(ApprovalTrace) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalMode {
    ReadOnly,
    Strict,
    Smart,
    WorkspaceAccess,
    FullAccess,
}

impl ApprovalMode {
    /// 会话里可以切换到的档位，按「从紧到松」排。`read-only` 不在其中：它是
    /// 工作区策略，不是会话偏好。
    pub const SELECTABLE: [Self; 4] = [
        Self::Strict,
        Self::Smart,
        Self::WorkspaceAccess,
        Self::FullAccess,
    ];

    /// Shift+Tab 循环的档位。刻意不含 `full-access`：一个手滑就交出整台机器
    /// 的快捷键，不该存在。
    pub const CYCLE: [Self; 3] = [Self::Strict, Self::Smart, Self::WorkspaceAccess];

    /// 配置文件、斜杠命令与 Runtime 协议共用的拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Strict => "strict",
            Self::Smart => "smart",
            Self::WorkspaceAccess => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }

    /// 接受规范拼写与历史别名。别名来自已经发布过的配置（`ask`、
    /// `auto-review`）和 Xedit 的档位名（`request-every-time`、`silent`）。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "read-only" | "readonly" => Some(Self::ReadOnly),
            "strict" | "ask" | "request-every-time" => Some(Self::Strict),
            "smart" | "auto-review" => Some(Self::Smart),
            "workspace-write" | "workspace-access" | "workspace" => Some(Self::WorkspaceAccess),
            "full-access" | "full" | "silent" => Some(Self::FullAccess),
            _ => None,
        }
    }

    /// Shift+Tab 的下一档。不在循环里的档位（`full-access`、`read-only`）
    /// 回到循环的第一档，而不是继续放宽。
    pub fn next_in_cycle(self) -> Self {
        match Self::CYCLE.iter().position(|mode| *mode == self) {
            Some(index) => Self::CYCLE[(index + 1) % Self::CYCLE.len()],
            None => Self::CYCLE[0],
        }
    }

    fn to_bits(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Strict => 1,
            Self::Smart => 2,
            Self::WorkspaceAccess => 3,
            Self::FullAccess => 4,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::Strict,
            2 => Self::Smart,
            3 => Self::WorkspaceAccess,
            4 => Self::FullAccess,
            // 读到意外的值按最紧的档位处理：宁可多拦，不可多放。
            _ => Self::ReadOnly,
        }
    }
}

/// 可以在会话中途切换的审批档位。克隆出来的句柄共享同一个值。
#[derive(Clone, Debug)]
pub struct SharedApprovalMode(Arc<AtomicU8>);

impl SharedApprovalMode {
    pub fn new(mode: ApprovalMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode.to_bits())))
    }

    pub fn get(&self) -> ApprovalMode {
        ApprovalMode::from_bits(self.0.load(Ordering::SeqCst))
    }

    /// 返回切换前的档位。
    pub fn set(&self, mode: ApprovalMode) -> ApprovalMode {
        ApprovalMode::from_bits(self.0.swap(mode.to_bits(), Ordering::SeqCst))
    }
}

impl ToolRegistry {
    pub fn approval_mode(&self) -> ApprovalMode {
        if self.inherits_full_access() {
            return ApprovalMode::FullAccess;
        }
        self.approval_mode.get()
    }

    /// 父会话此刻是否处于 `full-access`。只对挂了父档位句柄的子 Agent 注册表有意义。
    pub(crate) fn inherits_full_access(&self) -> bool {
        self.parent_approval_mode
            .as_ref()
            .is_some_and(|parent| parent.get() == ApprovalMode::FullAccess)
    }

    /// 子 Agent 跟随父会话的 `full-access`：用户已经明确允许不审批，Worker 再弹
    /// 审批卡只会打断人。跟随是实时的——父会话切回别的档位，下一次工具调用起
    /// Worker 就回到自己工种的档位。
    pub fn with_parent_approval_mode(mut self, parent: SharedApprovalMode) -> Self {
        self.parent_approval_mode = Some(parent);
        self
    }

    /// 与本注册表共享的档位句柄。前端拿它切档，不必重建 Agent。
    pub fn approval_mode_handle(&self) -> SharedApprovalMode {
        self.approval_mode.clone()
    }

    /// 改用调用方持有的档位句柄。Runtime 用它让「正在跑的这一轮」也能切档。
    pub fn with_shared_approval_mode(mut self, mode: SharedApprovalMode) -> Self {
        self.approval_mode = mode;
        self
    }

    /// `workspace-write` 档使用的写入围栏。`None` 表示这台机器或这份配置
    /// 给不了围栏，此时该档对未分类的命令只能问人。
    pub fn with_workspace_sandbox(mut self, sandbox: Option<SandboxSpec>) -> Self {
        self.workspace_sandbox = sandbox;
        self
    }

    /// 当前档位实际套用的围栏。`full-access` 摘掉围栏——用户已经明确允许
    /// 工作区外写入，再让内核拦着只会得到一堆莫名其妙的失败。
    pub(crate) fn effective_sandbox(&self) -> SandboxSpec {
        match self.approval_mode() {
            ApprovalMode::FullAccess => SandboxSpec::new(SandboxPolicy::Off, []),
            ApprovalMode::WorkspaceAccess => self
                .workspace_sandbox
                .clone()
                .unwrap_or_else(|| self.sandbox.clone()),
            _ => self.sandbox.clone(),
        }
    }

    /// `full-access` 下直接放行并记一笔审计；其它档位返回 `false`，调用方
    /// 继续走原来的审批。
    pub(super) fn allowed_by_full_access(&self, action: &str) -> bool {
        if self.approval_mode() != ApprovalMode::FullAccess {
            return false;
        }
        self.report_approval(
            action,
            ApprovalSource::FullAccess,
            "full access: operator allows this without review".to_owned(),
        );
        true
    }

    pub(super) async fn require_mcp_approval(&self, name: &str) -> Result<(), ToolError> {
        let action = format!("call MCP tool: {name}");
        if self.allowed_by_full_access(&action) {
            return Ok(());
        }
        self.require_rememberable_approval(&action, format!("mcp:{name}"))
            .await
    }

    /// 主 Agent 的 Shell 审批。子 Agent 的几种收窄策略在调用方已经处理完。
    pub(super) async fn gate_main_command(
        &self,
        command: &str,
        description: &str,
    ) -> Result<(), ToolError> {
        let escalate = |registry: &Self, detail: String| {
            registry.report_approval(command, ApprovalSource::User, detail);
        };
        let mode = self.approval_mode();
        if matches!(mode, ApprovalMode::Strict | ApprovalMode::ReadOnly) {
            escalate(self, "strict approval mode".to_owned());
            return self.ask_for_command(command, description).await;
        }
        if let Some(signature) = command_signature(command)
            && self
                .always_allowed
                .lock()
                .expect("always allow rules")
                .contains(&signature)
        {
            self.report_approval(
                command,
                ApprovalSource::AlwaysAllowList,
                "operator marked this exact command always-allowed".to_owned(),
            );
            return Ok(());
        }

        match crate::safety::classify_with_workspace_write(command, true) {
            CommandSafety::AlwaysSafe => {
                self.report_approval(
                    command,
                    ApprovalSource::StaticAllowlist,
                    "static rule: read-only or bounded workspace command".to_owned(),
                );
                return Ok(());
            }
            CommandSafety::AlwaysDangerous => {
                // Destructive shapes never reach the judge — a model must not
                // be able to talk its way into `rm -rf`. Full access does not
                // change that: it skips review, not the denylist.
                escalate(
                    self,
                    "static rule: destructive shape, judge bypassed".to_owned(),
                );
                return self.ask_for_command(command, description).await;
            }
            CommandSafety::NeedsJudgment => {}
        }

        match mode {
            ApprovalMode::FullAccess => {
                self.report_approval(
                    command,
                    ApprovalSource::FullAccess,
                    "full access: operator allows commands without review".to_owned(),
                );
                return Ok(());
            }
            ApprovalMode::WorkspaceAccess => {
                return self
                    .gate_workspace_command(command, description, escalate)
                    .await;
            }
            _ => {}
        }

        let Some(judge) = &self.safety_judge else {
            escalate(self, "no AI judge configured".to_owned());
            return self.ask_for_command(command, description).await;
        };
        let task_context = self.task_context.lock().expect("task context").clone();
        let verdict = judge
            .judge(JudgeRequest {
                tool: "run_command".to_owned(),
                command: command.to_owned(),
                task_context,
            })
            .await;
        // The judge model goes into every trace, not just the failures: an
        // operator comparing "why does the CLI ask more than the app" needs to
        // see which model answered, and a silent model swap is otherwise
        // invisible in the audit trail.
        let model = judge.model();
        match verdict {
            JudgeVerdict::Allow => {
                self.report_approval(
                    command,
                    ApprovalSource::Judge,
                    format!("AI review ({model}): bounded and consistent with the current task"),
                );
                Ok(())
            }
            JudgeVerdict::Deny => {
                escalate(self, format!("AI review ({model}) declined"));
                self.ask_for_command(command, description).await
            }
            JudgeVerdict::Unavailable(reason) => {
                escalate(self, format!("AI review ({model}) unavailable: {reason}"));
                self.ask_for_command(command, description).await
            }
        }
    }

    /// `workspace-write` 不请 AI 审核，靠的是两件确定的事：内核围栏把写入
    /// 关在工作区里，命令本身也看不出要去别的主机。两件缺一件就问人——
    /// 这一档的承诺是「工作区内的事不打扰你」，不是「猜它大概没事」。
    async fn gate_workspace_command(
        &self,
        command: &str,
        description: &str,
        escalate: impl Fn(&Self, String),
    ) -> Result<(), ToolError> {
        let fence = self.effective_sandbox();
        if !fence.policy.is_enforcing() || !crate::sandbox::available() {
            escalate(
                self,
                "workspace write: no OS write fence, cannot prove the command stays in the workspace"
                    .to_owned(),
            );
            return self.ask_for_command(command, description).await;
        }
        if crate::safety::reaches_outside_workspace(command) {
            escalate(
                self,
                "workspace write: command reaches another host or a privileged service".to_owned(),
            );
            return self.ask_for_command(command, description).await;
        }
        self.report_approval(
            command,
            ApprovalSource::WorkspaceAccess,
            "workspace write: contained by the OS write fence".to_owned(),
        );
        Ok(())
    }

    /// 网络围栏的逃生口：模型声明这条命令必须联网。放不放只有人能定——判官判的
    /// 是命令危不危险，不是「要不要把围栏的网打开」；`full-access` 本来就没有围栏。
    /// 「始终允许」记的是带 `network:` 前缀的规范化命令，与不联网的同一条命令分开记。
    pub(super) async fn gate_network_escalation(
        &self,
        command: &str,
        description: &str,
    ) -> Result<(), ToolError> {
        let action = format!("allow network access for command: {description}");
        if self.allowed_by_full_access(&action) {
            return Ok(());
        }
        self.report_approval(
            command,
            ApprovalSource::User,
            "network fence: the command declared it must reach the network".to_owned(),
        );
        match command_signature(command) {
            Some(signature) => {
                self.require_rememberable_approval(&action, format!("network:{signature}"))
                    .await
            }
            None => self.require_approval(&action, false).await,
        }
    }

    async fn ask_for_command(&self, command: &str, description: &str) -> Result<(), ToolError> {
        match command_signature(command) {
            Some(signature) => {
                self.require_rememberable_approval(
                    &format!("run command: {description}"),
                    signature,
                )
                .await
            }
            None => {
                self.require_approval(&format!("run command: {description}"), false)
                    .await
            }
        }
    }

    pub(super) fn report_approval(&self, command: &str, source: ApprovalSource, detail: String) {
        let Some(reporter) = &self.approval_reporter else {
            return;
        };
        reporter(ApprovalTrace {
            command: command.to_owned(),
            source,
            detail,
        });
    }

    /// POST 是对外写操作，除 `full-access` 外所有审批模式都要过一遍，
    /// `read-only` 策略直接拒。
    ///
    /// 「始终允许」按注册域名收敛，而不是像 shell 命令那样逐字记：POST 的 URL
    /// 常带一次性 id、body 每次都不同，逐字规则下一次就对不上，等于没有。规则
    /// 里只有域名，body 中的密钥不会被写进 always-allow.json。
    pub(super) async fn require_web_post_approval(
        &self,
        url: &reqwest::Url,
        body_bytes: usize,
        content_type: &str,
    ) -> Result<(), ToolError> {
        if self.approval_mode() == ApprovalMode::ReadOnly {
            return Err(ToolError::ReadOnlyPolicy(format!("POST to {url}")));
        }
        if self.allowed_by_full_access(&format!("POST to {url}")) {
            return Ok(());
        }
        let domain = registrable_domain(url);
        let description = format!(
            "POST to {url}\nbody: {body_bytes} B ({content_type})\nAlways allow scope: every POST to {domain}"
        );
        self.require_rememberable_approval(
            &description,
            format!("{WEB_POST_SIGNATURE_PREFIX}{domain}"),
        )
        .await
    }

    /// 只读的公网抓取（web_fetch / web_search）不改动本地状态，SSRF 目标已被
    /// `validate_public_url` 拦下，所以只有 Strict 模式才逐次询问。
    pub(super) async fn require_network_read_approval(
        &self,
        description: &str,
    ) -> Result<(), ToolError> {
        if self.approval_mode() != ApprovalMode::Strict {
            return Ok(());
        }
        self.require_approval(description, false).await
    }

    pub(super) async fn require_approval(
        &self,
        description: &str,
        workspace_write: bool,
    ) -> Result<(), ToolError> {
        let workspace_write_allowed = workspace_write
            && matches!(
                self.approval_mode(),
                ApprovalMode::Smart | ApprovalMode::WorkspaceAccess | ApprovalMode::FullAccess
            );
        if workspace_write_allowed {
            return Ok(());
        }
        match self.approver.approve(description, false).await {
            ApprovalDecision::AllowOnce | ApprovalDecision::AlwaysAllow => Ok(()),
            ApprovalDecision::Deny => Err(ToolError::ApprovalDenied(description.to_owned())),
        }
    }

    pub(crate) async fn approve_uncertain_replay(&self, call: &ToolCall) -> Result<(), ToolError> {
        let description = format!(
            "Retry {} with the same arguments as an interrupted call whose effects are unknown? This may repeat an external or file side effect. Approval applies only to this attempt.",
            call.name
        );
        match self.approver.approve(&description, false).await {
            ApprovalDecision::AllowOnce => Ok(()),
            _ => Err(ToolError::ApprovalDenied(
                "uncertain replay requires one-time approval".into(),
            )),
        }
    }

    pub(super) async fn require_rememberable_approval(
        &self,
        description: &str,
        signature: String,
    ) -> Result<(), ToolError> {
        if self
            .always_allowed
            .lock()
            .expect("always allow rules")
            .contains(&signature)
        {
            return Ok(());
        }
        match self.approver.approve(description, true).await {
            ApprovalDecision::AllowOnce => Ok(()),
            ApprovalDecision::AlwaysAllow => {
                self.always_allowed
                    .lock()
                    .expect("always allow rules")
                    .insert(signature);
                self.persist_always_allowed()?;
                Ok(())
            }
            ApprovalDecision::Deny => Err(ToolError::ApprovalDenied(description.to_owned())),
        }
    }

    pub(super) fn persist_always_allowed(&self) -> Result<(), ToolError> {
        let Some(path) = &self.always_allow_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut rules = self
            .always_allowed
            .lock()
            .expect("always allow rules")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        rules.sort();
        let bytes = serde_json::to_vec_pretty(&rules)
            .map_err(|error| ToolError::Network(error.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_names_and_published_aliases() {
        for mode in [
            ApprovalMode::ReadOnly,
            ApprovalMode::Strict,
            ApprovalMode::Smart,
            ApprovalMode::WorkspaceAccess,
            ApprovalMode::FullAccess,
        ] {
            assert_eq!(ApprovalMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(ApprovalMode::parse("ask"), Some(ApprovalMode::Strict));
        assert_eq!(
            ApprovalMode::parse("auto-review"),
            Some(ApprovalMode::Smart)
        );
        assert_eq!(
            ApprovalMode::parse("workspace_access"),
            Some(ApprovalMode::WorkspaceAccess)
        );
        assert_eq!(
            ApprovalMode::parse(" Full "),
            Some(ApprovalMode::FullAccess)
        );
        assert_eq!(ApprovalMode::parse("yolo"), None);
    }

    #[test]
    fn cycle_never_lands_on_full_access() {
        let mut mode = ApprovalMode::FullAccess;
        for _ in 0..10 {
            mode = mode.next_in_cycle();
            assert_ne!(mode, ApprovalMode::FullAccess);
        }
        assert_eq!(
            ApprovalMode::Smart.next_in_cycle(),
            ApprovalMode::WorkspaceAccess
        );
        assert_eq!(
            ApprovalMode::WorkspaceAccess.next_in_cycle(),
            ApprovalMode::Strict
        );
    }

    use crate::judge::SafetyJudge;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// 被问到就数一次；只要被请到，这一档就没做到「不过 AI」。
    struct CountingJudge(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl SafetyJudge for CountingJudge {
        async fn judge(&self, _request: JudgeRequest) -> JudgeVerdict {
            self.0.fetch_add(1, Ordering::SeqCst);
            JudgeVerdict::Allow
        }
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("willdeep-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create fixture workspace");
        path
    }

    fn command(text: &str) -> ToolCall {
        ToolCall {
            id: "call".to_owned(),
            name: "run_command".to_owned(),
            arguments: serde_json::json!({ "command": text }).to_string(),
        }
    }

    fn traced(registry: ToolRegistry) -> (ToolRegistry, Arc<Mutex<Vec<ApprovalTrace>>>) {
        let traces = Arc::new(Mutex::new(Vec::new()));
        let sink = traces.clone();
        let registry = registry.with_approval_reporter(move |trace| {
            sink.lock().expect("traces").push(trace);
        });
        (registry, traces)
    }

    /// 默认审批器一律拒绝，所以「命令跑了」就等于「没问人」。
    #[tokio::test]
    async fn full_access_skips_review_but_not_the_destructive_denylist() {
        let root = fixture("full-access");
        let judged = Arc::new(AtomicUsize::new(0));
        let (registry, traces) = traced(
            ToolRegistry::new(&root, ApprovalMode::FullAccess)
                .expect("registry")
                .with_safety_judge(Arc::new(CountingJudge(judged.clone()))),
        );
        registry
            .execute(&command("printf hi > out.txt"))
            .await
            .expect("unclassified command runs without asking");
        assert_eq!(std::fs::read_to_string(root.join("out.txt")).unwrap(), "hi");
        assert_eq!(judged.load(Ordering::SeqCst), 0);
        assert!(
            traces
                .lock()
                .unwrap()
                .iter()
                .any(|trace| trace.source == ApprovalSource::FullAccess)
        );

        let destructive = registry.execute(&command("rm -rf out.txt")).await;
        assert!(matches!(destructive, Err(ToolError::ApprovalDenied(_))));
        assert!(root.join("out.txt").exists());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn workspace_write_without_a_fence_asks_instead_of_consulting_the_judge() {
        let root = fixture("workspace-no-fence");
        let judged = Arc::new(AtomicUsize::new(0));
        let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
            .expect("registry")
            .with_safety_judge(Arc::new(CountingJudge(judged.clone())));
        let asked = registry.execute(&command("printf hi > out.txt")).await;
        assert!(matches!(asked, Err(ToolError::ApprovalDenied(_))));
        assert_eq!(judged.load(Ordering::SeqCst), 0);
        registry
            .execute(&command("ls"))
            .await
            .expect("static-safe command still runs");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn workspace_write_runs_fenced_commands_and_asks_for_remote_ones() {
        if !crate::sandbox::available() {
            eprintln!("skipping: no OS sandbox backend on this machine");
            return;
        }
        let root = fixture("workspace-fence");
        let judged = Arc::new(AtomicUsize::new(0));
        let fence = SandboxSpec::new(
            SandboxPolicy::WorkspaceWrite,
            [root.clone(), std::env::temp_dir()],
        );
        let (registry, traces) = traced(
            ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
                .expect("registry")
                .with_workspace_sandbox(Some(fence))
                .with_safety_judge(Arc::new(CountingJudge(judged.clone()))),
        );
        registry
            .execute(&command("printf hi > out.txt"))
            .await
            .expect("fenced workspace command runs");
        assert!(
            traces
                .lock()
                .unwrap()
                .iter()
                .any(|trace| trace.source == ApprovalSource::WorkspaceAccess)
        );
        let remote = registry
            .execute(&command("curl -s https://example.com -o page.html"))
            .await;
        assert!(matches!(remote, Err(ToolError::ApprovalDenied(_))));
        assert_eq!(judged.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// 网络围栏的逃生口：断网的围栏下 `network: true` 要问人；默认审批器拒绝，
    /// 所以命令没跑、审计里留了一笔「命令声明要联网」；同一条命令不带 `network`
    /// 照常在围栏里跑。
    #[tokio::test]
    async fn network_escalation_asks_the_user_and_is_audited() {
        if !crate::sandbox::available() {
            eprintln!("skipping: no OS sandbox backend on this machine");
            return;
        }
        let root = fixture("network-escalation");
        let fence = SandboxSpec::new(
            SandboxPolicy::WorkspaceWrite,
            [root.clone(), std::env::temp_dir()],
        )
        .with_network(crate::sandbox::NetworkPolicy::Deny);
        let (registry, traces) = traced(
            ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
                .expect("registry")
                .with_workspace_sandbox(Some(fence)),
        );
        let escalated = ToolCall {
            id: "call".to_owned(),
            name: "run_command".to_owned(),
            arguments: serde_json::json!({ "command": "printf hi > out.txt", "network": true })
                .to_string(),
        };
        let asked = registry.execute(&escalated).await;
        assert!(
            matches!(asked, Err(ToolError::ApprovalDenied(_))),
            "{asked:?}"
        );
        assert!(!root.join("out.txt").exists());
        assert!(traces.lock().unwrap().iter().any(|trace| {
            trace.source == ApprovalSource::User && trace.detail.contains("network fence")
        }));

        registry
            .execute(&command("printf hi > out.txt"))
            .await
            .expect("the same command without network runs inside the fence");
        assert_eq!(std::fs::read_to_string(root.join("out.txt")).unwrap(), "hi");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn switching_the_shared_handle_changes_the_next_call() {
        let root = fixture("mode-switch");
        let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
        let handle = registry.approval_mode_handle();
        assert!(matches!(
            registry.execute(&command("printf a > a.txt")).await,
            Err(ToolError::ApprovalDenied(_))
        ));
        handle.set(ApprovalMode::FullAccess);
        assert_eq!(registry.approval_mode(), ApprovalMode::FullAccess);
        registry
            .execute(&command("printf a > a.txt"))
            .await
            .expect("runs after switching to full access");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn shared_mode_handles_see_each_other() {
        let first = SharedApprovalMode::new(ApprovalMode::Smart);
        let second = first.clone();
        assert_eq!(second.set(ApprovalMode::FullAccess), ApprovalMode::Smart);
        assert_eq!(first.get(), ApprovalMode::FullAccess);
    }
}
