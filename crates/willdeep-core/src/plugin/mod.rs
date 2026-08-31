//! WillDeep 插件系统。
//!
//! 与 macOS 版（Xedit）**共享插件包**，不共享运行状态。包住在
//! `~/.willdeep/plugins/<plugin-id>/<version>/`，两端各自发现；启用与权限审批
//! 各存各的，因为两个宿主的沙箱边界不是一回事。契约见仓库 `docs/PLUGINS.md`，
//! 上游设计见 Xedit `docs/WILLDEEP_PLUGIN_SYSTEM_DESIGN.md`。
//!
//! 分层：
//!
//! - [`manifest`]：`.codex-plugin` / `.willdeep-plugin` 两份清单的解析与校验。
//! - [`package`]：包发现、路径安全、本地化与内容 digest。
//! - [`registry`]：启用状态与审批记录（0600 落盘）。
//! - [`declarative`]：声明式 UI 文档的限制校验。
//! - [`host`]：把上面几样组装成运行时，并持有每插件隔离的 MCP 连接。

pub mod declarative;
pub mod host;
pub mod manifest;
pub mod package;
pub mod registry;

pub use declarative::{DeclarativeDocument, DeclarativeError};
pub use host::{
    CommandOutcome, DestinationContext, HostError, PluginHost, PluginLoadFailure,
    qualified_destination,
};
pub use manifest::{
    CodexManifest, CommandHandler, HostAction, ManifestError, PageRuntime, PluginCommand,
    PluginDestination, PluginManifest, PluginMenuLocation, PluginPage, PluginPermission,
    PluginSetting, PluginSidebar, SettingType, SidebarMode,
};
pub use package::{
    McpServerSpec, PackageError, PluginPackage, PluginSource, discover, installed_versions,
    load_package, package_digest, safe_resource_path,
};
pub use registry::{ApprovalGap, PluginApproval, PluginRegistry, PluginState, RegistryError};
