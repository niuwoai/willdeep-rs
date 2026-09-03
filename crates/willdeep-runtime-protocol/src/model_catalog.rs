use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

pub const MODEL_CATALOG_SCHEMA_VERSION: &str = "willdeep.model-catalog.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalog {
    pub schema_version: String,
    pub revision: String,
    pub updated_at: String,
    pub credential_store: CredentialStore,
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
    pub routing: CatalogRouting,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl ModelCatalog {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ModelCatalogError> {
        let catalog = serde_json::from_slice::<Self>(bytes)
            .map_err(|error| ModelCatalogError::new(format!("decode model catalog: {error}")))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), ModelCatalogError> {
        if self.schema_version != MODEL_CATALOG_SCHEMA_VERSION {
            return Err(ModelCatalogError::new(format!(
                "unsupported model catalog schema: {}",
                self.schema_version
            )));
        }
        require_text("revision", &self.revision, 128)?;
        require_text("updated_at", &self.updated_at, 128)?;
        if self.credential_store.namespace != "willdeep.providers" {
            return Err(ModelCatalogError::new(
                "credential_store.namespace must be willdeep.providers",
            ));
        }
        if self.credential_store.allow_inline_secrets {
            return Err(ModelCatalogError::new(
                "inline secrets are forbidden in the shared model catalog",
            ));
        }
        reject_secret_fields(&serde_json::to_value(self).map_err(|error| {
            ModelCatalogError::new(format!("inspect model catalog for secrets: {error}"))
        })?)?;

        let provider_ids = unique_ids(
            "provider",
            self.providers.iter().map(|provider| provider.id.as_str()),
        )?;
        for provider in &self.providers {
            validate_provider(provider)?;
        }

        let model_ids = unique_ids("model", self.models.iter().map(|model| model.id.as_str()))?;
        let mut provider_models = BTreeSet::new();
        for model in &self.models {
            validate_model(model)?;
            if !provider_ids.contains(model.provider_id.as_str()) {
                return Err(ModelCatalogError::new(format!(
                    "model {} references unknown provider {}",
                    model.id, model.provider_id
                )));
            }
            let identity = (model.provider_id.as_str(), model.model.as_str());
            if !provider_models.insert(identity) {
                return Err(ModelCatalogError::new(format!(
                    "duplicate provider/model identity: {}/{}",
                    model.provider_id, model.model
                )));
            }
        }

        let policy_ids = unique_ids(
            "routing policy",
            self.routing
                .policies
                .iter()
                .map(|policy| policy.id.as_str()),
        )?;
        if !policy_ids.contains(self.routing.active_policy.as_str()) {
            return Err(ModelCatalogError::new(format!(
                "active routing policy does not exist: {}",
                self.routing.active_policy
            )));
        }
        for policy in &self.routing.policies {
            validate_policy(policy)?;
        }

        let profile_ids = unique_ids(
            "routing profile",
            self.routing
                .profiles
                .iter()
                .map(|profile| profile.id.as_str()),
        )?;
        let profiles = self
            .routing
            .profiles
            .iter()
            .map(|profile| (profile.id.as_str(), profile))
            .collect::<BTreeMap<_, _>>();
        for profile in &self.routing.profiles {
            validate_profile(profile)?;
            for candidate in &profile.candidate_model_ids {
                if !model_ids.contains(candidate.as_str()) {
                    return Err(ModelCatalogError::new(format!(
                        "routing profile {} references unknown model {}",
                        profile.id, candidate
                    )));
                }
            }
            if let Some(next) = profile.escalates_to.as_deref()
                && !profile_ids.contains(next)
            {
                return Err(ModelCatalogError::new(format!(
                    "routing profile {} escalates to unknown profile {next}",
                    profile.id
                )));
            }
        }
        validate_escalation_graph(&profiles)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CredentialStore {
    pub namespace: String,
    pub allow_inline_secrets: bool,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogProvider {
    pub id: String,
    pub display_name: String,
    pub provider_kind: String,
    pub api_dialect: CatalogApiDialect,
    pub base_url: String,
    pub network_scope: NetworkScope,
    pub credential_ref: Option<String>,
    pub credential_env: Option<String>,
    pub supports_model_discovery: bool,
    pub enabled: bool,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CatalogApiDialect {
    #[serde(rename = "chat-completions")]
    ChatCompletions,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "gemini-generate-content")]
    GeminiGenerateContent,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum NetworkScope {
    Local,
    Lan,
    PrivateCloud,
    Public,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub id: String,
    pub provider_id: String,
    pub model: String,
    pub display_name: String,
    pub kind: CatalogModelKind,
    pub billing_model_id: Option<String>,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub capabilities: Vec<ModelCapability>,
    pub input_modalities: Vec<ModelModality>,
    pub output_modalities: Vec<ModelModality>,
    pub tags: Vec<String>,
    pub pricing: Option<ModelPricing>,
    pub metadata_source: ModelMetadataSource,
    pub observed_at: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogModelKind {
    Physical,
    Virtual,
    LocalAlias,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Text,
    ToolCalling,
    StructuredOutput,
    Coding,
    Reasoning,
    Vision,
    NativeWeb,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelModality {
    Text,
    Image,
    Audio,
    Video,
    File,
    Embedding,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMetadataSource {
    Provider,
    User,
    Managed,
    Bundled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPricing {
    pub currency: String,
    pub input_per_1m: Option<String>,
    pub output_per_1m: Option<String>,
    pub cache_read_per_1m: Option<String>,
    pub cache_write_per_1m: Option<String>,
    pub per_request: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogRouting {
    pub active_policy: String,
    pub policies: Vec<RoutingPolicyEntry>,
    pub profiles: Vec<RoutingProfileEntry>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyEntry {
    pub id: String,
    pub allowed_network_scopes: Vec<NetworkScope>,
    pub quality_floor: f64,
    pub weights: RoutingWeights,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoutingWeights {
    pub cost: f64,
    pub latency: f64,
    pub public_exposure: f64,
    pub failure_risk: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoutingProfileEntry {
    pub id: String,
    pub required_capabilities: Vec<ModelCapability>,
    pub candidate_model_ids: Vec<String>,
    pub context_utilization_limit: f64,
    pub max_same_tier_retries: u8,
    pub escalates_to: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCatalogError {
    message: String,
}

impl ModelCatalogError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ModelCatalogError {}

fn validate_provider(provider: &CatalogProvider) -> Result<(), ModelCatalogError> {
    require_identifier("provider id", &provider.id)?;
    require_text("provider display_name", &provider.display_name, 128)?;
    require_text("provider_kind", &provider.provider_kind, 64)?;
    let base_url = provider.base_url.parse::<http::Uri>().map_err(|error| {
        ModelCatalogError::new(format!(
            "provider {} has invalid base_url: {error}",
            provider.id
        ))
    })?;
    if !matches!(base_url.scheme_str(), Some("http" | "https"))
        || base_url.authority().is_none()
        || base_url
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return Err(ModelCatalogError::new(format!(
            "provider {} base_url must be an http(s) URI with an authority and no user info",
            provider.id
        )));
    }
    if let Some(reference) = provider.credential_ref.as_deref()
        && !valid_credential_reference(reference)
    {
        return Err(ModelCatalogError::new(format!(
            "provider {} has invalid credential_ref",
            provider.id
        )));
    }
    if let Some(variable) = provider.credential_env.as_deref()
        && !valid_environment_name(variable)
    {
        return Err(ModelCatalogError::new(format!(
            "provider {} has invalid credential_env",
            provider.id
        )));
    }
    Ok(())
}

fn reject_secret_fields(value: &serde_json::Value) -> Result<(), ModelCatalogError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                let normalized = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                if matches!(
                    normalized.as_str(),
                    "apikey"
                        | "accesstoken"
                        | "refreshtoken"
                        | "bearertoken"
                        | "clientsecret"
                        | "credentialvalue"
                        | "password"
                        | "secret"
                        | "token"
                ) {
                    return Err(ModelCatalogError::new(format!(
                        "secret-bearing field is forbidden in the shared model catalog: {key}"
                    )));
                }
                reject_secret_fields(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                reject_secret_fields(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_model(model: &CatalogModel) -> Result<(), ModelCatalogError> {
    require_identifier("model id", &model.id)?;
    require_identifier("model provider_id", &model.provider_id)?;
    require_text("model request id", &model.model, 256)?;
    if model.model.chars().any(char::is_whitespace) {
        return Err(ModelCatalogError::new(format!(
            "model {} request id cannot contain whitespace",
            model.id
        )));
    }
    require_text("model display_name", &model.display_name, 128)?;
    if let Some(id) = model.billing_model_id.as_deref() {
        require_identifier("billing_model_id", id)?;
    }
    if model
        .context_window_tokens
        .is_some_and(|value| !(4_000..=10_000_000).contains(&value))
    {
        return Err(ModelCatalogError::new(format!(
            "model {} context_window_tokens must be between 4000 and 10000000",
            model.id
        )));
    }
    if model
        .max_output_tokens
        .is_some_and(|value| !(1..=1_000_000).contains(&value))
    {
        return Err(ModelCatalogError::new(format!(
            "model {} max_output_tokens must be between 1 and 1000000",
            model.id
        )));
    }
    unique_values("model capability", &model.id, &model.capabilities)?;
    unique_values("input modality", &model.id, &model.input_modalities)?;
    unique_values("output modality", &model.id, &model.output_modalities)?;
    unique_strings("model tag", &model.id, &model.tags)?;
    for tag in &model.tags {
        require_identifier("model tag", tag)?;
    }
    if let Some(pricing) = &model.pricing {
        validate_pricing(&model.id, pricing)?;
    }
    if let Some(observed_at) = model.observed_at.as_deref() {
        require_text("observed_at", observed_at, 128)?;
    }
    Ok(())
}

fn validate_pricing(model_id: &str, pricing: &ModelPricing) -> Result<(), ModelCatalogError> {
    if pricing.currency.len() != 3
        || !pricing
            .currency
            .chars()
            .all(|value| value.is_ascii_uppercase())
    {
        return Err(ModelCatalogError::new(format!(
            "model {model_id} pricing currency must be a three-letter uppercase code"
        )));
    }
    for (name, value) in [
        ("input_per_1m", pricing.input_per_1m.as_deref()),
        ("output_per_1m", pricing.output_per_1m.as_deref()),
        ("cache_read_per_1m", pricing.cache_read_per_1m.as_deref()),
        ("cache_write_per_1m", pricing.cache_write_per_1m.as_deref()),
        ("per_request", pricing.per_request.as_deref()),
    ] {
        if value.is_some_and(|value| !valid_decimal(value)) {
            return Err(ModelCatalogError::new(format!(
                "model {model_id} pricing {name} must be a non-negative decimal string"
            )));
        }
    }
    Ok(())
}

fn validate_policy(policy: &RoutingPolicyEntry) -> Result<(), ModelCatalogError> {
    require_identifier("routing policy id", &policy.id)?;
    unique_values(
        "allowed network scope",
        &policy.id,
        &policy.allowed_network_scopes,
    )?;
    if policy.allowed_network_scopes.is_empty() {
        return Err(ModelCatalogError::new(format!(
            "routing policy {} must allow at least one network scope",
            policy.id
        )));
    }
    if !policy.quality_floor.is_finite() || !(0.0..=1.0).contains(&policy.quality_floor) {
        return Err(ModelCatalogError::new(format!(
            "routing policy {} quality_floor must be between 0 and 1",
            policy.id
        )));
    }
    for (name, value) in [
        ("cost", policy.weights.cost),
        ("latency", policy.weights.latency),
        ("public_exposure", policy.weights.public_exposure),
        ("failure_risk", policy.weights.failure_risk),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(ModelCatalogError::new(format!(
                "routing policy {} weight {name} must be finite and non-negative",
                policy.id
            )));
        }
    }
    Ok(())
}

fn validate_profile(profile: &RoutingProfileEntry) -> Result<(), ModelCatalogError> {
    require_identifier("routing profile id", &profile.id)?;
    unique_values(
        "required capability",
        &profile.id,
        &profile.required_capabilities,
    )?;
    unique_strings("candidate model", &profile.id, &profile.candidate_model_ids)?;
    if profile.candidate_model_ids.is_empty() {
        return Err(ModelCatalogError::new(format!(
            "routing profile {} must have at least one candidate model",
            profile.id
        )));
    }
    if !profile.context_utilization_limit.is_finite()
        || profile.context_utilization_limit <= 0.0
        || profile.context_utilization_limit > 0.9
    {
        return Err(ModelCatalogError::new(format!(
            "routing profile {} context_utilization_limit must be greater than 0 and at most 0.9",
            profile.id
        )));
    }
    if profile.max_same_tier_retries > 3 {
        return Err(ModelCatalogError::new(format!(
            "routing profile {} max_same_tier_retries cannot exceed 3",
            profile.id
        )));
    }
    Ok(())
}

fn validate_escalation_graph(
    profiles: &BTreeMap<&str, &RoutingProfileEntry>,
) -> Result<(), ModelCatalogError> {
    for start in profiles.keys() {
        let mut seen = BTreeSet::new();
        let mut current = Some(*start);
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(ModelCatalogError::new(format!(
                    "routing escalation cycle detected from profile {start}"
                )));
            }
            current = profiles
                .get(id)
                .and_then(|profile| profile.escalates_to.as_deref());
        }
    }
    Ok(())
}

fn unique_ids<'a>(
    label: &str,
    values: impl Iterator<Item = &'a str>,
) -> Result<BTreeSet<&'a str>, ModelCatalogError> {
    let mut unique = BTreeSet::new();
    for value in values {
        require_identifier(label, value)?;
        if !unique.insert(value) {
            return Err(ModelCatalogError::new(format!(
                "duplicate {label} id: {value}"
            )));
        }
    }
    Ok(unique)
}

fn unique_values<T: Ord>(label: &str, owner: &str, values: &[T]) -> Result<(), ModelCatalogError> {
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(ModelCatalogError::new(format!(
            "{owner} contains duplicate {label} values"
        )));
    }
    Ok(())
}

fn unique_strings(label: &str, owner: &str, values: &[String]) -> Result<(), ModelCatalogError> {
    let unique = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(ModelCatalogError::new(format!(
            "{owner} contains duplicate {label} values"
        )));
    }
    Ok(())
}

fn require_identifier(label: &str, value: &str) -> Result<(), ModelCatalogError> {
    require_text(label, value, 256)?;
    let mut characters = value.chars();
    if !characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        || !characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '/' | '-')
        })
    {
        return Err(ModelCatalogError::new(format!(
            "{label} contains unsupported characters: {value}"
        )));
    }
    Ok(())
}

fn require_text(label: &str, value: &str, maximum: usize) -> Result<(), ModelCatalogError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(|character| character.is_control())
    {
        return Err(ModelCatalogError::new(format!(
            "{label} must contain 1 to {maximum} bytes without control characters"
        )));
    }
    Ok(())
}

fn valid_environment_name(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|value| value == '_' || value.is_ascii_uppercase())
        && value.len() <= 128
        && characters
            .all(|value| value == '_' || value.is_ascii_uppercase() || value.is_ascii_digit())
}

fn valid_credential_reference(value: &str) -> bool {
    let Some(identifier) = value.strip_prefix("credential:") else {
        return false;
    };
    let mut characters = identifier.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && identifier.len() <= 256
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '/' | '-')
        })
}

