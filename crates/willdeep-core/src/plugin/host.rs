//! 插件宿主：把包、审批状态与每插件隔离的 MCP 连接组装成一份可用的运行时。
//!
//! 与 macOS 版的分工一致：真正的业务能力走插件自己的 MCP 服务，宿主只负责
//! 「谁能跑、跑什么、拿得到什么上下文」。页面本身永远拿不到文件、进程、网络
//! 或密钥——所有真实动作都必须映射到清单里声明过的命令。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::mcp::{McpError, McpRegistry, McpServerConfig};

use super::declarative::{DeclarativeDocument, DeclarativeError};
use super::manifest::{
    CommandHandler, HostAction, PageRuntime, PluginMenuLocation, PluginPermission, SidebarMode,
};
use super::package::{
    MAX_MANIFEST_BYTES, MAX_PAGE_BYTES, PackageError, PluginPackage, PluginSource, discover,
};
use super::registry::{ApprovalGap, PluginRegistry, RegistryError};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("no plugin with id `{0}` is installed")]
    UnknownPlugin(String),
    #[error("plugin `{0}` is not enabled")]
    NotEnabled(String),
    #[error("plugin `{plugin}` has no {kind} `{id}`")]
    UnknownContribution {
        plugin: String,
        kind: &'static str,
        id: String,
    },
    #[error("plugin `{plugin}` does not declare the `{permission}` permission")]
    PermissionDenied { plugin: String, permission: String },
    #[error("plugin `{plugin}` declares MCP server `{server}` but the package does not define it")]
    UnknownServer { plugin: String, server: String },
    #[error("plugin `{plugin}` sidebar `{sidebar}` is not declarative")]
    NotDeclarative { plugin: String, sidebar: String },
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Declarative(#[from] DeclarativeError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error("MCP resource `{uri}` did not return usable content")]
    EmptyResource { uri: String },
    #[error("MCP App resource `{uri}` must be text/html;profile=mcp-app, got `{mime}`")]
    WrongResourceMime { uri: String, mime: String },
}

/// 一次命令执行的结果。宿主命令与跳转由界面消化，MCP 工具的返回原样交回页面。
#[derive(Clone, Debug)]
pub enum CommandOutcome {
    Host(HostAction),
    Navigate { destination: String },
    Tool(Value),
}

/// 页面拿到的上下文。工作区与会话引用只在插件声明了对应权限时才给，
/// 且永远只是引用——不传聊天正文，也不传文件内容。
#[derive(Clone, Debug, Default)]
pub struct DestinationContext {
    pub destination_id: String,
    pub selected_item_id: Option<String>,
    pub workspace_reference: Option<String>,
    pub session_reference: Option<String>,
    pub locale: String,
    pub color_scheme: String,
}

/// 一个插件的加载失败。发现阶段不该因为一个坏包就整个瞎掉，
/// 但也不能假装它不存在——插件中心要把原因显示出来。
#[derive(Clone, Debug)]
pub struct PluginLoadFailure {
    pub path: PathBuf,
    pub reason: String,
}

pub struct PluginHost {
    home: PathBuf,
    packages: Vec<PluginPackage>,
    failures: Vec<PluginLoadFailure>,
    registry: Mutex<PluginRegistry>,
    /// pluginID → 该插件自己的 MCP 注册表。隔离靠实例边界：一个插件永远
    /// 拿不到另一个插件的连接，哪怕两边的 server 重名。
    connections: Mutex<BTreeMap<String, Arc<McpRegistry>>>,
}

impl PluginHost {
    pub fn shared_root(home: &Path) -> PathBuf {
        home.join("plugins")
    }

    pub fn draft_root(home: &Path) -> PathBuf {
        home.join("plugin-drafts")
    }

