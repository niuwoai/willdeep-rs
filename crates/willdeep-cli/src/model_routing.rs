use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigFile, LoadedConfig};

pub const STALE_CONFIG_MESSAGE: &str = "model routing settings are stale; reload before saving";

/// 与 `willdeep_core::PUBLIC_SUBAGENT_IDS` 同一份名单：五个职责，模型档位
/// 是另一根轴（见 `willdeep_core::WorkerTier`）。
const PROFILE_IDS: &[&str] = &[
    "generalist",
    "implementer",
    "tester",
    "reviewer",
    "ops_runner",
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
    /// 三个模型档位。与 `profiles` 是两根正交的轴：那边是「这个职责平时用
    /// 什么」，这边是「派工时说要贵一档，贵成什么样」。
    pub tiers: Vec<TierRoutingSettings>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TierRoutingSettings {
    pub id: String,
    pub provider_profile: Option<String>,
    pub model: Option<String>,
    pub context_window: u64,
    /// 没有任何显式配置——此时用的是网关默认表或父模型回落。
    pub automatic: bool,
    pub effective_provider: String,
    pub effective_model: String,
    /// 这一档在 some.im 网关上的默认模型，留空即采用它。
    pub recommended_model: Option<String>,
    /// 这一档是否需要升级票据。UI 要说清楚：把专家档绑到多贵的模型，都还有
    /// 这道闸门兜着。
    pub requires_admission: bool,
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
    /// 缺省表示「这次不动档位」，与提交一个空列表是两回事——老客户端不带这个
    /// 字段，不该因为升级了服务端就把用户的档位配置清空。
    #[serde(default)]
    pub tiers: Option<Vec<TierRoutingUpdate>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRoutingUpdate {
    pub id: String,
    pub provider_profile: Option<String>,
    pub model: Option<String>,
    /// `None` 表示这一档不写 `context_window`，沿用档位自己的预算。
    pub context_window: Option<u64>,
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
            tiers: Some(
                self.tiers
                    .iter()
                    .map(|tier| TierRoutingUpdate {
                        id: tier.id.clone(),
                        provider_profile: tier.provider_profile.clone(),
                        model: tier.model.clone(),
                        // automatic 的档不写回 context_window：写了就等于把
                        // 当前默认值钉死，以后改了档位预算这里也跟不上。
                        context_window: (!tier.automatic).then_some(tier.context_window),
                    })
                    .collect(),
            ),
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
        let section = format!(
            "subagents.{}",
            configured_section_id(&current.file, &profile.id)
        );
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
    // 缺省表示「这次不动档位」。老客户端不带 tiers 字段，升级服务端不该把
    // 用户的档位配置清空。
    for tier in update.tiers.iter().flatten() {
        let section = format!("worker_tiers.{}", tier.id);
        patched = patch_key(
            &patched,
            Some(&section),
            "provider_profile",
            tier.provider_profile.as_deref().map(toml_string),
        );
        patched = patch_key(
            &patched,
            Some(&section),
            "model",
            tier.model.as_deref().map(str::trim).map(toml_string),
        );
        patched = patch_key(
            &patched,
            Some(&section),
            "context_window",
            tier.context_window.map(|value| value.to_string()),
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
    let tiers = willdeep_core::WorkerTier::ALL
        .iter()
        .map(|tier| tier_settings(file, *tier, &default_provider, &root_model))
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
        tiers,
    })
}

/// 一个档位在界面上显示成什么。
///
/// 这里的「有效值」必须和 `harness::resolve_tier_bindings` 的解析阶梯说同一件
/// 事，否则设置面板会显示一个运行时并不会用的模型——0.51 那个「表只在文档里」
/// 的毛病，换个地方重演一遍。
fn tier_settings(
    file: &ConfigFile,
    tier: willdeep_core::WorkerTier,
    root_provider: &str,
    root_model: &str,
) -> Result<TierRoutingSettings> {
    let configured = file.worker_tiers.get(tier.as_str());
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
    let automatic = configured.is_none_or(|settings| {
        settings.provider_profile.is_none()
            && settings.model.is_none()
            && settings.context_window.is_none()
    });
    // 基础档在网关上就是工种自己的模型，没有独立的「档位默认」可推荐。
    let recommended_model = (is_some_im && tier != willdeep_core::WorkerTier::Standard)
        .then(|| tier.default_hosted_model().to_owned());
    let effective_model = model
        .clone()
        .or_else(|| recommended_model.clone())
        .unwrap_or_else(|| {
            // 落到这里就是「没配、网关也没这一档的表」：专家档回落父模型，其余
            // 保持工种自己的绑定，与 harness 同一条阶梯。
            provider
                .model
                .clone()
                .unwrap_or_else(|| root_model.to_owned())
        });
    Ok(TierRoutingSettings {
        id: tier.as_str().to_owned(),
        automatic,
        provider_profile,
        model,
        context_window: configured
            .and_then(|settings| settings.context_window)
            .unwrap_or_else(|| tier.context_budget()),
        effective_provider,
        effective_model,
        recommended_model,
        requires_admission: tier.requires_admission(),
    })
}

/// 这个工种改名前叫什么。已经写在别人 config 里的段落不该因为一次改名就失效。
fn legacy_section_ids(id: &str) -> &'static [&'static str] {
    match id {
        "generalist" => &["reader", "deep"],
        "reviewer" => &["judge"],
        _ => &[],
    }
}

/// 定位一个工种在 config 里实际使用的段落名：新名优先，其次是改名前的名字。
/// 保存时也用它，免得写出一个新段落、把用户原来那段晾在旁边失效。
fn configured_section_id(file: &ConfigFile, id: &str) -> String {
    if file.subagents.contains_key(id) {
        return id.to_owned();
    }
    legacy_section_ids(id)
        .iter()
        .find(|legacy| file.subagents.contains_key(**legacy))
        .map(|legacy| (*legacy).to_owned())
        .unwrap_or_else(|| id.to_owned())
}

fn profile_settings(
    file: &ConfigFile,
    id: &str,
    root_provider: &str,
    root_model: &str,
) -> Result<ProfileRoutingSettings> {
    let configured = file.subagents.get(id).or_else(|| {
        legacy_section_ids(id)
            .iter()
            .find_map(|legacy| file.subagents.get(*legacy))
    });
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
            if is_some_im {
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
    let mut seen_tiers = HashSet::new();
    for tier in update.tiers.iter().flatten() {
        // 与 `config::validate` 同一条规矩：段名只认三个正名，`deep` 之类的
        // 别名做段名会和 `expert` 撞成同一档。
        if willdeep_core::WorkerTier::parse(&tier.id)
            .is_none_or(|parsed| parsed.as_str() != tier.id)
            || !seen_tiers.insert(tier.id.as_str())
        {
            bail!("unknown or duplicate worker tier: {}", tier.id);
        }
        if let Some(provider) = tier.provider_profile.as_deref()
            && (!safe_bare_key(provider) || !file.providers.contains_key(provider))
        {
            bail!(
                "unknown or unsupported provider profile for tier {}",
                tier.id
            );
        }
        if let Some(model) = tier.model.as_deref() {
            validate_model(&tier.id, model)?;
        }
        if tier
            .context_window
            .is_some_and(|value| !(4_000..=1_000_000).contains(&value))
        {
            bail!(
                "context_window for tier {} must be between 4000 and 1000000",
                tier.id
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

/// 工种的默认上下文预算。
///
/// 这些是**职责**各自需要多少材料，与 [`willdeep_core::WorkerTier`] 的档位预算
/// 是两回事：档位说的是「这一档最多给多少」，这里说的是「这个职责默认要多少」，
/// 显式配置覆盖两者。
fn default_context_window(id: &str) -> u64 {
    match id {
        // 调查要跨文件跟线索，材料量本来就比有界工种大。
        "generalist" => willdeep_core::WorkerTier::Standard.context_budget(),
        "implementer" => 262_144,
        "tester" => 65_536,
        "reviewer" | "ops_runner" => 49_152,
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
        (None, Some(value)) => {
            // `end` 是下一个段头的位置，它前面通常有一行空行分隔两段。直接插在
            // `end` 会把新键写到那行空行**之后**，于是它紧贴着下一个段头，
            // 段间的空行被吃掉。TOML 照样解析得了，但这是一份人要手改的文件，
            // 不该被工具越改越难读。
            let mut at = end;
            while at > start && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            lines.insert(at, format!("{key} = {value}"));
        }
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

[subagents.reader]
# preserve worker comments
context_window = 49152

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
            Some("subagents.reader"),
            "model",
            Some(toml_string("custom-reader")),
        );
        value = patch_key(
            &value,
            Some("subagents.reader"),
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
        assert!(value.contains("model = \"custom-reader\""));
        assert!(value.contains("provider_profile = \"local\""));
        // 新键要落在段落最后一行有效内容之后，而不是段间空行之后——否则它会
        // 紧贴下一个段头，把两段之间的空行吃掉。
        assert!(
            value.contains("provider_profile = \"local\"\n\n[notifications]"),
            "新键不应吃掉段间空行:\n{value}"
        );
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
        // 七个旧工种别名已收敛到基础档：职责提示词随请求走，网关不再
        // 按工种各铺一条链。
        assert_eq!(settings.profiles[0].effective_model, "someim-32b");
        let mut update = settings.to_update();
        update.default_provider = "local".to_owned();
        update.root_model = "local-coder-v2".to_owned();
        update.max_deep_calls_per_harness = 0;
        // 样例 config 里写的是改名前的 `[subagents.reader]`。它必须继续被读到，
        // 保存也必须落回同一个段落——否则一次改名就让别人的配置变成死字，
        // 界面上还多出一个空的新段落。
        let generalist = update
            .profiles
            .iter_mut()
            .find(|profile| profile.id == "generalist")
            .expect("generalist update");
        generalist.provider_profile = Some("some-im".to_owned());
        generalist.model = Some("custom-reader".to_owned());
        let saved = save(&path, None, &update).expect("save settings");
        assert_eq!(saved.default_provider, "local");
        assert_eq!(saved.root_model, "local-coder-v2");
        assert_eq!(saved.max_deep_calls_per_harness, 0);
        let generalist = saved
            .profiles
            .iter()
            .find(|profile| profile.id == "generalist")
            .expect("saved generalist");
        assert_eq!(generalist.model.as_deref(), Some("custom-reader"));
        let written = std::fs::read_to_string(&path).expect("saved config");
        assert!(written.contains("# preserve worker comments"));
        assert!(
            written.contains("[subagents.reader]") && !written.contains("[subagents.generalist]"),
            "the existing section must be updated in place, not duplicated under the new name"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tier_rows_round_trip_into_the_shared_worker_tiers_section() {
        let root =
            std::env::temp_dir().join(format!("willdeep-tier-routing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("temp root");
        let path = root.join("config.toml");
        std::fs::write(&path, sample()).expect("sample config");

        let settings = load(&path, None).expect("load settings");
        assert_eq!(settings.tiers.len(), 3);
        // 什么都没配时显示的必须是运行时真会用的那个模型，不是一张只写在
        // 文档里的表——这正是 0.51 踩过的坑。
        assert!(settings.tiers.iter().all(|tier| tier.automatic));
        assert_eq!(settings.tiers[1].effective_model, "deepseek-v4-flash");
        assert_eq!(settings.tiers[2].effective_model, "gpt-5.6-sol");
        assert!(settings.tiers[2].requires_admission);

        let mut update = settings.to_update();
        let tiers = update.tiers.as_mut().expect("tiers");
        let expert = tiers
            .iter_mut()
            .find(|tier| tier.id == "expert")
            .expect("expert");
        expert.provider_profile = Some("local".to_owned());
        expert.model = Some("local-opus".to_owned());
        expert.context_window = Some(400_000);
        let saved = save(&path, None, &update).expect("save settings");

        let expert = saved
            .tiers
            .iter()
            .find(|tier| tier.id == "expert")
            .expect("expert");
        assert!(!expert.automatic);
        assert_eq!(expert.effective_provider, "local");
        assert_eq!(expert.effective_model, "local-opus");
        assert_eq!(expert.context_window, 400_000);
        // 没动过的档不该被写出一个空段落。
        let written = std::fs::read_to_string(&path).expect("written config");
        assert!(written.contains("[worker_tiers.expert]"));
        assert!(!written.contains("[worker_tiers.standard]"));
        assert!(written.contains("# keep this comment"));
        assert!(written.contains("# preserve worker comments"));
        crate::config::validate(
            &toml::from_str::<ConfigFile>(&written).expect("parse written config"),
            &path,
        )
        .expect("written config stays valid");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn a_tier_alias_is_refused_as_a_section_name() {
        let file: ConfigFile = toml::from_str(&sample()).expect("sample config");
        let mut update = load_update_fixture(&file);
        update.tiers = Some(vec![TierRoutingUpdate {
            // `deep` 做参数值还通，做段名会和 `expert` 撞成同一档。
            id: "deep".to_owned(),
            provider_profile: None,
            model: Some("opus-5".to_owned()),
            context_window: None,
        }]);
        assert!(validate_update(&file, &update).is_err());
    }

    fn load_update_fixture(file: &ConfigFile) -> ModelRoutingUpdate {
        let _ = file;
        ModelRoutingUpdate {
            revision: "r1".to_owned(),
            default_provider: "some-im".to_owned(),
            root_model: "glm-5".to_owned(),
            small_model_routing: true,
            auto_dispatch_read_only: true,
            max_deep_calls_per_harness: 1,
            profiles: Vec::new(),
            tiers: None,
        }
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
