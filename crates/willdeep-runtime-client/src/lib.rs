use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use willdeep_runtime_protocol::{
    AgentPromptParams, AgentWaitParams, AnswerQuestionParams, ApiRequest, ApiResponse,
    ArchiveSessionParams, CreateSessionParams, DeleteSessionParams, DiffAttribution,
    DiffCommitPreview, DiffCommitPreviewParams, DiffContent, DiffContentParams, DiffRevertParams,
    DiffRevertResult, DiffReview, DiffReviewParams, DiffSnapshot, DiffSnapshotParams,
    DiffSnapshotQueryParams, DiffVerification, DiffVerificationParams, EmptyParams,
    EventListParams, ForkSessionParams, IdParams, ListArtifactsParams, ListToolsParams,
    ListTurnsParams, ObjectMutationResult, PendingApproval, PendingQuestion,
    RegisterWorkspaceParams, RenameSessionParams, ResolveApprovalParams, RuntimeAgent,
    RuntimeAgentCommand, RuntimeArtifact, RuntimeCapabilities, RuntimeEvent,
    RuntimeInteractionResult, RuntimeSession, RuntimeStatus, RuntimeTask, RuntimeTool, RuntimeTurn,
    RuntimeWorkspace, RuntimeWorktreeAudit, RuntimeWorktreeMergeResult,
    RuntimeWorktreeQuarantineResult, RuntimeWorktreeReview, SearchSessionsParams, SubmitTurnParams,
    UpdateSessionModelParams, WorkspaceEnsureParams, WorktreeMergeParams, WorktreeQuarantineParams,
    WorktreeReviewParams,
};

