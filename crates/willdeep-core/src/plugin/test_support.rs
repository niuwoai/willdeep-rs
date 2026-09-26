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
    write(
        &root.join("mcp.json"),
        &format!(
            r#"{{"mcpServers":{{"srv":{{"command":"python3","args":["${{pluginRoot}}/server.py"],
                "env":{{"FAKE_MCP_LOG":"{}"}},"startup_timeout_sec":5}}}}}}"#,
            log.display()
        ),
    );
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
