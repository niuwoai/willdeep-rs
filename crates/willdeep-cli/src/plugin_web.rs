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
    /// 与聊天端**同一份**工作区白名单（同一个 Arc，不是副本）。
    /// `window.willdeep.fs.*` 与 `process.run` 的边界就是它。
    pub workspaces: Arc<std::sync::RwLock<Vec<PathBuf>>>,
    storage_lock: Mutex<()>,
    /// 在跑的 `ai.complete`，按页面给的 streamID 索引。没传 streamID 的
    /// 请求停不了——页面手上没有别的把手，这一点与 macOS 宿主一致。
    ai_streams: Mutex<BTreeMap<String, tokio::sync::oneshot::Sender<()>>>,
}

impl PluginWebState {
    pub fn new(
        host: Arc<PluginHost>,
        config_path: PathBuf,
        home: PathBuf,
        workspaces: Arc<std::sync::RwLock<Vec<PathBuf>>>,
    ) -> Self {
        Self {
            host,
            config_path,
            home,
            workspaces,
            storage_lock: Mutex::new(()),
            ai_streams: Mutex::new(BTreeMap::new()),
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
        .route("/api/plugins/{plugin}/ai/cancel", post(ai_cancel))
        .route(
            "/api/plugins/{plugin}/ai/image",
            post(crate::plugin_capabilities::ai_generate_image),
        )
        .route(
            "/api/plugins/{plugin}/skills",
            get(crate::plugin_capabilities::skills_list),
        )
        .route(
            "/api/plugins/{plugin}/fs/{action}",
            post(crate::plugin_capabilities::fs_endpoint),
        )
        .route(
            "/api/plugins/{plugin}/process/run",
            post(crate::plugin_capabilities::process_run),
        )
        .route(
            "/api/plugins/{plugin}/net/fetch",
            post(crate::plugin_capabilities::net_fetch),
        )
        .route(
            "/api/plugins/{plugin}/host/{action}",
            post(crate::plugin_capabilities::host_action),
        )
        .route(
            "/api/plugins/{plugin}/storage",
            get(read_plugin_storage)
                .post(write_plugin_storage)
                .delete(clear_plugin_storage),
        )
        .route("/api/plugins/{plugin}/files", post(upload_plugin_file))
        .route("/plugin-media/{plugin}/{file}", get(serve_plugin_media))
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
    /// 本宿主还不认识的清单词汇（`permission:x` / `hostAction:y` / `menu:z`）。
    /// 与 macOS 宿主同形：包照装，界面标明这几条本宿主不支持。
    unsupported: Vec<String>,
    /// 要由浏览器弹文件框、而不是交给 MCP 服务的命令。
    file_picker_commands: Vec<String>,
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
                            // 另一侧宿主才有的处理方式：命令照列，前端据此
                            // 置灰，而不是让用户点一个永远没反应的菜单项。
                            willdeep_core::plugin::CommandHandler::Unsupported { .. } => {
                                "unsupported".into()
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
            unsupported: manifest
                .map(|manifest| {
                    manifest
                        .unsupported
                        .iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            file_picker_commands: manifest
                .map(|manifest| {
                    manifest
                        .commands
                        .iter()
                        .filter(|command| match &command.handler {
                            willdeep_core::plugin::CommandHandler::McpTool { server, tool } => {
                                intercepts_file_picker(&package.id, server, tool)
                            }
                            _ => false,
                        })
                        .map(|command| command.id.clone())
                        .collect()
                })
                .unwrap_or_default(),
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
    // 「选文件」类命令在这里改道，**不进** MCP 服务：那边只会去弹一个
    // 没人看得见的原生框（界面在浏览器里，服务可能在另一台机器上），
    // 然后一直等到超时。浏览器选好、上传落地之后，宿主直接合成那个工具
    // 本该返回的结果。
    {
        let package = state.host.package(&plugin)?;
        if command_intercepts_file_picker(package, &plugin, &command) {
            return Ok(Json(file_picker_response(&state, &plugin, &arguments)?));
        }
    }
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
    /// 插件媒体目录里的本地图片路径，只认 user 消息。见 `plugin_ai_media`。
    #[serde(default, rename = "imagePaths")]
    image_paths: Vec<String>,
    /// 本宿主不支持视频（没有解码器抽帧），带了就拒，不静默丢。
    #[serde(default, rename = "videoPaths")]
    video_paths: Vec<String>,
}

/// 附件在起模型之前全部校验完：视频不支持、数量上限、只认 user 消息。
/// 路径钳制与解码放在拼消息那一步，同样在请求发出之前。返回图片总数。
fn media_attachment_count(messages: &[AiMessage]) -> Result<usize, &'static str> {
    if messages
        .iter()
        .any(|message| !crate::plugin_ai_media::clean_paths(&message.video_paths).is_empty())
    {
        return Err("videosUnsupported");
    }
    let count: usize = messages
        .iter()
        .map(|message| crate::plugin_ai_media::clean_paths(&message.image_paths).len())
        .sum();
    if count > crate::plugin_ai_media::MAX_IMAGES {
        return Err("tooManyImages");
    }
    // 与拼消息时的角色判定一致：只有 system / assistant 算非 user。
    if messages.iter().any(|message| {
        matches!(message.role.as_str(), "system" | "assistant")
            && !crate::plugin_ai_media::clean_paths(&message.image_paths).is_empty()
    }) {
        return Err("mediaOnNonUserMessage");
    }
    Ok(count)
}

#[derive(Deserialize)]
struct AiCancelRequest {
    #[serde(default, rename = "streamID")]
    stream_id: String,
}

/// `window.willdeep.ai.cancel`。按 `complete()` 里传的 streamID 找那一条。
///
/// 没传 streamID 的请求停不了，如实回 `{"cancelled": false}`——报成成功
/// 只会让页面以为停住了，然后继续等一个还在跑的回答。
async fn ai_cancel(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<AiCancelRequest>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.permits(&plugin, PluginPermission::AiChat)?;
    let key = stream_key(&plugin, request.stream_id.trim());
    let sender = state.ai_streams.lock().await.remove(&key);
    let cancelled = sender.is_some_and(|sender| sender.send(()).is_ok());
    Ok(Json(json!({"cancelled": cancelled})))
}

fn stream_key(plugin: &str, stream_id: &str) -> String {
    format!("{plugin}\u{0}{stream_id}")
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
    /// 页面给这一轮起的名字，`ai.cancel` 按它停。不传就停不了。
    #[serde(default, rename = "streamID")]
    stream_id: Option<String>,
    /// 技能 identifier。正文由**宿主**读出来注入，页面既拿不到正文也拿不到
    /// 磁盘路径。需要 skills.read。
    #[serde(default)]
    skills: Vec<String>,
    /// 页面声明的工具。宿主只负责把声明递给模型、把模型的调用请求交回页面，
    /// **不替页面执行**任何一个——执行发生在插件自己的代码里。
    #[serde(default)]
    tools: Vec<AiToolDefinition>,
}

/// 与 macOS 宿主同值：一轮最多 3 个技能、4 个工具。放宽等于让插件把整个
/// 上下文预算吃掉，而这两项都是它自己声明的。
const MAX_AI_SKILLS: usize = 3;
const MAX_AI_TOOLS: usize = 4;

#[derive(Deserialize)]
struct AiToolDefinition {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Option<Value>,
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
    let image_count = media_attachment_count(&request.messages)
        .map_err(|code| PluginWebError::BadRequest(code.to_owned()))?;

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

    if request.skills.len() > MAX_AI_SKILLS {
        return Err(PluginWebError::BadRequest("tooManySkills".to_owned()));
    }
    if request.tools.len() > MAX_AI_TOOLS {
        return Err(PluginWebError::BadRequest("tooManyTools".to_owned()));
    }
    // 技能正文在宿主这一侧读出来拼进 system，页面全程接触不到文件。
    let mut skill_text = String::new();
    if !request.skills.is_empty() {
        state.host.permits(&plugin, PluginPermission::SkillsRead)?;
        let roots = state
            .workspaces
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let root = roots
            .first()
            .cloned()
            .ok_or_else(|| PluginWebError::BadRequest("noWorkspace".to_owned()))?;
        let catalog = willdeep_core::SkillCatalog::discover(&root, &[]);
        for identifier in &request.skills {
            let body = catalog
                .read(identifier, None)
                .map_err(|_| PluginWebError::BadRequest(format!("unknownSkill: {identifier}")))?;
            skill_text.push_str(&body);
            skill_text.push('\n');
        }
    }
    let tools: Vec<willdeep_core::types::ToolDefinition> = request
        .tools
        .iter()
        .map(|tool| willdeep_core::types::ToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool
                .parameters
                .clone()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        })
        .collect();

    let mut messages = Vec::new();
    if let Some(system) = request.system.filter(|item| !item.trim().is_empty()) {
        messages.push(Message::system(system));
    }
    if !skill_text.trim().is_empty() {
        messages.push(Message::system(skill_text));
    }
    let media_root = if image_count > 0 {
        Some(crate::plugin_capabilities::plugin_media_directory(
            &state.home,
            &plugin,
        )?)
    } else {
        None
    };
    for message in request.messages {
        let image_paths = crate::plugin_ai_media::clean_paths(&message.image_paths);
        messages.push(match message.role.as_str() {
            "system" => Message::system(message.content),
            "assistant" => Message::assistant(message.content, Vec::new()),
            _ => match (&media_root, image_paths.is_empty()) {
                (Some(root), false) => {
                    let mut attachments = Vec::with_capacity(image_paths.len());
                    for raw in &image_paths {
                        let path = crate::plugin_ai_media::clamp(raw, root)
                            .map_err(|error| PluginWebError::BadRequest(error.code()))?;
                        attachments.push(
                            crate::plugin_ai_media::image_attachment(&path)
                                .map_err(|error| PluginWebError::BadRequest(error.code()))?,
                        );
                    }
                    Message::user_with_attachments(message.content, attachments)
                }
                _ => Message::user(message.content),
            },
        });
    }
    // 注册可取消句柄。页面不传 streamID 就没有把手，这一轮跑到底为止。
    let cancel_key = request
        .stream_id
        .as_deref()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| stream_key(&plugin, item));
    let mut cancel_rx = match &cancel_key {
        Some(key) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.ai_streams.lock().await.insert(key.clone(), tx);
            Some(rx)
        }
        None => None,
    };
    let completion = {
        let call = provider.complete(&messages, &tools);
        let outcome = match cancel_rx.as_mut() {
            Some(rx) => tokio::select! {
                result = call => Some(result),
                _ = rx => None,
            },
            None => Some(call.await),
        };
        if let Some(key) = &cancel_key {
            state.ai_streams.lock().await.remove(key);
        }
        match outcome {
            Some(result) => result
                .map_err(|error| PluginWebError::BadRequest(format!("unavailable: {error}")))?,
            None => return Err(PluginWebError::BadRequest("cancelled".to_owned())),
        }
    };
    let text = completion.content.trim().to_owned();
    // 只发了工具调用、正文为空是**正常**的一轮，不是空响应：按空响应报错
    // 会把整条工具链在第一步掐断。
    if text.is_empty() && completion.tool_calls.is_empty() {
        return Err(PluginWebError::BadRequest("emptyResponse".to_owned()));
    }
    let tool_calls: Vec<Value> = completion
        .tool_calls
        .iter()
        .map(|call| {
            json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.parsed_arguments().unwrap_or_else(|_| json!({})),
            })
        })
        .collect();
    Ok(Json(json!({
        "text": text,
        "model": model_name,
        "providerID": profile_name,
        "toolCalls": tool_calls,
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

/// 结构化存储（`window.willdeep.storage.*`）在同一份文件里的键前缀。
///
/// localStorage 垫片写的是裸键，两套 API 共用一个文件；不分开的话，一个
/// 插件同时用两套 API 就会互相覆盖，而且垫片会把 JSON 当字符串吐回去。
/// 前缀里的控制字符是故意的：合法的 localStorage 键不会长这样。
const STORE_PREFIX: &str = "\u{1}store:";

#[derive(Deserialize)]
struct StorageWrite {
    key: String,
    value: Option<String>,
    /// `"store"` 是结构化 API，缺省是 localStorage 垫片。
    #[serde(default)]
    scope: Option<String>,
    /// 结构化 API 的值：任意 JSON。垫片走上面的 `value`。
    #[serde(default)]
    json: Option<Value>,
}

#[derive(Deserialize)]
struct StorageQuery {
    #[serde(default)]
    key: Option<String>,
}

/// `window.willdeep.storage.get` / `.keys`。带 key 回一条，不带回键名清单。
async fn read_plugin_storage(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Query(query): Query<StorageQuery>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.package(&plugin)?;
    let data = read_storage(&state.home, &plugin);
    match query.key {
        Some(key) => {
            let stored = data
                .get(&format!("{STORE_PREFIX}{key}"))
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .unwrap_or(Value::Null);
            Ok(Json(json!({"value": stored})))
        }
        None => {
            let keys: Vec<&str> = data
                .keys()
                .filter_map(|key| key.strip_prefix(STORE_PREFIX))
                .collect();
            Ok(Json(json!({"keys": keys})))
        }
    }
}

async fn write_plugin_storage(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<StorageWrite>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.package(&plugin)?;
    let _guard = state.storage_lock.lock().await;
    let mut data = read_storage(&state.home, &plugin);
    let structured = request.scope.as_deref() == Some("store");
    let key = if structured {
        format!("{STORE_PREFIX}{}", request.key)
    } else {
        request.key.clone()
    };
    let value = if structured {
        request
            .json
            .map(|item| serde_json::to_string(&item).unwrap_or_else(|_| "null".to_owned()))
    } else {
        request.value
    };
    match value {
        Some(value) => {
            if value.len() > MAX_STORAGE_BYTES {
                return Err(PluginWebError::BadRequest("value too large".to_owned()));
            }
            data.insert(key, value);
        }
        None => {
            data.remove(&key);
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

// ---------------------------------------------------------------- 远程选文件

/// 上传上限。参照图这类东西 20 MiB 足够，再大就该走插件自己的服务。
const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// **本宿主要接管的「选文件」工具。**
///
/// macOS 宿主上，这些工具在 MCP 服务里 `osascript` 弹一个原生选择框。
/// rs 的界面在浏览器里、服务可能跑在另一台机器上，那条路根本不成立：
/// 弹出来的框（如果有）在服务器的屏幕上，用户看不见。
///
/// 所以这里把这类调用拦下来，改成「浏览器选文件 → 上传到本插件隔离的
/// 媒体目录 → 把落地的**服务端绝对路径**当作选择结果交回去」。插件包
/// 一行不用改：它拿到的仍然是一个能给后续工具用的路径。
///
/// 做成表而不是 if-else，是因为下一个要选文件的插件只该加一行，不该
/// 再写一遍这段逻辑。三元组是 (插件 ID, MCP 服务, 工具名)。
const FILE_PICKER_TOOLS: [(&str, &str, &str); 1] = [(
    "willdeep-video-studio",
    "video-studio",
    "video.pick_reference",
)];

pub(crate) fn intercepts_file_picker(plugin: &str, server: &str, tool: &str) -> bool {
    FILE_PICKER_TOOLS
        .iter()
        .any(|(id, service, name)| *id == plugin && *service == server && *name == tool)
}

/// 命令 ID 走的是同一张表：命令的 handler 指向某个 MCP 工具，
/// 拦截要发生在「派发之前」，否则请求已经进了 MCP 服务，那边只会去弹
/// 一个没人看得见的框，然后超时。
pub(crate) fn command_intercepts_file_picker(
    package: &willdeep_core::plugin::PluginPackage,
    plugin: &str,
    command_id: &str,
) -> bool {
    let Some(command) = package
        .manifest
        .as_ref()
        .and_then(|manifest| manifest.command(command_id))
    else {
        return false;
    };
    match &command.handler {
        willdeep_core::plugin::CommandHandler::McpTool { server, tool } => {
            intercepts_file_picker(plugin, server, tool)
        }
        _ => false,
    }
}

/// 合成被拦截的那个工具本该返回的结果。
///
/// 外面再包一层 MCP 的 `content[0].text`：插件解析的是那一层，直接给业务
/// 对象的话，`payload.ok` 永远是 undefined，每条命令都会被当成失败。
fn file_picker_response(
    state: &PluginWebState,
    plugin: &str,
    arguments: &Value,
) -> Result<CommandResponse, PluginWebError> {
    let raw = arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        // 没带路径说明调用方没走「浏览器选文件」那一步。如实报错，
        // 不要退回去调那个在本宿主上必然失败的工具。
        .ok_or_else(|| PluginWebError::BadRequest("filePickerRequired".to_owned()))?;
    let directory = crate::plugin_capabilities::plugin_media_directory(&state.home, plugin)?;
    // 路径必须是本插件媒体目录里刚落地的那一份。页面自报一个
    // `/etc/passwd` 就能把任意文件喂给后续工具——这道门关在这里。
    let canonical = std::path::Path::new(raw)
        .canonicalize()
        .map_err(|_| PluginWebError::BadRequest("invalidSelection".to_owned()))?;
    let root = directory
        .canonicalize()
        .map_err(|error| PluginWebError::Internal(error.to_string()))?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err(PluginWebError::BadRequest("invalidSelection".to_owned()));
    }
    let payload = json!({"ok": true, "path": canonical.display().to_string()});
    Ok(CommandResponse {
        kind: "tool",
        action: None,
        destination: None,
        result: Some(json!({
            "content": [{"type": "text", "text": payload.to_string()}],
        })),
    })
}

#[derive(Deserialize)]
struct UploadRequest {
    #[serde(default)]
    name: String,
    /// base64 的文件内容。走 JSON 而不是 multipart：上传这条路只有宿主
    /// 页面会走，省一个解析器就少一处攻击面。
    #[serde(default)]
    data: String,
}

/// 浏览器选好的文件落到服务端。回的是**服务端绝对路径**——这正是被拦截的
/// 那个工具原本要返回的东西，插件因此不用知道文件从哪来。
async fn upload_plugin_file(
    State(state): State<Arc<PluginWebState>>,
    Path(plugin): Path<String>,
    Json(request): Json<UploadRequest>,
) -> Result<Json<Value>, PluginWebError> {
    state.host.package(&plugin)?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(request.data.as_bytes())
        .map_err(|_| PluginWebError::BadRequest("invalidData".to_owned()))?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(PluginWebError::BadRequest("invalidSize".to_owned()));
    }
    // 文件名由浏览器给，只取扩展名并净化：名字里的路径分隔符和 `..`
    // 一个都不许活到落盘那一刻。
    let extension = std::path::Path::new(&request.name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| value.len() <= 8 && value.chars().all(|item| item.is_ascii_alphanumeric()))
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let directory = crate::plugin_capabilities::plugin_media_directory(&state.home, &plugin)?;
    let filename = format!(
        "upload-{}.{extension}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or_default()
    );
    let file = directory.join(&filename);
    std::fs::write(&file, &bytes).map_err(|error| PluginWebError::Internal(error.to_string()))?;
    Ok(Json(json!({
        "path": file.display().to_string(),
        "mediaURL": format!("/plugin-media/{plugin}/{filename}"),
        "byteSize": bytes.len(),
    })))
}

/// 插件媒体目录的只读出口：生成图与上传件。文件名只认本目录里的一层，
/// 不接受任何分隔符，所以走不出这个目录。
async fn serve_plugin_media(
    State(state): State<Arc<PluginWebState>>,
    Path((plugin, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, PluginWebError> {
    state.host.package(&plugin)?;
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return Err(PluginWebError::BadRequest("invalidPath".to_owned()));
    }
    let directory = crate::plugin_capabilities::plugin_media_directory(&state.home, &plugin)?;
    let path = directory.join(&file);
    let bytes = std::fs::read(&path).map_err(|_| {
        PluginWebError::Host(HostError::UnknownContribution {
            plugin: plugin.clone(),
            kind: "media",
            id: file.clone(),
        })
    })?;
    let mut response = (
        [(header::CONTENT_TYPE, mime_for(&file))],
        [(header::CACHE_CONTROL, "private, max-age=0, must-revalidate")],
        bytes,
    )
        .into_response();
    apply_sandbox_cors(&mut response, &headers);
    Ok(response)
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
    // 只把垫片自己的键注进快照：结构化存储走异步 API，塞进 localStorage
    // 快照只会让同名的两份数据互相打架。
    let shim: BTreeMap<&str, &str> = storage
        .iter()
        .filter(|(key, _)| !key.starts_with(STORE_PREFIX))
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let storage_json = serde_json::to_string(&shim).unwrap_or_else(|_| "{}".to_owned());
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

    fn ai_message(role: &str, images: &[&str], videos: &[&str]) -> AiMessage {
        AiMessage {
            role: role.to_owned(),
            content: "check".to_owned(),
            image_paths: images.iter().map(|item| (*item).to_owned()).collect(),
            video_paths: videos.iter().map(|item| (*item).to_owned()).collect(),
        }
    }

    #[test]
    fn media_attachments_are_counted_and_policed_before_any_model_call() {
        assert_eq!(
            media_attachment_count(&[ai_message("user", &[], &[])]),
            Ok(0)
        );
        assert_eq!(
            media_attachment_count(&[ai_message("user", &["/a.png", " /a.png ", ""], &[])]),
            Ok(1),
            "duplicates and blanks do not count"
        );
        // 本宿主没有视频解码器：带视频要拒，不能假装审过。
        assert_eq!(
            media_attachment_count(&[ai_message("user", &[], &["/clip.mp4"])]),
            Err("videosUnsupported")
        );
        let many: Vec<String> = (0..=crate::plugin_ai_media::MAX_IMAGES)
            .map(|index| format!("/{index}.png"))
            .collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            media_attachment_count(&[ai_message("user", &many, &[])]),
            Err("tooManyImages")
        );
        assert_eq!(
            media_attachment_count(&[ai_message("assistant", &["/a.png"], &[])]),
            Err("mediaOnNonUserMessage")
        );
        // 认不出的角色按 user 处理，和拼消息时一致。
        assert_eq!(
            media_attachment_count(&[ai_message("reviewer", &["/a.png"], &[])]),
            Ok(1)
        );
    }

    #[test]
    fn ai_messages_accept_camel_case_attachment_fields() {
        let message: AiMessage = serde_json::from_value(json!({
            "role": "user", "content": "x", "imagePaths": ["/a.png"], "videoPaths": ["/b.mp4"]
        }))
        .unwrap();
        assert_eq!(message.image_paths, vec!["/a.png".to_owned()]);
        assert_eq!(message.video_paths, vec!["/b.mp4".to_owned()]);
        let plain: AiMessage =
            serde_json::from_value(json!({"role": "user", "content": "x"})).unwrap();
        assert!(plain.image_paths.is_empty() && plain.video_paths.is_empty());
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
