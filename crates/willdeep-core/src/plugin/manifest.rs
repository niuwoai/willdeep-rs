//! WillDeep 插件清单：`.codex-plugin/plugin.json` 与 `.willdeep-plugin/plugin.json`。
//!
//! 这里刻意手写校验而不是拉一个 JSON Schema 运行时进来：schema 是跨仓共享的
//! 契约（Xedit `docs/plugin-schema/willdeep-plugin.schema.json`），两端各自实现
//! 同一份规则，任何一侧偷偷放宽都会被对方的解析测试当场抓住。规则见
//! `docs/PLUGINS.md`，与 Swift 侧 `AgentPluginPackageLoader.swift` 对齐。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// 清单里 ID 的通用形状：首字符是字母数字，其后允许 `_ . -`，最长 160。
/// 与共享 schema 的 `$defs/id` 逐字对应。
pub(crate) fn is_valid_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() || value.chars().count() > 160 {
        return false;
    }
    chars.all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '.' | '-'))
}

/// `networkDomains` 里一条的形状：`example.com` 或 `*.example.com`。
///
/// 刻意不接受裸 `*`、带协议、带路径或带端口的写法。这份名单是
/// `net.fetch` 唯一的门，一条写法含糊的规则等于一扇关不上的门。
pub(crate) fn is_valid_domain(value: &str) -> bool {
    let host = value.strip_prefix("*.").unwrap_or(value);
    if host.is_empty() || host.len() > 253 || host == "*" {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|item| item.is_ascii_alphanumeric() || item == '-')
    }) && host.contains('.')
}

/// 本地化键：非空且不含空白。
fn is_valid_key(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

/// 契约往前长时的兼容面。
///
/// 插件包由两个宿主共享，词汇表（权限、宿主动作、菜单位置、字段名）却各自
/// 用白名单校验。一侧先加了一项，另一侧就把整个包判非法——用户看到的是
/// 「装不上」，而真相只是这个宿主还没实现其中一个能力。实测中 Xedit 自带的
/// 十个插件里有三个卡在这上面（待办的 `conversation.write`、短剧工坊的
/// `ai.image`、历史回溯的 `session.open`）。
///
/// 所以未知词汇一律记在这里，不再中断解析：宿主照常装、照常显示，并在审批
/// 清单里明说这几项本宿主不支持；调用到那一条时才拒。判非法只留给真正的
/// 结构性错误（引用悬空、页面缺必填字段、schemaVersion 不认识）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnsupportedItems(BTreeSet<String>);

impl UnsupportedItems {
    fn note(&mut self, kind: &str, value: &str) {
        self.0.insert(format!("{kind}:{value}"));
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin manifest is not valid JSON: {0}")]
    Json(String),
    #[error("plugin manifest field `{field}` is missing or invalid")]
    Field { field: String },
    #[error("unknown plugin manifest field `{0}`")]
    UnknownField(String),
    #[error("unsupported plugin schemaVersion {0}, this build understands 1")]
    SchemaVersion(u64),
    #[error("plugin id `{0}` is not a valid identifier")]
    InvalidId(String),
    #[error("plugin version `{0}` is not a semantic version")]
    InvalidVersion(String),
    #[error("duplicate {kind} id `{id}`")]
    DuplicateId { kind: &'static str, id: String },
    #[error("{kind} `{id}` references unknown {target} `{reference}`")]
    DanglingReference {
        kind: &'static str,
        id: String,
        target: &'static str,
        reference: String,
    },
    #[error("unknown menu location `{0}`")]
    UnknownMenuLocation(String),
    #[error("unknown permission `{0}`")]
    UnknownPermission(String),
    #[error("page `{id}` with runtime {runtime} is missing `{field}`")]
    IncompletePage {
        id: String,
        runtime: &'static str,
        field: &'static str,
    },
    #[error("mcpApp resourceURI `{0}` must start with ui://")]
    InvalidResourceUri(String),
    #[error("localization key `{0}` is missing from the English locale")]
    MissingLocalization(String),
}

/// 插件声明的能力。宿主在页面调用时按这份名单现场核。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginPermission {
    ConversationRead,
    ConversationWrite,
    WorkspaceRead,
    WorkspaceWrite,
    ProcessExecute,
    NetworkAccess,
    CredentialsUse,
    AiChat,
    AiImage,
    ProvidersRead,
    SkillsRead,
    ClipboardWrite,
    Notifications,
}

impl PluginPermission {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "conversation.read" => Self::ConversationRead,
            "conversation.write" => Self::ConversationWrite,
            "workspace.read" => Self::WorkspaceRead,
            "workspace.write" => Self::WorkspaceWrite,
            "process.execute" => Self::ProcessExecute,
            "network.access" => Self::NetworkAccess,
            "credentials.use" => Self::CredentialsUse,
            "ai.chat" => Self::AiChat,
            "ai.image" => Self::AiImage,
            "providers.read" => Self::ProvidersRead,
            "skills.read" => Self::SkillsRead,
            "clipboard.write" => Self::ClipboardWrite,
            "notifications" => Self::Notifications,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConversationRead => "conversation.read",
            Self::ConversationWrite => "conversation.write",
            Self::WorkspaceRead => "workspace.read",
            Self::WorkspaceWrite => "workspace.write",
            Self::ProcessExecute => "process.execute",
            Self::NetworkAccess => "network.access",
            Self::CredentialsUse => "credentials.use",
            Self::AiChat => "ai.chat",
            Self::AiImage => "ai.image",
            Self::ProvidersRead => "providers.read",
            Self::SkillsRead => "skills.read",
            Self::ClipboardWrite => "clipboard.write",
            Self::Notifications => "notifications",
        }
    }
}

