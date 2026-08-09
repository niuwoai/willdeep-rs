use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::types::ToolDefinition;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub startup_timeout_seconds: u64,
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn default_timeout() -> u64 {
    30
}
fn enabled() -> bool {
    true
}

#[derive(Clone, Default)]
pub struct McpRegistry {
    servers: BTreeMap<String, Arc<McpServer>>,
    tools: BTreeMap<String, McpTool>,
}

#[derive(Clone)]
struct McpTool {
    server: String,
    remote_name: String,
    definition: ToolDefinition,
}
struct McpServer {
    name: String,
    state: Mutex<McpConnection>,
    timeout: Duration,
}
struct McpConnection {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpRegistry {
    pub async fn connect(configs: &BTreeMap<String, McpServerConfig>) -> Result<Self, McpError> {
        let mut registry = Self::default();
        for (name, config) in configs.iter().filter(|(_, c)| c.enabled) {
            let server = Arc::new(McpServer::start(name, config).await?);
            let listed = server.request("tools/list", json!({})).await?;
            for item in listed
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(remote_name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let local_name = format!("mcp__{}__{}", sanitize(name), sanitize(remote_name));
                let definition = ToolDefinition {
                    name: local_name.clone(),
                    description: item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("MCP tool")
                        .to_owned(),
                    parameters: item
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object"})),
                };
                registry.tools.insert(
                    local_name,
                    McpTool {
                        server: name.clone(),
                        remote_name: remote_name.to_owned(),
                        definition,
                    },
                );
            }
            registry.servers.insert(name.clone(), server);
        }
        Ok(registry)
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|v| v.definition.clone()).collect()
    }
    pub fn handles(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
    pub async fn call(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| McpError::UnknownTool(name.to_owned()))?;
        let server = self
            .servers
            .get(&tool.server)
            .ok_or_else(|| McpError::MissingServer(tool.server.clone()))?;
        let result = server
            .request(
                "tools/call",
                json!({"name": tool.remote_name, "arguments": arguments}),
            )
            .await?;
        Ok(serde_json::to_string_pretty(&result)?)
    }
}

impl McpServer {
    async fn start(name: &str, config: &McpServerConfig) -> Result<Self, McpError> {
        if config.command.trim().is_empty() {
            return Err(McpError::InvalidConfig(format!(
                "mcp_servers.{name}.command is empty"
            )));
        }
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or(McpError::MissingPipe)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(McpError::MissingPipe)?);
        let server = Self {
            name: name.to_owned(),
            state: Mutex::new(McpConnection {
                _child: child,
                stdin,
                stdout,
                next_id: 1,
            }),
            timeout: Duration::from_secs(config.startup_timeout_seconds.clamp(1, 300)),
        };
        server.request("initialize", json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"willdeep","version":crate::VERSION}})).await?;
        server
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(server)
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        tokio::time::timeout(self.timeout, async {
            let mut state = self.state.lock().await;
            let id = state.next_id;
            state.next_id += 1;
            write_message(
                &mut state.stdin,
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            )
            .await?;
            loop {
                let mut line = String::new();
                if state.stdout.read_line(&mut line).await? == 0 {
                    return Err(McpError::Exited(self.name.clone()));
                }
                let value: Value = serde_json::from_str(&line)?;
                if value.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Err(McpError::Remote(error.to_string()));
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        })
        .await
        .map_err(|_| McpError::Timeout(self.name.clone()))?
    }
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let mut state = self.state.lock().await;
        write_message(
            &mut state.stdin,
            &json!({"jsonrpc":"2.0","method":method,"params":params}),
        )
        .await
    }
}
async fn write_message(stdin: &mut ChildStdin, value: &Value) -> Result<(), McpError> {
    stdin
        .write_all(format!("{}\n", serde_json::to_string(value)?).as_bytes())
        .await?;
    stdin.flush().await?;
    Ok(())
}
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("invalid MCP configuration: {0}")]
    InvalidConfig(String),
    #[error("MCP process is missing a stdio pipe")]
    MissingPipe,
    #[error("MCP server exited: {0}")]
    Exited(String),
    #[error("MCP server timed out: {0}")]
    Timeout(String),
    #[error("MCP server returned an error: {0}")]
    Remote(String),
    #[error("unknown MCP tool: {0}")]
    UnknownTool(String),
    #[error("MCP server is missing: {0}")]
    MissingServer(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespaces_external_tool_names() {
        assert_eq!(sanitize("github.com"), "github_com");
        assert_eq!(sanitize("create-issue"), "create-issue");
    }
    #[test]
    fn parses_stdio_server_config() {
        let config: McpServerConfig = serde_json::from_value(
            json!({"command":"npx","args":["server"],"env":{"TOKEN":"from-env"}}),
        )
        .unwrap();
        assert!(config.enabled);
        assert_eq!(config.startup_timeout_seconds, 30);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completes_stdio_handshake_discovery_and_call() {
        let script = r#"read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}'
read initialized
read list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}]}}'
read call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}]}}'
"#;
        let mut configs = BTreeMap::new();
        configs.insert(
            "mock".to_owned(),
            McpServerConfig {
                command: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), script.to_owned()],
                env: BTreeMap::new(),
                startup_timeout_seconds: 5,
                enabled: true,
            },
        );
        let registry = McpRegistry::connect(&configs).await.unwrap();
        assert!(registry.handles("mcp__mock__echo"));
        let result = registry
            .call("mcp__mock__echo", json!({"text":"ping"}))
            .await
            .unwrap();
        assert!(result.contains("pong"));
    }
}
