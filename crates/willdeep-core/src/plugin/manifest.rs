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

/// 本地化键：非空且不含空白。
fn is_valid_key(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
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
    WorkspaceRead,
    WorkspaceWrite,
    ProcessExecute,
    NetworkAccess,
    CredentialsUse,
    AiChat,
    ProvidersRead,
    ClipboardWrite,
    Notifications,
}

impl PluginPermission {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "conversation.read" => Self::ConversationRead,
            "workspace.read" => Self::WorkspaceRead,
            "workspace.write" => Self::WorkspaceWrite,
            "process.execute" => Self::ProcessExecute,
            "network.access" => Self::NetworkAccess,
            "credentials.use" => Self::CredentialsUse,
            "ai.chat" => Self::AiChat,
            "providers.read" => Self::ProvidersRead,
            "clipboard.write" => Self::ClipboardWrite,
            "notifications" => Self::Notifications,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConversationRead => "conversation.read",
            Self::WorkspaceRead => "workspace.read",
            Self::WorkspaceWrite => "workspace.write",
            Self::ProcessExecute => "process.execute",
            Self::NetworkAccess => "network.access",
            Self::CredentialsUse => "credentials.use",
            Self::AiChat => "ai.chat",
            Self::ProvidersRead => "providers.read",
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
}

/// 公开 Host Command v1。任意 selector、类名与脚本文本都被拒绝——白名单是
/// **字面量**，不是"凡是 plugin. 开头"的命名规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAction {
    PluginRefresh,
    DestinationSelect,
    SettingsMcp,
    PluginsOpenCenter,
}

impl HostAction {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "plugin.refresh" => Self::PluginRefresh,
            "destination.select" => Self::DestinationSelect,
            "settings.mcp" => Self::SettingsMcp,
            "plugins.open-center" => Self::PluginsOpenCenter,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PluginRefresh => "plugin.refresh",
            Self::DestinationSelect => "destination.select",
            Self::SettingsMcp => "settings.mcp",
            Self::PluginsOpenCenter => "plugins.open-center",
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
    pub dependencies: PluginDependencies,
    pub destinations: Vec<PluginDestination>,
    pub sidebars: Vec<PluginSidebar>,
    pub pages: Vec<PluginPage>,
    pub commands: Vec<PluginCommand>,
    pub menus: BTreeMap<PluginMenuLocation, Vec<String>>,
    pub settings: Vec<PluginSetting>,
}

const ROOT_FIELDS: [&str; 4] = [
    "schemaVersion",
    "minimumWillDeepVersion",
    "permissions",
    "dependencies",
];

impl PluginManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let value: Value =
            serde_json::from_str(source).map_err(|error| ManifestError::Json(error.to_string()))?;
        let object = value.as_object().ok_or(ManifestError::Field {
            field: "<root>".into(),
        })?;
        for key in object.keys() {
            // `$schema` 是编辑器提示，不是契约的一部分，放行。
            if key != "$schema" && key != "contributes" && !ROOT_FIELDS.contains(&key.as_str()) {
                return Err(ManifestError::UnknownField(key.clone()));
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
            let permission = PluginPermission::parse(raw)
                .ok_or_else(|| ManifestError::UnknownPermission(raw.to_owned()))?;
            manifest.permissions.insert(permission);
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
            manifest.destinations.push(parse_destination(item)?);
        }
        for item in array_of(contributes.get("sidebars"), "contributes.sidebars")? {
            manifest.sidebars.push(parse_sidebar(item)?);
        }
        for item in array_of(contributes.get("pages"), "contributes.pages")? {
            manifest.pages.push(parse_page(item)?);
        }
        for item in array_of(contributes.get("commands"), "contributes.commands")? {
            manifest.commands.push(parse_command(item)?);
        }
        if let Some(menus) = contributes.get("menus") {
            let menus = menus.as_object().ok_or(ManifestError::Field {
                field: "contributes.menus".into(),
            })?;
            for (key, value) in menus {
                let location = PluginMenuLocation::parse(key)
                    .ok_or_else(|| ManifestError::UnknownMenuLocation(key.clone()))?;
                manifest
                    .menus
                    .insert(location, id_array(Some(value), "contributes.menus[]")?);
            }
        }
        for item in array_of(contributes.get("settings"), "contributes.settings")? {
            manifest.settings.push(parse_setting(item)?);
        }

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

fn reject_unknown(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<(), ManifestError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ManifestError::UnknownField(key.clone()));
        }
    }
    Ok(())
}

