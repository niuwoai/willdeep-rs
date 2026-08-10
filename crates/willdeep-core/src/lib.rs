pub mod agent;
pub mod attention;
pub mod background;
pub mod mcp;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod skills;
pub mod subagent;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, AgentOutcome, EventSink};
pub use attention::{
    AttentionItem, AttentionSection, AttentionSource, RuntimeScopeKind, RuntimeStatus,
    StatusRollup, rollup_status, sort_attention_items,
};
pub use background::{
    BackgroundTaskEvent, BackgroundTaskKind, BackgroundTaskRegistry, BackgroundTaskSnapshot,
    BackgroundTaskStatus,
};
pub use mcp::{McpRegistry, McpServerConfig};
pub use provider::{ApiDialect, ProviderConfig, ProviderKind, build_provider};
pub use session::{Session, SessionStore};
pub use skills::SkillCatalog;
pub use subagent::{SubagentCatalog, SubagentProfile, builtin_profiles};
pub use tools::{
    ApprovalDecision, ApprovalMode, Approver, ToolRegistry, UserQuestion, WebToolConfig,
};
pub use types::{Message, MessageAttachment, Role, ToolCall};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
