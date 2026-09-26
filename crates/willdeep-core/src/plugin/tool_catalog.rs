//! 已启用插件的 MCP 工具目录：把 `tools/list` 的结果存一份，聊天建工具表时读它。
//!
//! 与 macOS 宿主 `AgentPluginMCPToolCatalog` 同一个思路：建工具表不能现拉
//! `tools/list`——那会把 stdio 子进程拉起来，而这是每回合都走的路径。所以存一份，
//! 靠这几个时机刷新：宿主连上插件（页面打开、命令执行）、插件 MCP 网关确保插件
//! 在跑、模型调 `list_mcp_tools` 时补齐缺的、插件停用或卸载时失效。
//!
//! 缓存过期不是灾难：名字对不上时调用会由插件服务自己报错，模型看得到。真正要防
//! 的是反过来——把一个已停用插件的工具端上去，所以读的一方必须按现状过一遍
//! 启用状态与权限（见 [`super::host::PluginHost::runnable_servers`]）。
//!
//! 文件在 `<home>/plugin-mcp-tools.json`。Web 进程与 daemon 进程都会写，
//! 先写临时文件再改名，最多丢一次并发写入的条目，下次刷新补回来。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::types::ToolDefinition;

pub const CATALOG_FILE: &str = "plugin-mcp-tools.json";
const CATALOG_VERSION: u32 = 1;
/// 暴露名前缀，与配置里的 MCP 工具同一套命名：`mcp__<服务>__<工具>`。
pub const EXPOSED_NAME_PREFIX: &str = "mcp__";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTool {
    /// 服务自己报的工具名，可能带点号等 provider 不收的字符。
    pub raw_name: String,
    /// 暴露给模型的名字。
    pub exposed_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_object_schema")]
    pub input_schema: Value,
    /// MCP 标准注解 `annotations.readOnlyHint`。只认布尔 true。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogServer {
    #[serde(rename = "pluginID")]
    pub plugin_id: String,
    pub server_name: String,
    pub tools: Vec<CatalogTool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogFile {
    version: u32,
    #[serde(default)]
    servers: BTreeMap<String, CatalogServer>,
}

impl Default for CatalogFile {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            servers: BTreeMap::new(),
        }
    }
}

fn empty_object_schema() -> Value {
    json!({"type": "object"})
}

/// 目录的键：`plugin:<插件ID>:<服务名>`，与 macOS 宿主的服务 id 同形。
pub fn server_key(plugin_id: &str, server: &str) -> String {
    format!("plugin:{plugin_id}:{server}")
}

/// 暴露名：`mcp__<服务>__<工具>`，两段都收敛成 provider 接受的字符。
pub fn exposed_name(server: &str, tool: &str) -> String {
    format!(
        "{EXPOSED_NAME_PREFIX}{}__{}",
        crate::mcp::sanitize(server),
        crate::mcp::sanitize(tool)
    )
}

/// 把一次 `tools/list` 的结果解析成目录条目。收敛后撞名的加序号：宁可名字丑一点，
/// 也不能两个工具共用一个名字——那会让调用打到错的那个。
pub fn parse_tools(listed: &Value, server: &str) -> Option<Vec<CatalogTool>> {
    let items = listed.get("tools")?.as_array()?;
    let mut used = BTreeSet::new();
    let mut tools = Vec::new();
    for item in items {
        let Some(name) = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let mut exposed = exposed_name(server, name);
        if used.contains(&exposed) {
            let mut suffix = 2;
            while used.contains(&format!("{exposed}_{suffix}")) {
                suffix += 1;
            }
            exposed = format!("{exposed}_{suffix}");
        }
        used.insert(exposed.clone());
        tools.push(CatalogTool {
            raw_name: name.to_owned(),
            exposed_name: exposed,
            description: item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            input_schema: item
                .get("inputSchema")
                .filter(|schema| schema.is_object())
                .cloned()
                .unwrap_or_else(empty_object_schema),
            read_only_hint: (item
                .get("annotations")
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(Value::as_bool)
                == Some(true))
            .then_some(true),
        });
    }
    Some(tools)
}