/// 菜单贡献点白名单。这份名单是跨仓契约的一部分，新增一项必须两端同时加，
/// 否则一侧安装得上的插件在另一侧会被判非法。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginMenuLocation {
    CommandPalette,
    SessionContext,
    ComposerMore,
    PageToolbar,
    SidebarRowContext,
    ChatSelection,
}

impl PluginMenuLocation {
    pub const ALL: [Self; 6] = [
        Self::CommandPalette,
        Self::SessionContext,
        Self::ComposerMore,
        Self::PageToolbar,
        Self::SidebarRowContext,
        Self::ChatSelection,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "commandPalette" => Self::CommandPalette,
            "session.context" => Self::SessionContext,
            "composer.more" => Self::ComposerMore,
            "plugin.page.toolbar" => Self::PageToolbar,
            "plugin.sidebar.row.context" => Self::SidebarRowContext,
            "chat.selection" => Self::ChatSelection,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandPalette => "commandPalette",
            Self::SessionContext => "session.context",
            Self::ComposerMore => "composer.more",
            Self::PageToolbar => "plugin.page.toolbar",
            Self::SidebarRowContext => "plugin.sidebar.row.context",
            Self::ChatSelection => "chat.selection",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    SessionList,
    Declarative,
    None,
}

impl SidebarMode {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "sessionList" => Self::SessionList,
            "declarative" => Self::Declarative,
            "none" => Self::None,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionList => "sessionList",
            Self::Declarative => "declarative",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageRuntime {
    LocalWeb,
    McpApp,
    Declarative,
}

impl PageRuntime {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "localWeb" => Self::LocalWeb,
            "mcpApp" => Self::McpApp,
            "declarative" => Self::Declarative,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalWeb => "localWeb",
            Self::McpApp => "mcpApp",
            Self::Declarative => "declarative",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpResourceRef {
    pub server: String,
    pub uri: String,
}

#[derive(Clone, Debug)]
pub struct PluginDestination {
    pub id: String,
    pub title_key: String,
    pub icon: Option<String>,
    pub main_page: String,
    pub companion_sidebar: Option<String>,
    pub toolbar_command_ids: Vec<String>,
    pub default_pinned: bool,
}

#[derive(Clone, Debug)]
pub struct PluginSidebar {
    pub id: String,
    pub mode: SidebarMode,
    pub schema: Option<String>,
    pub resource: Option<McpResourceRef>,
}

#[derive(Clone, Debug)]
pub struct PluginPage {
    pub id: String,
    pub runtime: PageRuntime,
    pub entry_path: Option<String>,
    pub schema: Option<String>,
    pub server: Option<String>,
    pub resource_uri: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandHandler {
    /// 公开 Host Command 白名单里的一条。
    Host { action: HostAction },
    /// 调用本插件已配置的 MCP Tool。
    McpTool { server: String, tool: String },
    /// 跳转到已安装并启用的插件目的地。
    Navigate { destination: String },
    /// 本宿主不认识的处理方式（另一侧新加的宿主动作或 handler 类型）。
    /// 命令仍然存在，菜单引用因此不会悬空，点下去只回一句「不支持」。
    Unsupported { detail: String },
}

/// 公开 Host Command v1。任意 selector、类名与脚本文本都被拒绝——白名单是
/// **字面量**，不是"凡是 plugin. 开头"的命名规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAction {
    PluginRefresh,
    DestinationSelect,
    SettingsMcp,
    PluginsOpenCenter,
    SessionOpen,
}

impl HostAction {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "plugin.refresh" => Self::PluginRefresh,
            "destination.select" => Self::DestinationSelect,
            "settings.mcp" => Self::SettingsMcp,
            "plugins.open-center" => Self::PluginsOpenCenter,
            "session.open" => Self::SessionOpen,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PluginRefresh => "plugin.refresh",
            Self::DestinationSelect => "destination.select",
            Self::SettingsMcp => "settings.mcp",
            Self::PluginsOpenCenter => "plugins.open-center",
            Self::SessionOpen => "session.open",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PluginCommand {
    pub id: String,
    pub title_key: String,
    pub icon: Option<String>,
    pub handler: CommandHandler,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingType {
    String,
    Number,
    Boolean,
    Enum,
    Secret,
}

impl SettingType {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "string" => Self::String,
            "number" => Self::Number,
            "boolean" => Self::Boolean,
            "enum" => Self::Enum,
            "secret" => Self::Secret,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Enum => "enum",
            Self::Secret => "secret",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PluginSetting {
    pub id: String,
    pub setting_type: SettingType,
    pub title_key: String,
    pub description_key: Option<String>,
    pub default_value: Option<String>,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PluginDependencies {
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
}

/// `.codex-plugin/plugin.json`：插件 ID、版本与展示元数据的唯一来源。
/// 只有 Codex 清单的包仍可提供 Skill/MCP，但不会自动生成一级入口。
#[derive(Clone, Debug)]
pub struct CodexManifest {
    pub id: String,
    pub version: String,
    pub description: Option<String>,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub composer_icon: Option<String>,
}

impl CodexManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let value: Value =
            serde_json::from_str(source).map_err(|error| ManifestError::Json(error.to_string()))?;
        let object = value.as_object().ok_or(ManifestError::Field {
            field: "<root>".into(),
        })?;
        let id = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ManifestError::Field {
                field: "name".into(),
            })?;
        if !is_valid_id(id) {
            return Err(ManifestError::InvalidId(id.to_owned()));
        }
        let version =
            object
                .get("version")
                .and_then(Value::as_str)
                .ok_or(ManifestError::Field {
                    field: "version".into(),
                })?;
        if !is_semver(version) {
            return Err(ManifestError::InvalidVersion(version.to_owned()));
        }
        let interface = object.get("interface").and_then(Value::as_object);
        let read = |key: &str| {
            interface
                .and_then(|item| item.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Ok(Self {
            id: id.to_owned(),
            version: version.to_owned(),
            description: object
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            display_name: read("displayName"),
            short_description: read("shortDescription"),
            composer_icon: read("composerIcon"),
        })
    }
}

/// 版本号：`MAJOR.MINOR.PATCH` 加可选预发布标识。
pub(crate) fn is_semver(value: &str) -> bool {
    let core = value.split_once('-').map_or(
        value,
        |(head, tail)| {
            if tail.is_empty() { "" } else { head }
        },
    );
    if core.is_empty() {
        return false;
    }
    let mut parts = core.split('.');
    let ok = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|item| !item.is_empty() && item.chars().all(|c| c.is_ascii_digit()))
    });
    ok && parts.next().is_none()
}

/// `.willdeep-plugin/plugin.json`：只声明宿主贡献，不含 ID 与版本。
#[derive(Clone, Debug, Default)]
pub struct PluginManifest {
    pub minimum_willdeep_version: Option<String>,
    pub permissions: BTreeSet<PluginPermission>,
    /// 宿主代发 HTTP 的域名白名单（`window.willdeep.net.fetch`）。
    /// `network.access` 只是开关，真正的门是这份名单。
    pub network_domains: Vec<String>,
    pub dependencies: PluginDependencies,
    pub destinations: Vec<PluginDestination>,
    pub sidebars: Vec<PluginSidebar>,
    pub pages: Vec<PluginPage>,
    pub commands: Vec<PluginCommand>,
    pub menus: BTreeMap<PluginMenuLocation, Vec<String>>,
    pub settings: Vec<PluginSetting>,
    /// 解析时遇到的、本宿主还不认识的词汇与字段。见 [`UnsupportedItems`]。
    pub unsupported: UnsupportedItems,
}

const ROOT_FIELDS: [&str; 5] = [
    "schemaVersion",
    "minimumWillDeepVersion",
    "permissions",
    "networkDomains",
    "dependencies",
];

impl PluginManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let value: Value =
            serde_json::from_str(source).map_err(|error| ManifestError::Json(error.to_string()))?;
        let object = value.as_object().ok_or(ManifestError::Field {
            field: "<root>".into(),
        })?;
        let mut unsupported = UnsupportedItems::default();
        for key in object.keys() {
            // `$schema` 是编辑器提示，不是契约的一部分，放行。
            if key != "$schema" && key != "contributes" && !ROOT_FIELDS.contains(&key.as_str()) {
                unsupported.note("field", key);
            }
        }
        match object.get("schemaVersion").and_then(Value::as_u64) {
            Some(1) => {}
            Some(other) => return Err(ManifestError::SchemaVersion(other)),
            None => {
                return Err(ManifestError::Field {
                    field: "schemaVersion".into(),
                });
            }
        }

