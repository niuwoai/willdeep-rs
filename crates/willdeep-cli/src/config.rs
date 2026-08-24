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
    /// Cross-client attention delivery settings shared with WillDeep.app.
    /// The CLI accepts the full section even when only webhook fields are
    /// relevant on a headless machine; desktop-only sound fields remain
    /// harmless metadata.
    #[serde(default)]
    pub notifications: NotificationSettings,
    /// 生命周期挂钩：审计留痕与门禁拦截。
    ///
    /// 与 `[notifications]` 那条 webhook 分开是有意的：webhook 是事后的礼貌
    /// 通知，跑在关键路径外、可以丢；hook 是事中的裁决，跑在关键路径上、
    /// 非零退出会真的拦下动作。把审计需求接到 webhook 上会丢事件。
    #[serde(default)]
    pub hooks: Vec<HookSettings>,
}

/// 一条 hook 的配置。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSettings {
    /// 出现在拒绝理由里的名字。不给就用事件名——但配了两条同事件的 hook 时，
    /// 拒绝理由会分不清是谁拦的，所以强烈建议起个名。
    pub name: Option<String>,
    /// `pre_tool` / `post_tool` / `approval_resolved`。
    pub event: String,
    pub command: String,
    /// 非零退出是否拦截。只有 `pre_tool` 上有意义——`post_tool` 发生在事后，
    /// 让它"拦截"只会制造一种拦得住的错觉。
    #[serde(default)]
    pub blocking: bool,
    /// 默认 10 秒。它在关键路径上，给太长等于给 agent 装了个刹车。
    pub timeout_seconds: Option<u64>,
    /// hook 自己超时或起不来时：`deny`（默认）或 `ignore`。
    /// 默认拦，是因为坏掉就自动放行的门禁恰好会在出事时失效。
    pub on_error: Option<String>,
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
    /// Model used for context-compression summaries. On some.im defaults to
    /// the gateway-hosted `someim-32b-compressor`; elsewhere defaults to the
    /// session model with the inline instruction.
    pub compressor_model: Option<String>,
    /// 自动整理会话标题。默认开：一屏 `New session` 的历史列表等于没有列表，
    /// 而这条链路只在会话**第一轮**花一次便宜调用，不随对话长度增长。
    /// 关掉后标题停在第一条提示词的确定性派生，仍然可读。
    pub auto_title: Option<bool>,
    /// 会话标题摘要模型。默认取会话模型——标题请求只发一问一答各 800 字，
    /// 成本可忽略，而另指一个端点意味着它可能缺凭据、然后静默退化。
    pub title_model: Option<String>,
    /// Deterministic worker/standard/deep routing. Defaults to enabled.
    pub small_model_routing: Option<bool>,
    /// Dispatch high-confidence read-only requests before the root model sees
    /// the repository. Defaults to enabled when routing is enabled.
    pub auto_dispatch_read_only: Option<bool>,
    /// Admission budget for the scarce deep profile in one harness.
    pub max_deep_calls_per_harness: Option<usize>,
    /// OS 级写入围栏（macOS Seatbelt / Linux bubblewrap）。默认关。
    ///
    /// 默认关不是因为它不重要，是因为它会**改变已经在跑的命令的行为**：
    /// 围栏开着时 `cargo fetch` 写不了工作区外的 `~/.cargo/registry`，除非把
    /// 那条路径列进 `sandbox_writable_roots`。这种破坏该由用户在知情时打开，
    /// 而不是升级一次二进制就突然撞上。
    pub sandbox: Option<bool>,
    /// 围栏开着时，除工作区与临时目录之外还允许写入的根。
    /// 典型用途是工具链缓存：`~/.cargo/registry`、`~/.npm`。
    #[serde(default)]
    pub sandbox_writable_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSettings {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

/// Deliberately *not* `deny_unknown_fields`: this is the one section both
/// WillDeep.app and the CLI read out of the same file. Rejecting unknown keys
/// would mean the app cannot add a field without breaking every CLI that has
/// not been upgraded yet, which would weld the two release trains together.
/// Unknown keys here are ignored; the model-routing editor patches only its
/// named scalar keys, so desktop-only values survive a TUI/Web save.
#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)] // Sound fields belong to the desktop client; the CLI only carries them.
pub struct NotificationSettings {
    pub sound: Option<String>,
    pub custom_sound_file: Option<String>,
    pub custom_sound_display_name: Option<String>,
    pub webhook_enabled: Option<bool>,
    pub webhook_url: Option<String>,
    pub webhook_on_task_completed: Option<bool>,
    pub webhook_on_attention_required: Option<bool>,
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
    /// Byte cap on this worker's tool payloads. Keep it proportional to the
    /// window: a payload cap larger than the window is not a cap.
    pub tool_output_limit: Option<usize>,
    /// Attempts a verified run makes before it reports failure and escalates.
    pub max_attempts: Option<usize>,
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

pub(crate) fn validate(file: &ConfigFile, path: &Path) -> Result<()> {
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
    if file
        .agent
        .max_deep_calls_per_harness
        .is_some_and(|value| value > 16)
    {
        bail!("agent.max_deep_calls_per_harness must be between 0 and 16");
    }
    crate::i18n::Language::parse(file.agent.language.as_deref())?;
    if file.notifications.webhook_enabled.unwrap_or(false) {
        let webhook_url = file
            .notifications
            .webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("notifications.webhook_url is required when webhook_enabled is true")?;
        let parsed = reqwest::Url::parse(webhook_url)
            .context("notifications.webhook_url must be a valid URL")?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            bail!("notifications.webhook_url must use http or https");
        }
    }
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
        if !matches!(
            name.as_str(),
            "scout"
                | "reader"
                | "deep"
                | "editor"
                | "implementer"
                | "test_fixer"
                | "build_fixer"
                | "log_inspector"
                | "git_detective"
        ) {
            bail!("unknown subagent profile: {name}");
        }
        // 4K is below any usable worker budget once the system prompt and
        // tool schemas are paid for; past 1M nothing real is being described.
        if subagent
            .context_window
            .is_some_and(|value| !(4_000..=1_000_000).contains(&value))
        {
            bail!("subagents.{name}.context_window must be between 4000 and 1000000");
        }
        if subagent
            .max_attempts
            .is_some_and(|value| !(1..=6).contains(&value))
        {
            bail!("subagents.{name}.max_attempts must be between 1 and 6");
        }
        if subagent
            .tool_output_limit
            .is_some_and(|value| !(1_024..=131_072).contains(&value))
        {
            bail!("subagents.{name}.tool_output_limit must be between 1024 and 131072");
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
    fn deep_budget_is_bounded_and_can_be_disabled() {
        let disabled: ConfigFile =
            toml::from_str("version = 1\n[agent]\nmax_deep_calls_per_harness = 0\n")
                .expect("parse disabled deep budget");
        validate(&disabled, Path::new("config.toml")).expect("zero disables deep");

        let excessive: ConfigFile =
            toml::from_str("version = 1\n[agent]\nmax_deep_calls_per_harness = 17\n")
                .expect("parse excessive deep budget");
        assert!(validate(&excessive, Path::new("config.toml")).is_err());
    }

    #[test]
    fn example_config_stays_valid() {
        let parsed: ConfigFile = toml::from_str(include_str!("../../../config.example.toml"))
            .expect("parse config.example.toml");
        assert_eq!(parsed.mcp_servers.len(), 1);
        assert_eq!(parsed.providers.len(), 3);
        assert_eq!(
            parsed.notifications.sound.as_deref(),
            Some("system-default")
        );
        assert_eq!(
            parsed.notifications.webhook_url.as_deref(),
            Some("http://127.0.0.1:8787/willdeep")
        );
        let some_im = parsed.providers.get("some-im").expect("some.im profile");
        assert_eq!(some_im.model.as_deref(), Some("glm-5"));
        assert_eq!(parsed.agent.small_model_routing, Some(true));
        assert_eq!(parsed.agent.auto_dispatch_read_only, Some(true));
        assert_eq!(parsed.agent.max_deep_calls_per_harness, Some(1));
        for hosted in [
            "scout",
            "reader",
            "editor",
            "test_fixer",
            "build_fixer",
            "log_inspector",
            "git_detective",
        ] {
            let profile = parsed.subagents.get(hosted).expect("hosted worker policy");
            assert_eq!(profile.provider_profile, None, "{hosted} provider override");
            assert_eq!(profile.model, None, "{hosted} model override");
        }
        let deep = parsed.subagents.get("deep").expect("deep profile");
        assert_eq!(deep.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn notification_schema_matches_the_macos_app() {
        let parsed: ConfigFile = toml::from_str(
            r#"
version = 1

[notifications]
sound = "custom"
custom_sound_file = "WillDeep-Custom-Alert.mp3"
custom_sound_display_name = "message.mp3"
webhook_enabled = true
webhook_url = "http://127.0.0.1:8787/willdeep"
webhook_on_task_completed = true
webhook_on_attention_required = false
"#,
        )
        .expect("parse shared notifications section");

        validate(&parsed, Path::new("config.toml")).expect("validate local webhook");
        assert_eq!(parsed.notifications.sound.as_deref(), Some("custom"));
        assert_eq!(parsed.notifications.webhook_enabled, Some(true));
        assert_eq!(
            parsed.notifications.webhook_on_attention_required,
            Some(false)
        );
    }

    #[test]
    fn notifications_tolerate_fields_only_the_desktop_app_knows() {
        // WillDeep.app must be able to ship a new key into the shared file
        // without bricking every CLI that has not been upgraded yet.
        let parsed: ConfigFile = toml::from_str(
            "version = 1\n[notifications]\nsound = \"custom\"\nwebhook_retry_count = 3\n",
        )
        .expect("ignore unknown desktop-only fields");

        assert_eq!(parsed.notifications.sound.as_deref(), Some("custom"));
        validate(&parsed, Path::new("config.toml")).expect("unknown fields stay valid");
    }

    #[test]
    fn unknown_keys_outside_notifications_are_still_rejected() {
        let error = toml::from_str::<ConfigFile>("version = 1\nnot_a_real_key = true\n")
            .expect_err("top-level typos must still fail loudly");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn enabled_webhook_rejects_non_http_urls() {
        let parsed: ConfigFile = toml::from_str(
            "version = 1\n[notifications]\nwebhook_enabled = true\nwebhook_url = \"file:///tmp/hook\"\n",
        )
        .expect("parse notifications section");

        let error = validate(&parsed, Path::new("config.toml")).expect_err("reject file URL");
        assert!(error.to_string().contains("must use http or https"));
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
