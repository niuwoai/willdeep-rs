//! 插件宿主的 Web 面：快照 API、命令派发、页面与资源服务。
//!
//! 与 macOS 宿主的分工完全一致，只是渲染面换成了浏览器：
//!
//! - 页面跑在 `sandbox="allow-scripts"` 的 iframe 里（opaque origin），
//!   拿不到父页面的 DOM / cookie / localStorage。
//! - 页面到宿主只有 postMessage 一条路，父页面再代理到这里的 API。
//! - CSP 的 `connect-src 'none'` 挡掉 fetch / XHR / WebSocket，
//!   所以插件页面自己**够不着**这些接口——它只能请父窗口代劳。
//!
//! CSP 里不用 `'self'`：sandbox 出来的文档是 opaque origin，`'self'` 在那里
//! 不匹配任何东西，脚本会连自己的 js 都加载不了。改用请求 Host 推出来的
//! 显式 origin，效果一样而且真的生效。

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use willdeep_core::plugin::{
    ApprovalGap, CommandOutcome, HostError, PluginHost, PluginPermission, PluginSource,
    qualified_destination,
};
use willdeep_core::{Message, build_provider};

/// 插件自己的浏览器存储上限。游戏最高分、界面偏好这类东西，
/// 256 KiB 绰绰有余；再大就该走插件自己的 MCP 服务落盘。
const MAX_STORAGE_BYTES: usize = 256 * 1024;
/// `ai.complete` 的硬上限，与 macOS 宿主同值。页面报什么都夹到这里面。
const MAX_AI_MESSAGES: usize = 24;
const MAX_AI_CHARS: usize = 32_000;
const MAX_AI_OUTPUT_TOKENS: u32 = 4_096;

const BRIDGE_SCRIPT: &str = include_str!("plugin_bridge.js");

pub(crate) struct PluginWebState {
    pub host: Arc<PluginHost>,
    pub config_path: PathBuf,
    pub home: PathBuf,
    storage_lock: Mutex<()>,
}

impl PluginWebState {
    pub fn new(host: Arc<PluginHost>, config_path: PathBuf, home: PathBuf) -> Self {
        Self {
            host,
            config_path,
            home,
            storage_lock: Mutex::new(()),
        }
    }
}

pub(crate) fn router(state: Arc<PluginWebState>) -> Router {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/{plugin}/approve", post(approve_plugin))
        .route("/api/plugins/{plugin}/enabled", post(set_plugin_enabled))
        .route("/api/plugins/{plugin}/pin", post(pin_plugin))
        .route("/api/plugins/{plugin}", delete(uninstall_plugin))
        .route(
            "/api/plugins/{plugin}/settings/{key}",
            post(update_plugin_setting),
        )
        .route(
            "/api/plugins/{plugin}/sidebars/{sidebar}",
            get(sidebar_document),
        )
        .route(
            "/api/plugins/{plugin}/commands/{command}",
            post(execute_command),
        )
        .route("/api/plugins/{plugin}/mcp/call", post(call_plugin_tool))
        .route(
            "/api/plugins/{plugin}/mcp/resource",
            post(read_plugin_resource),
        )
        .route("/api/plugins/{plugin}/ai/providers", get(ai_providers))
        .route("/api/plugins/{plugin}/ai/complete", post(ai_complete))
        .route(
            "/api/plugins/{plugin}/storage",
            post(write_plugin_storage).delete(clear_plugin_storage),
        )
        .route("/plugin-page/{plugin}/{page}", get(serve_plugin_page))
        .route("/plugin-host/{plugin}/{*path}", get(serve_plugin_asset))
        .with_state(state)
}

// ---------------------------------------------------------------- 快照

#[derive(Serialize)]
struct PluginDestinationView {
    id: String,
    qualified_id: String,
    title: String,
    icon: Option<String>,
    main_page: String,
    page_runtime: String,
    /// localWeb 页面的 iframe 地址；mcpApp / declarative 页面为 None。
    page_url: Option<String>,
    /// mcpApp 页面所属的 MCP 服务。页面的 `tools/call` 与 `resources/read`
    /// 只能落到这一个服务上，不接受页面自报的服务名。
    page_server: Option<String>,
    sidebar: Option<PluginSidebarView>,
    toolbar_commands: Vec<PluginCommandView>,
    default_pinned: bool,
    pinned_order: Option<u32>,
}

#[derive(Serialize)]
struct PluginSidebarView {
    id: String,
    mode: String,
}

#[derive(Serialize)]
struct PluginCommandView {
    id: String,
    title: String,
    icon: Option<String>,
    handler: String,
}

