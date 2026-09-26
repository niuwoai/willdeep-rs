//! 聊天里的插件 MCP 工具：已启用插件的工具进 `list_mcp_tools` / `call_mcp_tool`，
//! 用到时才拉起插件进程。
//!
//! 对应 macOS 宿主的 `AgentPluginMCPToolCatalog` + `AgentMCPBridge.callPluginTool`。
//! 差别只在进程结构：macOS 宿主是一个进程，这边插件宿主在 `willdeep web` 进程、
//! 聊天 harness 在 daemon 或 CLI 进程。同一个插件被两个进程各拉起一份，会有两个
//! 插件进程抢着写同一份数据文件——所以调用**优先经插件 MCP 网关**（在 Web 进程
//! 里，和页面共用同一个插件进程）；网关不在（没开 Web）时才用本进程自己的插件
//! 宿主，那时也没有别的进程在跑这个插件。
//!
//! 工具名与配置里的 MCP 工具同一套：`mcp__<服务>__<工具>`。审批、只读模式的拦截
//! 都在 `ToolRuntime` 里按名字统一处理，与配置的 MCP 工具完全一致。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::gateway::{GatewayCallError, call_via_gateway};
use super::host::{HostError, PluginHost};
use super::host_requests::PluginHostRequests;
use super::registry::PluginRegistry;
use super::tool_catalog::{PluginToolCatalog, server_key};
use crate::mcp::{LazyToolSource, McpError};
use crate::types::ToolDefinition;

/// 某一时刻的启用快照：注册表文件没变就一直用它（和它背后那个插件宿主）。
struct Snapshot {
    registry_modified: Option<SystemTime>,
    host: Arc<PluginHost>,
    /// `plugin:<id>:<server>` —— 已启用且能运行的服务。
    allowed: BTreeSet<String>,
}

pub struct PluginChatTools {
    home: PathBuf,
    catalog: PluginToolCatalog,
    host_requests: Option<Arc<dyn PluginHostRequests>>,
    /// 为 false 时不经网关（测试、以及明确只要本进程宿主的场合）。
    use_gateway: bool,
    snapshot: StdMutex<Option<Arc<Snapshot>>>,
}