        let mut manifest = Self {
            minimum_willdeep_version: object
                .get("minimumWillDeepVersion")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Self::default()
        };
        if let Some(version) = &manifest.minimum_willdeep_version
            && !is_semver(version)
        {
            return Err(ManifestError::InvalidVersion(version.clone()));
        }

        for item in array_of(object.get("permissions"), "permissions")? {
            let raw = item.as_str().ok_or(ManifestError::Field {
                field: "permissions[]".into(),
            })?;
            match PluginPermission::parse(raw) {
                Some(permission) => {
                    manifest.permissions.insert(permission);
                }
                None => unsupported.note("permission", raw),
            }
        }

        // `networkDomains` 不走「认不出就降级」那条路：它是 `net.fetch` 唯一的
        // 门，写法含糊等于一扇关不上的门。与共享 schema 的 pattern 逐字对应，
        // Swift 侧同名规则在 `AgentPluginPackageLoader.isValidNetworkDomain`。
        for item in array_of(object.get("networkDomains"), "networkDomains")? {
            let raw = item
                .as_str()
                .map(str::trim)
                .filter(|value| is_valid_domain(value))
                .ok_or(ManifestError::Field {
                    field: "networkDomains[]".into(),
                })?;
            manifest.network_domains.push(raw.to_ascii_lowercase());
        }