    /// 扫描全部来源并载入注册状态。Codex 缓存按用户主目录推断，找不到就跳过。
    pub fn discover(home: &Path) -> Result<Self, RegistryError> {
        let mut packages = Vec::new();
        let mut failures = Vec::new();
        let mut sources = vec![
            (Self::shared_root(home), PluginSource::Shared),
            (Self::draft_root(home), PluginSource::Draft),
        ];
        if let Some(parent) = home.parent() {
            sources.push((
                parent.join(".codex").join("plugins").join("cache"),
                PluginSource::CodexCache,
            ));
        }
        for (root, source) in sources {
            for result in discover(&root, source) {
                match result {
                    Ok(package) => {
                        // 同一个 ID 被多个来源提供时，共享安装目录优先：
                        // 它是用户明确装过的那一份。
                        if let Some(existing) = packages
                            .iter()
                            .position(|item: &PluginPackage| item.id == package.id)
                        {
                            if packages[existing].source != PluginSource::Shared {
                                packages[existing] = package;
                            }
                        } else {
                            packages.push(package);
                        }
                    }
                    Err(error) => failures.push(PluginLoadFailure {
                        path: root.clone(),
                        reason: error.to_string(),
                    }),
                }
            }
        }
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        let registry = PluginRegistry::load(&PluginRegistry::default_path(home))?;
        Ok(Self {
            home: home.to_path_buf(),
            packages,
            failures,
            registry: Mutex::new(registry),
            connections: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn packages(&self) -> &[PluginPackage] {
        &self.packages
    }

    pub fn failures(&self) -> &[PluginLoadFailure] {
        &self.failures
    }

    pub fn package(&self, plugin_id: &str) -> Result<&PluginPackage, HostError> {
        self.packages
            .iter()
            .find(|item| item.id == plugin_id)
            .ok_or_else(|| HostError::UnknownPlugin(plugin_id.to_owned()))
    }

    pub async fn is_enabled(&self, plugin_id: &str) -> bool {
        self.registry.lock().await.is_enabled(plugin_id)
    }

    pub async fn approval_gap(&self, plugin_id: &str) -> Result<Option<ApprovalGap>, HostError> {
        let package = self.package(plugin_id)?;
        Ok(self.registry.lock().await.approval_gap(package)?)
    }

    pub async fn approve(&self, plugin_id: &str, now: u64) -> Result<(), HostError> {
        let package = self.package(plugin_id)?;
        self.registry.lock().await.approve(package, now)?;
        Ok(())
    }

    pub async fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<Result<(), ApprovalGap>, HostError> {
        let package = self.package(plugin_id)?;
        let outcome = self.registry.lock().await.set_enabled(package, enabled)?;
        if !enabled {
            // 停用即断连：一个被停用的插件不该还留着一个活着的子进程。
            self.connections.lock().await.remove(plugin_id);
        }
        Ok(outcome)
    }

    /// 忘掉一个插件的全部授权与运行状态。包文件由调用方删除——
    /// 这里只负责状态，好让"删文件失败"不会留下一份仍然有效的审批。
    pub async fn forget(&self, plugin_id: &str) -> Result<(), HostError> {
        self.connections.lock().await.remove(plugin_id);
        self.registry.lock().await.forget(plugin_id)?;
        Ok(())
    }

    pub async fn set_pinned_order(
        &self,
        plugin_id: &str,
        order: Option<u32>,
    ) -> Result<(), HostError> {
        self.package(plugin_id)?;
        self.registry
            .lock()
            .await
            .set_pinned_order(plugin_id, order)?;
        Ok(())
    }

    pub async fn setting(&self, plugin_id: &str, key: &str) -> Option<String> {
        self.registry
            .lock()
            .await
            .state(plugin_id)
            .and_then(|state| state.settings.get(key).cloned())
    }

    pub async fn set_setting(
        &self,
        plugin_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), HostError> {
        self.package(plugin_id)?;
        self.registry
            .lock()
            .await
            .set_setting(plugin_id, key, value)?;
        // 设置变了，MCP 服务的环境变量可能也变了，下次用时重连。
        self.connections.lock().await.remove(plugin_id);
        Ok(())
    }

    pub async fn pinned_order(&self, plugin_id: &str) -> Option<u32> {
        self.registry
            .lock()
            .await
            .state(plugin_id)
            .and_then(|state| state.pinned_order)
    }

    pub fn permits(&self, plugin_id: &str, permission: PluginPermission) -> Result<(), HostError> {
        let package = self.package(plugin_id)?;
        let granted = package
            .manifest
            .as_ref()
            .is_some_and(|manifest| manifest.permissions.contains(&permission));
        if granted {
            Ok(())
        } else {
            Err(HostError::PermissionDenied {
                plugin: plugin_id.to_owned(),
                permission: permission.as_str().to_owned(),
            })
        }
    }

    async fn require_enabled(&self, plugin_id: &str) -> Result<&PluginPackage, HostError> {
        let package = self.package(plugin_id)?;
        if !self.registry.lock().await.is_enabled(plugin_id) {
            return Err(HostError::NotEnabled(plugin_id.to_owned()));
        }
        Ok(package)
    }

    /// 取（必要时建立）某个插件的 MCP 连接。
    ///
    /// `${pluginRoot}` 与 `${setting:<id>}` 在这里展开：前者让插件包可以被安装到
    /// 任意路径，后者让密钥不必写进包里。展开只在这一处发生，别处拿到的都是原文。
    pub async fn mcp(&self, plugin_id: &str) -> Result<Arc<McpRegistry>, HostError> {
        let package = self.require_enabled(plugin_id).await?;
        if let Some(existing) = self.connections.lock().await.get(plugin_id) {
            return Ok(existing.clone());
        }
        let root = package.root.display().to_string();
        let settings = self
            .registry
            .lock()
            .await
            .state(plugin_id)
            .map(|state| state.settings.clone())
            .unwrap_or_default();
        let mut configs = BTreeMap::new();
        for (name, spec) in &package.mcp_servers {
            let expand = |value: &str| expand_variables(value, &root, &settings);
            configs.insert(
                name.clone(),
                McpServerConfig {
                    command: expand(&spec.command),
                    args: spec.args.iter().map(|item| expand(item)).collect(),
                    env: spec
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), expand(value)))
                        .collect(),
                    startup_timeout_seconds: spec.startup_timeout_seconds,
                    enabled: true,
                },
            );
        }
        let registry = Arc::new(McpRegistry::connect(&configs).await?);
        self.connections
            .lock()
            .await
            .insert(plugin_id.to_owned(), registry.clone());
        Ok(registry)
    }

