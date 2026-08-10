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
    "turn.submit",
    "turn.get",
    "turn.stop",
    "approval.list",
    "approval.resolve",
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
}
