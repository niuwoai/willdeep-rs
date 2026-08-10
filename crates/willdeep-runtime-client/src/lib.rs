use std::pin::Pin;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use willdeep_runtime_protocol::{ApiRequest, ApiResponse, RuntimeCapabilities};

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

    #[test]
    fn rejects_non_loopback_runtime_endpoints() {
        assert!(matches!(
            RuntimeClient::new("https://example.com", "secret"),
            Err(ClientError::UnsafeEndpoint)
        ));
        assert!(RuntimeClient::new("http://127.0.0.1:9345", "secret").is_ok());
        assert!(RuntimeClient::new("http://[::1]:9345", "secret").is_ok());
    }
}
