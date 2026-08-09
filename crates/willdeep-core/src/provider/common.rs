use reqwest::{Client, RequestBuilder, StatusCode, Url};

use super::{ProviderConfig, ProviderError, ProviderKind};

const ERROR_BODY_LIMIT: usize = 8 * 1024;

pub fn client(config: &ProviderConfig) -> Result<Client, ProviderError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(config.request_timeout_secs))
        .user_agent(format!("willdeep/{}", crate::VERSION))
        .build()
        .map_err(|error| ProviderError::Client(error.to_string()))
}

pub fn endpoint(base_url: &str, suffix: &str) -> Result<Url, ProviderError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    if trimmed.to_ascii_lowercase().ends_with(suffix) {
        return Url::parse(trimmed)
            .map_err(|error| ProviderError::InvalidBaseUrl(error.to_string()));
    }
    Url::parse(&format!("{trimmed}/{suffix}"))
        .map_err(|error| ProviderError::InvalidBaseUrl(error.to_string()))
}

pub fn anthropic_endpoint(base_url: &str) -> Result<Url, ProviderError> {
    let mut trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed = trimmed.trim_end_matches("/v1");
    }
    endpoint(trimmed, "v1/messages")
}

pub fn openai_auth(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    let request = request.bearer_auth(config.api_key.trim());
    apply_some_im_headers(request, config)
}

pub fn anthropic_auth(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    let request = if config.kind == ProviderKind::SomeIm {
        request.bearer_auth(config.api_key.trim())
    } else {
        request.header("x-api-key", config.api_key.trim())
    };
    let request = request.header("anthropic-version", "2023-06-01");
    apply_some_im_headers(request, config)
}

fn apply_some_im_headers(request: RequestBuilder, config: &ProviderConfig) -> RequestBuilder {
    if config.kind != ProviderKind::SomeIm {
        return request;
    }
    request
        .header("x-willdeep-session-id", &config.session_id)
        .header("x-willdeep-workspace-id", &config.workspace_id)
}

pub async fn decode_success(
    response: reqwest::Response,
    config: &ProviderConfig,
) -> Result<Vec<u8>, ProviderError> {
    let status = response.status();
    let bytes = response.bytes().await?.to_vec();
    if status.is_success() {
        return Ok(bytes);
    }
    Err(ProviderError::Http {
        status,
        body: safe_error_body(status, &bytes, Some(config.api_key.trim())),
    })
}

fn safe_error_body(status: StatusCode, bytes: &[u8], api_key: Option<&str>) -> String {
    let length = bytes.len().min(ERROR_BODY_LIMIT);
    let mut body = String::from_utf8_lossy(&bytes[..length]).into_owned();
    for marker in ["Bearer ", "sk-", "api_key\":\""] {
        body = body.replace(marker, "[REDACTED]");
    }
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        body = body.replace(api_key, "[REDACTED]");
    }
    if bytes.len() > length {
        body.push_str(" [truncated]");
    }
    if body.trim().is_empty() {
        format!("{status} with empty response body")
    } else {
        body
    }
}
