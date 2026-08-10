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
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    HttpLoopback,
    ServerSentEvents,
    Ndjson,
    UnixSocket,
    WindowsNamedPipe,
    #[serde(other)]
    Unknown,
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
    #[serde(other)]
    Unknown,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionParams {
    pub id: Option<uuid::Uuid>,
    pub workspace: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdParams {
    pub id: uuid::Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameSessionParams {
    pub id: uuid::Uuid,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ForkSessionParams {
    pub id: uuid::Uuid,
    pub title: Option<String>,
    pub through_turn_id: Option<uuid::Uuid>,
    pub provider_profile: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSessionParams {
    pub id: uuid::Uuid,
    pub archived: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeleteSessionParams {
    pub id: uuid::Uuid,
    pub confirmation: uuid::Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchSessionsParams {
    pub query: Option<String>,
    pub workspace: Option<String>,
    pub status: Option<SessionStatus>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub updated_after: Option<u64>,
    pub updated_before: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSearchResult {
    pub id: uuid::Uuid,
    pub title: String,
    pub workspace: Option<String>,
    pub status: SessionStatus,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub updated_at: u64,
    pub message_count: usize,
    pub snippet: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageAttachment {
    Text {
        name: String,
        content: String,
    },
    Image {
        name: String,
        media_type: String,
        data: String,
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SubmitTurnParams {
    pub session_id: uuid::Uuid,
    pub turn_request_id: uuid::Uuid,
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListTurnsParams {
    pub session_id: uuid::Uuid,
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
pub enum ToolStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeTool {
    pub id: uuid::Uuid,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub name: String,
    pub status: ToolStatus,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListToolsParams {
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: Option<uuid::Uuid>,
    pub agent_id: Option<uuid::Uuid>,
    pub status: Option<ToolStatus>,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    WorkspaceChange,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeArtifact {
    pub id: uuid::Uuid,
    pub kind: ArtifactKind,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub title: String,
    pub source_id: String,
    pub item_count: usize,
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListArtifactsParams {
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: Option<uuid::Uuid>,
    pub agent_id: Option<uuid::Uuid>,
    pub kind: Option<ArtifactKind>,
    pub limit: Option<usize>,
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

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffArea {
    Staged,
    Unstaged,
    #[default]
    Combined,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: DiffFileKind,
    pub staged: bool,
    pub unstaged: bool,
    pub binary: bool,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSnapshot {
    pub id: String,
    pub workspace: Option<String>,
    pub head: Option<String>,
    pub files: Vec<DiffFile>,
    pub additions: u64,
    pub deletions: u64,
    pub has_conflicts: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffContent {
    pub snapshot_id: String,
    pub path: String,
    pub area: DiffArea,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accepted,
    Rejected,
    ChangesRequested,
    Reviewed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffReview {
    pub id: uuid::Uuid,
    pub snapshot_id: String,
    pub workspace: Option<String>,
    pub path: String,
    pub decision: ReviewDecision,
    pub note: Option<String>,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Passed,
    Failed,
    TimedOut,
    LaunchFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffVerification {
    pub id: uuid::Uuid,
    pub snapshot_id: String,
    pub workspace: Option<String>,
    pub command: String,
    pub exit_code: Option<i32>,
    pub outcome: VerificationOutcome,
    pub summary: String,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttributionConfidence {
    ToolWindow,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffAttribution {
    pub id: uuid::Uuid,
    pub before_snapshot_id: String,
    pub after_snapshot_id: String,
    pub workspace: Option<String>,
    pub session_id: Option<uuid::Uuid>,
    pub turn_id: Option<uuid::Uuid>,
    pub task_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub tool: String,
    pub paths: Vec<String>,
    pub confidence: AttributionConfidence,
    pub created_at: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Warning,
    Blocker,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SensitiveFinding {
    pub path: String,
    pub code: String,
    pub severity: FindingSeverity,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffCommitPreview {
    pub snapshot_id: String,
    pub workspace: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub message: String,
    pub staged_files: Vec<String>,
    pub unstaged_files: Vec<String>,
    pub sensitive_findings: Vec<SensitiveFinding>,
    pub remote: String,
    pub push_target: Option<String>,
    pub tag: Option<String>,
    pub blockers: Vec<String>,
    pub requires_confirmation: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffRevertResult {
    pub previous_snapshot_id: String,
    pub current_snapshot_id: String,
    pub path: String,
    pub recovery_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffSnapshotParams {
    pub workspace: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffSnapshotQueryParams {
    pub workspace: String,
    pub snapshot_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffContentParams {
    pub workspace: String,
    pub snapshot_id: String,
    pub path: String,
    #[serde(default)]
    pub area: DiffArea,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffReviewParams {
    pub workspace: String,
    pub snapshot_id: String,
    pub path: String,
    pub decision: ReviewDecision,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffVerificationParams {
    pub workspace: String,
    pub snapshot_id: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub outcome: VerificationOutcome,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffCommitPreviewParams {
    pub workspace: String,
    pub snapshot_id: String,
    pub message: String,
    pub remote: String,
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffRevertParams {
    pub workspace: String,
    pub snapshot_id: String,
    pub path: String,
    #[serde(default)]
    pub area: DiffArea,
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
    "session.search",
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
    "turn.list",
    "turn.get",
    "turn.stop",
    "tool.list",
    "tool.get",
    "artifact.list",
    "artifact.get",
    "approval.list",
    "approval.resolve",
    "question.list",
    "question.answer",
    "event.list",
    "event.stream",
    "diff.snapshot",
    "diff.content",
    "diff.reviews",
    "diff.review",
    "diff.verifications",
    "diff.verification.record",
    "diff.attributions",
    "diff.commit_preview",
    "diff.revert",
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

    fn decode_fixture<T: serde::de::DeserializeOwned>(
        responses: &serde_json::Map<String, serde_json::Value>,
        name: &str,
    ) {
        let value = responses
            .get(name)
            .unwrap_or_else(|| panic!("missing {name} fixture"));
        let _: ApiResponse<T> = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("invalid {name} fixture: {error}"));
    }

    #[test]
    fn public_api_fixture_decodes_every_stable_object_without_secrets() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/public-api-v1.json")).unwrap();
        assert_eq!(fixture["fixture_version"], "1");
        assert_eq!(fixture["protocol_version"], PROTOCOL_VERSION);
        let serialized = serde_json::to_string(&fixture).unwrap();
        for forbidden in ["api_key", "authorization", "x-willdeep-token"] {
            assert!(!serialized.to_ascii_lowercase().contains(forbidden));
        }
        let responses = fixture["responses"].as_object().unwrap();
        decode_fixture::<RuntimeCapabilities>(responses, "runtime");
        decode_fixture::<RuntimeWorkspace>(responses, "workspace");
        decode_fixture::<RuntimeSession>(responses, "session");
        decode_fixture::<RuntimeAgent>(responses, "agent");
        decode_fixture::<RuntimeTurn>(responses, "turn");
        decode_fixture::<RuntimeTool>(responses, "tool");
        decode_fixture::<RuntimeTask>(responses, "task");
        decode_fixture::<PendingApproval>(responses, "approval");
        decode_fixture::<PendingQuestion>(responses, "question");
        decode_fixture::<RuntimeArtifact>(responses, "artifact");
        decode_fixture::<RuntimeEvent>(responses, "event");
        assert_eq!(responses.len(), 11);
    }

    #[test]
    fn capability_response_tolerates_future_object_capability_and_transport_values() {
        let mut value = serde_json::to_value(ApiResponse::ok(
            RuntimeCapabilities::current("test"),
            "test",
            None,
        ))
        .unwrap();
        let data = value["data"].as_object_mut().unwrap();
        data.get_mut("objects")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("future_object"));
        data.get_mut("capabilities")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("future_capability"));
        data.get_mut("transports")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("future_transport"));

        let decoded: ApiResponse<RuntimeCapabilities> = serde_json::from_value(value).unwrap();
        let ApiResponse::Ok { data, .. } = decoded else {
            panic!("expected a successful capabilities response");
        };
        assert_eq!(data.objects.last(), Some(&ObjectKind::Unknown));
        assert_eq!(data.capabilities.last(), Some(&Capability::Unknown));
        assert_eq!(data.transports.last(), Some(&TransportKind::Unknown));
    }

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

    #[test]
    fn public_tool_activity_contains_lifecycle_but_no_payloads() {
        let tool = RuntimeTool {
            id: uuid::Uuid::new_v4(),
            session_id: Some(uuid::Uuid::new_v4()),
            turn_id: Some(uuid::Uuid::new_v4()),
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            name: "run_command".to_owned(),
            status: ToolStatus::Completed,
            started_at_ms: 10,
            completed_at_ms: Some(20),
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            serde_json::from_value::<RuntimeTool>(value.clone()).unwrap(),
            tool
        );
        let object = value.as_object().unwrap();
        assert!(!object.contains_key("arguments"));
        assert!(!object.contains_key("output"));
        assert!(!object.contains_key("workspace"));
        assert!(SUPPORTED_OPERATIONS.contains(&"tool.list"));
        assert!(SUPPORTED_OPERATIONS.contains(&"tool.get"));
    }

    #[test]
    fn workspace_change_artifact_exposes_source_but_not_paths_or_content() {
        let artifact = RuntimeArtifact {
            id: uuid::Uuid::new_v4(),
            kind: ArtifactKind::WorkspaceChange,
            session_id: Some(uuid::Uuid::new_v4()),
            turn_id: Some(uuid::Uuid::new_v4()),
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            title: "edit_file workspace changes".to_owned(),
            source_id: "snapshot-hash".to_owned(),
            item_count: 2,
            created_at: 10,
        };
        let value = serde_json::to_value(&artifact).unwrap();
        assert_eq!(
            serde_json::from_value::<RuntimeArtifact>(value.clone()).unwrap(),
            artifact
        );
        let object = value.as_object().unwrap();
        assert!(!object.contains_key("paths"));
        assert!(!object.contains_key("content"));
        assert!(!object.contains_key("workspace"));
        assert!(SUPPORTED_OPERATIONS.contains(&"artifact.list"));
        assert!(SUPPORTED_OPERATIONS.contains(&"artifact.get"));
    }

    #[test]
    fn diff_contract_is_typed_and_rejects_ambiguous_mutation_params() {
        for operation in [
            "diff.snapshot",
            "diff.content",
            "diff.reviews",
            "diff.review",
            "diff.verifications",
            "diff.verification.record",
            "diff.attributions",
            "diff.commit_preview",
            "diff.revert",
        ] {
            assert!(SUPPORTED_OPERATIONS.contains(&operation));
        }
        let snapshot = DiffSnapshot {
            id: "diff-1".to_owned(),
            workspace: None,
            head: Some("abc".to_owned()),
            files: vec![DiffFile {
                path: "src/lib.rs".to_owned(),
                old_path: None,
                kind: DiffFileKind::Modified,
                staged: false,
                unstaged: true,
                binary: false,
                additions: 2,
                deletions: 1,
            }],
            additions: 2,
            deletions: 1,
            has_conflicts: false,
        };
        let encoded = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_value::<DiffSnapshot>(encoded).unwrap(),
            snapshot
        );
        assert!(
            serde_json::from_value::<DiffRevertParams>(serde_json::json!({
                "workspace": "/workspace",
                "snapshot_id": "diff-1",
                "path": "src/lib.rs",
                "area": "combined",
                "force": true
            }))
            .is_err()
        );
    }

    #[test]
    fn session_management_params_require_explicit_targets() {
        let id = uuid::Uuid::new_v4();
        assert!(
            serde_json::from_value::<CreateSessionParams>(serde_json::json!({
                "id": id,
                "workspace": "/workspace",
                "profile": "default",
                "model": null,
                "title": "Private config stays server-side",
                "config": "/private/config.toml"
            }))
            .is_err()
        );
        let rename = RenameSessionParams {
            id,
            title: "Renamed".to_owned(),
        };
        assert_eq!(
            serde_json::from_value::<RenameSessionParams>(serde_json::to_value(&rename).unwrap())
                .unwrap(),
            rename
        );
        assert!(
            serde_json::from_value::<DeleteSessionParams>(serde_json::json!({
                "id": id,
                "confirmation": id,
                "force": true
            }))
            .is_err()
        );
        for operation in [
            "session.create",
            "session.rename",
            "session.fork",
            "session.archive",
            "session.delete",
            "session.export",
        ] {
            assert!(SUPPORTED_OPERATIONS.contains(&operation));
        }
    }

    #[test]
    fn turn_submission_round_trips_typed_attachments_and_rejects_extra_controls() {
        let params = SubmitTurnParams {
            session_id: uuid::Uuid::new_v4(),
            turn_request_id: uuid::Uuid::new_v4(),
            prompt: "inspect the image".to_owned(),
            attachments: vec![MessageAttachment::Image {
                name: "screen.png".to_owned(),
                media_type: "image/png".to_owned(),
                data: "aGVsbG8=".to_owned(),
                width: 1,
                height: 1,
            }],
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(
            serde_json::from_value::<SubmitTurnParams>(value).unwrap(),
            params
        );
        assert!(
            serde_json::from_value::<SubmitTurnParams>(serde_json::json!({
                "session_id": uuid::Uuid::new_v4(),
                "turn_request_id": uuid::Uuid::new_v4(),
                "prompt": "hello",
                "attachments": [],
                "workspace": "/escape"
            }))
            .is_err()
        );
        for operation in [
            "session.search",
            "turn.submit",
            "turn.list",
            "turn.get",
            "turn.stop",
        ] {
            assert!(SUPPORTED_OPERATIONS.contains(&operation));
        }
    }
}
