mod anthropic;
mod chat_completions;
mod common;
mod responses;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Url;
use uuid::Uuid;

use crate::types::{Completion, Message, ToolDefinition};

pub use anthropic::AnthropicMessagesProvider;
pub use chat_completions::ChatCompletionsProvider;
pub use responses::ResponsesProvider;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiDialect {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompatible,
    SomeIm,
    Anthropic,
}

impl ProviderKind {
    pub fn infer(base_url: &str) -> Self {
        let host = Url::parse(base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
        match host.as_deref() {
            Some("some.im") | Some("api.some.im") | Some("api.niuwoai.com") => Self::SomeIm,
            Some("api.anthropic.com") => Self::Anthropic,
            _ => Self::OpenAiCompatible,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub dialect: ApiDialect,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub session_id: String,
    pub workspace_id: String,
    pub request_timeout_secs: u64,
    pub max_output_tokens: u32,
}

impl ProviderConfig {
    pub fn new(
        kind: ProviderKind,
        dialect: ApiDialect,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            dialect,
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            session_id: Uuid::new_v4().to_string(),
            workspace_id: Uuid::new_v4().to_string(),
            request_timeout_secs: 600,
            max_output_tokens: 16_384,
        }
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        let parsed = Url::parse(self.base_url.trim())
            .map_err(|error| ProviderError::InvalidBaseUrl(error.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ProviderError::InvalidBaseUrl(
                "scheme must be http or https".to_owned(),
            ));
        }
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::MissingApiKey);
        }
        if self.model.trim().is_empty() {
            return Err(ProviderError::MissingModel);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("API key is required")]
    MissingApiKey,
    #[error("model is required")]
    MissingModel,
    #[error("invalid API base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("failed to build HTTP client: {0}")]
    Client(String),
    #[error("provider request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("provider returned HTTP {status}: {body}")]
    Http {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),
    #[error("provider response did not include a completion")]
    EmptyResponse,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn with_model(&self, _model: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        Err(ProviderError::InvalidResponse(
            "provider does not support model reconfiguration".to_owned(),
        ))
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Completion, ProviderError>;
}

pub fn build_provider(config: ProviderConfig) -> Result<Arc<dyn Provider>, ProviderError> {
    config.validate()?;
    match config.dialect {
        ApiDialect::ChatCompletions => Ok(Arc::new(ChatCompletionsProvider::new(config)?)),
        ApiDialect::Responses => Ok(Arc::new(ResponsesProvider::new(config)?)),
        ApiDialect::AnthropicMessages => Ok(Arc::new(AnthropicMessagesProvider::new(config)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kind_inference_is_host_exact() {
        assert_eq!(
            ProviderKind::infer("https://some.im/v1"),
            ProviderKind::SomeIm
        );
        assert_eq!(
            ProviderKind::infer("https://api.niuwoai.com/v1"),
            ProviderKind::SomeIm
        );
        assert_eq!(
            ProviderKind::infer("https://api.anthropic.com"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::infer("https://some.im.example/v1"),
            ProviderKind::OpenAiCompatible
        );
    }
}
