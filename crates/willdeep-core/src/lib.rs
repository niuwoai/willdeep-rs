pub mod agent;
pub mod attention;
pub mod background;
pub mod goal;
pub mod mcp;
pub mod prompt;
pub mod provider;
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
pub use mcp::{McpRegistry, McpServerConfig};
pub use provider::{ApiDialect, ProviderConfig, ProviderKind, build_provider};
pub use session::{Session, SessionDigest, SessionStore};
pub use skills::SkillCatalog;
pub use subagent::{SubagentCatalog, SubagentProfile, builtin_profiles};
pub use subagent_worktree::SubagentWorktreePolicy;
pub use tools::{
    ApprovalDecision, ApprovalMode, Approver, CommandVerification, ToolRegistry, UserQuestion,
    VerificationStatus, WebToolConfig, run_background_supervisor,
};
pub use types::{Message, MessageAttachment, Role, ToolCall};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