impl PluginChatTools {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            catalog: PluginToolCatalog::new(home),
            host_requests: None,
            use_gateway: true,
            snapshot: StdMutex::new(None),
        }
    }

    /// 本进程宿主拉起的插件也能反向请求出图、问模型。
    pub fn with_host_requests(mut self, handler: Arc<dyn PluginHostRequests>) -> Self {
        self.host_requests = Some(handler);
        self
    }

    pub fn without_gateway(mut self) -> Self {
        self.use_gateway = false;
        self
    }

    fn registry_modified(&self) -> Option<SystemTime> {
        std::fs::metadata(PluginRegistry::default_path(&self.home))
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    /// 当前快照。注册表文件改过（启用、停用、审批）就重新发现一遍——旧宿主
    /// 连同它拉起的插件进程一起丢掉，免得一个刚停用的插件还在被调用。
    fn snapshot(&self) -> Option<Arc<Snapshot>> {
        let modified = self.registry_modified();
        let mut guard = self
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = guard.as_ref()
            && existing.registry_modified == modified
        {
            return Some(existing.clone());
        }
        let host = match PluginHost::discover(&self.home) {
            Ok(host) => host,
            Err(error) => {
                eprintln!("warning: plugin_chat_tools_unavailable error={error}");
                *guard = None;
                return None;
            }
        };
        if let Some(handler) = &self.host_requests {
            host.set_host_requests(handler.clone());
        }
        let registry = PluginRegistry::load(&PluginRegistry::default_path(&self.home)).ok();
        let mut allowed = BTreeSet::new();
        for package in host.packages() {
            if !registry
                .as_ref()
                .is_some_and(|registry| registry.is_enabled(&package.id))
            {
                continue;
            }
            for server in host.runnable_servers(&package.id) {
                allowed.insert(server_key(&package.id, &server));
            }
        }
        let snapshot = Arc::new(Snapshot {
            registry_modified: modified,
            host: Arc::new(host),
            allowed,
        });
        *guard = Some(snapshot.clone());
        Some(snapshot)
    }

    /// 暴露名 → (插件, 服务, 原始工具名)。只在已启用、能运行的服务里找。
    fn route(&self, name: &str) -> Option<(String, String, String)> {
        let snapshot = self.snapshot()?;
        self.catalog
            .load()
            .into_iter()
            .filter(|(key, _)| snapshot.allowed.contains(key))
            .find_map(|(_, server)| {
                server
                    .tools
                    .iter()
                    .find(|tool| tool.exposed_name == name)
                    .map(|tool| {
                        (
                            server.plugin_id.clone(),
                            server.server_name.clone(),
                            tool.raw_name.clone(),
                        )
                    })
            })
    }

    /// 先经网关，网关不在再用本进程宿主。
    async fn request(
        &self,
        plugin_id: &str,
        server: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, McpError> {
        if self.use_gateway {
            match call_via_gateway(&self.home, plugin_id, server, method, params.clone()).await {
                Ok(result) => return Ok(result),
                Err(GatewayCallError::Failed(error)) => return Err(error),
                Err(GatewayCallError::Unavailable(_)) => {}
            }
        }
        let snapshot = self
            .snapshot()
            .ok_or_else(|| McpError::Transport("plugin host is unavailable".to_owned()))?;
        if method == "tools/list" {
            // 经宿主刷新：顺手记进目录。
            return snapshot
                .host
                .refresh_tools(plugin_id, server)
                .await
                .map_err(host_error);
        }
        let mcp = snapshot.host.mcp(plugin_id).await.map_err(host_error)?;
        mcp.request(server, method, params).await
    }
}

fn host_error(error: HostError) -> McpError {
    match error {
        HostError::Mcp(error) => error,
        other => McpError::Transport(other.to_string()),
    }
}

#[async_trait]
impl LazyToolSource for PluginChatTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (key, server) in self.catalog.load() {
            if !snapshot.allowed.contains(&key) {
                continue;
            }
            for tool in &server.tools {
                // 两个插件各带一个同名服务、同名工具时只留第一个：名字相同的两个
                // 工具，调用必然会打到其中一个错的上。
                if seen.insert(tool.exposed_name.clone()) {
                    out.push(tool.definition(&server));
                }
            }
        }
        out
    }

    fn available(&self) -> bool {
        self.snapshot()
            .is_some_and(|snapshot| !snapshot.allowed.is_empty())
    }

    async fn refresh_missing(&self) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        let known = self.catalog.load();
        for key in snapshot
            .allowed
            .iter()
            .filter(|key| !known.contains_key(*key))
        {
            let Some((plugin_id, server)) = key
                .strip_prefix("plugin:")
                .and_then(|rest| rest.split_once(':'))
            else {
                continue;
            };
            match self
                .request(plugin_id, server, "tools/list", json!({}))
                .await
            {
                // 经网关时网关那头已经记过；这里再记一次无妨（内容相同就不写盘）。
                Ok(listed) => self.catalog.record(plugin_id, server, &listed),
                Err(error) => eprintln!(
                    "warning: plugin_tools_refresh_failed plugin={plugin_id} server={server} error={error}"
                ),
            }
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        // 名字对不上多半是目录过期或插件刚停用：报出来让模型看见，别静默回落。
        let (plugin_id, server, tool) = self
            .route(name)
            .ok_or_else(|| McpError::UnknownTool(name.to_owned()))?;
        let result = self
            .request(
                &plugin_id,
                &server,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::test_support::{
        events, install_fake_plugin, python_available, scratch_home,
    };

    #[tokio::test]
    async fn only_enabled_runnable_plugins_are_exposed_and_spawned_on_demand() {
        if !python_available() {
            return;
        }
        let home = scratch_home("chat-tools");
        let log = install_fake_plugin(&home, "demo", r#"["process.execute"]"#, r#"["srv"]"#);
        install_fake_plugin(&home, "noexec", r#"["ai.chat"]"#, r#"["srv"]"#);
        let setup = PluginHost::discover(&home).expect("host");
        for id in ["demo", "noexec"] {
            setup.approve(id, 1).await.expect("approve");
            setup
                .set_enabled(id, true)
                .await
                .expect("write")
                .expect("enabled");
        }
        // 一个没有 process.execute 的插件，哪怕目录里有它的条目也不暴露。
        setup
            .tool_catalog()
            .record("noexec", "srv", &json!({"tools": [{"name": "sneaky"}]}));

        let tools = PluginChatTools::new(&home).without_gateway();
        assert!(
            tools.available(),
            "an enabled server makes the meta tools appear"
        );
        assert!(tools.definitions().is_empty(), "nothing cached yet");
        assert!(
            events(&log, "spawned").is_empty(),
            "building the tool list spawns nothing"
        );

        tools.refresh_missing().await;
        let names: Vec<String> = tools
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains(&"mcp__srv__echo".to_owned()), "{names:?}");
        assert!(!names.contains(&"mcp__srv__sneaky".to_owned()));
        assert_eq!(events(&log, "spawned").len(), 1);

        let output = tools
            .call("mcp__srv__echo", json!({"hello": "world"}))
            .await
            .expect("call");
        assert!(output.contains("hello"), "{output}");
        assert!(matches!(
            tools.call("mcp__srv__sneaky", json!({})).await,
            Err(McpError::UnknownTool(_))
        ));

        // 另一处（Web 进程）停用插件：注册表文件变了，这边立刻看不到它的工具。
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        setup
            .set_enabled("demo", false)
            .await
            .expect("write")
            .expect("disabled");
        assert!(tools.definitions().is_empty());
        assert!(matches!(
            tools.call("mcp__srv__echo", json!({})).await,
            Err(McpError::UnknownTool(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }
}
