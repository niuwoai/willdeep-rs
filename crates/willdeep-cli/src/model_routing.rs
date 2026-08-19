use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigFile, LoadedConfig};

pub const STALE_CONFIG_MESSAGE: &str = "model routing settings are stale; reload before saving";

const PROFILE_IDS: &[&str] = &[
    "scout",
    "reader",
    "editor",
    "implementer",
    "test_fixer",
    "build_fixer",
    "log_inspector",
    "git_detective",
    "deep",
];

#[derive(Clone, Debug, Serialize)]
pub struct ModelRoutingSettings {
    pub revision: String,
    pub default_provider: String,
    pub active_provider_override: Option<String>,
    pub root_model: String,
    pub small_model_routing: bool,
    pub auto_dispatch_read_only: bool,
    pub max_deep_calls_per_harness: usize,
    pub providers: Vec<ModelProviderOption>,
    pub profiles: Vec<ProfileRoutingSettings>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelProviderOption {
    pub id: String,
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileRoutingSettings {
    pub id: String,
    pub provider_profile: Option<String>,
    pub model: Option<String>,
    pub context_window: u64,
    pub automatic: bool,
    pub effective_provider: String,
    pub effective_model: String,
    pub recommended_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoutingUpdate {
    pub revision: String,
    pub default_provider: String,
    pub root_model: String,
    pub small_model_routing: bool,
    pub auto_dispatch_read_only: bool,
    pub max_deep_calls_per_harness: usize,
    pub profiles: Vec<ProfileRoutingUpdate>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRoutingUpdate {
    pub id: String,
    pub provider_profile: Option<String>,
    pub model: Option<String>,
    pub context_window: u64,
}

impl ModelRoutingSettings {
    pub fn to_update(&self) -> ModelRoutingUpdate {
        ModelRoutingUpdate {
            revision: self.revision.clone(),
            default_provider: self.default_provider.clone(),
            root_model: self.root_model.clone(),
            small_model_routing: self.small_model_routing,
            auto_dispatch_read_only: self.auto_dispatch_read_only,
            max_deep_calls_per_harness: self.max_deep_calls_per_harness,
            profiles: self
                .profiles
                .iter()
                .map(|profile| ProfileRoutingUpdate {
                    id: profile.id.clone(),
                    provider_profile: profile.provider_profile.clone(),
                    model: profile.model.clone(),
                    context_window: profile.context_window,
                })
                .collect(),
        }
    }
}

pub fn load(path: &Path, active_profile: Option<&str>) -> Result<ModelRoutingSettings> {
    if !path.exists() {
        bail!(
            "configuration file does not exist: {}; run `willdeep config init` first",
            path.display()
        );
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read configuration file: {}", path.display()))?;
    let loaded = LoadedConfig::load(Some(path))?;
    from_config(&loaded.file, revision(&contents), active_profile)
}

pub fn save(
    path: &Path,
    active_profile: Option<&str>,
    update: &ModelRoutingUpdate,
) -> Result<ModelRoutingSettings> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read configuration file: {}", path.display()))?;
    if update.revision != revision(&contents) {
        bail!(STALE_CONFIG_MESSAGE);
    }
    let current = LoadedConfig::load(Some(path))?;
    validate_update(&current.file, update)?;

    let mut patched = contents;
    patched = patch_key(
        &patched,
        None,
        "default_provider",
        Some(toml_string(&update.default_provider)),
    );
    patched = patch_key(
        &patched,
        Some(&format!("providers.{}", update.default_provider)),
        "model",
        Some(toml_string(update.root_model.trim())),
    );
    patched = patch_key(
        &patched,
        Some("agent"),
        "small_model_routing",
        Some(update.small_model_routing.to_string()),
    );
    patched = patch_key(
        &patched,
        Some("agent"),
        "auto_dispatch_read_only",
        Some(update.auto_dispatch_read_only.to_string()),
    );
    patched = patch_key(
        &patched,
        Some("agent"),
        "max_deep_calls_per_harness",
        Some(update.max_deep_calls_per_harness.to_string()),
    );
    for profile in &update.profiles {
        let section = format!("subagents.{}", profile.id);
        patched = patch_key(
            &patched,
            Some(&section),
            "provider_profile",
            profile.provider_profile.as_deref().map(toml_string),
        );
        patched = patch_key(
            &patched,
            Some(&section),
            "model",
            profile.model.as_deref().map(str::trim).map(toml_string),
        );
        patched = patch_key(
            &patched,
            Some(&section),
            "context_window",
            Some(profile.context_window.to_string()),
        );
    }

    let parsed: ConfigFile = toml::from_str(&patched)
        .with_context(|| format!("validate updated configuration: {}", path.display()))?;
    crate::config::validate(&parsed, path)?;
    write_private_atomic(path, patched.as_bytes())?;
    load(path, active_profile)
}

fn from_config(
    file: &ConfigFile,
    revision: String,
    active_profile: Option<&str>,
) -> Result<ModelRoutingSettings> {
    let default_provider = file
        .default_provider
        .clone()
        .or_else(|| {
            (file.providers.len() == 1)
                .then(|| file.providers.keys().next().cloned())
                .flatten()
        })
        .context("model routing settings require default_provider when multiple providers exist")?;
    let root = file
        .providers
        .get(&default_provider)
        .with_context(|| format!("provider profile not found in config: {default_provider}"))?;
    let root_model = root.model.clone().unwrap_or_default();
    let providers = file
        .providers
        .iter()
        .map(|(id, provider)| ModelProviderOption {
            id: id.clone(),
            provider: provider
                .provider
                .clone()
                .unwrap_or_else(|| "openai-compatible".to_owned()),
            model: provider.model.clone().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let profiles = PROFILE_IDS
        .iter()
        .map(|id| profile_settings(file, id, &default_provider, &root_model))
        .collect::<Result<Vec<_>>>()?;
    let active_provider_override = active_profile
        .filter(|profile| *profile != default_provider)
        .map(str::to_owned);
    Ok(ModelRoutingSettings {
        revision,
        default_provider,
        active_provider_override,
        root_model,
        small_model_routing: file.agent.small_model_routing.unwrap_or(true),
        auto_dispatch_read_only: file.agent.auto_dispatch_read_only.unwrap_or(true),
        max_deep_calls_per_harness: file.agent.max_deep_calls_per_harness.unwrap_or(1),
        providers,
        profiles,
    })
}

fn profile_settings(
    file: &ConfigFile,
    id: &str,
    root_provider: &str,
    root_model: &str,
) -> Result<ProfileRoutingSettings> {
    let configured = file.subagents.get(id);
    let provider_profile = configured.and_then(|settings| settings.provider_profile.clone());
    let model = configured.and_then(|settings| settings.model.clone());
    let effective_provider = provider_profile
        .clone()
        .unwrap_or_else(|| root_provider.to_owned());
    let provider = file
        .providers
        .get(&effective_provider)
        .with_context(|| format!("provider profile not found in config: {effective_provider}"))?;
    let is_some_im = provider.provider.as_deref() == Some("some-im");
    let automatic = provider_profile.is_none() && model.is_none();
    let recommended_model = (automatic && is_some_im)
        .then(|| willdeep_core::subagent::hosted_worker_model(id))
        .flatten();
    let provider_model = provider.model.as_deref().unwrap_or(root_model);
    let effective_model = model.clone().unwrap_or_else(|| {
        recommended_model.clone().unwrap_or_else(|| {
            if is_some_im && id != "deep" {
                "glm-5".to_owned()
            } else {
                provider_model.to_owned()
            }
        })
    });
    Ok(ProfileRoutingSettings {
        id: id.to_owned(),
        automatic,
        provider_profile,
        model,
        context_window: configured
            .and_then(|settings| settings.context_window)
            .unwrap_or_else(|| default_context_window(id)),
        effective_provider,
        effective_model,
        recommended_model,
    })
}

fn validate_update(file: &ConfigFile, update: &ModelRoutingUpdate) -> Result<()> {
    if !safe_bare_key(&update.default_provider)
        || !file.providers.contains_key(&update.default_provider)
    {
        bail!("unknown or unsupported default provider profile");
    }
    validate_model("root", &update.root_model)?;
    if update.max_deep_calls_per_harness > 16 {
        bail!("max_deep_calls_per_harness must be between 0 and 16");
    }
    let mut seen = HashSet::new();
    for profile in &update.profiles {
        if !PROFILE_IDS.contains(&profile.id.as_str()) || !seen.insert(profile.id.as_str()) {
            bail!("unknown or duplicate subagent profile: {}", profile.id);
        }
        if let Some(provider) = profile.provider_profile.as_deref()
            && (!safe_bare_key(provider) || !file.providers.contains_key(provider))
        {
            bail!("unknown or unsupported provider profile for {}", profile.id);
        }
        if let Some(model) = profile.model.as_deref() {
            validate_model(&profile.id, model)?;
        }
        if !(4_000..=1_000_000).contains(&profile.context_window) {
            bail!(
                "context_window for {} must be between 4000 and 1000000",
                profile.id
            );
        }
    }
    Ok(())
}

fn validate_model(label: &str, model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        bail!("model for {label} must contain 1 to 256 bytes without control characters");
    }
    Ok(())
}

fn safe_bare_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn default_context_window(id: &str) -> u64 {
    match id {
        "scout" | "log_inspector" | "git_detective" => 32_768,
        "reader" | "editor" | "build_fixer" => 49_152,
        "test_fixer" => 65_536,
        "implementer" => 262_144,
        "deep" => 1_000_000,
        _ => 32_768,
    }
}

fn revision(contents: &str) -> String {
    format!("{:08x}", crc32fast::hash(contents.as_bytes()))
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn patch_key(contents: &str, section: Option<&str>, key: &str, value: Option<String>) -> String {
    let trailing_newline = contents.ends_with('\n');
    let mut lines = contents.lines().map(str::to_owned).collect::<Vec<_>>();
    let range = section.and_then(|name| section_range(&lines, name));
    let (start, end) = match (section, range) {
        (None, _) => (0, first_section(&lines)),
        (Some(_), Some(range)) => range,
        (Some(name), None) => {
            let Some(value) = value else {
                return contents.to_owned();
            };
            if !lines.is_empty() && !lines.last().is_some_and(String::is_empty) {
                lines.push(String::new());
            }
            lines.push(format!("[{name}]"));
            lines.push(format!("{key} = {value}"));
            let mut result = lines.join("\n");
            result.push('\n');
            return result;
        }
    };
    let existing = (start..end).find(|index| assignment_is(&lines[*index], key));
    match (existing, value) {
        (Some(index), Some(value)) => {
            let comment = inline_comment(&lines[index])
                .map(|value| format!(" {value}"))
                .unwrap_or_default();
            lines[index] = format!("{key} = {value}{comment}");
        }
        (Some(index), None) => {
            if let Some(comment) = inline_comment(&lines[index]) {
                lines[index] = comment.to_owned();
            } else {
                lines.remove(index);
            }
        }
        (None, Some(value)) => lines.insert(end, format!("{key} = {value}")),
        (None, None) => return contents.to_owned(),
    }
    let mut result = lines.join("\n");
    if trailing_newline || !result.is_empty() {
        result.push('\n');
    }
    result
}

fn first_section(lines: &[String]) -> usize {
    lines
        .iter()
        .position(|line| is_section_header(line))
        .unwrap_or(lines.len())
}

fn section_range(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let header = format!("[{section}]");
    let header_index = lines.iter().position(|line| line.trim() == header)?;
    let start = header_index + 1;
    let end = lines[start..]
        .iter()
        .position(|line| is_section_header(line))
        .map(|offset| start + offset)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn is_section_header(line: &str) -> bool {
    let value = line.trim();
    value.starts_with('[') && value.ends_with(']')
}

fn assignment_is(line: &str, key: &str) -> bool {
    let value = line.trim_start();
    !value.starts_with('#')
        && value
            .split_once('=')
            .is_some_and(|(candidate, _)| candidate.trim() == key)
}

fn inline_comment(line: &str) -> Option<&str> {
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && double_quoted {
            escaped = true;
            continue;
        }
        match character {
            '\'' if !double_quoted => single_quoted = !single_quoted,
            '"' if !single_quoted => double_quoted = !double_quoted,
            '#' if !single_quoted && !double_quoted => return Some(&line[index..]),
            _ => {}
        }
    }
    None
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("configuration path has no parent: {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config.toml");
    let temporary = parent.join(format!(".{name}.routing-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create temporary configuration: {}", temporary.display()))?;
        file.write_all(contents)
            .with_context(|| format!("write temporary configuration: {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary configuration: {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("replace configuration atomically: {}", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        r#"# keep this comment
version = 1
default_provider = "some-im"

[agent]
small_model_routing = true # keep nearby comments

[providers.some-im]
provider = "some-im"
model = "glm-5"
api_key_env = "SOMEIM_API_KEY"

[providers.local]
provider = "openai-compatible"
model = "local-coder"

[subagents.scout]
# preserve worker comments
context_window = 32768

[notifications]
future_desktop_field = "preserved"
"#
        .to_owned()
    }

    #[test]
    fn patches_only_managed_keys_and_preserves_comments() {
        let mut value = sample();
        value = patch_key(
            &value,
            Some("subagents.scout"),
            "model",
            Some(toml_string("custom-scout")),
        );
        value = patch_key(
            &value,
            Some("subagents.scout"),
            "provider_profile",
            Some(toml_string("local")),
        );
        value = patch_key(
            &value,
            Some("agent"),
            "small_model_routing",
            Some(false.to_string()),
        );
        assert!(value.contains("# keep this comment"));
        assert!(value.contains("small_model_routing = false # keep nearby comments"));
        assert!(value.contains("# preserve worker comments"));
        assert!(value.contains("future_desktop_field = \"preserved\""));
        assert!(value.contains("model = \"custom-scout\""));
        assert!(value.contains("provider_profile = \"local\""));
        toml::from_str::<ConfigFile>(&value).expect("patched config");
    }

    #[test]
    fn save_round_trips_explicit_and_recommended_worker_models() {
        let root =
            std::env::temp_dir().join(format!("willdeep-model-routing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp root");
        let path = root.join("config.toml");
        std::fs::write(&path, sample()).expect("sample config");
        let settings = load(&path, None).expect("load settings");
        assert_eq!(settings.profiles[0].effective_model, "someim-32b-scout");
        let mut update = settings.to_update();
        update.default_provider = "local".to_owned();
        update.root_model = "local-coder-v2".to_owned();
        update.max_deep_calls_per_harness = 0;
        let scout = update
            .profiles
            .iter_mut()
            .find(|profile| profile.id == "scout")
            .expect("scout update");
        scout.provider_profile = Some("some-im".to_owned());
        scout.model = Some("custom-scout".to_owned());
        let saved = save(&path, None, &update).expect("save settings");
        assert_eq!(saved.default_provider, "local");
        assert_eq!(saved.root_model, "local-coder-v2");
        assert_eq!(saved.max_deep_calls_per_harness, 0);
        let scout = saved
            .profiles
            .iter()
            .find(|profile| profile.id == "scout")
            .expect("saved scout");
        assert_eq!(scout.model.as_deref(), Some("custom-scout"));
        assert!(
            std::fs::read_to_string(&path)
                .expect("saved config")
                .contains("# preserve worker comments")
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn stale_revision_is_rejected_before_writing() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-model-routing-stale-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root");
        let path = root.join("config.toml");
        std::fs::write(&path, sample()).expect("sample config");
        let settings = load(&path, None).expect("load settings");
        let mut update = settings.to_update();
        update.revision = "stale".to_owned();
        let error = save(&path, None, &update).expect_err("stale update");
        assert!(error.to_string().contains(STALE_CONFIG_MESSAGE));
        assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), sample());
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