fn valid_decimal(value: &str) -> bool {
    if value.is_empty() || !value.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    let mut dot_seen = false;
    let mut fraction_digits = 0_usize;
    for character in value.chars() {
        if character == '.' && !dot_seen {
            dot_seen = true;
            continue;
        }
        if !character.is_ascii_digit() {
            return false;
        }
        if dot_seen {
            fraction_digits = fraction_digits.saturating_add(1);
        }
    }
    (!dot_seen || fraction_digits > 0)
        && (value == "0" || !value.starts_with('0') || value.starts_with("0."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> ModelCatalog {
        ModelCatalog::from_json(include_bytes!(
            "../../../docs/examples/model-catalog.v1.json"
        ))
        .expect("decode shared model catalog example")
    }

    #[test]
    fn shared_example_is_valid_and_contains_no_secret_slot() {
        let catalog = example();
        assert_eq!(catalog.providers.len(), 2);
        assert_eq!(catalog.models.len(), 3);
        assert_eq!(catalog.routing.active_policy, "balanced");

        let encoded = serde_json::to_value(catalog).unwrap();
        fn assert_no_secret_key(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    for (key, value) in object {
                        assert!(!matches!(
                            key.as_str(),
                            "api_key" | "token" | "secret" | "password"
                        ));
                        assert_no_secret_key(value);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        assert_no_secret_key(value);
                    }
                }
                _ => {}
            }
        }
        assert_no_secret_key(&encoded);
    }

    #[test]
    fn dangling_candidates_and_escalation_cycles_are_rejected() {
        let mut dangling = example();
        dangling.routing.profiles[0].candidate_model_ids = vec!["missing".to_owned()];
        assert!(
            dangling
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unknown model")
        );

        let mut cycle = example();
        let deep = cycle
            .routing
            .profiles
            .iter_mut()
            .find(|profile| profile.id == "deep")
            .unwrap();
        deep.escalates_to = Some("scout".to_owned());
        assert!(cycle.validate().unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn duplicate_provider_model_identity_is_rejected() {
        let mut catalog = example();
        let mut duplicate = catalog.models[0].clone();
        duplicate.id = "different-catalog-id".to_owned();
        catalog.models.push(duplicate);
        assert!(
            catalog
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate provider/model")
        );
    }

    #[test]
    fn inline_secrets_and_unknown_v1_fields_fail_closed() {
        let mut inline = example();
        inline.credential_store.allow_inline_secrets = true;
        assert!(
            inline
                .validate()
                .unwrap_err()
                .to_string()
                .contains("forbidden")
        );

        let mut value = serde_json::to_value(example()).unwrap();
        value["providers"][0]["api_key"] = serde_json::json!("must-not-decode");
        assert!(serde_json::from_value::<ModelCatalog>(value).is_err());

        let mut extension_secret = example();
        extension_secret.extensions.insert(
            "access-token".to_owned(),
            serde_json::json!("must-not-validate"),
        );
        assert!(
            extension_secret
                .validate()
                .unwrap_err()
                .to_string()
                .contains("secret-bearing field")
        );
    }

    #[test]
    fn credential_environment_name_is_strict() {
        let mut catalog = example();
        catalog.providers[1].credential_env = Some("SomeIm-Key".to_owned());
        assert!(
            catalog
                .validate()
                .unwrap_err()
                .to_string()
                .contains("credential_env")
        );

        let mut invalid_reference = example();
        invalid_reference.providers[1].credential_ref = Some("credential:".to_owned());
        assert!(invalid_reference.validate().is_err());
    }

    #[test]
    fn provider_url_and_decimal_price_match_the_schema_contract() {
        let mut user_info = example();
        user_info.providers[1].base_url = "https://user:password@some.im/v1".to_owned();
        assert!(user_info.validate().is_err());

        let mut leading_decimal_point = example();
        leading_decimal_point.models[0].pricing = Some(ModelPricing {
            currency: "USD".to_owned(),
            input_per_1m: Some(".5".to_owned()),
            output_per_1m: None,
            cache_read_per_1m: None,
            cache_write_per_1m: None,
            per_request: None,
            extensions: BTreeMap::new(),
        });
        assert!(leading_decimal_point.validate().is_err());
    }
}
