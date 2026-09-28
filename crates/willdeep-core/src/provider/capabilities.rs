//! 网关的模型能力声明（`GET /v1/model-capabilities`，some.im / tokenhub 扩展接口）。
//!
//! 客户端过去只能按模型名硬编码绕坑（哪些模型不能看图、上下文多大）。网关现在按
//! 入站协议给出细粒度能力，并标出哪些经过上游能力探测验证；这里负责拉取、宽松解析
//! 与落盘缓存，具体怎么用由调用方决定。
//!
//! 信任规则：只有 `verified=true` 的协议声明才当作权威结论。未验证的声明多半是网关
//! 的默认值（例如默认声明纯文本输入），拿它当「不支持」会把能用的能力关掉。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ApiDialect, ProviderConfig, ProviderError, ProviderKind, common};

/// 拉取能力表的请求期限。它挡在会话启动的路上，宁可拿不到也不能让用户干等。
const FETCH_TIMEOUT_SECS: u64 = 5;
/// 缓存新鲜期：网关侧线路声明只在运营应用探测结果时变化。
const FRESH_TTL: Duration = Duration::from_secs(10 * 60);
/// 网关不提供该接口（404 等）时的负缓存期，免得每次启动都白打一次。
const UNSUPPORTED_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// 拉取失败时仍可使用的陈旧缓存上限。
const STALE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const CACHE_SCHEMA_VERSION: u32 = 1;

pub const PROTOCOL_CHAT_COMPLETIONS: &str = "chat_completions";
pub const PROTOCOL_RESPONSES: &str = "responses";
pub const PROTOCOL_ANTHROPIC_MESSAGES: &str = "anthropic_messages";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GatewayProtocolCapability {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub routes: u32,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub function_tools: bool,
    #[serde(default)]
    pub tool_choices: Vec<String>,
    #[serde(default)]
    pub strict_json_schema: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub thinking: Option<bool>,
    #[serde(default)]
    pub previous_response_id: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GatewayCapabilityVerification {
    #[serde(default)]
    pub coverage: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub routes: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct GatewayModelCapability {
    pub id: String,
    #[serde(default)]
    pub capability_kinds: Vec<String>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub protocols: BTreeMap<String, GatewayProtocolCapability>,
    #[serde(default)]
    pub capability_verification: Option<GatewayCapabilityVerification>,
}

pub fn protocol_key(dialect: ApiDialect) -> &'static str {
    match dialect {
        ApiDialect::ChatCompletions => PROTOCOL_CHAT_COMPLETIONS,
        ApiDialect::Responses => PROTOCOL_RESPONSES,
        ApiDialect::AnthropicMessages => PROTOCOL_ANTHROPIC_MESSAGES,
    }
}

impl GatewayModelCapability {
    pub fn protocol(&self, dialect: ApiDialect) -> Option<&GatewayProtocolCapability> {
        self.protocols.get(protocol_key(dialect))
    }

    /// 网关对「这个模型走 `dialect` 能不能看图」的权威结论；给不出权威结论时返回 `None`，
    /// 调用方回落到自己的启发式。
    ///
    /// 验证过的协议声明说了算，真假都信；目录模态里明确列了 image 也算能看图。
    /// 未验证声明里的 `image_input=false` 不信——那往往只是默认的纯文本模态。
    pub fn accepts_images(&self, dialect: ApiDialect) -> Option<bool> {
        if let Some(protocol) = self.protocol(dialect).filter(|protocol| protocol.verified) {
            return Some(protocol.image_input);
        }
        self.input_modalities
            .iter()
            .any(|modality| modality.eq_ignore_ascii_case("image"))
            .then_some(true)
    }

    pub fn context_window(&self) -> Option<u64> {
        self.context_window.filter(|window| *window > 0)
    }

    /// 网关没有任何线路能以 `dialect` 服务这个模型时，给出一个能用的协议。
    /// 协议表为空（网关没解析到线路或是旧版网关）时不下结论。
    pub fn fallback_dialect(&self, dialect: ApiDialect) -> Option<ApiDialect> {
        if self.protocols.is_empty() || self.protocol(dialect).is_some() {
            return None;
        }
        [
            ApiDialect::ChatCompletions,
            ApiDialect::Responses,
            ApiDialect::AnthropicMessages,
        ]
        .into_iter()
        .find(|candidate| self.protocol(*candidate).is_some())
    }

    /// 经验证、明确不支持函数工具。Agent 离不开工具，调用方应当提示用户换模型。
    pub fn verified_without_tools(&self, dialect: ApiDialect) -> bool {
        self.protocol(dialect)
            .is_some_and(|protocol| protocol.verified && !protocol.function_tools)
    }
}

pub fn find_model<'a>(
    models: &'a [GatewayModelCapability],
    model: &str,
) -> Option<&'a GatewayModelCapability> {
    let model = model.trim();
    models
        .iter()
        .find(|candidate| candidate.id.trim().eq_ignore_ascii_case(model))
}

