pub mod agent;
pub mod attention;
pub mod background;
pub mod goal;
pub mod hooks;
pub mod judge;
pub mod kernel;
pub mod kernel_store;
/// 实弹靶场：真 Provider、真缺陷、真退出码。仅测试构建，默认 `#[ignore]`。
#[cfg(test)]
mod livefire;
pub mod mcp;
pub mod plugin;
pub mod prompt;
pub mod provider;
pub mod routing;
pub mod safety;
pub mod sandbox;
pub mod session;
pub mod session_title;
pub mod skills;
pub mod subagent;
mod subagent_worktree;
pub mod tools;
pub mod types;
pub mod worker_tier;

pub use agent::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentInstructionInbox, AgentOutcome,
    AgentStopReason, EventSink, SubagentLifecycleStatus,
};
pub use attention::{
    AttentionItem, AttentionSection, AttentionSource, RuntimeScopeKind, RuntimeStatus,
    StatusRollup, rollup_status, sort_attention_items,
};
pub use background::{
    BackgroundTaskEvent, BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskSnapshot,
    BackgroundTaskStatus,
};
pub use goal::{
    ContinuationDecision, ContinuationRung, GOAL_COMPLETE_MARKER, GoalBudget, GoalContinuation,
    RoundObservation, SoftStopReason,
};
pub use judge::{JudgeRequest, JudgeVerdict, ProviderSafetyJudge, SafetyJudge};
pub use kernel::{DedupPolicy, EventKernel, LeasedEvent, PublishOutcome, host_event};
pub use mcp::{McpRegistry, McpServerConfig};
pub use plugin::{
    ApprovalGap, CommandOutcome, DeclarativeDocument, HostError as PluginHostError, PluginHost,
    PluginManifest, PluginPackage, PluginPermission, PluginRegistry, PluginSource,
};
pub use provider::{ApiDialect, ProviderConfig, ProviderKind, build_provider};
pub use routing::{EscalationTicket, RouteDecision, RoutingGuard, RoutingPolicy, RoutingTier};
pub use safety::{CommandSafety, classify_with_workspace_write};
pub use session::{Session, SessionDigest, SessionStore, format_iso8601};
pub use session_title::TitleSource;
pub use skills::SkillCatalog;
pub use subagent::{
    PUBLIC_SUBAGENT_IDS, SubagentCatalog, SubagentProfile, SubagentWriteScope, TaskPacket,
    TaskVerifier, TierBinding, builtin_profiles, public_profile_id,
};
pub use subagent_worktree::SubagentWorktreePolicy;
pub use tools::{
    ApprovalDecision, ApprovalMode, Approver, CommandVerification, ToolRegistry, UserQuestion,
    VerificationStatus, WebToolConfig, run_background_supervisor,
};
pub use types::{Message, MessageAttachment, Role, ToolCall};
pub use worker_tier::{
    CONTEXT_WINDOW_MAX, CONTEXT_WINDOW_MIN, SELECTABLE_CONTEXT_WINDOWS, WorkerTier,
    context_window_label, hosts_job_prompt, normalize_hosted_model,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 客户端名。用作 Provider 请求的 `x-client-name`——那一头有 `x-client-version`
/// 作搭档，名字就该是纯名字。
pub const CLIENT_NAME: &str = "WillDeep Cli (some.im)";

/// 每一次对外请求（Provider API、遥测、通知 webhook）自报的 `User-Agent`：
/// 客户端名后面跟一个空格和版本号。`concat!` 在编译期拼好，省得每次建
/// client 都 `format!` 一遍。
pub const CLIENT_USER_AGENT: &str = concat!("WillDeep Cli (some.im) ", env!("CARGO_PKG_VERSION"));