        if let Some(dependencies) = object.get("dependencies") {
            let dependencies = dependencies.as_object().ok_or(ManifestError::Field {
                field: "dependencies".into(),
            })?;
            manifest.dependencies.mcp_servers =
                id_array(dependencies.get("mcpServers"), "dependencies.mcpServers")?;
            manifest.dependencies.skills =
                id_array(dependencies.get("skills"), "dependencies.skills")?;
        }

        let Some(contributes) = object.get("contributes") else {
            return Err(ManifestError::Field {
                field: "contributes".into(),
            });
        };
        let contributes = contributes.as_object().ok_or(ManifestError::Field {
            field: "contributes".into(),
        })?;

        for item in array_of(contributes.get("destinations"), "contributes.destinations")? {
            manifest
                .destinations
                .push(parse_destination(item, &mut unsupported)?);
        }
        for item in array_of(contributes.get("sidebars"), "contributes.sidebars")? {
            manifest
                .sidebars
                .push(parse_sidebar(item, &mut unsupported)?);
        }
        for item in array_of(contributes.get("pages"), "contributes.pages")? {
            manifest.pages.push(parse_page(item, &mut unsupported)?);
        }
        for item in array_of(contributes.get("commands"), "contributes.commands")? {
            manifest
                .commands
                .push(parse_command(item, &mut unsupported)?);
        }
        if let Some(menus) = contributes.get("menus") {
            let menus = menus.as_object().ok_or(ManifestError::Field {
                field: "contributes.menus".into(),
            })?;
            for (key, value) in menus {
                let Some(location) = PluginMenuLocation::parse(key) else {
                    unsupported.note("menu", key);
                    continue;
                };
                manifest
                    .menus
                    .insert(location, id_array(Some(value), "contributes.menus[]")?);
            }
        }
        for item in array_of(contributes.get("settings"), "contributes.settings")? {
            manifest
                .settings
                .push(parse_setting(item, &mut unsupported)?);
        }

        manifest.unsupported = unsupported;
        manifest.validate_references()?;
        Ok(manifest)
    }

    /// 引用完整性：重复 ID、指向不存在的页面/侧栏/命令，都在这里被拒。
    /// 一个半解析成功的插件比装不上更危险——用户会看到入口，点下去什么都没有。
    fn validate_references(&self) -> Result<(), ManifestError> {
        let page_ids = unique_ids(self.pages.iter().map(|item| item.id.as_str()), "page")?;
        let sidebar_ids = unique_ids(self.sidebars.iter().map(|item| item.id.as_str()), "sidebar")?;
        let command_ids = unique_ids(self.commands.iter().map(|item| item.id.as_str()), "command")?;
        let destination_ids = unique_ids(
            self.destinations.iter().map(|item| item.id.as_str()),
            "destination",
        )?;
        unique_ids(self.settings.iter().map(|item| item.id.as_str()), "setting")?;

        for destination in &self.destinations {
            if !page_ids.contains(destination.main_page.as_str()) {
                return Err(ManifestError::DanglingReference {
                    kind: "destination",
                    id: destination.id.clone(),
                    target: "page",
                    reference: destination.main_page.clone(),
                });
            }
            if let Some(sidebar) = &destination.companion_sidebar
                && !sidebar_ids.contains(sidebar.as_str())
            {
                return Err(ManifestError::DanglingReference {
                    kind: "destination",
                    id: destination.id.clone(),
                    target: "sidebar",
                    reference: sidebar.clone(),
                });
            }
            for command in &destination.toolbar_command_ids {
                if !command_ids.contains(command.as_str()) {
                    return Err(ManifestError::DanglingReference {
                        kind: "destination",
                        id: destination.id.clone(),
                        target: "command",
                        reference: command.clone(),
                    });
                }
            }
        }
        for (location, commands) in &self.menus {
            for command in commands {
                if !command_ids.contains(command.as_str()) {
                    return Err(ManifestError::DanglingReference {
                        kind: "menu",
                        id: location.as_str().to_owned(),
                        target: "command",
                        reference: command.clone(),
                    });
                }
            }
        }
        for command in &self.commands {
            if let CommandHandler::Navigate { destination } = &command.handler
                && !destination_ids.contains(destination.as_str())
            {
                return Err(ManifestError::DanglingReference {
                    kind: "command",
                    id: command.id.clone(),
                    target: "destination",
                    reference: destination.clone(),
                });
            }
        }
        Ok(())
    }

