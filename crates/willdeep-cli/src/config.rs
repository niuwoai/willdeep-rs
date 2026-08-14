use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde::Deserialize;
use willdeep_core::McpServerConfig;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigAction {
    /// Create a private starter config without overwriting an existing file.
    Init,
    /// Parse and validate the effective config.
    Check,
    /// Print the validated config with inline API keys redacted.
    Show,
}

pub fn handle(action: ConfigAction, explicit_path: Option<&Path>) -> Result<()> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(default_config_path)?;
    match action {
        ConfigAction::Init => init(&path),
        ConfigAction::Check => {
            load_required(&path)?;
            println!("valid\t{}\tversion={CONFIG_VERSION}", path.display());
            Ok(())
        }
        ConfigAction::Show => show(&path),
    }
}

fn init(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("configuration file already exists: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create configuration directory: {}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options
        .open(path)
        .with_context(|| format!("create configuration file: {}", path.display()))?
        .write_all(include_bytes!("../../../config.example.toml"))
        .with_context(|| format!("write configuration file: {}", path.display()))?;
    println!("created\t{}", path.display());
    Ok(())
}

fn show(path: &Path) -> Result<()> {
    load_required(path)?;
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read configuration file: {}", path.display()))?;
    let mut value: toml::Value = toml::from_str(&contents)
        .with_context(|| format!("parse TOML configuration: {}", path.display()))?;
    redact_inline_api_keys(&mut value);
    println!("{}", toml::to_string_pretty(&value)?);
    Ok(())
}

fn load_required(path: &Path) -> Result<LoadedConfig> {
    if !path.exists() {
        bail!("configuration file does not exist: {}", path.display());
    }
    LoadedConfig::load(Some(path))
}

fn redact_inline_api_keys(value: &mut toml::Value) {
    let Some(providers) = value
        .get_mut("providers")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    for (_, provider) in providers.iter_mut() {
        if let Some(table) = provider.as_table_mut()
            && table.contains_key("api_key")
        {
            table.insert(
                "api_key".to_owned(),
                toml::Value::String("[REDACTED]".to_owned()),
            );
        }
    }
}

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
    /// Consult the AI judge for commands the static classifier cannot
    /// decide. Defaults to on; turning it off means every ambiguous command
    /// goes straight to an approval card.
    pub safety_judge: Option<bool>,
    /// Model used for the judge. Defaults to the profile's cheap model.
    pub judge_model: Option<String>,
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
    pub token_budget: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub max_consecutive_failures: Option<usize>,
    pub worktree: Option<String>,
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
        if subagent
            .token_budget
            .is_some_and(|value| !(1_000..=10_000_000).contains(&value))
        {
            bail!("subagents.{name}.token_budget must be between 1000 and 10000000");
        }
        if subagent
            .timeout_seconds
            .is_some_and(|value| !(10..=86_400).contains(&value))
        {
            bail!("subagents.{name}.timeout_seconds must be between 10 and 86400");
        }
        if subagent
            .max_consecutive_failures
            .is_some_and(|value| !(1..=20).contains(&value))
        {
            bail!("subagents.{name}.max_consecutive_failures must be between 1 and 20");
        }
        if subagent
            .worktree
            .as_deref()
            .is_some_and(|value| !matches!(value, "shared" | "dedicated"))
        {
            bail!("subagents.{name}.worktree must be shared or dedicated");
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
    fn safety_judge_defaults_to_on_and_stays_configurable() {
        let default: ConfigFile = toml::from_str("version = 1\n[agent]\napproval = \"smart\"\n")
            .expect("parse minimal config");
        assert_eq!(default.agent.safety_judge, None);
        assert!(
            default.agent.safety_judge.unwrap_or(true),
            "an unset safety_judge must mean the judge is active"
        );

        let configured: ConfigFile =
            toml::from_str("version = 1\n[agent]\nsafety_judge = false\njudge_model = \"glm-5\"\n")
                .expect("parse configured judge");
        assert_eq!(configured.agent.safety_judge, Some(false));
        assert_eq!(configured.agent.judge_model.as_deref(), Some("glm-5"));
    }

    #[test]
    fn example_config_stays_valid() {
        let parsed: ConfigFile = toml::from_str(include_str!("../../../config.example.toml"))
            .expect("parse config.example.toml");
        assert_eq!(parsed.mcp_servers.len(), 1);
        assert_eq!(parsed.providers.len(), 3);
    }

    #[test]
    fn redacts_only_inline_provider_secrets() {
        let mut value: toml::Value = toml::from_str(
            r#"
[providers.inline]
api_key = "secret"
model = "model-a"

[providers.environment]
api_key_env = "PROVIDER_API_KEY"
"#,
        )
        .unwrap();
        redact_inline_api_keys(&mut value);
        assert_eq!(
            value["providers"]["inline"]["api_key"].as_str(),
            Some("[REDACTED]")
        );
        assert_eq!(
            value["providers"]["environment"]["api_key_env"].as_str(),
            Some("PROVIDER_API_KEY")
        );
    }

    #[test]
    fn init_is_private_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("willdeep-config-{}", uuid::Uuid::new_v4()));
        let path = root.join("nested/config.toml");
        init(&path).unwrap();
        assert!(path.exists());
        assert!(init(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
