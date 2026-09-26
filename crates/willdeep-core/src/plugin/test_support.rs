//! 测试夹具：在临时家目录里装一个带 stdio MCP 服务（`fake_plugin_mcp.py`）的插件。
//! 只给本 crate 与 CLI 的测试用，从不碰真实的 `~/.willdeep`。

use std::path::{Path, PathBuf};

pub const FAKE_PLUGIN_SERVER: &str = include_str!("../../tests/fixtures/fake_plugin_mcp.py");

/// 有没有 python3。没有就跳过依赖假服务的测试。
pub fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn scratch_home(label: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "willdeep-plugin-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).expect("scratch home");
    home
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(path, contents).expect("write fixture");
}

/// 装一个插件：`permissions` 与 `servers`（`dependencies.mcpServers`）都是 JSON 数组
/// 字面量；`mcp.json` 定义一个名为 `srv` 的服务，跑假服务脚本，事件写到
/// `<home>/fake-<id>.jsonl`。返回事件日志路径。
pub fn install_fake_plugin(home: &Path, id: &str, permissions: &str, servers: &str) -> PathBuf {
    let root = home.join("plugins").join(id).join("1.0.0");
    write(
        &root.join(".codex-plugin/plugin.json"),
        &format!(r#"{{"name":"{id}","version":"1.0.0","interface":{{"displayName":"{id}"}}}}"#),
    );
    write(
        &root.join(".willdeep-plugin/plugin.json"),
        &format!(
            r#"{{"schemaVersion":1,"permissions":{permissions},"dependencies":{{"mcpServers":{servers}}},
                "contributes":{{"destinations":[],"pages":[],"commands":[]}}}}"#
        ),
    );
    write(&root.join(".willdeep-plugin/locales/en.json"), "{}");
    write(&root.join("server.py"), FAKE_PLUGIN_SERVER);
    let log = home.join(format!("fake-{id}.jsonl"));
    // 路径必须经 JSON 转义：Windows 的 `C:\Users\…` 直接拼进字符串是非法转义，
    // mcp.json 解析失败，整个插件包会被发现逻辑跳过。
    let mcp = serde_json::json!({
        "mcpServers": {
            "srv": {
                "command": "python3",
                "args": ["${pluginRoot}/server.py"],
                "env": { "FAKE_MCP_LOG": log.to_string_lossy() },
                "startup_timeout_sec": 5
            }
        }
    });
    write(&root.join("mcp.json"), &mcp.to_string());
    log
}

pub fn events(log: &Path, name: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 的临时目录是 `C:\Users\…`；Unix 目录名里也能放反斜杠，借它在任意平台
    /// 复现：夹具写出的 mcp.json 必须仍是合法 JSON，插件才会被发现。
    #[tokio::test]
    async fn fake_plugin_is_discovered_when_home_contains_backslashes() {
        let home = scratch_home("back\\slash");
        install_fake_plugin(&home, "demo", r#"["process.execute"]"#, r#"["srv"]"#);
        let host = crate::plugin::host::PluginHost::discover(&home).expect("host");
        let approved = host.approve("demo", 1).await;
        let _ = std::fs::remove_dir_all(&home);
        approved.expect("fixture plugin should be discovered");
    }
}
