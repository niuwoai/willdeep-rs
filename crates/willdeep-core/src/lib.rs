pub mod agent;
pub mod attention;
pub mod background;
pub mod goal;
pub mod judge;
/// 实弹靶场：真 Provider、真缺陷、真退出码。仅测试构建，默认 `#[ignore]`。
#[cfg(test)]
mod livefire;
pub mod mcp;
pub mod prompt;
pub mod provider;
pub mod routing;
pub mod safety;
pub mod session;
pub mod skills;
pub mod subagent;
mod subagent_worktree;
pub mod tools;
pub mod types;

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
pub use mcp::{McpRegistry, McpServerConfig};
pub use provider::{ApiDialect, ProviderConfig, ProviderKind, build_provider};
pub use routing::{EscalationTicket, RouteDecision, RoutingGuard, RoutingPolicy, RoutingTier};
pub use safety::{CommandSafety, classify_with_workspace_write};
pub use session::{Session, SessionDigest, SessionStore, format_iso8601};
pub use skills::SkillCatalog;
pub use subagent::{
    SubagentCatalog, SubagentProfile, SubagentWriteScope, TaskPacket, TaskVerifier,
    builtin_profiles,
};
pub use subagent_worktree::SubagentWorktreePolicy;
pub use tools::{
    ApprovalDecision, ApprovalMode, Approver, CommandVerification, ToolRegistry, UserQuestion,
    VerificationStatus, WebToolConfig, run_background_supervisor,
};
pub use types::{Message, MessageAttachment, Role, ToolCall};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
