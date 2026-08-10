use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use willdeep_runtime_protocol::{
    ApiRequest, ApiResponse, IdParams, ListArtifactsParams, ListToolsParams, RuntimeArtifact,
    RuntimeCapabilities, RuntimeTool,
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

    pub async fn tools(
        &self,
        params: &ListToolsParams,
    ) -> Result<ApiResponse<Vec<RuntimeTool>>, ClientError> {
        self.call("tool.list", params, None).await
    }

    pub async fn tool(
        &self,
        id: uuid::Uuid,
    ) -> Result<ApiResponse<Option<RuntimeTool>>, ClientError> {
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
    ) -> Result<ApiResponse<Option<RuntimeArtifact>>, ClientError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