impl CatalogTool {
    pub fn definition(&self, server: &CatalogServer) -> ToolDefinition {
        ToolDefinition {
            name: self.exposed_name.clone(),
            description: if self.description.trim().is_empty() {
                format!(
                    "Tool {} provided by plugin {} MCP server {}.",
                    self.raw_name, server.plugin_id, server.server_name
                )
            } else {
                self.description.clone()
            },
            parameters: self.input_schema.clone(),
        }
    }
}

pub struct PluginToolCatalog {
    path: PathBuf,
    /// 同一进程里的读改写串行；跨进程靠原子改名兜底。
    lock: Mutex<()>,
}

impl PluginToolCatalog {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(CATALOG_FILE),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读不出来（没有、损坏、版本不认识）一律当空目录：它只是缓存。
    pub fn load(&self) -> BTreeMap<String, CatalogServer> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|source| serde_json::from_str::<CatalogFile>(&source).ok())
            .filter(|file| file.version == CATALOG_VERSION)
            .map(|file| file.servers)
            .unwrap_or_default()
    }

    /// 收下一次 `tools/list` 的结果。空列表照样写入：「这个服务现在没有工具」
    /// 和「还没问过」是两种状态，压成一种会让刚清空的服务一直显示旧工具。
    pub fn record(&self, plugin_id: &str, server: &str, listed: &Value) {
        let Some(tools) = parse_tools(listed, server) else {
            return;
        };
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut servers = self.load();
        let entry = CatalogServer {
            plugin_id: plugin_id.to_owned(),
            server_name: server.to_owned(),
            tools,
        };
        let key = server_key(plugin_id, server);
        if servers.get(&key) == Some(&entry) {
            return;
        }
        servers.insert(key, entry);
        self.save(servers);
    }

    /// 插件停用、卸载：它的条目全部作废。
    pub fn invalidate(&self, plugin_id: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut servers = self.load();
        let before = servers.len();
        servers.retain(|_, server| server.plugin_id != plugin_id);
        if servers.len() != before {
            self.save(servers);
        }
    }

    fn save(&self, servers: BTreeMap<String, CatalogServer>) {
        let file = CatalogFile {
            version: CATALOG_VERSION,
            servers,
        };
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = self
                .path
                .with_extension(format!("json.{}.tmp", std::process::id()));
            let body = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
            super::gateway::write_private(&temporary, &body)?;
            std::fs::rename(&temporary, &self.path)
        })();
        if let Err(error) = result {
            eprintln!(
                "warning: plugin_tool_catalog_write_failed path={} error={error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_names_are_sanitized_and_collisions_get_suffixes() {
        let listed = json!({"tools": [
            {"name": "drama.list", "description": "List dramas", "inputSchema": {"type":"object","properties":{"q":{"type":"string"}}}},
            {"name": "drama_list"},
            {"name": "  "},
            {"name": "peek", "annotations": {"readOnlyHint": true}},
            {"name": "poke", "annotations": {"readOnlyHint": "true"}}
        ]});
        let tools = parse_tools(&listed, "video-studio").expect("tools");
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0].exposed_name, "mcp__video-studio__drama_list");
        assert_eq!(tools[1].exposed_name, "mcp__video-studio__drama_list_2");
        assert_eq!(tools[0].raw_name, "drama.list");
        assert_eq!(tools[1].input_schema, json!({"type":"object"}));
        assert_eq!(tools[2].read_only_hint, Some(true));
        assert_eq!(tools[3].read_only_hint, None, "only boolean true counts");
    }

    #[test]
    fn records_invalidates_and_survives_corruption() {
        let home = std::env::temp_dir().join(format!(
            "willdeep-tool-catalog-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).expect("home");
        let catalog = PluginToolCatalog::new(&home);
        assert!(catalog.load().is_empty());
        catalog.record("demo", "srv", &json!({"tools": [{"name": "echo"}]}));
        catalog.record("other", "srv", &json!({"tools": []}));
        let loaded = catalog.load();
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded["plugin:demo:srv"].tools[0].exposed_name,
            "mcp__srv__echo"
        );
        assert!(
            loaded["plugin:other:srv"].tools.is_empty(),
            "empty lists are recorded"
        );
        catalog.invalidate("demo");
        assert_eq!(catalog.load().len(), 1);
        std::fs::write(catalog.path(), "{not json").expect("corrupt");
        assert!(catalog.load().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }
}
