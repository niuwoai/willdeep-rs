use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.0";
pub const MIN_CLIENT_PROTOCOL_VERSION: &str = "1.0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiRequest {
    pub protocol_version: String,
    pub request_id: uuid::Uuid,
    pub operation: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ApiRequest {
    pub fn new(operation: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            request_id: uuid::Uuid::new_v4(),
            operation: operation.into(),
            params,
        }
    }

    pub fn is_protocol_compatible(&self) -> bool {
        protocol_major(&self.protocol_version) == protocol_major(PROTOCOL_VERSION)
    }
}

fn protocol_major(version: &str) -> Option<&str> {
    let major = version.split('.').next()?;
    (!major.is_empty() && major.chars().all(|value| value.is_ascii_digit())).then_some(major)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Runtime,
    Workspace,
    Session,
    Agent,
    Turn,
    Tool,
    Task,
    Approval,
    Question,
    Artifact,
    Event,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    HttpLoopback,
    ServerSentEvents,
    Ndjson,
    UnixSocket,
    WindowsNamedPipe,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    RuntimeObserve,
    WorkspaceManage,
    SessionManage,
    SessionFork,
    AgentObserve,
    AgentControl,
    AgentWorktree,
    TurnSubmit,
    TurnCancel,
    ApprovalResolve,
    QuestionAnswer,
    DiffReview,
    EventReplay,
    EventStream,
    AttachmentImage,
    SkillDiscover,
    McpTools,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub sequence: u64,
    pub timestamp: u64,
    pub kind: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Failed,
    Interrupted,
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSession {
    pub id: uuid::Uuid,
    pub root_agent_id: uuid::Uuid,
    pub workspace: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub status: SessionStatus,
    pub active_turn_id: Option<uuid::Uuid>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeTurn {
    pub id: uuid::Uuid,
    pub session_id: uuid::Uuid,
    pub request_id: uuid::Uuid,
    pub queue_sequence: u64,
    pub status: TurnStatus,
    pub active_task_id: Option<uuid::Uuid>,
    pub attempts: u32,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeAgent {
    pub id: uuid::Uuid,
    pub parent_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub label: Option<String>,
    pub background: bool,
    pub workspace: Option<String>,
    pub root_workspace: Option<String>,
    pub worktree_branch: Option<String>,
    pub dedicated_worktree: bool,
    pub profile: Option<String>,
    pub status: AgentStatus,
    pub current_turn: u64,
    pub current_tool: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub max_turns: Option<u64>,
    pub token_budget: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub report: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub completed_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Cancelling,
    WaitingApproval,
    WaitingAnswer,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeTask {
    pub id: uuid::Uuid,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub agent_id: Option<uuid::Uuid>,
    pub event_start_sequence: u64,
    pub status: TaskStatus,
    pub workspace: Option<String>,
    pub profile: Option<String>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingApproval {
    pub id: uuid::Uuid,
    pub task_id: uuid::Uuid,
    pub description: String,
    pub always_allow_available: bool,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingQuestion {
    pub id: uuid::Uuid,
    pub task_id: uuid::Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAccess {
    ReadOnly,
    Smart,
    #[default]
    WorkspaceWrite,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeWorkspace {
    pub id: uuid::Uuid,
    pub name: String,
    pub root: Option<String>,
    pub access: WorkspaceAccess,
    pub provider_profile: Option<String>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisterWorkspaceParams {
    pub root: String,
    pub name: Option<String>,
    #[serde(default)]
    pub access: WorkspaceAccess,
    pub provider_profile: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolLimits {
    pub max_event_page: u32,
    pub max_prompt_bytes: u64,
    pub max_attachment_bytes: u64,
    pub max_worktree_patch_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub protocol_version: String,
    pub min_client_protocol_version: String,
    pub server_version: String,
    pub objects: Vec<ObjectKind>,
    pub capabilities: Vec<Capability>,
    pub operations: Vec<String>,
    pub transports: Vec<TransportKind>,
    pub limits: ProtocolLimits,
}

impl RuntimeCapabilities {
    pub fn current(server_version: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            min_client_protocol_version: MIN_CLIENT_PROTOCOL_VERSION.to_owned(),
            server_version: server_version.into(),
            objects: vec![
                ObjectKind::Runtime,
                ObjectKind::Workspace,
                ObjectKind::Session,
                ObjectKind::Agent,
                ObjectKind::Turn,
                ObjectKind::Tool,
                ObjectKind::Task,
                ObjectKind::Approval,
                ObjectKind::Question,
                ObjectKind::Artifact,
                ObjectKind::Event,
            ],
            capabilities: vec![
                Capability::RuntimeObserve,
                Capability::WorkspaceManage,
                Capability::SessionManage,
                Capability::SessionFork,
                Capability::AgentObserve,
                Capability::AgentControl,
                Capability::AgentWorktree,
                Capability::TurnSubmit,
                Capability::TurnCancel,
                Capability::ApprovalResolve,
                Capability::QuestionAnswer,
                Capability::DiffReview,
                Capability::EventReplay,
                Capability::EventStream,
                Capability::AttachmentImage,
                Capability::SkillDiscover,
                Capability::McpTools,
            ],
            operations: SUPPORTED_OPERATIONS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            transports: vec![
                TransportKind::HttpLoopback,
                TransportKind::ServerSentEvents,
                TransportKind::Ndjson,
            ],
            limits: ProtocolLimits {
                max_event_page: 1_000,
                max_prompt_bytes: 1024 * 1024,
                max_attachment_bytes: 10 * 1024 * 1024,
                max_worktree_patch_bytes: 2 * 1024 * 1024,
            },
        }
    }
}

pub const SUPPORTED_OPERATIONS: &[&str] = &[
    "runtime.capabilities",
    "runtime.status",
    "workspace.list",
    "workspace.register",
    "workspace.ensure",
    "workspace.activate",
    "workspace.remove",
    "session.create",
    "session.list",
    "session.get",
    "session.rename",
    "session.fork",
    "session.archive",
    "session.delete",
    "session.export",
    "agent.list",
    "agent.get",
    "agent.prompt",
    "agent.wait",
    "agent.stop",
    "agent.retry",
    "task.list",
    "task.get",
    "task.cancel",
    "turn.submit",
    "turn.get",
    "turn.stop",
    "approval.list",
    "approval.resolve",
    "question.list",
    "question.answer",
    "event.list",
    "event.stream",
    "diff.snapshot",
    "diff.review",
    "worktree.review",
    "worktree.merge",
    "worktree.audit",
    "worktree.quarantine",
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    StaleSnapshot,
    UnsupportedOperation,
    UnsupportedProtocol,
    RateLimited,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResponseMeta {
    pub protocol_version: String,
    pub server_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApiResponse<T> {
    Ok { data: T, meta: ResponseMeta },
    Error { error: ApiError, meta: ResponseMeta },
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T, server_version: impl Into<String>, request_id: Option<uuid::Uuid>) -> Self {
        Self::Ok {
            data,
            meta: ResponseMeta {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                server_version: server_version.into(),
                request_id,
            },
        }
    }

    pub fn error(
        code: ErrorCode,
        message: impl Into<String>,
        retryable: bool,
        server_version: impl Into<String>,
        request_id: Option<uuid::Uuid>,
    ) -> Self {
        Self::Error {
            error: ApiError {
                code,
                message: message.into(),
                retryable,
                fields: BTreeMap::new(),
            },
            meta: ResponseMeta {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                server_version: server_version.into(),
                request_id,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn operation_names_are_unique_and_namespaced() {
        let unique = SUPPORTED_OPERATIONS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), SUPPORTED_OPERATIONS.len());
        assert!(SUPPORTED_OPERATIONS.iter().all(|operation| {
            let (namespace, method) = operation.split_once('.').unwrap_or_default();
            !namespace.is_empty() && !method.is_empty()
        }));
    }

    #[test]
    fn capabilities_and_envelope_round_trip_without_credentials() {
        let response = ApiResponse::ok(
            RuntimeCapabilities::current("0.21.0-rc1"),
            "0.21.0-rc1",
            Some(uuid::Uuid::nil()),
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("token"));
        assert!(!json.contains("api_key"));
        let decoded: ApiResponse<RuntimeCapabilities> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn request_requires_the_same_protocol_major() {
        let mut request = ApiRequest::new("session.list", serde_json::Value::Null);
        assert!(request.is_protocol_compatible());
        request.protocol_version = "1.99".to_owned();
        assert!(request.is_protocol_compatible());
        request.protocol_version = "2.0".to_owned();
        assert!(!request.is_protocol_compatible());
        request.protocol_version = "invalid".to_owned();
        assert!(!request.is_protocol_compatible());
    }

    #[test]
    fn public_session_agent_and_turn_dtos_exclude_private_request_content() {
        let session = RuntimeSession {
            id: uuid::Uuid::new_v4(),
            root_agent_id: uuid::Uuid::new_v4(),
            workspace: Some("/workspace".to_owned()),
            profile: Some("coding".to_owned()),
            model: Some("model".to_owned()),
            status: SessionStatus::Idle,
            active_turn_id: None,
            created_at: 1,
            updated_at: 2,
        };
        let json = serde_json::to_value(&session).unwrap();
        assert!(json.get("config").is_none());
        assert!(json.get("last_error").is_none());

        let turn = RuntimeTurn {
            id: uuid::Uuid::new_v4(),
            session_id: session.id,
            request_id: uuid::Uuid::new_v4(),
            queue_sequence: 1,
            status: TurnStatus::Queued,
            active_task_id: None,
            attempts: 0,
            created_at: 1,
            started_at: None,
            completed_at: None,
        };
        let json = serde_json::to_value(&turn).unwrap();
        assert!(json.get("prompt").is_none());
        assert!(json.get("attachments").is_none());
        assert!(json.get("error").is_none());

        let task = RuntimeTask {
            id: uuid::Uuid::new_v4(),
            session_id: Some(session.id),
            turn_id: Some(turn.id),
            agent_id: None,
            event_start_sequence: 1,
            status: TaskStatus::Running,
            workspace: Some("/workspace".to_owned()),
            profile: None,
            created_at: 1,
            started_at: Some(2),
            completed_at: None,
            exit_code: None,
        };
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("pid").is_none());
        assert!(json.get("error").is_none());

        let approval = PendingApproval {
            id: uuid::Uuid::new_v4(),
            task_id: task.id,
            description: "run tests".to_owned(),
            always_allow_available: true,
            created_at: 1,
        };
        let json = serde_json::to_value(&approval).unwrap();
        assert!(json.get("resolution").is_none());
        assert!(json.get("resolved_at").is_none());

        let workspace = RuntimeWorkspace {
            id: uuid::Uuid::new_v4(),
            name: "Project".to_owned(),
            root: None,
            access: WorkspaceAccess::WorkspaceWrite,
            provider_profile: None,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            created_at: 1,
            updated_at: 2,
            active: true,
        };
        let json = serde_json::to_value(&workspace).unwrap();
        assert!(json.get("schema").is_none());
        assert_eq!(json["root"], serde_json::Value::Null);
        assert!(
            serde_json::from_value::<RegisterWorkspaceParams>(serde_json::json!({
                "root": "/workspace",
                "name": null,
                "access": "smart",
                "provider_profile": null,
                "skills": [],
                "mcp_servers": [],
                "unexpected": true
            }))
            .is_err()
        );
    }
}
