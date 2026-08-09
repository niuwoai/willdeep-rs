pub mod agent;
pub mod mcp;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod skills;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent, AgentOutcome, EventSink};
pub use mcp::{McpRegistry, McpServerConfig};
pub use provider::{ApiDialect, ProviderConfig, ProviderKind, build_provider};
pub use session::{Session, SessionStore};
pub use skills::SkillCatalog;
pub use tools::{ApprovalMode, Approver, ToolRegistry};
pub use types::{Message, MessageAttachment, Role, ToolCall};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