const TOKEN_HEADER: &str = "x-willdeep-token";
const REQUEST_ID_HEADER: &str = "x-willdeep-request-id";
const DEFAULT_MAX_NDJSON_LINE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct RuntimeClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl RuntimeClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self, ClientError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !base_url.starts_with("http://127.0.0.1:")
            && !base_url.starts_with("http://[::1]:")
            && !base_url.starts_with("http://localhost:")
        {
            return Err(ClientError::UnsafeEndpoint);
        }
        Ok(Self {
            base_url,
            token: token.into(),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .build()?,
        })
    }

    #[cfg(unix)]
    pub fn new_unix_socket(
        path: impl Into<std::path::PathBuf>,
        token: impl Into<String>,
    ) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .unix_socket(path.into())
            .build()?;
        Ok(Self {
            base_url: "http://localhost".to_owned(),
            token: token.into(),
            http,
        })
    }

    #[cfg(windows)]
    pub fn new_windows_named_pipe(
        name: impl Into<std::ffi::OsString>,
        token: impl Into<String>,
    ) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .windows_named_pipe(name.into())
            .build()?;
        Ok(Self {
            base_url: "http://localhost".to_owned(),
            token: token.into(),
            http,
        })
    }

    pub async fn capabilities(
        &self,
        request_id: Option<uuid::Uuid>,
    ) -> Result<ApiResponse<RuntimeCapabilities>, ClientError> {
        let mut request = self
            .http
            .get(format!("{}/v1/capabilities", self.base_url))
            .header(TOKEN_HEADER, &self.token);
        if let Some(request_id) = request_id {
            request = request.header(REQUEST_ID_HEADER, request_id.to_string());
        }
        decode_response(request.send().await?).await
    }

    pub async fn status(&self) -> Result<ApiResponse<RuntimeStatus>, ClientError> {
        self.call("runtime.status", &EmptyParams::default(), None)
            .await
    }

    pub async fn get_json<T>(&self, path: &str) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
    {
        decode_raw_response(
            self.http
                .get(format!("{}{}", self.base_url, normalized_path(path)))
                .header(TOKEN_HEADER, &self.token)
                .send()
                .await?,
        )
        .await
    }

    pub async fn post_empty(&self, path: &str) -> Result<(), ClientError> {
        let response = self
            .http
            .post(format!("{}{}", self.base_url, normalized_path(path)))
            .header(TOKEN_HEADER, &self.token)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(ClientError::HttpStatus(response.status().as_u16()))
        }
    }

    pub async fn call<P, T>(
        &self,
        operation: impl Into<String>,
        params: &P,
        request_id: Option<uuid::Uuid>,
    ) -> Result<ApiResponse<T>, ClientError>
    where
        P: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let mut request = ApiRequest::new(operation, serde_json::to_value(params)?);
        if let Some(request_id) = request_id {
            request.request_id = request_id;
        }
        decode_response(
            self.http
                .post(format!("{}/v1/api", self.base_url))
                .header(TOKEN_HEADER, &self.token)
                .json(&request)
                .send()
                .await?,
        )
        .await
    }

    pub async fn workspaces(&self) -> Result<ApiResponse<Vec<RuntimeWorkspace>>, ClientError> {
        self.call("workspace.list", &EmptyParams::default(), None)
            .await
    }

    pub async fn register_workspace(
        &self,
        params: &RegisterWorkspaceParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeWorkspace>, ClientError> {
        self.call("workspace.register", params, Some(request_id))
            .await
    }

    pub async fn ensure_workspace(
        &self,
        params: &WorkspaceEnsureParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeWorkspace>, ClientError> {
        self.call("workspace.ensure", params, Some(request_id))
            .await
    }

    pub async fn activate_workspace(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeWorkspace>, ClientError> {
        self.call("workspace.activate", &IdParams { id }, Some(request_id))
            .await
    }

    pub async fn remove_workspace(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<ObjectMutationResult>, ClientError> {
        self.call("workspace.remove", &IdParams { id }, Some(request_id))
            .await
    }

    pub async fn sessions(&self) -> Result<ApiResponse<Vec<RuntimeSession>>, ClientError> {
        self.call("session.list", &EmptyParams::default(), None)
            .await
    }

    pub async fn create_session(
        &self,
        params: &CreateSessionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.create", params, Some(request_id)).await
    }

    pub async fn search_sessions(
        &self,
        params: &SearchSessionsParams,
    ) -> Result<ApiResponse<Vec<willdeep_runtime_protocol::SessionSearchResult>>, ClientError> {
        self.call("session.search", params, None).await
    }

    pub async fn session(
        &self,
        id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.get", &IdParams { id }, None).await
    }

    pub async fn rename_session(
        &self,
        params: &RenameSessionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.rename", params, Some(request_id)).await
    }

    pub async fn update_session_model(
        &self,
        params: &UpdateSessionModelParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.update_model", params, Some(request_id))
            .await
    }

    pub async fn fork_session(
        &self,
        params: &ForkSessionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.fork", params, Some(request_id)).await
    }

    pub async fn archive_session(
        &self,
        params: &ArchiveSessionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeSession>, ClientError> {
        self.call("session.archive", params, Some(request_id)).await
    }

    pub async fn delete_session(
        &self,
        params: &DeleteSessionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<ObjectMutationResult>, ClientError> {
        self.call("session.delete", params, Some(request_id)).await
    }

    pub async fn export_session(
        &self,
        id: uuid::Uuid,
    ) -> Result<ApiResponse<serde_json::Value>, ClientError> {
        self.call("session.export", &IdParams { id }, None).await
    }

    pub async fn agents(&self) -> Result<ApiResponse<Vec<RuntimeAgent>>, ClientError> {
        self.call("agent.list", &EmptyParams::default(), None).await
    }

    pub async fn agent(&self, id: uuid::Uuid) -> Result<ApiResponse<RuntimeAgent>, ClientError> {
        self.call("agent.get", &IdParams { id }, None).await
    }

    pub async fn spawn_agent(
        &self,
        params: &willdeep_runtime_protocol::SpawnAgentParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeAgent>, ClientError> {
        self.call("agent.spawn", params, Some(request_id)).await
    }

    pub async fn prompt_agent(
        &self,
        params: &AgentPromptParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeAgentCommand>, ClientError> {
        self.call("agent.prompt", params, Some(request_id)).await
    }

    pub async fn wait_agent(
        &self,
        params: &AgentWaitParams,
    ) -> Result<ApiResponse<RuntimeAgent>, ClientError> {
        self.call("agent.wait", params, None).await
    }

    pub async fn stop_agent(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeAgentCommand>, ClientError> {
        self.call("agent.stop", &IdParams { id }, Some(request_id))
            .await
    }

    pub async fn retry_agent(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeAgentCommand>, ClientError> {
        self.retry_agent_with_model(id, None, request_id).await
    }

    pub async fn retry_agent_with_model(
        &self,
        id: uuid::Uuid,
        model: Option<String>,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeAgentCommand>, ClientError> {
        self.call(
            "agent.retry",
            &willdeep_runtime_protocol::RetryAgentParams { id, model },
            Some(request_id),
        )
        .await
    }

    pub async fn tasks(&self) -> Result<ApiResponse<Vec<RuntimeTask>>, ClientError> {
        self.call("task.list", &EmptyParams::default(), None).await
    }

    pub async fn task(&self, id: uuid::Uuid) -> Result<ApiResponse<RuntimeTask>, ClientError> {
        self.call("task.get", &IdParams { id }, None).await
    }

    pub async fn cancel_task(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeTask>, ClientError> {
        self.call("task.cancel", &IdParams { id }, Some(request_id))
            .await
    }

    pub async fn turns(
        &self,
        session_id: uuid::Uuid,
    ) -> Result<ApiResponse<Vec<RuntimeTurn>>, ClientError> {
        self.call("turn.list", &ListTurnsParams { session_id }, None)
            .await
    }

    pub async fn turn(&self, id: uuid::Uuid) -> Result<ApiResponse<RuntimeTurn>, ClientError> {
        self.call("turn.get", &IdParams { id }, None).await
    }

    pub async fn submit_turn(
        &self,
        params: &SubmitTurnParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeTurn>, ClientError> {
        self.call("turn.submit", params, Some(request_id)).await
    }

    pub async fn stop_turn(
        &self,
        id: uuid::Uuid,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeTurn>, ClientError> {
        self.call("turn.stop", &IdParams { id }, Some(request_id))
            .await
    }

    pub async fn approvals(&self) -> Result<ApiResponse<Vec<PendingApproval>>, ClientError> {
        self.call("approval.list", &EmptyParams::default(), None)
            .await
    }

    pub async fn resolve_approval(
        &self,
        params: &ResolveApprovalParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeInteractionResult>, ClientError> {
        self.call("approval.resolve", params, Some(request_id))
            .await
    }

    pub async fn questions(&self) -> Result<ApiResponse<Vec<PendingQuestion>>, ClientError> {
        self.call("question.list", &EmptyParams::default(), None)
            .await
    }

    pub async fn answer_question(
        &self,
        params: &AnswerQuestionParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeInteractionResult>, ClientError> {
        self.call("question.answer", params, Some(request_id)).await
    }

    pub async fn events(
        &self,
        params: &EventListParams,
    ) -> Result<ApiResponse<Vec<RuntimeEvent>>, ClientError> {
        self.call("event.list", params, None).await
    }

    pub async fn diff_snapshot(
        &self,
        params: &DiffSnapshotParams,
    ) -> Result<ApiResponse<DiffSnapshot>, ClientError> {
        self.call("diff.snapshot", params, None).await
    }

    pub async fn diff_content(
        &self,
        params: &DiffContentParams,
    ) -> Result<ApiResponse<DiffContent>, ClientError> {
        self.call("diff.content", params, None).await
    }

    pub async fn diff_reviews(
        &self,
        params: &DiffSnapshotQueryParams,
    ) -> Result<ApiResponse<Vec<DiffReview>>, ClientError> {
        self.call("diff.reviews", params, None).await
    }

    pub async fn review_diff(
        &self,
        params: &DiffReviewParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<DiffReview>, ClientError> {
        self.call("diff.review", params, Some(request_id)).await
    }

    pub async fn diff_verifications(
        &self,
        params: &DiffSnapshotQueryParams,
    ) -> Result<ApiResponse<Vec<DiffVerification>>, ClientError> {
        self.call("diff.verifications", params, None).await
    }

    pub async fn record_diff_verification(
        &self,
        params: &DiffVerificationParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<DiffVerification>, ClientError> {
        self.call("diff.verification.record", params, Some(request_id))
            .await
    }

    pub async fn diff_attributions(
        &self,
        params: &DiffSnapshotQueryParams,
    ) -> Result<ApiResponse<Vec<DiffAttribution>>, ClientError> {
        self.call("diff.attributions", params, None).await
    }

    pub async fn diff_commit_preview(
        &self,
        params: &DiffCommitPreviewParams,
    ) -> Result<ApiResponse<DiffCommitPreview>, ClientError> {
        self.call("diff.commit_preview", params, None).await
    }

    pub async fn revert_diff(
        &self,
        params: &DiffRevertParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<DiffRevertResult>, ClientError> {
        self.call("diff.revert", params, Some(request_id)).await
    }

    pub async fn worktree_review(
        &self,
        params: &WorktreeReviewParams,
    ) -> Result<ApiResponse<RuntimeWorktreeReview>, ClientError> {
        self.call("worktree.review", params, None).await
    }

    pub async fn merge_worktree(
        &self,
        params: &WorktreeMergeParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeWorktreeMergeResult>, ClientError> {
        self.call("worktree.merge", params, Some(request_id)).await
    }

    pub async fn audit_worktrees(
        &self,
    ) -> Result<ApiResponse<Vec<RuntimeWorktreeAudit>>, ClientError> {
        self.call("worktree.audit", &EmptyParams::default(), None)
            .await
    }

    pub async fn quarantine_worktree(
        &self,
        params: &WorktreeQuarantineParams,
        request_id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeWorktreeQuarantineResult>, ClientError> {
        self.call("worktree.quarantine", params, Some(request_id))
            .await
    }

    pub async fn tools(
        &self,
        params: &ListToolsParams,
    ) -> Result<ApiResponse<Vec<RuntimeTool>>, ClientError> {
        self.call("tool.list", params, None).await
    }

    pub async fn tool(&self, id: uuid::Uuid) -> Result<ApiResponse<RuntimeTool>, ClientError> {
        self.call("tool.get", &IdParams { id }, None).await
    }

    pub async fn artifacts(
        &self,
        params: &ListArtifactsParams,
    ) -> Result<ApiResponse<Vec<RuntimeArtifact>>, ClientError> {
        self.call("artifact.list", params, None).await
    }

    pub async fn artifact(
        &self,
        id: uuid::Uuid,
    ) -> Result<ApiResponse<RuntimeArtifact>, ClientError> {
        self.call("artifact.get", &IdParams { id }, None).await
    }

    pub async fn stream_events(
        &self,
        after: u64,
        limit: usize,
        request_id: Option<uuid::Uuid>,
    ) -> Result<NdjsonEventStream, ClientError> {
        let mut request = self
            .http
            .get(format!(
                "{}/v1/events/stream.ndjson?after={after}&limit={}",
                self.base_url,
                limit.clamp(1, 1_000)
            ))
            .header(TOKEN_HEADER, &self.token);
        if let Some(request_id) = request_id {
            request = request.header(REQUEST_ID_HEADER, request_id.to_string());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(ClientError::HttpStatus(response.status().as_u16()));
        }
        Ok(NdjsonEventStream {
            chunks: Box::pin(response.bytes_stream()),
            buffer: Vec::new(),
            max_line_bytes: DEFAULT_MAX_NDJSON_LINE_BYTES,
        })
    }
}

fn normalized_path(path: &str) -> String {
    format!("/{}", path.trim_start_matches('/'))
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<ApiResponse<T>, ClientError> {
    let status = response.status();
    let body = response.bytes().await?;
    serde_json::from_slice(&body).map_err(|source| ClientError::InvalidResponse {
        status: status.as_u16(),
        source,
    })
}

async fn decode_raw_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ClientError> {
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        return Err(ClientError::HttpStatus(status.as_u16()));
    }
    serde_json::from_slice(&body).map_err(|source| ClientError::InvalidResponse {
        status: status.as_u16(),
        source,
    })
}

pub struct NdjsonEventStream {
    chunks: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
    max_line_bytes: usize,
}

impl NdjsonEventStream {
    pub async fn next<T: DeserializeOwned>(
        &mut self,
    ) -> Result<Option<ApiResponse<T>>, ClientError> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = self.buffer.drain(..=newline).collect::<Vec<_>>();
                if line[..line.len() - 1].iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return serde_json::from_slice(&line[..line.len() - 1])
                    .map(Some)
                    .map_err(ClientError::InvalidNdjson);
            }
            let Some(chunk) = self.chunks.next().await else {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                return Err(ClientError::TruncatedNdjson);
            };
            self.buffer.extend_from_slice(&chunk?);
            if self.buffer.len() > self.max_line_bytes {
                return Err(ClientError::NdjsonLineTooLarge(self.max_line_bytes));
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Runtime Client only accepts loopback HTTP endpoints")]
    UnsafeEndpoint,
    #[error("Runtime HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serialize Runtime request: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Runtime returned HTTP {0} without a usable stream")]
    HttpStatus(u16),
    #[error("Runtime returned an invalid response for HTTP {status}: {source}")]
    InvalidResponse {
        status: u16,
        source: serde_json::Error,
    },
    #[error("Runtime returned an invalid NDJSON line: {0}")]
    InvalidNdjson(serde_json::Error),
    #[error("Runtime NDJSON stream ended in the middle of a line")]
    TruncatedNdjson,
    #[error("Runtime NDJSON line exceeds {0} bytes")]
    NdjsonLineTooLarge(usize),
}

impl ClientError {
    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::HttpStatus(status) | Self::InvalidResponse { status, .. } => Some(*status),
            Self::Http(error) => error.status().map(|status| status.as_u16()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_http_status_for_transport_diagnostics() {
        let invalid = ClientError::InvalidResponse {
            status: 404,
            source: serde_json::from_slice::<serde_json::Value>(b"").unwrap_err(),
        };
        assert_eq!(invalid.status_code(), Some(404));
        assert_eq!(ClientError::HttpStatus(503).status_code(), Some(503));
        assert_eq!(ClientError::UnsafeEndpoint.status_code(), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sends_http_requests_over_a_unix_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::path::Path::new("/private/tmp")
            .join(format!("willdeep-runtime-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let length = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("GET /v1/health HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-willdeep-token: secret")
            );
            let body = r#"{"status":"ok"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = RuntimeClient::new_unix_socket(&socket, "secret").unwrap();
        let response: serde_json::Value = client.get_json("/v1/health").await.unwrap();
        assert_eq!(response["status"], "ok");
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typed_runtime_workspace_and_session_methods_preserve_contracts() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn receive_request(stream: &mut tokio::net::UnixStream) -> ApiRequest {
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            loop {
                let length = stream.read(&mut chunk).await.unwrap();
                assert_ne!(length, 0, "request ended before its JSON body arrived");
                request.extend_from_slice(&chunk[..length]);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                assert!(headers.starts_with("POST /v1/api HTTP/1.1"));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("x-willdeep-token: secret")
                );
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    return serde_json::from_slice(&request[header_end + 4..]).unwrap();
                }
            }
        }

        async fn send_response<T: Serialize>(
            stream: &mut tokio::net::UnixStream,
            value: &ApiResponse<T>,
        ) {
            let body = serde_json::to_string(value).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }

        let root = std::path::Path::new("/private/tmp")
            .join(format!("willdeep-runtime-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let workspace_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        let workspace_request_id = uuid::Uuid::new_v4();
        let session_request_id = uuid::Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = receive_request(&mut stream).await;
            assert_eq!(request.request_id, workspace_request_id);
            assert_eq!(request.operation, "workspace.register");
            let params: RegisterWorkspaceParams = serde_json::from_value(request.params).unwrap();
            assert_eq!(params.root, "/workspace");
            assert_eq!(
                params.access,
                willdeep_runtime_protocol::WorkspaceAccess::WorkspaceWrite
            );
            send_response(
                &mut stream,
                &ApiResponse::ok(
                    RuntimeWorkspace {
                        id: workspace_id,
                        name: "workspace".to_owned(),
                        root: Some("/workspace".to_owned()),
                        access: willdeep_runtime_protocol::WorkspaceAccess::WorkspaceWrite,
                        provider_profile: None,
                        skills: Vec::new(),
                        mcp_servers: Vec::new(),
                        created_at: 1,
                        updated_at: 1,
                        active: true,
                    },
                    "test",
                    Some(request.request_id),
                ),
            )
            .await;

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = receive_request(&mut stream).await;
            assert_eq!(request.request_id, session_request_id);
            assert_eq!(request.operation, "session.delete");
            let params: DeleteSessionParams = serde_json::from_value(request.params).unwrap();
            assert_eq!(params.id, session_id);
            assert_eq!(params.confirmation, session_id);
            send_response(
                &mut stream,
                &ApiResponse::ok(
                    ObjectMutationResult {
                        id: session_id,
                        status: willdeep_runtime_protocol::ObjectMutationStatus::Deleted,
                    },
                    "test",
                    Some(request.request_id),
                ),
            )
            .await;

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = receive_request(&mut stream).await;
            assert_eq!(request.operation, "runtime.status");
            assert_eq!(request.params, serde_json::json!({}));
            send_response(
                &mut stream,
                &ApiResponse::ok(
                    RuntimeStatus {
                        status: willdeep_runtime_protocol::RuntimeHealth::Draining,
                        version: "test".to_owned(),
                        pid: 42,
                        uptime_seconds: 10,
                        event_sequence: 7,
                    },
                    "test",
                    Some(request.request_id),
                ),
            )
            .await;
        });

        let client = RuntimeClient::new_unix_socket(&socket, "secret").unwrap();
        let workspace = client
            .register_workspace(
                &RegisterWorkspaceParams {
                    root: "/workspace".to_owned(),
                    name: None,
                    access: willdeep_runtime_protocol::WorkspaceAccess::WorkspaceWrite,
                    provider_profile: None,
                    skills: Vec::new(),
                    mcp_servers: Vec::new(),
                },
                workspace_request_id,
            )
            .await
            .unwrap();
        assert!(matches!(workspace, ApiResponse::Ok { data, .. } if data.id == workspace_id));

        let deleted = client
            .delete_session(
                &DeleteSessionParams {
                    id: session_id,
                    confirmation: session_id,
                },
                session_request_id,
            )
            .await
            .unwrap();
        assert!(matches!(
            deleted,
            ApiResponse::Ok { data, .. }
                if data.id == session_id
                    && data.status == willdeep_runtime_protocol::ObjectMutationStatus::Deleted
        ));

        let status = client.status().await.unwrap();
        assert!(matches!(
            status,
            ApiResponse::Ok { data, .. }
                if data.status == willdeep_runtime_protocol::RuntimeHealth::Draining
                    && data.event_sequence == 7
        ));
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typed_tool_list_uses_the_stable_operation_and_decodes_dto() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::path::Path::new("/private/tmp")
            .join(format!("willdeep-runtime-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let tool_id = uuid::Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            loop {
                let length = stream.read(&mut chunk).await.unwrap();
                assert_ne!(length, 0, "request ended before its JSON body arrived");
                request.extend_from_slice(&chunk[..length]);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|value| value == b"\r\n\r\n")
                .unwrap();
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(headers.starts_with("POST /v1/api HTTP/1.1"));
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("x-willdeep-token: secret")
            );
            let request: ApiRequest = serde_json::from_slice(&request[header_end + 4..]).unwrap();
            assert_eq!(request.operation, "tool.list");
            assert_eq!(request.params["task_id"], task_id.to_string());

            let body = serde_json::to_string(&ApiResponse::ok(
                vec![RuntimeTool {
                    id: tool_id,
                    session_id: None,
                    turn_id: None,
                    task_id,
                    agent_id,
                    name: "read_file".to_owned(),
                    status: willdeep_runtime_protocol::ToolStatus::Completed,
                    started_at_ms: 10,
                    completed_at_ms: Some(20),
                }],
                "test",
                Some(request.request_id),
            ))
            .unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = RuntimeClient::new_unix_socket(&socket, "secret").unwrap();
        let response = client
            .tools(&ListToolsParams {
                task_id: Some(task_id),
                ..ListToolsParams::default()
            })
            .await
            .unwrap();
        assert!(matches!(response, ApiResponse::Ok { data, .. } if data[0].id == tool_id));
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typed_tool_get_decodes_a_direct_object_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = std::path::Path::new("/private/tmp")
            .join(format!("willdeep-runtime-client-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let tool_id = uuid::Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 2048];
            loop {
                let length = stream.read(&mut chunk).await.unwrap();
                assert_ne!(length, 0, "request ended before its JSON body arrived");
                request.extend_from_slice(&chunk[..length]);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let body_start = request
                .windows(4)
                .position(|value| value == b"\r\n\r\n")
                .unwrap()
                + 4;
            let request: ApiRequest = serde_json::from_slice(&request[body_start..]).unwrap();
            assert_eq!(request.operation, "tool.get");
            assert_eq!(request.params["id"], tool_id.to_string());
            let body = serde_json::to_string(&ApiResponse::ok(
                RuntimeTool {
                    id: tool_id,
                    session_id: None,
                    turn_id: None,
                    task_id: uuid::Uuid::nil(),
                    agent_id: uuid::Uuid::nil(),
                    name: "read_file".to_owned(),
                    status: willdeep_runtime_protocol::ToolStatus::Completed,
                    started_at_ms: 10,
                    completed_at_ms: Some(20),
                },
                "test",
                Some(request.request_id),
            ))
            .unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = RuntimeClient::new_unix_socket(&socket, "secret").unwrap();
        let response = client.tool(tool_id).await.unwrap();
        assert!(matches!(response, ApiResponse::Ok { data, .. } if data.id == tool_id));
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_loopback_runtime_endpoints() {
        assert!(matches!(
            RuntimeClient::new("https://example.com", "secret"),
            Err(ClientError::UnsafeEndpoint)
        ));
        assert!(RuntimeClient::new("http://127.0.0.1:9345", "secret").is_ok());
        assert!(RuntimeClient::new("http://[::1]:9345", "secret").is_ok());
        #[cfg(unix)]
        assert!(RuntimeClient::new_unix_socket("/tmp/willdeep.sock", "secret").is_ok());
    }

    #[tokio::test]
    async fn ndjson_decoder_handles_split_utf8_and_multiple_envelopes() {
        let first = ApiResponse::ok(
            willdeep_runtime_protocol::RuntimeEvent {
                sequence: 7,
                timestamp: 1,
                kind: "模型.事件".to_owned(),
                message: "中文消息".to_owned(),
            },
            "test",
            None,
        );
        let second = ApiResponse::ok(
            willdeep_runtime_protocol::RuntimeEvent {
                sequence: 8,
                timestamp: 2,
                kind: "task.completed".to_owned(),
                message: "done".to_owned(),
            },
            "test",
            None,
        );
        let payload = format!(
            "{}\n{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        let split = payload.find("中文").unwrap() + 1;
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, reqwest::Error>(Bytes::copy_from_slice(&payload.as_bytes()[..split])),
            Ok::<_, reqwest::Error>(Bytes::copy_from_slice(&payload.as_bytes()[split..])),
        ]);
        let mut stream = NdjsonEventStream {
            chunks: Box::pin(chunks),
            buffer: Vec::new(),
            max_line_bytes: DEFAULT_MAX_NDJSON_LINE_BYTES,
        };
        let first: ApiResponse<willdeep_runtime_protocol::RuntimeEvent> =
            stream.next().await.unwrap().unwrap();
        let second: ApiResponse<willdeep_runtime_protocol::RuntimeEvent> =
            stream.next().await.unwrap().unwrap();
        assert!(matches!(first, ApiResponse::Ok { data, .. } if data.sequence == 7));
        assert!(matches!(second, ApiResponse::Ok { data, .. } if data.sequence == 8));
        assert!(
            stream
                .next::<willdeep_runtime_protocol::RuntimeEvent>()
                .await
                .unwrap()
                .is_none()
        );
    }
}