#[derive(Serialize)]
struct PluginSettingView {
    id: String,
    #[serde(rename = "type")]
    setting_type: String,
    title: String,
    description: Option<String>,
    default_value: Option<String>,
    options: Vec<String>,
    /// secret 类型永远不回显当前值，只说有没有设过。
    value: Option<String>,
    configured: bool,
}

#[derive(Serialize)]
struct PluginView {
    id: String,
    name: String,
    version: String,
    description: Option<String>,
    source: String,
    enabled: bool,
    approval_gap: Option<ApprovalGapView>,
    permissions: Vec<String>,
    /// 没有 WillDeep 清单的 Codex 兼容包：按 mcp.json 推断出来的权限。
    inferred_permissions: Vec<String>,
    mcp_servers: Vec<String>,
    destinations: Vec<PluginDestinationView>,
    commands: Vec<PluginCommandView>,
    menus: BTreeMap<String, Vec<String>>,
    settings: Vec<PluginSettingView>,
    /// 内容指纹。从没批准过的包这里是空的——算它要读遍包内容，而那一步
    /// 属于「点批准」的时候，不属于「列个清单」的时候。
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

#[derive(Serialize)]
struct ApprovalGapView {
    reason: &'static str,
    detail: Option<String>,
}

#[derive(Serialize)]
struct PluginFailureView {
    path: String,
    reason: String,
}

#[derive(Serialize)]
struct PluginsResponse {
    plugins: Vec<PluginView>,
    failures: Vec<PluginFailureView>,
}

#[derive(Deserialize)]
struct LocaleQuery {
    #[serde(default)]
    locale: Option<String>,
}

fn gap_view(gap: ApprovalGap) -> ApprovalGapView {
    let detail = match &gap {
        ApprovalGap::VersionChanged { approved } => Some(approved.clone()),
        ApprovalGap::SourceChanged { approved } => Some(approved.clone()),
        ApprovalGap::NewPermissions(added) => Some(added.join(", ")),
        _ => None,
    };
    ApprovalGapView {
        reason: gap.as_str(),
        detail,
    }
}

async fn list_plugins(
    State(state): State<Arc<PluginWebState>>,
    Query(query): Query<LocaleQuery>,
) -> Result<Json<PluginsResponse>, PluginWebError> {
    let locale = query.locale.as_deref().unwrap_or("en");
    let mut plugins = Vec::new();
    for package in state.host.packages() {
        let enabled = state.host.is_enabled(&package.id).await;
        let gap = state.host.approval_gap(&package.id).await?;
        let never_approved = matches!(gap, Some(ApprovalGap::NeverApproved));
        let stored_settings = package_settings(&state, package).await;
        let manifest = package.manifest.as_ref();

        let commands: Vec<PluginCommandView> = manifest
            .map(|manifest| {
                manifest
                    .commands
                    .iter()
                    .map(|command| PluginCommandView {
                        id: command.id.clone(),
                        title: package.localized(&command.title_key, locale),
                        icon: command.icon.clone(),
                        handler: match &command.handler {
                            willdeep_core::plugin::CommandHandler::Host { .. } => "host".into(),
                            willdeep_core::plugin::CommandHandler::McpTool { .. } => {
                                "mcpTool".into()
                            }
                            willdeep_core::plugin::CommandHandler::Navigate { .. } => {
                                "navigate".into()
                            }
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut destinations = Vec::new();
        for destination in manifest
            .map(|item| item.destinations.as_slice())
            .unwrap_or(&[])
        {
            let page = manifest.and_then(|manifest| manifest.page(&destination.main_page));
            let runtime = page
                .map(|page| page.runtime.as_str().to_owned())
                .unwrap_or_else(|| "unknown".to_owned());
            let page_url = page.and_then(|page| {
                page.entry_path.as_ref().map(|entry| {
                    format!(
                        "/plugin-host/{}/{}",
                        urlencoding(&package.id),
                        entry.trim_start_matches('/')
                    )
                })
            });
            let sidebar = destination
                .companion_sidebar
                .as_ref()
                .and_then(|id| manifest.and_then(|manifest| manifest.sidebar(id)))
                .map(|sidebar| PluginSidebarView {
                    id: sidebar.id.clone(),
                    mode: sidebar.mode.as_str().to_owned(),
                });
            destinations.push(PluginDestinationView {
                id: destination.id.clone(),
                qualified_id: qualified_destination(&package.id, &destination.id),
                title: package.localized(&destination.title_key, locale),
                icon: destination.icon.clone(),
                main_page: destination.main_page.clone(),
                page_runtime: runtime,
                page_url,
                page_server: page.and_then(|page| page.server.clone()),
                sidebar,
                toolbar_commands: destination
                    .toolbar_command_ids
                    .iter()
                    .filter_map(|id| commands.iter().find(|command| &command.id == id))
                    .map(|command| PluginCommandView {
                        id: command.id.clone(),
                        title: command.title.clone(),
                        icon: command.icon.clone(),
                        handler: command.handler.clone(),
                    })
                    .collect(),
                default_pinned: destination.default_pinned,
                pinned_order: state.host.pinned_order(&package.id).await,
            });
        }

        let settings = manifest
            .map(|manifest| {
                manifest
                    .settings
                    .iter()
                    .map(|setting| {
                        let stored = stored_settings.get(&setting.id).cloned();
                        let is_secret =
                            setting.setting_type == willdeep_core::plugin::SettingType::Secret;
                        PluginSettingView {
                            id: setting.id.clone(),
                            setting_type: setting.setting_type.as_str().to_owned(),
                            title: package.localized(&setting.title_key, locale),
                            description: setting
                                .description_key
                                .as_ref()
                                .map(|key| package.localized(key, locale)),
                            default_value: setting.default_value.clone(),
                            options: setting
                                .options
                                .iter()
                                .map(|key| package.localized(key, locale))
                                .collect(),
                            configured: stored.is_some(),
                            // secret 不回显：一个能被 GET 回来的密钥等于没存过。
                            value: if is_secret { None } else { stored },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        plugins.push(PluginView {
            id: package.id.clone(),
            name: package.display_name(),
            version: package.version.clone(),
            description: package
                .codex
                .short_description
                .clone()
                .or_else(|| package.codex.description.clone()),
            source: package.source.as_str().to_owned(),
            enabled,
            approval_gap: gap.map(gap_view),
            permissions: manifest
                .map(|manifest| {
                    manifest
                        .permissions
                        .iter()
                        .map(|item| item.as_str().to_owned())
                        .collect()
                })
                .unwrap_or_default(),
            inferred_permissions: willdeep_core::plugin::registry::inferred_permissions(package)
                .into_iter()
                .map(|item| item.as_str().to_owned())
                .collect(),
            mcp_servers: package.mcp_servers.keys().cloned().collect(),
            destinations,
            commands,
            menus: manifest
                .map(|manifest| {
                    manifest
                        .menus
                        .iter()
                        .map(|(location, ids)| (location.as_str().to_owned(), ids.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            settings,
            // 批准过的包在 approval_gap 里已经算过一次，这里走进程内缓存。
            digest: if never_approved {
                None
            } else {
                package.digest().ok()
            },
        });
    }
    Ok(Json(PluginsResponse {
        plugins,
        failures: state
            .host
            .failures()
            .iter()
            .map(|failure| PluginFailureView {
                path: failure.path.display().to_string(),
                reason: failure.reason.clone(),
            })
            .collect(),
    }))
}

async fn package_settings(
    state: &Arc<PluginWebState>,
    package: &willdeep_core::plugin::PluginPackage,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(manifest) = &package.manifest else {
        return out;
    };
    for setting in &manifest.settings {
        if let Some(value) = state.host.setting(&package.id, &setting.id).await {
            out.insert(setting.id.clone(), value);
        }
    }
    out
}

// ---------------------------------------------------------------- 生命周期

#[derive(Deserialize)]
struct EnabledRequest {
    enabled: bool,
}

#[derive(Serialize)]
struct EnabledResponse {
    enabled: bool,
    approval_gap: Option<ApprovalGapView>,
}

async fn approve_plugin(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.approve(&plugin, now_seconds()).await?;
    Ok(Json(json!({"approved": true})))
}

async fn set_plugin_enabled(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<EnabledRequest>,
) -> Result<Json<EnabledResponse>, PluginWebError> {
    match state.host.set_enabled(&plugin, request.enabled).await? {
        Ok(()) => Ok(Json(EnabledResponse {
            enabled: request.enabled,
            approval_gap: None,
        })),
        Err(gap) => Ok(Json(EnabledResponse {
            enabled: false,
            approval_gap: Some(gap_view(gap)),
        })),
    }
}

#[derive(Deserialize)]
struct PinRequest {
    order: Option<u32>,
}

async fn pin_plugin(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<PinRequest>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.set_pinned_order(&plugin, request.order).await?;
    Ok(Json(json!({"order": request.order})))
}

#[derive(Deserialize)]
struct SettingRequest {
    value: Option<String>,
}

async fn update_plugin_setting(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, key)): Path<(String, String)>,
    Json(request): Json<SettingRequest>,
) -> Result<Json<Value>, PluginWebError> {
    let package = state.host.package(&plugin)?;
    let declared = package
        .manifest
        .as_ref()
        .is_some_and(|manifest| manifest.settings.iter().any(|item| item.id == key));
    if !declared {
        return Err(PluginWebError::BadRequest(format!(
            "plugin `{plugin}` does not declare a setting named `{key}`"
        )));
    }
    state
        .host
        .set_setting(&plugin, &key, request.value.as_deref())
        .await?;
    Ok(Json(json!({"saved": true})))
}

/// 卸载：删掉该插件全部已安装版本与本机授权状态。
/// 只允许删共享安装目录里的东西——Codex 缓存与草案不归这个宿主管。
async fn uninstall_plugin(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
) -> Result<Json<Value>, PluginWebError> {
    let package = state.host.package(&plugin)?;
    if package.source != PluginSource::Shared {
        return Err(PluginWebError::BadRequest(format!(
            "plugin `{plugin}` comes from {} and is not managed here",
            package.source.as_str()
        )));
    }
    let root = PluginHost::shared_root(&state.home).join(&plugin);
    let canonical_root = root.canonicalize().map_err(|error| {
        PluginWebError::BadRequest(format!("cannot resolve {}: {error}", root.display()))
    })?;
    let shared = PluginHost::shared_root(&state.home)
        .canonicalize()
        .map_err(|error| PluginWebError::BadRequest(error.to_string()))?;
    // 删除前再确认一次目标确实在共享插件目录之下。一个 `..` 拼进来的
    // plugin id 不该变成 rm -rf 用户主目录。
    if !canonical_root.starts_with(&shared) || canonical_root == shared {
        return Err(PluginWebError::BadRequest(
            "refusing to remove a path outside the plugin directory".to_owned(),
        ));
    }
    std::fs::remove_dir_all(&canonical_root)
        .map_err(|error| PluginWebError::Internal(error.to_string()))?;
    state.host.forget(&plugin).await?;
    Ok(Json(json!({"removed": true})))
}

// ---------------------------------------------------------------- 侧栏与命令

#[derive(Serialize)]
struct SidebarResponse {
    document: Value,
    /// 动态 Resource 读失败并回落到包内 Schema 时，把原因带给界面：
    /// 侧栏该显示"数据是旧的"，而不是假装一切正常。
    degraded: Option<String>,
    strings: BTreeMap<String, String>,
}

async fn sidebar_document(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, sidebar)): Path<(String, String)>,
    Query(query): Query<LocaleQuery>,
) -> Result<Json<SidebarResponse>, PluginWebError> {
    let (document, degraded) = state.host.sidebar_document(&plugin, &sidebar).await?;
    let package = state.host.package(&plugin)?;
    let locale = query.locale.as_deref().unwrap_or("en");
    Ok(Json(SidebarResponse {
        strings: collect_strings(&document.to_value(), package, locale),
        document: document.to_value(),
        degraded,
    }))
}

/// 把文档里出现的所有 `*Key` 一次性翻译好交给前端，省得前端为每个键往回问。
fn collect_strings(
    document: &Value,
    package: &willdeep_core::plugin::PluginPackage,
    locale: &str,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    fn walk(
        value: &Value,
        package: &willdeep_core::plugin::PluginPackage,
        locale: &str,
        out: &mut BTreeMap<String, String>,
    ) {
        match value {
            Value::Object(object) => {
                for (key, item) in object {
                    if key.ends_with("Key")
                        && let Some(name) = item.as_str()
                    {
                        out.insert(name.to_owned(), package.localized(name, locale));
                    }
                    walk(item, package, locale, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, package, locale, out);
                }
            }
            _ => {}
        }
    }
    walk(document, package, locale, &mut out);
    out
}

#[derive(Deserialize)]
struct CommandRequest {
    #[serde(default)]
    arguments: Value,
}

#[derive(Serialize)]
struct CommandResponse {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
}

async fn execute_command(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, command)): Path<(String, String)>,
    Json(request): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, PluginWebError> {
    let arguments = if request.arguments.is_null() {
        json!({})
    } else {
        request.arguments
    };
    Ok(Json(
        match state
            .host
            .execute_command(&plugin, &command, arguments)
            .await?
        {
            CommandOutcome::Host(action) => CommandResponse {
                kind: "host",
                action: Some(action.as_str().to_owned()),
                destination: None,
                result: None,
            },
            CommandOutcome::Navigate { destination } => CommandResponse {
                kind: "navigate",
                action: None,
                destination: Some(destination),
                result: None,
            },
            CommandOutcome::Tool(result) => CommandResponse {
                kind: "tool",
                action: None,
                destination: None,
                result: Some(result),
            },
        },
    ))
}

// ------------------------------------------------- MCP App 的工具与资源

#[derive(Deserialize)]
struct ToolCallRequest {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

/// MCP App 页面的 `tools/call`。只允许**本插件包定义过的服务**——
/// 页面报上来的 server 名要在 mcp.json 里对得上，否则一个页面就能借宿主
/// 的手去敲别的插件的服务。
async fn call_plugin_tool(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<ToolCallRequest>,
) -> Result<Json<Value>, PluginWebError> {
    let package = state.host.package(&plugin)?;
    if !package.mcp_servers.contains_key(&request.server) {
        return Err(PluginWebError::Host(HostError::UnknownServer {
            plugin: plugin.clone(),
            server: request.server.clone(),
        }));
    }
    let mcp = state.host.mcp(&plugin).await?;
    let arguments = if request.arguments.is_null() {
        json!({})
    } else {
        request.arguments
    };
    Ok(Json(
        mcp.call_tool_on(&request.server, &request.tool, arguments)
            .await
            .map_err(|error| PluginWebError::Internal(error.to_string()))?,
    ))
}

#[derive(Deserialize)]
struct ResourceRequest {
    server: String,
    uri: String,
}

async fn read_plugin_resource(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<ResourceRequest>,
) -> Result<Json<Value>, PluginWebError> {
    let package = state.host.package(&plugin)?;
    if !package.mcp_servers.contains_key(&request.server) {
        return Err(PluginWebError::Host(HostError::UnknownServer {
            plugin: plugin.clone(),
            server: request.server.clone(),
        }));
    }
    let mcp = state.host.mcp(&plugin).await?;
    Ok(Json(
        mcp.read_resource(&request.server, &request.uri)
            .await
            .map_err(|error| PluginWebError::Internal(error.to_string()))?,
    ))
}

// ---------------------------------------------------------------- 问模型

#[derive(Serialize)]
struct ProviderView {
    provider_id: String,
    display_name: String,
    is_active: bool,
    is_local: bool,
    models: Vec<String>,
    flash_model: Option<String>,
}

async fn ai_providers(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
) -> Result<Json<Value>, PluginWebError> {
    // ai.chat 隐含这项能力；只想展示模型选择器的插件单独声明 providers.read。
    if state
        .host
        .permits(&plugin, PluginPermission::ProvidersRead)
        .is_err()
    {
        state.host.permits(&plugin, PluginPermission::AiChat)?;
    }
    let config = crate::config::LoadedConfig::load(Some(&state.config_path))
        .map_err(|error| PluginWebError::Internal(error.to_string()))?;
    let file = &config.file;
    let providers: Vec<ProviderView> = file
        .providers
        .iter()
        .map(|(name, profile)| {
            let base = profile.api_base.clone().unwrap_or_default();
            let mut models: Vec<String> = profile.model.iter().cloned().collect();
            if let Some(vision) = &profile.vision_model
                && !models.contains(vision)
            {
                models.push(vision.clone());
            }
            ProviderView {
                provider_id: name.clone(),
                display_name: name.clone(),
                is_active: file.default_provider.as_deref() == Some(name.as_str()),
                is_local: base.contains("localhost") || base.contains("127.0.0.1"),
                flash_model: profile.model.clone(),
                models,
                // baseURL 与 api_key 一个都不给：插件拿得到的只有能标识和展示的字段。
            }
        })
        .collect();
    Ok(Json(json!({"providers": providers})))
}

#[derive(Deserialize)]
struct AiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AiCompleteRequest {
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    messages: Vec<AiMessage>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

/// `window.willdeep.ai.complete`：让插件借宿主的手问一次模型。
///
/// 三条不变量：密钥永不出宿主（页面拿到的只有 provider id 与模型名）、
/// 能力必须在清单里声明过、条数字数与输出上限由宿主收口。页面递上来的
/// baseURL 一律不认——否则插件就能把用户的对话发去任意端点。
async fn ai_complete(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<AiCompleteRequest>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.permits(&plugin, PluginPermission::AiChat)?;
    if request.messages.is_empty() {
        return Err(PluginWebError::BadRequest("emptyRequest".to_owned()));
    }
    if request.messages.len() > MAX_AI_MESSAGES {
        return Err(PluginWebError::BadRequest("tooManyMessages".to_owned()));
    }
    let total: usize = request
        .messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum::<usize>()
        + request
            .system
            .as_deref()
            .map_or(0, |item| item.chars().count());
    if total > MAX_AI_CHARS {
        return Err(PluginWebError::BadRequest("tooLong".to_owned()));
    }

    let config = crate::config::LoadedConfig::load(Some(&state.config_path))
        .map_err(|error| PluginWebError::Internal(error.to_string()))?;
    let file = &config.file;
    let profile_name = match &request.provider {
        Some(name) => {
            if !file.providers.contains_key(name) {
                return Err(PluginWebError::BadRequest("unknownProvider".to_owned()));
            }
            name.clone()
        }
        None => file
            .default_provider
            .clone()
            .or_else(|| file.providers.keys().next().cloned())
            .ok_or_else(|| PluginWebError::BadRequest("unavailable".to_owned()))?,
    };
    let mut provider_config = crate::provider_config_from_profile(file, &profile_name)
        .map_err(|_| PluginWebError::BadRequest("unavailable".to_owned()))?;
    if let Some(model) = &request.model {
        // 模型必须是这个 profile 自己列出来的：否则插件就能借用户的
        // 凭据去点一个更贵、或者根本不该被这条凭据访问的模型。
        let profile = file.providers.get(&profile_name);
        let allowed = profile.is_some_and(|profile| {
            profile.model.as_deref() == Some(model.as_str())
                || profile.vision_model.as_deref() == Some(model.as_str())
        });
        if !allowed {
            return Err(PluginWebError::BadRequest("unknownModel".to_owned()));
        }
        provider_config.model = model.clone();
    }
    provider_config.max_output_tokens = request
        .max_output_tokens
        .unwrap_or(1_024)
        .clamp(1, MAX_AI_OUTPUT_TOKENS);
    let model_name = provider_config.model.clone();

    let provider = build_provider(provider_config)
        .map_err(|_| PluginWebError::BadRequest("unavailable".to_owned()))?;
    let mut messages = Vec::new();
    if let Some(system) = request.system.filter(|item| !item.trim().is_empty()) {
        messages.push(Message::system(system));
    }
    for message in request.messages {
        messages.push(match message.role.as_str() {
            "system" => Message::system(message.content),
            "assistant" => Message::assistant(message.content, Vec::new()),
            _ => Message::user(message.content),
        });
    }
    let completion = provider
        .complete(&messages, &[])
        .await
        .map_err(|error| PluginWebError::BadRequest(format!("unavailable: {error}")))?;
    let text = completion.content.trim().to_owned();
    if text.is_empty() {
        return Err(PluginWebError::BadRequest("emptyResponse".to_owned()));
    }
    Ok(Json(json!({
        "text": text,
        "model": model_name,
        "providerID": profile_name,
    })))
}

// ---------------------------------------------------------------- 页面存储

fn storage_path(home: &FsPath, plugin: &str) -> PathBuf {
    // 插件 ID 已经过 `is_valid_id` 校验（无 `/`、无 `..`），这里再套一层
    // 文件名净化，免得日后校验放宽时这里成为一个目录穿越点。
    let safe: String = plugin
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || matches!(item, '_' | '-' | '.') {
                item
            } else {
                '_'
            }
        })
        .collect();
    home.join("plugin-web-storage").join(format!("{safe}.json"))
}

fn read_storage(home: &FsPath, plugin: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(storage_path(home, plugin))
        .ok()
        .and_then(|source| serde_json::from_str(&source).ok())
        .unwrap_or_default()
}

fn write_storage(
    home: &FsPath,
    plugin: &str,
    data: &BTreeMap<String, String>,
) -> Result<(), PluginWebError> {
    let path = storage_path(home, plugin);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| PluginWebError::Internal(error.to_string()))?;
    }
    let source =
        serde_json::to_string(data).map_err(|error| PluginWebError::Internal(error.to_string()))?;
    if source.len() > MAX_STORAGE_BYTES {
        return Err(PluginWebError::BadRequest(
            "storage quota exceeded".to_owned(),
        ));
    }
    std::fs::write(&path, source).map_err(|error| PluginWebError::Internal(error.to_string()))
}

#[derive(Deserialize)]
struct StorageWrite {
    key: String,
    value: Option<String>,
}

async fn write_plugin_storage(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<StorageWrite>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.package(&plugin)?;
    let _guard = state.storage_lock.lock().await;
    let mut data = read_storage(&state.home, &plugin);
    match request.value {
        Some(value) => {
            if value.len() > MAX_STORAGE_BYTES {
                return Err(PluginWebError::BadRequest("value too large".to_owned()));
            }
            data.insert(request.key, value);
        }
        None => {
            data.remove(&request.key);
        }
    }
    write_storage(&state.home, &plugin, &data)?;
    Ok(Json(json!({"saved": true})))
}

async fn clear_plugin_storage(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.package(&plugin)?;
    let _guard = state.storage_lock.lock().await;
    write_storage(&state.home, &plugin, &BTreeMap::new())?;
    Ok(Json(json!({"cleared": true})))
}

// ---------------------------------------------------------------- 页面服务

/// 页面用的 CSP。
///
/// `'self'` 在 opaque origin（sandbox 无 allow-same-origin）里不匹配任何东西，
/// 所以这里用请求 Host 推出来的显式 origin。`connect-src 'none'` 是关键的一条：
/// 页面因此够不着任何网络端点，包括宿主自己的 API——想让宿主做事只能走 bridge。
fn content_security_policy(origin: &str) -> String {
    format!(
        "default-src 'none'; \
         script-src {origin} 'unsafe-inline' blob: data:; \
         style-src {origin} 'unsafe-inline' blob: data:; \
         img-src {origin} data: blob:; \
         font-src {origin} data: blob:; \
         media-src {origin} data: blob:; \
         connect-src 'none'; frame-src 'none'; child-src 'none'; \
         object-src 'none'; base-uri 'none'; form-action 'none'"
    )
}

/// 沙箱 iframe 是 opaque origin，它发出的请求带 `Origin: null`。
///
/// 这件事会咬人是因为 Vite 的产物默认写 `<script type="module" crossorigin>`
/// 和 `<link rel="stylesheet" crossorigin>`——带 crossorigin 的请求走 CORS，
/// 没有 `Access-Control-Allow-Origin` 就整个被拒，页面白屏，而且 CSP 面板上
/// 什么违规都看不到。macOS 宿主碰不到这一条：那边是自定义 scheme 加载，不是
/// 沙箱 iframe。
///
/// 只放行 `null`，不写 `*`：普通网页有真实 origin，拿不到本机插件包的内容。
fn sandbox_cors(headers: &HeaderMap) -> Option<(header::HeaderName, String)> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())?;
    (origin == "null").then(|| (header::ACCESS_CONTROL_ALLOW_ORIGIN, "null".to_owned()))
}

fn request_origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1");
    // 本地宿主一律 http；反代到 https 的部署里浏览器会把 http: 源视作
    // 混合内容并拦下，所以两种 scheme 都列上，由浏览器挑匹配的那个。
    format!("http://{host} https://{host}")
}

fn html_response(body: String, headers: &HeaderMap) -> Response {
    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
            (
                header::CONTENT_SECURITY_POLICY,
                content_security_policy(&request_origin(headers)),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        body,
    )
        .into_response();
    apply_sandbox_cors(&mut response, headers);
    response
}

fn apply_sandbox_cors(response: &mut Response, headers: &HeaderMap) {
    if let Some((name, value)) = sandbox_cors(headers)
        && let Ok(value) = value.parse()
    {
        response.headers_mut().insert(name, value);
    }
}

/// 把宿主桥注入页面的 `<head>`。找不到 `<head>` 就自己包一层——
/// 插件页面不一定是完整文档，MCP App 资源尤其常是个片段。
fn compose_page(source: &str, storage: &BTreeMap<String, String>) -> String {
    let storage_json = serde_json::to_string(storage).unwrap_or_else(|_| "{}".to_owned());
    let bootstrap = format!(
        "<script>window.__WILLDEEP_STORAGE__ = {storage_json};</script>\n<script>{BRIDGE_SCRIPT}</script>"
    );
    let lowered = source.to_ascii_lowercase();
    if let Some(start) = lowered.find("<head")
        && let Some(offset) = source[start..].find('>')
    {
        let split = start + offset + 1;
        return format!("{}{bootstrap}{}", &source[..split], &source[split..]);
    }
    format!("<!doctype html><html><head>{bootstrap}</head><body>{source}</body></html>")
}

/// mcpApp / declarative 页面：文档不在包里，从 MCP 资源读。
async fn serve_plugin_page(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, page)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, PluginWebError> {
    let source = state.host.read_page_resource(&plugin, &page).await?;
    let storage = read_storage(&state.home, &plugin);
    Ok(html_response(compose_page(&source, &storage), &headers))
}

/// localWeb 页面与它的包内资源。
async fn serve_plugin_asset(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, PluginWebError> {
    // 停用的插件连静态资源都不给：一个被停掉的插件不该还能在页面里活着。
    if !state.host.is_enabled(&plugin).await {
        return Err(PluginWebError::Host(HostError::NotEnabled(plugin)));
    }
    let package = state.host.package(&plugin)?;
    let bytes = package
        .read_resource(&path, willdeep_core::plugin::package::MAX_PAGE_BYTES)
        .map_err(|error| PluginWebError::Host(HostError::Package(error)))?;
    let mime = mime_for(&path);
    if mime == "text/html" {
        let source = String::from_utf8(bytes)
            .map_err(|_| PluginWebError::BadRequest("page is not valid UTF-8".to_owned()))?;
        let storage = read_storage(&state.home, &plugin);
        return Ok(html_response(compose_page(&source, &storage), &headers));
    }
    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (
                header::CONTENT_SECURITY_POLICY,
                content_security_policy(&request_origin(&headers)),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Body::from(bytes),
    )
        .into_response();
    apply_sandbox_cors(&mut response, &headers);
    Ok(response)
}

fn mime_for(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "txt" | "md" => "text/plain; charset=utf-8",
        // 认不出来的东西一律当字节流下发，绝不让浏览器自己去嗅。
        _ => "application/octet-stream",
    }
}

fn urlencoding(value: &str) -> String {
    value
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || matches!(item, '-' | '_' | '.' | '~') {
                item.to_string()
            } else {
                format!("%{:02X}", item as u32)
            }
        })
        .collect()
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 错误

pub(crate) enum PluginWebError {
    Host(HostError),
    BadRequest(String),
    Internal(String),
}

impl From<HostError> for PluginWebError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}

impl IntoResponse for PluginWebError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Host(HostError::UnknownPlugin(id)) => {
                (StatusCode::NOT_FOUND, format!("unknown plugin: {id}"))
            }
            Self::Host(HostError::UnknownContribution { .. }) => {
                (StatusCode::NOT_FOUND, "unknown contribution".to_owned())
            }
            Self::Host(error @ HostError::NotEnabled(_)) => {
                (StatusCode::CONFLICT, error.to_string())
            }
            Self::Host(error @ HostError::PermissionDenied { .. }) => {
                (StatusCode::FORBIDDEN, error.to_string())
            }
            Self::Host(error) => (StatusCode::BAD_GATEWAY, error.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_lands_inside_an_existing_head() {
        let page = "<!doctype html><html><head><title>x</title></head><body>hi</body></html>";
        let composed = compose_page(page, &BTreeMap::new());
        let head = composed.find("<head>").expect("head");
        let title = composed.find("<title>").expect("title");
        let bridge = composed.find("window.willdeep").expect("bridge");
        assert!(
            head < bridge && bridge < title,
            "bridge must run before page scripts"
        );
    }

    #[test]
    fn fragments_without_a_head_are_wrapped() {
        let composed = compose_page("<div>fragment</div>", &BTreeMap::new());
        assert!(composed.starts_with("<!doctype html>"));
        assert!(composed.contains("window.willdeep"));
        assert!(composed.contains("<div>fragment</div>"));
    }

    #[test]
    fn storage_snapshot_is_injected_for_the_shim() {
        let mut storage = BTreeMap::new();
        storage.insert("arcade.best.tetris".to_owned(), "4200".to_owned());
        let composed = compose_page("<html><head></head><body></body></html>", &storage);
        assert!(composed.contains("__WILLDEEP_STORAGE__"));
        assert!(composed.contains("arcade.best.tetris"));
    }

    #[test]
    fn the_policy_blocks_every_network_egress_the_page_could_attempt() {
        let policy = content_security_policy("http://127.0.0.1:8787");
        // connect-src 是这份策略的重点：页面因此够不着任何端点，
        // 包括宿主自己的 API——要宿主做事只能走 bridge。
        assert!(policy.contains("connect-src 'none'"));
        assert!(policy.contains("form-action 'none'"));
        assert!(policy.contains("frame-src 'none'"));
        assert!(policy.contains("object-src 'none'"));
        // 'self' 在 opaque origin 里不匹配任何东西，用了等于页面加载不了自己的脚本。
        assert!(!policy.contains("'self'"));
    }

    #[test]
    fn unknown_extensions_are_never_sniffed() {
        assert_eq!(mime_for("ui/dist/index.html"), "text/html");
        assert_eq!(mime_for("a/b/c.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("icons/tag.svg"), "image/svg+xml");
        assert_eq!(mime_for("weird.xyz"), "application/octet-stream");
        assert_eq!(mime_for("noextension"), "application/octet-stream");
    }

    #[test]
    fn only_opaque_origins_get_a_cors_header() {
        // 沙箱 iframe 报 `Origin: null`，Vite 产物的 crossorigin 脚本非它不可；
        // 而任何带真实 origin 的网页都不该能读到本机插件包的内容。
        let mut sandboxed = HeaderMap::new();
        sandboxed.insert(header::ORIGIN, "null".parse().expect("header"));
        assert_eq!(
            sandbox_cors(&sandboxed).map(|(_, value)| value),
            Some("null".to_owned())
        );

        let mut foreign = HeaderMap::new();
        foreign.insert(
            header::ORIGIN,
            "https://evil.example".parse().expect("header"),
        );
        assert!(sandbox_cors(&foreign).is_none());

        // 同源请求根本不带 Origin，也就不需要放行头。
        assert!(sandbox_cors(&HeaderMap::new()).is_none());
    }

    #[test]
    fn storage_paths_stay_inside_the_storage_directory() {
        let home = FsPath::new("/home/.willdeep");
        let path = storage_path(home, "../../etc/passwd");
        assert!(path.starts_with("/home/.willdeep/plugin-web-storage"));
        // 分隔符与 `..` 段都被净化掉了，结果只能是目录下的一个文件名。
        assert_eq!(path.components().count(), 5);
        assert!(
            !path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(".._.._etc_passwd.json")
        );
    }
}