    /// 所有用户可见的本地化键必须在英文 locale 里存在且非空。
    /// 少一个键，界面上就是一串裸 key，宁可拒装。
    pub fn validate_localization(
        &self,
        english: &BTreeMap<String, String>,
    ) -> Result<(), ManifestError> {
        let mut keys: Vec<&str> = Vec::new();
        for destination in &self.destinations {
            keys.push(&destination.title_key);
        }
        for command in &self.commands {
            keys.push(&command.title_key);
        }
        for setting in &self.settings {
            keys.push(&setting.title_key);
            if let Some(description) = &setting.description_key {
                keys.push(description);
            }
            for option in &setting.options {
                keys.push(option);
            }
        }
        for key in keys {
            if english.get(key).is_none_or(|value| value.trim().is_empty()) {
                return Err(ManifestError::MissingLocalization(key.to_owned()));
            }
        }
        Ok(())
    }

    pub fn page(&self, id: &str) -> Option<&PluginPage> {
        self.pages.iter().find(|item| item.id == id)
    }

    pub fn sidebar(&self, id: &str) -> Option<&PluginSidebar> {
        self.sidebars.iter().find(|item| item.id == id)
    }

    pub fn command(&self, id: &str) -> Option<&PluginCommand> {
        self.commands.iter().find(|item| item.id == id)
    }
}

fn unique_ids<'a>(
    values: impl Iterator<Item = &'a str>,
    kind: &'static str,
) -> Result<BTreeSet<&'a str>, ManifestError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(ManifestError::DuplicateId {
                kind,
                id: value.to_owned(),
            });
        }
    }
    Ok(seen)
}

fn array_of<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a [Value], ManifestError> {
    match value {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(ManifestError::Field {
            field: field.to_owned(),
        }),
    }
}

fn id_array(value: Option<&Value>, field: &str) -> Result<Vec<String>, ManifestError> {
    let mut out = Vec::new();
    for item in array_of(value, field)? {
        let raw = item.as_str().ok_or(ManifestError::Field {
            field: field.to_owned(),
        })?;
        if !is_valid_id(raw) {
            return Err(ManifestError::InvalidId(raw.to_owned()));
        }
        out.push(raw.to_owned());
    }
    Ok(out)
}

fn required_id(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<String, ManifestError> {
    let raw = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(ManifestError::Field {
            field: field.to_owned(),
        })?;
    if !is_valid_id(raw) {
        return Err(ManifestError::InvalidId(raw.to_owned()));
    }
    Ok(raw.to_owned())
}

fn required_key(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<String, ManifestError> {
    let raw = object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| is_valid_key(value))
        .ok_or(ManifestError::Field {
            field: field.to_owned(),
        })?;
    Ok(raw.to_owned())
}

fn optional_string(object: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    object.get(field).and_then(Value::as_str).map(str::to_owned)
}

/// 记下这一层里本宿主不认识的字段名，不中断解析。`path` 是给人看的位置，
/// 比如 `command.handler`，好让 `plugin install` 的提示能指到地方。
fn note_unknown(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    path: &str,
    unsupported: &mut UnsupportedItems,
) {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            unsupported.note("field", &format!("{path}.{key}"));
        }
    }
}

fn object_of<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, Value>, ManifestError> {
    value.as_object().ok_or(ManifestError::Field {
        field: field.to_owned(),
    })
}