/// 拉取网关能力表。Anthropic 官方端点没有这个扩展接口，直接返回空表。
pub async fn fetch_model_capabilities(
    config: &ProviderConfig,
) -> Result<Vec<GatewayModelCapability>, ProviderError> {
    if config.kind == ProviderKind::Anthropic {
        return Ok(Vec::new());
    }
    let mut config = config.clone();
    config.request_timeout_secs = config.request_timeout_secs.min(FETCH_TIMEOUT_SECS);
    let endpoint = common::endpoint(&config.base_url, "model-capabilities")?;
    let request = common::openai_auth(common::client(&config)?.get(endpoint), &config);
    let bytes = common::send_retrying(request, &config).await?;
    parse_model_capabilities(&bytes)
}

/// 宽松解析：单条解析不了就跳过，不因为网关多了个字段或某条数据异常而整表作废。
pub fn parse_model_capabilities(
    bytes: &[u8],
) -> Result<Vec<GatewayModelCapability>, ProviderError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    let entries = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .ok_or_else(|| {
            ProviderError::InvalidResponse(
                "model capabilities must contain a data/models array".to_owned(),
            )
        })?;
    Ok(entries
        .iter()
        .filter_map(|entry| serde_json::from_value::<GatewayModelCapability>(entry.clone()).ok())
        .filter(|entry| !entry.id.trim().is_empty())
        .collect())
}

#[derive(Debug, Deserialize, Serialize)]
struct CacheFile {
    schema_version: u32,
    fetched_at_unix: u64,
    supported: bool,
    #[serde(default)]
    models: Vec<GatewayModelCapability>,
}

/// 能力表的落盘缓存目录：`<home>/cache/model-capabilities/`。
pub fn cache_dir(home: &Path) -> PathBuf {
    home.join("cache").join("model-capabilities")
}