    /// 执行一条清单里声明过的命令。清单外的东西一律进不来——
    /// 这是页面能让宿主做事的唯一入口。
    pub async fn execute_command(
        &self,
        plugin_id: &str,
        command_id: &str,
        arguments: Value,
    ) -> Result<CommandOutcome, HostError> {
        let package = self.require_enabled(plugin_id).await?;
        let command = package
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.command(command_id))
            .ok_or_else(|| HostError::UnknownContribution {
                plugin: plugin_id.to_owned(),
                kind: "command",
                id: command_id.to_owned(),
            })?;
        match &command.handler {
            CommandHandler::Host { action } => Ok(CommandOutcome::Host(*action)),
            CommandHandler::Navigate { destination } => Ok(CommandOutcome::Navigate {
                destination: qualified_destination(plugin_id, destination),
            }),
            CommandHandler::McpTool { server, tool } => {
                if !package.mcp_servers.contains_key(server) {
                    return Err(HostError::UnknownServer {
                        plugin: plugin_id.to_owned(),
                        server: server.clone(),
                    });
                }
                let mcp = self.mcp(plugin_id).await?;
                Ok(CommandOutcome::Tool(
                    mcp.call_tool_on(server, tool, arguments).await?,
                ))
            }
        }
    }

    /// MCP App 页面从同一个服务读一条资源。只允许本插件声明的服务，
    /// 页面递上来的服务名不作数。
    pub async fn read_page_resource(
        &self,
        plugin_id: &str,
        page_id: &str,
    ) -> Result<String, HostError> {
        let package = self.require_enabled(plugin_id).await?;
        let page = package
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.page(page_id))
            .ok_or_else(|| HostError::UnknownContribution {
                plugin: plugin_id.to_owned(),
                kind: "page",
                id: page_id.to_owned(),
            })?;
        match page.runtime {
            PageRuntime::LocalWeb => {
                let entry = page.entry_path.clone().unwrap_or_default();
                Ok(package.read_resource_text(&entry, MAX_PAGE_BYTES)?)
            }
            PageRuntime::McpApp => {
                let server = page.server.clone().unwrap_or_default();
                let uri = page.resource_uri.clone().unwrap_or_default();
                let mcp = self.mcp(plugin_id).await?;
                let payload = mcp.read_resource(&server, &uri).await?;
                let (text, mime) = crate::mcp::resource_text(&payload, &uri)
                    .ok_or_else(|| HostError::EmptyResource { uri: uri.clone() })?;
                // MIME 必须自称 MCP App，否则一个普通 text/plain 资源就能被当页面渲染。
                if !mime.starts_with("text/html") {
                    return Err(HostError::WrongResourceMime { uri, mime });
                }
                if text.len() as u64 > MAX_PAGE_BYTES {
                    return Err(HostError::EmptyResource { uri });
                }
                // 订阅失败不影响这次读取：还有进入目的地与手动刷新两条路。
                let _ = mcp.subscribe_resource(&server, &uri).await;
                Ok(text)
            }
            PageRuntime::Declarative => {
                let schema = page.schema.clone().unwrap_or_default();
                Ok(package.read_resource_text(&schema, MAX_MANIFEST_BYTES)?)
            }
        }
    }

    /// 声明式侧栏文档。动态 Resource 优先，失败时回落到包内 Schema——
    /// 侧栏 Resource 挂了不该把中央页面一起拖下水。
    pub async fn sidebar_document(
        &self,
        plugin_id: &str,
        sidebar_id: &str,
    ) -> Result<(DeclarativeDocument, Option<String>), HostError> {
        let package = self.require_enabled(plugin_id).await?;
        let manifest = package.manifest.as_ref();
        let sidebar = manifest
            .and_then(|manifest| manifest.sidebar(sidebar_id))
            .ok_or_else(|| HostError::UnknownContribution {
                plugin: plugin_id.to_owned(),
                kind: "sidebar",
                id: sidebar_id.to_owned(),
            })?;
        if sidebar.mode != SidebarMode::Declarative {
            return Err(HostError::NotDeclarative {
                plugin: plugin_id.to_owned(),
                sidebar: sidebar_id.to_owned(),
            });
        }
        let commands: BTreeSet<String> = manifest
            .map(|manifest| {
                manifest
                    .commands
                    .iter()
                    .map(|item| item.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        let mut degraded = None;
        if let Some(resource) = &sidebar.resource {
            match self
                .read_sidebar_resource(plugin_id, resource, &commands)
                .await
            {
                Ok(document) => return Ok((document, None)),
                Err(error) => degraded = Some(error.to_string()),
            }
        }
        let Some(schema) = &sidebar.schema else {
            return Err(HostError::UnknownContribution {
                plugin: plugin_id.to_owned(),
                kind: "sidebar schema",
                id: sidebar_id.to_owned(),
            });
        };
        let source = package.read_resource_text(schema, MAX_MANIFEST_BYTES)?;
        Ok((DeclarativeDocument::parse(&source, &commands)?, degraded))
    }

    async fn read_sidebar_resource(
        &self,
        plugin_id: &str,
        resource: &super::manifest::McpResourceRef,
        commands: &BTreeSet<String>,
    ) -> Result<DeclarativeDocument, HostError> {
        let mcp = self.mcp(plugin_id).await?;
        let payload = mcp.read_resource(&resource.server, &resource.uri).await?;
        let (text, _) = crate::mcp::resource_text(&payload, &resource.uri).ok_or_else(|| {
            HostError::EmptyResource {
                uri: resource.uri.clone(),
            }
        })?;
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(HostError::EmptyResource {
                uri: resource.uri.clone(),
            });
        }
        let document = DeclarativeDocument::parse(&text, commands)?;
        let _ = mcp
            .subscribe_resource(&resource.server, &resource.uri)
            .await;
        Ok(document)
    }

    /// 目的地上下文。权限没声明的字段一律给 `None`，不是给空串——
    /// 页面应该看得出"没有"和"有但为空"的区别。
    pub fn destination_context(
        &self,
        plugin_id: &str,
        destination_id: &str,
        workspace: Option<&str>,
        session: Option<&str>,
        locale: &str,
        color_scheme: &str,
    ) -> DestinationContext {
        let permissions = self
            .package(plugin_id)
            .ok()
            .and_then(|package| package.manifest.as_ref())
            .map(|manifest| manifest.permissions.clone())
            .unwrap_or_default();
        let workspace_reference = (permissions.contains(&PluginPermission::WorkspaceRead)
            || permissions.contains(&PluginPermission::WorkspaceWrite))
        .then(|| workspace.map(str::to_owned))
        .flatten();
        let session_reference = permissions
            .contains(&PluginPermission::ConversationRead)
            .then(|| session.map(str::to_owned))
            .flatten();
        DestinationContext {
            destination_id: qualified_destination(plugin_id, destination_id),
            selected_item_id: None,
            workspace_reference,
            session_reference,
            locale: locale.to_owned(),
            color_scheme: color_scheme.to_owned(),
        }
    }

    /// 某个菜单位置上，所有已启用插件贡献的命令。
    pub async fn menu_commands(
        &self,
        location: PluginMenuLocation,
    ) -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        for package in &self.packages {
            if !self.registry.lock().await.is_enabled(&package.id) {
                continue;
            }
            let Some(manifest) = &package.manifest else {
                continue;
            };
            let Some(commands) = manifest.menus.get(&location) else {
                continue;
            };
            for command_id in commands {
                if let Some(command) = manifest.command(command_id) {
                    out.push((package.id.clone(), command.id.clone(), command.icon.clone()));
                }
            }
        }
        out
    }
}