fn parse_destination(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<PluginDestination, ManifestError> {
    let object = object_of(value, "destination")?;
    note_unknown(
        object,
        &[
            "id",
            "titleKey",
            "icon",
            "mainPage",
            "companionSidebar",
            "toolbarCommandIDs",
            "defaultPinned",
        ],
        "destination",
        unsupported,
    );
    Ok(PluginDestination {
        id: required_id(object, "id")?,
        title_key: required_key(object, "titleKey")?,
        icon: optional_string(object, "icon"),
        main_page: required_id(object, "mainPage")?,
        companion_sidebar: match object.get("companionSidebar") {
            None => None,
            Some(_) => Some(required_id(object, "companionSidebar")?),
        },
        toolbar_command_ids: id_array(object.get("toolbarCommandIDs"), "toolbarCommandIDs")?,
        default_pinned: object
            .get("defaultPinned")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_resource(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<McpResourceRef, ManifestError> {
    let object = object_of(value, "resource")?;
    note_unknown(object, &["type", "server", "uri"], "resource", unsupported);
    if let Some(kind) = object.get("type").and_then(Value::as_str)
        && kind != "mcpResource"
    {
        return Err(ManifestError::Field {
            field: "resource.type".into(),
        });
    }
    let uri = object
        .get("uri")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ManifestError::Field {
            field: "resource.uri".into(),
        })?;
    Ok(McpResourceRef {
        server: required_id(object, "server")?,
        uri: uri.to_owned(),
    })
}

fn parse_sidebar(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<PluginSidebar, ManifestError> {
    let object = object_of(value, "sidebar")?;
    note_unknown(
        object,
        &["id", "mode", "schema", "resource"],
        "sidebar",
        unsupported,
    );
    let mode = match object.get("mode").and_then(Value::as_str) {
        // 未声明侧栏时默认复用会话列表，与 Swift 侧一致。
        None => SidebarMode::SessionList,
        Some(raw) => SidebarMode::parse(raw).ok_or(ManifestError::Field {
            field: "sidebar.mode".into(),
        })?,
    };
    Ok(PluginSidebar {
        id: required_id(object, "id")?,
        mode,
        schema: optional_string(object, "schema"),
        resource: match object.get("resource") {
            None => None,
            Some(item) => Some(parse_resource(item, unsupported)?),
        },
    })
}

fn parse_page(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<PluginPage, ManifestError> {
    let object = object_of(value, "page")?;
    note_unknown(
        object,
        &[
            "id",
            "runtime",
            "entryPath",
            "schema",
            "server",
            "resourceURI",
        ],
        "page",
        unsupported,
    );
    let id = required_id(object, "id")?;
    let runtime = object
        .get("runtime")
        .and_then(Value::as_str)
        .and_then(PageRuntime::parse)
        .ok_or(ManifestError::Field {
            field: "page.runtime".into(),
        })?;
    let page = PluginPage {
        id: id.clone(),
        runtime,
        entry_path: optional_string(object, "entryPath").filter(|value| !value.is_empty()),
        schema: optional_string(object, "schema").filter(|value| !value.is_empty()),
        server: match object.get("server") {
            None => None,
            Some(_) => Some(required_id(object, "server")?),
        },
        resource_uri: optional_string(object, "resourceURI"),
    };
    match runtime {
        PageRuntime::LocalWeb if page.entry_path.is_none() => {
            return Err(ManifestError::IncompletePage {
                id,
                runtime: "localWeb",
                field: "entryPath",
            });
        }
        PageRuntime::McpApp => {
            if page.server.is_none() {
                return Err(ManifestError::IncompletePage {
                    id,
                    runtime: "mcpApp",
                    field: "server",
                });
            }
            match &page.resource_uri {
                None => {
                    return Err(ManifestError::IncompletePage {
                        id,
                        runtime: "mcpApp",
                        field: "resourceURI",
                    });
                }
                Some(uri) if !uri.starts_with("ui://") => {
                    return Err(ManifestError::InvalidResourceUri(uri.clone()));
                }
                Some(_) => {}
            }
        }
        PageRuntime::Declarative if page.schema.is_none() => {
            return Err(ManifestError::IncompletePage {
                id,
                runtime: "declarative",
                field: "schema",
            });
        }
        _ => {}
    }
    Ok(page)
}

fn parse_command(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<PluginCommand, ManifestError> {
    let object = object_of(value, "command")?;
    note_unknown(
        object,
        &["id", "titleKey", "icon", "handler"],
        "command",
        unsupported,
    );
    let id = required_id(object, "id")?;
    let handler_value = object.get("handler").ok_or(ManifestError::Field {
        field: "command.handler".into(),
    })?;
    let handler_object = object_of(handler_value, "command.handler")?;
    note_unknown(
        handler_object,
        &["type", "action", "server", "tool", "destination"],
        "command.handler",
        unsupported,
    );
    let handler = match handler_object.get("type").and_then(Value::as_str) {
        Some("host") => {
            let raw = handler_object.get("action").and_then(Value::as_str).ok_or(
                ManifestError::Field {
                    field: "handler.action".into(),
                },
            )?;
            match HostAction::parse(raw) {
                Some(action) => CommandHandler::Host { action },
                None => {
                    unsupported.note("hostAction", raw);
                    CommandHandler::Unsupported {
                        detail: format!("host action `{raw}`"),
                    }
                }
            }
        }
        Some("mcpTool") => CommandHandler::McpTool {
            server: required_id(handler_object, "server")?,
            tool: required_id(handler_object, "tool")?,
        },
        Some("navigate") => CommandHandler::Navigate {
            destination: required_id(handler_object, "destination")?,
        },
        Some(other) => {
            unsupported.note("handlerType", other);
            CommandHandler::Unsupported {
                detail: format!("handler type `{other}`"),
            }
        }
        None => {
            return Err(ManifestError::Field {
                field: "handler.type".into(),
            });
        }
    };
    let icon = optional_string(object, "icon");
    if let Some(icon) = &icon
        && !icon.starts_with("sf:")
    {
        return Err(ManifestError::Field {
            field: "command.icon".into(),
        });
    }
    Ok(PluginCommand {
        id,
        title_key: required_key(object, "titleKey")?,
        icon,
        handler,
    })
}

fn parse_setting(
    value: &Value,
    unsupported: &mut UnsupportedItems,
) -> Result<PluginSetting, ManifestError> {
    let object = object_of(value, "setting")?;
    note_unknown(
        object,
        &[
            "id",
            "type",
            "titleKey",
            "descriptionKey",
            "defaultValue",
            "options",
        ],
        "setting",
        unsupported,
    );
    let setting_type = object
        .get("type")
        .and_then(Value::as_str)
        .and_then(SettingType::parse)
        .ok_or(ManifestError::Field {
            field: "setting.type".into(),
        })?;
    let default_value = optional_string(object, "defaultValue");
    // secret 永不带默认值：一个写在清单里的默认密钥就是一个泄漏的密钥。
    if setting_type == SettingType::Secret && default_value.is_some() {
        return Err(ManifestError::Field {
            field: "setting.defaultValue".into(),
        });
    }
    let mut options = Vec::new();
    for item in array_of(object.get("options"), "setting.options")? {
        let raw =
            item.as_str()
                .filter(|value| is_valid_key(value))
                .ok_or(ManifestError::Field {
                    field: "setting.options[]".into(),
                })?;
        options.push(raw.to_owned());
    }
    if setting_type == SettingType::Enum && options.is_empty() {
        return Err(ManifestError::Field {
            field: "setting.options".into(),
        });
    }
    // 默认值必须符合声明的类型，否则设置界面第一次渲染就会自相矛盾。
    if let Some(default) = &default_value {
        let valid = match setting_type {
            SettingType::Number => default.parse::<f64>().is_ok(),
            SettingType::Boolean => matches!(default.as_str(), "true" | "false"),
            SettingType::Enum => options.iter().any(|option| option == default),
            _ => true,
        };
        if !valid {
            return Err(ManifestError::Field {
                field: "setting.defaultValue".into(),
            });
        }
    }
    Ok(PluginSetting {
        id: required_id(object, "id")?,
        setting_type,
        title_key: required_key(object, "titleKey")?,
        description_key: optional_string(object, "descriptionKey"),
        default_value,
        options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "schemaVersion": 1,
        "contributes": {
            "destinations": [{"id":"demo","titleKey":"destination.demo","mainPage":"demo.main"}],
            "pages": [{"id":"demo.main","runtime":"localWeb","entryPath":"ui/index.html"}]
        }
    }"#;

    #[test]
    fn parses_a_minimal_manifest() {
        let manifest = PluginManifest::parse(MINIMAL).expect("minimal manifest parses");
        assert_eq!(manifest.destinations.len(), 1);
        assert_eq!(manifest.pages[0].runtime, PageRuntime::LocalWeb);
        assert!(!manifest.destinations[0].default_pinned);
    }

    #[test]
    fn network_domains_parse_and_lowercase() {
        let source = r#"{
            "schemaVersion": 1,
            "permissions": ["network.access"],
            "networkDomains": ["Some.IM", "*.example.com"],
            "contributes": {
                "destinations": [{"id":"demo","titleKey":"k","mainPage":"demo.main"}],
                "pages": [{"id":"demo.main","runtime":"localWeb","entryPath":"ui/index.html"}]
            }
        }"#;
        let manifest = PluginManifest::parse(source).expect("well-formed domains parse");
        assert_eq!(manifest.network_domains, vec!["some.im", "*.example.com"]);
        assert!(manifest.unsupported.is_empty());
    }

    /// `networkDomains` 是 `net.fetch` 唯一的门，所以它**不**走「认不出就降级」
    /// 那条路：写法不合共享 schema 的 pattern 就拒装。装得上却永远匹配不中，
    /// 插件作者只会看到一个解释不了的 hostNotDeclared。
    #[test]
    fn malformed_network_domains_are_refused() {
        for bad in [
            "*",
            "https://example.com",
            "example.com/path",
            "example.com:443",
            "-bad.example.com",
            "example",
        ] {
            let source = format!(
                r#"{{"schemaVersion":1,"networkDomains":["{bad}"],"contributes":{{
                    "destinations":[{{"id":"demo","titleKey":"k","mainPage":"demo.main"}}],
                    "pages":[{{"id":"demo.main","runtime":"localWeb","entryPath":"ui/index.html"}}]}}}}"#
            );
            assert!(
                PluginManifest::parse(&source).is_err(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn rejects_a_destination_pointing_at_a_missing_page() {
        let dangling = r#"{"schemaVersion":1,"contributes":{
            "destinations":[{"id":"demo","titleKey":"k","mainPage":"nope"}],
            "pages":[{"id":"demo.main","runtime":"localWeb","entryPath":"ui/index.html"}]}}"#;
        assert!(matches!(
            PluginManifest::parse(dangling),
            Err(ManifestError::DanglingReference { .. })
        ));
    }

    #[test]
    fn rejects_unknown_schema_version_but_only_notes_unknown_fields() {
        let source = MINIMAL.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert!(matches!(
            PluginManifest::parse(&source),
            Err(ManifestError::SchemaVersion(2))
        ));
        // schemaVersion 1 之内的新增字段是另一侧先长出来的东西，装得上、
        // 记下来，不能判整个包非法。
        let source = MINIMAL.replace(
            "\"schemaVersion\": 1,",
            "\"schemaVersion\": 1,\"requiredCapabilities\": [\"fs.write\"],",
        );
        let manifest = PluginManifest::parse(&source).expect("unknown root field is not fatal");
        assert!(
            manifest
                .unsupported
                .iter()
                .any(|item| item == "field:requiredCapabilities")
        );
    }

    #[test]
    fn rejects_duplicate_ids() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "pages":[{"id":"a","runtime":"localWeb","entryPath":"x"},
                     {"id":"a","runtime":"localWeb","entryPath":"y"}]}}"#;
        assert!(matches!(
            PluginManifest::parse(source),
            Err(ManifestError::DuplicateId { kind: "page", .. })
        ));
    }

    #[test]
    fn arbitrary_host_actions_are_kept_but_unrunnable() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "commands":[{"id":"c","titleKey":"k","handler":{"type":"host","action":"NSApplication.terminate:"}}]}}"#;
        let manifest = PluginManifest::parse(source).expect("an unknown action is not fatal");
        // 白名单之外的动作绝不执行，但也绝不因此把整个插件拒之门外：
        // 命令留在原地，点下去由宿主回一句「不支持」。
        assert!(matches!(
            manifest.command("c").map(|command| &command.handler),
            Some(CommandHandler::Unsupported { .. })
        ));
        assert!(
            manifest
                .unsupported
                .iter()
                .any(|item| item == "hostAction:NSApplication.terminate:")
        );
    }

    #[test]
    fn the_permission_vocabulary_matches_the_macos_host() {
        // 这份名单是跨仓契约：Xedit 的 docs/plugin-schema/willdeep-plugin.schema.json
        // 里有几项，这里就得认识几项，否则那边装得上的插件这边装不上。
        for raw in [
            "conversation.read",
            "conversation.write",
            "workspace.read",
            "workspace.write",
            "process.execute",
            "network.access",
            "credentials.use",
            "ai.chat",
            "ai.image",
            "providers.read",
            "skills.read",
            "clipboard.write",
            "notifications",
        ] {
            assert_eq!(
                PluginPermission::parse(raw).map(PluginPermission::as_str),
                Some(raw),
                "permission `{raw}` is in the shared schema but unknown here"
            );
        }
        assert_eq!(
            HostAction::parse("session.open"),
            Some(HostAction::SessionOpen)
        );
    }

    #[test]
    fn a_permission_this_host_lacks_does_not_block_installation() {
        let source = MINIMAL.replace(
            "\"schemaVersion\": 1,",
            "\"schemaVersion\": 1,\"permissions\": [\"workspace.read\", \"telepathy.read\"],",
        );
        let manifest = PluginManifest::parse(&source).expect("unknown permission is not fatal");
        assert!(
            manifest
                .permissions
                .contains(&PluginPermission::WorkspaceRead)
        );
        assert_eq!(manifest.permissions.len(), 1);
        assert!(
            manifest
                .unsupported
                .iter()
                .any(|item| item == "permission:telepathy.read")
        );
    }

    #[test]
    fn a_menu_location_this_host_lacks_is_dropped_not_fatal() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "commands":[{"id":"c","titleKey":"k","handler":{"type":"host","action":"plugin.refresh"}}],
            "menus":{"commandPalette":["c"],"editor.gutter":["c"]}}}"#;
        let manifest = PluginManifest::parse(source).expect("unknown menu location is not fatal");
        assert_eq!(manifest.menus.len(), 1);
        assert!(
            manifest
                .menus
                .contains_key(&PluginMenuLocation::CommandPalette)
        );
        assert!(
            manifest
                .unsupported
                .iter()
                .any(|item| item == "menu:editor.gutter")
        );
    }

    #[test]
    fn rejects_mcp_app_pages_without_a_ui_uri() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "pages":[{"id":"p","runtime":"mcpApp","server":"s","resourceURI":"https://example.com"}]}}"#;
        assert!(matches!(
            PluginManifest::parse(source),
            Err(ManifestError::InvalidResourceUri(_))
        ));
    }

    #[test]
    fn rejects_secret_settings_carrying_a_default_value() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "settings":[{"id":"token","type":"secret","titleKey":"k","defaultValue":"hunter2"}]}}"#;
        assert!(PluginManifest::parse(source).is_err());
    }

    #[test]
    fn every_menu_location_in_the_shared_schema_is_understood() {
        // 这条盯着跨仓契约：Xedit 的 AgentPluginMenuLocation.all 增加一项而这里
        // 没跟上时，一侧装得上的插件在另一侧会被判非法。
        for location in PluginMenuLocation::ALL {
            assert_eq!(
                PluginMenuLocation::parse(location.as_str()),
                Some(location),
                "menu location {} does not round-trip",
                location.as_str()
            );
        }
    }

    #[test]
    fn localization_gaps_are_rejected() {
        let manifest = PluginManifest::parse(MINIMAL).expect("parses");
        let empty = BTreeMap::new();
        assert!(matches!(
            manifest.validate_localization(&empty),
            Err(ManifestError::MissingLocalization(_))
        ));
        let mut english = BTreeMap::new();
        english.insert("destination.demo".to_owned(), "Demo".to_owned());
        assert!(manifest.validate_localization(&english).is_ok());
    }

    #[test]
    fn codex_manifest_supplies_identity() {
        let source = r#"{"name":"willdeep-todo","version":"1.1.0",
            "interface":{"displayName":"Todo","composerIcon":"sf:checklist"}}"#;
        let manifest = CodexManifest::parse(source).expect("parses");
        assert_eq!(manifest.id, "willdeep-todo");
        assert_eq!(manifest.display_name.as_deref(), Some("Todo"));
        let bad = r#"{"name":"willdeep-todo","version":"latest"}"#;
        assert!(matches!(
            CodexManifest::parse(bad),
            Err(ManifestError::InvalidVersion(_))
        ));
    }
}