/// 可见范围随 Key 所属租户变化，所以缓存按「端点 + Key」分文件；文件名只含摘要，不落 Key 本身。
fn cache_path(home: &Path, config: &ProviderConfig) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(config.base_url.trim().trim_end_matches('/').as_bytes());
    hasher.update([0]);
    hasher.update(config.api_key.trim().as_bytes());
    let digest = hasher.finalize();
    let name = digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    cache_dir(home).join(format!("{name}.json"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn read_cache(path: &Path) -> Option<CacheFile> {
    let bytes = std::fs::read(path).ok()?;
    let cache: CacheFile = serde_json::from_slice(&bytes).ok()?;
    (cache.schema_version == CACHE_SCHEMA_VERSION).then_some(cache)
}

fn write_cache(path: &Path, cache: &CacheFile) {
    let Some(parent) = path.parent() else {
        return;
    };
    // 缓存写不进去只是下次多拉一次，不影响本次会话，所以失败静默。
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn cache_age(cache: &CacheFile, now: u64) -> Duration {
    Duration::from_secs(now.saturating_sub(cache.fetched_at_unix))
}

/// 取某个模型的能力声明：优先新鲜缓存，否则拉取；拉取失败时用不超过 7 天的陈旧缓存。
/// 网关不提供该接口、或没有这个模型时返回 `None`，调用方沿用原有行为。
pub async fn load_model_capability(
    home: &Path,
    config: &ProviderConfig,
) -> Option<GatewayModelCapability> {
    if config.kind == ProviderKind::Anthropic || config.allow_unauthenticated {
        return None;
    }
    let path = cache_path(home, config);
    let now = now_unix();
    let cached = read_cache(&path);
    if let Some(cache) = cached.as_ref() {
        let ttl = if cache.supported {
            FRESH_TTL
        } else {
            UNSUPPORTED_TTL
        };
        if cache_age(cache, now) < ttl {
            return find_model(&cache.models, &config.model).cloned();
        }
    }
    match fetch_model_capabilities(config).await {
        Ok(models) => {
            let found = find_model(&models, &config.model).cloned();
            write_cache(
                &path,
                &CacheFile {
                    schema_version: CACHE_SCHEMA_VERSION,
                    fetched_at_unix: now,
                    supported: true,
                    models,
                },
            );
            found
        }
        Err(ProviderError::Http { status, .. })
            if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS =>
        {
            write_cache(
                &path,
                &CacheFile {
                    schema_version: CACHE_SCHEMA_VERSION,
                    fetched_at_unix: now,
                    supported: false,
                    models: Vec::new(),
                },
            );
            None
        }
        Err(_) => cached
            .filter(|cache| cache.supported && cache_age(cache, now) < STALE_TTL)
            .and_then(|cache| find_model(&cache.models, &config.model).cloned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "object": "list",
      "data": [
        {
          "id": "qwen3-coder",
          "capability_kinds": ["llm"],
          "input_modalities": ["text"],
          "context_window": 262144,
          "future_field": {"ignored": true},
          "protocols": {
            "chat_completions": {"mode": "native", "routes": 2, "verified": true, "streaming": true,
              "function_tools": true, "tool_choices": ["auto"], "strict_json_schema": false, "image_input": false},
            "responses": {"mode": "bridge", "routes": 2, "verified": false, "streaming": true,
              "function_tools": true, "tool_choices": ["auto"], "strict_json_schema": false, "image_input": false,
              "previous_response_id": false}
          },
          "capability_verification": {"coverage": "partial", "source": "active_probe", "routes": 2}
        },
        {"id": "vision-model", "input_modalities": ["text", "image"]},
        {"id": 42},
        {"id": "responses-only", "protocols": {"responses": {"mode": "native", "verified": true, "function_tools": false}}}
      ]
    }"#;

    fn sample() -> Vec<GatewayModelCapability> {
        parse_model_capabilities(SAMPLE.as_bytes()).expect("parse sample")
    }

    #[test]
    fn parse_skips_malformed_entries_and_ignores_unknown_fields() {
        let models = sample();
        let ids = models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["qwen3-coder", "vision-model", "responses-only"]);
        let chat = models[0].protocol(ApiDialect::ChatCompletions).unwrap();
        assert_eq!(chat.tool_choices, ["auto"]);
        assert_eq!(models[0].context_window(), Some(262_144));
    }

    #[test]
    fn image_verdict_trusts_only_verified_or_declared_image() {
        let models = sample();
        let cases = [
            ("qwen3-coder", ApiDialect::ChatCompletions, Some(false)),
            // 未验证的 image_input=false 可能只是默认模态，不下结论。
            ("qwen3-coder", ApiDialect::Responses, None),
            ("vision-model", ApiDialect::ChatCompletions, Some(true)),
            ("responses-only", ApiDialect::ChatCompletions, None),
        ];
        for (id, dialect, want) in cases {
            let model = find_model(&models, id).unwrap();
            assert_eq!(model.accepts_images(dialect), want, "{id} {dialect:?}");
        }
    }

    #[test]
    fn fallback_dialect_only_when_gateway_cannot_serve_requested_protocol() {
        let models = sample();
        let coder = find_model(&models, "QWEN3-CODER").unwrap();
        assert_eq!(coder.fallback_dialect(ApiDialect::ChatCompletions), None);
        assert_eq!(
            coder.fallback_dialect(ApiDialect::AnthropicMessages),
            Some(ApiDialect::ChatCompletions)
        );
        let only = find_model(&models, "responses-only").unwrap();
        assert_eq!(
            only.fallback_dialect(ApiDialect::ChatCompletions),
            Some(ApiDialect::Responses)
        );
        assert!(only.verified_without_tools(ApiDialect::Responses));
        let vision = find_model(&models, "vision-model").unwrap();
        assert_eq!(vision.fallback_dialect(ApiDialect::ChatCompletions), None);
    }

    #[test]
    fn cache_round_trip_and_key_isolation() {
        let home =
            std::env::temp_dir().join(format!("willdeep-capabilities-{}", uuid::Uuid::new_v4()));
        let mut config = ProviderConfig::new(
            ProviderKind::SomeIm,
            ApiDialect::ChatCompletions,
            "https://some.im/v1",
            "sk-a",
            "qwen3-coder",
        );
        let path = cache_path(&home, &config);
        write_cache(
            &path,
            &CacheFile {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at_unix: now_unix(),
                supported: true,
                models: sample(),
            },
        );
        let cached = read_cache(&path).unwrap();
        assert_eq!(
            find_model(&cached.models, "qwen3-coder").unwrap().id,
            "qwen3-coder"
        );
        config.api_key = "sk-b".to_owned();
        assert_ne!(cache_path(&home, &config), path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains("sk-a"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
