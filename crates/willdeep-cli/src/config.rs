use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use willdeep_core::McpServerConfig;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub version: Option<u32>,
    pub default_provider: Option<String>,
    #[serde(default)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderProfile>,
    #[serde(default)]
    pub subagents: BTreeMap<String, SubagentProfileSettings>,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    #[serde(default)]
    pub skills: SkillSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    pub max_turns: Option<usize>,
    pub approval: Option<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSettings {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub provider: Option<String>,
    pub api: Option<String>,
    pub api_base: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub model: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub context_window: Option<u64>,
    pub vision_model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentProfileSettings {
    pub provider_profile: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<usize>,
    pub context_window: Option<u64>,
}

pub struct LoadedConfig {
    pub file: ConfigFile,
}

impl LoadedConfig {
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let (path, required) = match explicit_path {
            Some(path) => (path.to_path_buf(), true),
            None => (default_config_path()?, false),
        };
        if !path.exists() {
            if required {
                bail!("configuration file does not exist: {}", path.display());
            }
            return Ok(Self {
                file: ConfigFile::default(),
            });
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("read configuration file: {}", path.display()))?;
        let file: ConfigFile = toml::from_str(&contents)
            .with_context(|| format!("parse TOML configuration: {}", path.display()))?;
        validate(&file, &path)?;
        Ok(Self { file })
    }

    pub fn select_provider(&self, requested: Option<&str>) -> Result<Option<&ProviderProfile>> {
        let selected = requested
            .map(str::to_owned)
            .or_else(|| self.file.default_provider.clone())
            .or_else(|| {
                (self.file.providers.len() == 1)
                    .then(|| self.file.providers.keys().next().cloned())
                    .flatten()
            });
        match selected {
            Some(name) => self
                .file
                .providers
                .get(&name)
                .map(Some)
                .with_context(|| format!("provider profile not found in config: {name}")),
            None => Ok(None),
        }
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("WILLDEEP_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("config.toml"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot find home directory; set WILLDEEP_HOME or pass --config")?;
    Ok(PathBuf::from(home).join(".willdeep").join("config.toml"))
}

pub fn willdeep_home() -> Result<PathBuf> {
    Ok(default_config_path()?
        .parent()
        .context("configuration path has no parent")?
        .to_path_buf())
}

fn validate(file: &ConfigFile, path: &Path) -> Result<()> {
    if let Some(version) = file.version
        && version != CONFIG_VERSION
    {
        bail!(
            "unsupported config version {version} in {}; expected {CONFIG_VERSION}",
            path.display()
        );
    }
    if let Some(max_turns) = file.agent.max_turns
        && !(1..=100).contains(&max_turns)
    {
        bail!("agent.max_turns must be between 1 and 100");
    }
    crate::i18n::Language::parse(file.agent.language.as_deref())?;
    for (name, provider) in &file.providers {
        if provider.api_key.as_deref().is_some_and(str::is_empty) {
            bail!("providers.{name}.api_key cannot be empty");
        }
        if provider.api_key_env.as_deref().is_some_and(str::is_empty) {
            bail!("providers.{name}.api_key_env cannot be empty");
        }
        if provider.api_key.is_some() && provider.api_key_env.is_some() {
            bail!("providers.{name} cannot define both api_key and api_key_env");
        }
    }
    for (name, subagent) in &file.subagents {
        if !matches!(name.as_str(), "scout" | "reader" | "deep" | "editor") {
            bail!("unknown subagent profile: {name}");
        }
        if subagent
            .max_turns
            .is_some_and(|value| !(1..=24).contains(&value))
        {
            bail!("subagents.{name}.max_turns must be between 1 and 24");
        }
        if let Some(provider) = &subagent.provider_profile
            && !file.providers.contains_key(provider)
        {
            bail!("subagents.{name}.provider_profile not found: {provider}");
        }
    }
    for (name, server) in &file.mcp_servers {
        if server.command.trim().is_empty() {
            bail!("mcp_servers.{name}.command cannot be empty");
        }
        if !(1..=300).contains(&server.startup_timeout_seconds) {
            bail!("mcp_servers.{name}.startup_timeout_seconds must be between 1 and 300");
        }
    }
    enforce_secret_file_permissions(file, path)
}

#[cfg(unix)]
fn enforce_secret_file_permissions(file: &ConfigFile, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if !file
        .providers
        .values()
        .any(|provider| provider.api_key.is_some())
    {
        return Ok(());
    }
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} contains api_key but permissions are {:o}; run `chmod 600 {}` or use api_key_env",
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_secret_file_permissions(_file: &ConfigFile, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_provider_profiles() {
        let parsed: ConfigFile = toml::from_str(
            r#"
version = 1
default_provider = "some-im"

[agent]
max_turns = 30
approval = "smart"

[providers.some-im]
provider = "some-im"
api = "chat-completions"
api_base = "https://some.im/v1"
api_key_env = "SOMEIM_API_KEY"
model = "deepseek-v4-flash"

[providers.anthropic]
provider = "anthropic"
api = "anthropic-messages"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-5"
"#,
        )
        .expect("parse config");
        assert_eq!(parsed.providers.len(), 2);
        assert_eq!(parsed.default_provider.as_deref(), Some("some-im"));
        assert_eq!(parsed.agent.max_turns, Some(30));
        let loaded = LoadedConfig { file: parsed };
        let selected = loaded
            .select_provider(None)
            .expect("select default")
            .expect("default provider");
        assert_eq!(selected.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = toml::from_str::<ConfigFile>(
            r#"
version = 1
[providers.test]
base_url = "https://example.com/v1"
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn example_config_stays_valid() {
        let parsed: ConfigFile = toml::from_str(include_str!("../../../config.example.toml"))
            .expect("parse config.example.toml");
        assert_eq!(parsed.mcp_servers.len(), 1);
        assert_eq!(parsed.providers.len(), 3);
    }
}
