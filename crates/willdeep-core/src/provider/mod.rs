mod anthropic;
mod chat_completions;
mod common;
mod responses;

use std::net::IpAddr;
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
    /// Explicitly allow an OpenAI-compatible endpoint to run without an API
    /// key. This is set only for the user-configured auxiliary model; normal
    /// Provider profiles continue to fail closed when credentials are absent.
    pub allow_unauthenticated: bool,
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
            allow_unauthenticated: false,
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
        if self.api_key.trim().is_empty()
            && !(self.kind == ProviderKind::OpenAiCompatible && self.allow_unauthenticated)
        {
            return Err(ProviderError::MissingApiKey);
        }
        if self.model.trim().is_empty() {
            return Err(ProviderError::MissingModel);
        }
        Ok(())
    }
}

/// Detect the conventional same-machine endpoints. Explicit auxiliary
/// endpoints may also live on another LAN host or domain; callers mark those
/// with `allow_unauthenticated` instead of pretending every remote Provider
/// profile is safe without credentials.
pub fn is_loopback_base_url(url: &Url) -> bool {
    match url.host_str().map(str::to_ascii_lowercase).as_deref() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
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

/// Return the model identifiers exposed by an OpenAI-compatible `/models`
/// endpoint. Providers in the wild use a few equivalent envelopes, so the
/// parser accepts `data`, `models`, a root array, and string/object entries.
pub async fn list_models(config: &ProviderConfig) -> Result<Vec<String>, ProviderError> {
    let endpoint = if config.kind == ProviderKind::Anthropic {
        let base_url = config
            .base_url
            .trim()
            .trim_end_matches('/')
            .trim_end_matches("/v1");
        common::endpoint(base_url, "v1/models")?
    } else {
        common::endpoint(&config.base_url, "models")?
    };
    let request = common::client(config)?.get(endpoint);
    let request = if config.kind == ProviderKind::Anthropic {
        common::anthropic_auth(request, config)
    } else {
        common::openai_auth(request, config)
    };
    let bytes = common::send_retrying(request, config).await?;
    parse_model_list(&bytes)
}

fn parse_model_list(bytes: &[u8]) -> Result<Vec<String>, ProviderError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    let entries = value
        .as_array()
        .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .ok_or_else(|| {
            ProviderError::InvalidResponse(
                "model list must be an array or contain a data/models array".to_owned(),
            )
        })?;
    let mut models = entries
        .iter()
        .filter_map(|entry| {
            entry.as_str().or_else(|| {
                ["id", "model", "name"]
                    .into_iter()
                    .find_map(|key| entry.get(key).and_then(serde_json::Value::as_str))
            })
        })
        .map(str::trim)
        .filter(|model| {
            !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort_by_key(|model| model.to_ascii_lowercase());
    models.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    Ok(models)
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

    #[test]
    fn credential_free_openai_requires_explicit_auxiliary_opt_in() {
        let mut auxiliary = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            ApiDialect::ChatCompletions,
            "https://models.home.example/v1",
            "",
            "gemma4:e4b-it-qat",
        );
        auxiliary.allow_unauthenticated = true;
        assert!(auxiliary.validate().is_ok());

        let ordinary_provider = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            ApiDialect::ChatCompletions,
            "https://provider.example/v1",
            "",
            "model",
        );
        assert!(matches!(
            ordinary_provider.validate(),
            Err(ProviderError::MissingApiKey)
        ));
    }

    #[test]
    fn model_list_accepts_common_envelopes_and_deduplicates_ids() {
        let models = parse_model_list(
            br#"{"data":[{"id":"qwen3"},{"model":"GPT-5"},"glm-5",{"name":"QWEN3"},"bad\nmodel"]}"#,
        )
        .unwrap();

        assert_eq!(models, ["glm-5", "GPT-5", "qwen3"]);
        assert_eq!(
            parse_model_list(br#"{"models":[{"name":"local-model"}]}"#).unwrap(),
            ["local-model"]
        );
    }
}