fn object_of<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, Value>, ManifestError> {
    value.as_object().ok_or(ManifestError::Field {
        field: field.to_owned(),
    })
}

fn parse_destination(value: &Value) -> Result<PluginDestination, ManifestError> {
    let object = object_of(value, "destination")?;
    reject_unknown(
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
    )?;
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

fn parse_resource(value: &Value) -> Result<McpResourceRef, ManifestError> {
    let object = object_of(value, "resource")?;
    reject_unknown(object, &["type", "server", "uri"])?;
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

fn parse_sidebar(value: &Value) -> Result<PluginSidebar, ManifestError> {
    let object = object_of(value, "sidebar")?;
    reject_unknown(object, &["id", "mode", "schema", "resource"])?;
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
            Some(item) => Some(parse_resource(item)?),
        },
    })
}

fn parse_page(value: &Value) -> Result<PluginPage, ManifestError> {
    let object = object_of(value, "page")?;
    reject_unknown(
        object,
        &[
            "id",
            "runtime",
            "entryPath",
            "schema",
            "server",
            "resourceURI",
        ],
    )?;
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

fn parse_command(value: &Value) -> Result<PluginCommand, ManifestError> {
    let object = object_of(value, "command")?;
    reject_unknown(object, &["id", "titleKey", "icon", "handler"])?;
    let id = required_id(object, "id")?;
    let handler_value = object.get("handler").ok_or(ManifestError::Field {
        field: "command.handler".into(),
    })?;
    let handler_object = object_of(handler_value, "command.handler")?;
    reject_unknown(
        handler_object,
        &["type", "action", "server", "tool", "destination"],
    )?;
    let handler = match handler_object.get("type").and_then(Value::as_str) {
        Some("host") => {
            let raw = handler_object.get("action").and_then(Value::as_str).ok_or(
                ManifestError::Field {
                    field: "handler.action".into(),
                },
            )?;
            CommandHandler::Host {
                action: HostAction::parse(raw).ok_or(ManifestError::Field {
                    field: "handler.action".into(),
                })?,
            }
        }
        Some("mcpTool") => CommandHandler::McpTool {
            server: required_id(handler_object, "server")?,
            tool: required_id(handler_object, "tool")?,
        },
        Some("navigate") => CommandHandler::Navigate {
            destination: required_id(handler_object, "destination")?,
        },
        _ => {
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

fn parse_setting(value: &Value) -> Result<PluginSetting, ManifestError> {
    let object = object_of(value, "setting")?;
    reject_unknown(
        object,
        &[
            "id",
            "type",
            "titleKey",
            "descriptionKey",
            "defaultValue",
            "options",
        ],
    )?;
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
    fn rejects_unknown_schema_version_and_fields() {
        let source = MINIMAL.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert!(matches!(
            PluginManifest::parse(&source),
            Err(ManifestError::SchemaVersion(2))
        ));
        let source = MINIMAL.replace(
            "\"schemaVersion\": 1,",
            "\"schemaVersion\": 1,\"extra\": 1,",
        );
        assert!(matches!(
            PluginManifest::parse(&source),
            Err(ManifestError::UnknownField(_))
        ));
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
    fn rejects_arbitrary_host_actions() {
        let source = r#"{"schemaVersion":1,"contributes":{
            "commands":[{"id":"c","titleKey":"k","handler":{"type":"host","action":"NSApplication.terminate:"}}]}}"#;
        assert!(PluginManifest::parse(source).is_err());
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
