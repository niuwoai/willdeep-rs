//! 随 willdeep 一起发的内置插件。
//!
//! 它们是**普通的插件包**：`willdeep plugin builtin install <name>` 把包写出来，
//! 走与第三方插件完全相同的安装 → 批准 → 启用流程，权限照样逐条声明、逐次
//! 批准。区别只在 MCP 服务端：`mcp.json` 写的是 `${willdeepExe} plugin
//! serve-builtin <id>`，也就是 willdeep 自己，不需要 Python、Node 或别的运行时。
//!
//! 与 Xedit 的对应关系见 `docs/BUILTIN_PLUGINS.md`。

pub(crate) mod roundtable;
pub(crate) mod scheduler;
pub(crate) mod stdio_server;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::json;

/// 一个内置插件包的静态描述。
pub(crate) struct BuiltinPackage {
    /// 插件 id（`.codex-plugin/plugin.json` 的 `name`）。
    pub id: &'static str,
    /// `plugin builtin install` 用的短名。
    pub short: &'static str,
    /// MCP 服务名：聊天里的工具名是 `mcp__<server>__<tool>`。
    pub server: &'static str,
    pub version: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub permissions: &'static [&'static str],
    /// 单次请求的无响应超时（秒）。宿主处理反向请求的时间不计入。
    pub timeout_seconds: u64,
}

pub(crate) const PACKAGES: &[BuiltinPackage] = &[
    BuiltinPackage {
        id: "willdeep-scheduler",
        short: "scheduler",
        server: "scheduler",
        version: "1.0.0",
        display_name: "Scheduled Tasks",
        description: "Recurring tasks that run on their own in fresh sessions: every N minutes, daily, on weekdays or weekly; goal-based tasks remove themselves once the goal is met.",
        permissions: &["process.execute", "conversation.write", "workspace.read"],
        timeout_seconds: 30,
    },
    BuiltinPackage {
        id: "willdeep-roundtable",
        short: "roundtable",
        server: "roundtable",
        version: "1.0.0",
        display_name: "Expert Roundtable",
        description: "Domain experts with deliberately different biases discuss an open question over several rounds and converge on a decision document.",
        permissions: &["process.execute", "ai.chat"],
        timeout_seconds: 120,
    },
];

pub(crate) fn package(name: &str) -> Option<&'static BuiltinPackage> {
    PACKAGES
        .iter()
        .find(|package| package.short == name || package.id == name)
}

/// 把插件包写进 `dir`：清单、本地化占位与 `mcp.json`。
pub(crate) fn write_package(package: &BuiltinPackage, dir: &Path) -> Result<()> {
    let write = |relative: &str, value: serde_json::Value| -> Result<()> {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&value)?)
            .with_context(|| format!("write {}", path.display()))
    };
    write(
        ".codex-plugin/plugin.json",
        json!({
            "name": package.id,
            "version": package.version,
            "description": package.description,
            "interface": {"displayName": package.display_name},
        }),
    )?;
    write(
        ".willdeep-plugin/plugin.json",
        json!({
            "schemaVersion": 1,
            "permissions": package.permissions,
            "dependencies": {"mcpServers": [package.server]},
            "contributes": {"destinations": [], "pages": [], "commands": []},
        }),
    )?;
    write(".willdeep-plugin/locales/en.json", json!({}))?;
    write(
        "mcp.json",
        json!({
            "mcpServers": {
                package.server: {
                    "command": "${willdeepExe}",
                    "args": ["plugin", "serve-builtin", package.id],
                    "startup_timeout_sec": package.timeout_seconds,
                }
            }
        }),
    )
}

/// `willdeep plugin builtin list`。
pub(crate) fn list() {
    for package in PACKAGES {
        println!(
            "{:<12} {} {}  permissions: {}\n             {}",
            package.short,
            package.id,
            package.version,
            package.permissions.join(", "),
            package.description
        );
    }
}

/// `willdeep plugin builtin install <name>`：写出包，再走普通的安装流程。
pub(crate) async fn install(home: &Path, name: &str, enable: bool) -> Result<()> {
    let Some(package) = package(name) else {
        bail!(
            "unknown built-in plugin {name}; available: {}",
            PACKAGES
                .iter()
                .map(|package| package.short)
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let staging = std::env::temp_dir().join(format!(
        "willdeep-builtin-{}-{}",
        package.id,
        uuid::Uuid::new_v4().simple()
    ));
    write_package(package, &staging)?;
    let result = crate::plugin_cmd::install_package(home, &staging, enable).await;
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    if package.short == "scheduler" {
        println!(
            "Scheduled runs are fired by the runtime daemon while this plugin is enabled.\n\
             Unattended runs wait for approval on tool calls below their approval mode; to let a\n\
             goal-based run remove itself without a prompt, allow `complete_scheduled_task` once\n\
             when it is first requested (Always Allow)."
        );
    }
    Ok(())
}

/// `willdeep plugin serve-builtin <id>`（隐藏）：插件宿主拉起的 MCP 服务端。
pub(crate) async fn serve(id: &str, home: PathBuf) -> Result<()> {
    let plugin: Arc<dyn stdio_server::BuiltinPlugin> =
        match package(id).map(|package| package.short) {
            Some("scheduler") => Arc::new(scheduler::Scheduler::new(
                home,
                std::env::current_dir().ok(),
            )),
            Some("roundtable") => Arc::new(roundtable::Roundtables::new(
                std::env::var_os("WILLDEEP_PLUGIN_DATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| willdeep_core::plugin::host::plugin_data_dir(&home, id)),
            )),
            _ => bail!("unknown built-in plugin {id}"),
        };
    stdio_server::serve(plugin, tokio::io::stdin(), tokio::io::stdout()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写出来的包能被宿主正常发现：清单合法、权限与服务声明齐全、命令用的是
    /// willdeep 自己。
    #[test]
    fn builtin_packages_load_as_ordinary_plugins() {
        for package in PACKAGES {
            let dir = std::env::temp_dir().join(format!(
                "willdeep-builtin-test-{}-{}",
                package.id,
                uuid::Uuid::new_v4().simple()
            ));
            write_package(package, &dir).unwrap();
            let loaded = willdeep_core::plugin::package::load_package(
                &dir,
                willdeep_core::plugin::package::PluginSource::Shared,
            )
            .unwrap();
            assert_eq!(loaded.id, package.id);
            assert_eq!(loaded.version, package.version);
            let server = loaded
                .mcp_servers
                .get(package.server)
                .expect("server declared in mcp.json");
            assert_eq!(server.command, "${willdeepExe}");
            assert_eq!(server.args, ["plugin", "serve-builtin", package.id]);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