pub fn qualified_destination(plugin_id: &str, destination_id: &str) -> String {
    format!("{plugin_id}:{destination_id}")
}

/// 展开 `${pluginRoot}` 与 `${setting:<id>}`。未知变量原样保留——
/// 静默替换成空串会让一条命令悄悄变成另一条命令。
fn expand_variables(value: &str, root: &str, settings: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        let token = &rest[start + 2..start + end];
        match token {
            "pluginRoot" => out.push_str(root),
            other => match other.strip_prefix("setting:") {
                Some(key) => out.push_str(settings.get(key).map(String::as_str).unwrap_or("")),
                None => out.push_str(&rest[start..start + end + 1]),
            },
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-host-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch directory");
        root
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn install(home: &Path, id: &str, permissions: &str) {
        let root = home.join("plugins").join(id).join("1.0.0");
        write(
            &root.join(".codex-plugin/plugin.json"),
            &format!(r#"{{"name":"{id}","version":"1.0.0","interface":{{"displayName":"{id}"}}}}"#),
        );
        write(
            &root.join(".willdeep-plugin/plugin.json"),
            &format!(
                r#"{{"schemaVersion":1,"permissions":{permissions},"contributes":{{
                    "destinations":[{{"id":"main","titleKey":"destination.main","mainPage":"main.page","toolbarCommandIDs":["main.refresh"]}}],
                    "pages":[{{"id":"main.page","runtime":"localWeb","entryPath":"ui/index.html"}}],
                    "commands":[{{"id":"main.refresh","titleKey":"command.refresh","handler":{{"type":"host","action":"plugin.refresh"}}}}],
                    "menus":{{"commandPalette":["main.refresh"]}}}}}}"#
            ),
        );
        write(
            &root.join(".willdeep-plugin/locales/en.json"),
            r#"{"destination.main":"Main","command.refresh":"Refresh"}"#,
        );
        write(
            &root.join("ui/index.html"),
            "<html><body>page</body></html>",
        );
    }

    #[tokio::test]
    async fn discovers_installs_and_gates_on_approval() {
        let home = scratch("discover");
        install(&home, "demo", "[]");
        let host = PluginHost::discover(&home).expect("host");
        assert_eq!(host.packages().len(), 1);
        assert!(!host.is_enabled("demo").await);

        // 未批准就启用要被拒，而不是悄悄放行。
        assert!(matches!(
            host.set_enabled("demo", true).await.expect("write"),
            Err(ApprovalGap::NeverApproved)
        ));
        // 未启用的插件不能执行命令。
        assert!(matches!(
            host.execute_command("demo", "main.refresh", Value::Null)
                .await,
            Err(HostError::NotEnabled(_))
        ));

        host.approve("demo", 1).await.expect("approve");
        assert!(host.set_enabled("demo", true).await.expect("write").is_ok());
        assert!(matches!(
            host.execute_command("demo", "main.refresh", Value::Null)
                .await,
            Ok(CommandOutcome::Host(HostAction::PluginRefresh))
        ));
    }

    #[tokio::test]
    async fn commands_outside_the_manifest_are_refused() {
        let home = scratch("unknown-command");
        install(&home, "demo", "[]");
        let host = PluginHost::discover(&home).expect("host");
        host.approve("demo", 1).await.expect("approve");
        host.set_enabled("demo", true)
            .await
            .expect("write")
            .expect("enabled");
        assert!(matches!(
            host.execute_command("demo", "main.evil", Value::Null).await,
            Err(HostError::UnknownContribution {
                kind: "command",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn context_withholds_references_the_plugin_never_asked_for() {
        let home = scratch("context");
        install(&home, "quiet", "[]");
        install(&home, "nosy", r#"["workspace.read","conversation.read"]"#);
        let host = PluginHost::discover(&home).expect("host");

        let quiet =
            host.destination_context("quiet", "main", Some("/repo"), Some("s1"), "en", "dark");
        assert_eq!(quiet.workspace_reference, None);
        assert_eq!(quiet.session_reference, None);
        assert_eq!(quiet.destination_id, "quiet:main");

        let nosy =
            host.destination_context("nosy", "main", Some("/repo"), Some("s1"), "en", "dark");
        assert_eq!(nosy.workspace_reference.as_deref(), Some("/repo"));
        assert_eq!(nosy.session_reference.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn menu_contributions_only_come_from_enabled_plugins() {
        let home = scratch("menus");
        install(&home, "demo", "[]");
        let host = PluginHost::discover(&home).expect("host");
        assert!(
            host.menu_commands(PluginMenuLocation::CommandPalette)
                .await
                .is_empty()
        );
        host.approve("demo", 1).await.expect("approve");
        host.set_enabled("demo", true)
            .await
            .expect("write")
            .expect("enabled");
        let commands = host.menu_commands(PluginMenuLocation::CommandPalette).await;
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].1, "main.refresh");
    }

    #[test]
    fn expands_plugin_root_and_settings_but_leaves_unknown_tokens_alone() {
        let mut settings = BTreeMap::new();
        settings.insert("token".to_owned(), "abc".to_owned());
        assert_eq!(
            expand_variables("${pluginRoot}/server/todo.rb", "/pkg", &settings),
            "/pkg/server/todo.rb"
        );
        assert_eq!(
            expand_variables("${setting:token}", "/pkg", &settings),
            "abc"
        );
        // 未知变量保留原文：静默变空串会把一条命令悄悄变成另一条。
        assert_eq!(
            expand_variables("${HOME}/x", "/pkg", &settings),
            "${HOME}/x"
        );
    }

    #[tokio::test]
    async fn permission_checks_read_the_manifest_not_the_request() {
        let home = scratch("permits");
        install(&home, "demo", r#"["ai.chat"]"#);
        let host = PluginHost::discover(&home).expect("host");
        assert!(host.permits("demo", PluginPermission::AiChat).is_ok());
        assert!(matches!(
            host.permits("demo", PluginPermission::NetworkAccess),
            Err(HostError::PermissionDenied { .. })
        ));
    }
}
