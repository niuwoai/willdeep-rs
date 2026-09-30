//! 内置插件的 MCP 服务端：willdeep 自己以 `plugin serve-builtin <id>` 跑在
//! 插件宿主的 stdio 另一头。
//!
//! 协议是逐行 JSON-RPC，与宿主侧 `willdeep_core::mcp::stdio` 对称：
//!
//! - 宿主发来的请求（有 `method` 有 `id`）：`initialize`、`tools/list`、
//!   `tools/call`。每条起一个任务处理——`tools/call` 可能要几分钟（圆桌讨论），
//!   而且中途要发反向请求，不能堵住读循环。
//! - 宿主对反向请求的响应（有 `id`、没有 `method`）：按 id 交给等它的调用。
//! - 通知（没有 `id`）：忽略。
//!
//! 写 stdout 的只有一把锁，行不会交错。stdout 只放协议，诊断写 stderr。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

/// 一个内置插件：声明工具，处理调用。
#[async_trait]
pub(crate) trait BuiltinPlugin: Send + Sync {
    /// MCP 服务名（插件包 `mcp.json` 里的键），也是 `serverInfo.name`。
    fn server_name(&self) -> &'static str;
    /// `tools/list` 的 `tools` 数组：`{name, description, inputSchema}`。
    fn tools(&self) -> Vec<Value>;
    /// 处理一次 `tools/call`。`Err` 以 `isError: true` 回给模型，不是协议错误。
    async fn call(&self, name: &str, arguments: Value, host: &HostClient)
    -> Result<String, String>;
}

type Writer = Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// 向宿主发反向请求（`willdeep/ai/complete` 等）的句柄。
#[derive(Clone)]
pub(crate) struct HostClient {
    writer: Writer,
    pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
}

impl HostClient {
    /// 发一条反向请求，等宿主的响应。宿主回 `error` 时返回它的 message。
    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        // 与宿主自己的请求 id 分开计数也不会冲突：两个方向各管各的 id。
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "host client is unavailable".to_owned())?
            .insert(id, sender);
        write_line(
            &self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await?;
        let response = receiver
            .await
            .map_err(|_| "the host closed the connection".to_owned())?;
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("host request failed")
                .to_owned());
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// `willdeep/ai/complete`：一问一答，返回模型的正文。
    pub(crate) async fn complete(
        &self,
        system: &str,
        user: &str,
        max_output_tokens: u32,
    ) -> Result<String, String> {
        let result = self
            .request(
                willdeep_core::plugin::host_requests::AI_COMPLETE,
                json!({
                    "system": system,
                    "messages": [{"role": "user", "content": user}],
                    "max_output_tokens": max_output_tokens,
                }),
            )
            .await?;
        result
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the host returned no text".to_owned())
    }
}

async fn write_line(writer: &Writer, message: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    line.push(b'\n');
    let mut writer = writer.lock().await;
    writer
        .write_all(&line)
        .await
        .map_err(|error| error.to_string())?;
    writer.flush().await.map_err(|error| error.to_string())
}

/// 跑到输入关闭为止。
pub(crate) async fn serve(
    plugin: Arc<dyn BuiltinPlugin>,
    input: impl AsyncRead + Send + Unpin + 'static,
    output: impl AsyncWrite + Send + Unpin + 'static,
) {
    let writer: Writer = Arc::new(Mutex::new(Box::new(output)));
    let host = HostClient {
        writer: writer.clone(),
        pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
        next_id: Arc::new(AtomicU64::new(1)),
    };
    let mut lines = BufReader::new(input).lines();
    let mut tasks = Vec::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str), id) {
            (Some(method), Some(id)) => {
                let plugin = plugin.clone();
                let host = host.clone();
                let writer = writer.clone();
                let method = method.to_owned();
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                tasks.push(tokio::spawn(async move {
                    let response = match handle(&*plugin, &method, params, &host).await {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err((code, message)) => json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": code, "message": message}
                        }),
                    };
                    if let Err(error) = write_line(&writer, &response).await {
                        eprintln!("willdeep builtin plugin: cannot answer {method}: {error}");
                    }
                }));
            }
            (None, Some(id)) => {
                let waiter = id
                    .as_u64()
                    .and_then(|id| host.pending.lock().ok()?.remove(&id));
                if let Some(waiter) = waiter {
                    let _ = waiter.send(message);
                }
            }
            _ => {}
        }
    }
    // 输入关了（宿主退出）：等在飞的调用写完最后一行再走。
    for task in tasks {
        let _ = task.await;
    }
}

async fn handle(
    plugin: &dyn BuiltinPlugin,
    method: &str,
    params: Value,
    host: &HostClient,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .cloned()
                .unwrap_or_else(|| json!("2025-06-18")),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": plugin.server_name(), "version": willdeep_core::VERSION},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": plugin.tools()})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "tools/call needs a tool name".to_owned()))?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = match plugin.call(name, arguments, host).await {
                Ok(text) => (text, false),
                Err(message) => (message, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        other => Err((-32601, format!("Method not found: {other}"))),
    }
}

/// 从工具参数里取字符串（去掉首尾空白，空串当没给）。
pub(crate) fn string_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| match value {
            Value::String(text) => Some(text.trim().to_owned()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct Echo;

    #[async_trait]
    impl BuiltinPlugin for Echo {
        fn server_name(&self) -> &'static str {
            "echo"
        }
        fn tools(&self) -> Vec<Value> {
            vec![
                json!({"name": "shout", "description": "ask the host", "inputSchema": {"type": "object"}}),
            ]
        }
        async fn call(
            &self,
            name: &str,
            arguments: Value,
            host: &HostClient,
        ) -> Result<String, String> {
            match name {
                "shout" => host
                    .complete(
                        "system",
                        &string_arg(&arguments, "text").unwrap_or_default(),
                        64,
                    )
                    .await
                    .map(|text| text.to_uppercase()),
                _ => Err(format!("unknown tool {name}")),
            }
        }
    }

    /// 宿主那一头：发请求、答反向请求，全走真实的逐行 JSON-RPC。
    #[tokio::test]
    async fn serves_tools_and_round_trips_a_reverse_request() {
        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server = tokio::spawn(serve(Arc::new(Echo), plugin_read, plugin_write));
        let (host_read, mut host_write) = tokio::io::split(host_side);
        let mut replies = BufReader::new(host_read).lines();
        async fn send_to(writer: &mut (impl AsyncWriteExt + Unpin), message: Value) {
            let mut line = message.to_string();
            line.push('\n');
            writer.write_all(line.as_bytes()).await.unwrap();
        }
        macro_rules! send {
            ($message:expr) => {
                send_to(&mut host_write, $message).await
            };
        }

        send!(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})
        );
        let init: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "echo");

        send!(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        send!(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let list: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], "shout");

        send!(
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "shout", "arguments": {"text": "hi"}}})
        );
        let reverse: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reverse["method"], "willdeep/ai/complete");
        assert_eq!(reverse["params"]["messages"][0]["content"], "hi");
        send!(json!({"jsonrpc": "2.0", "id": reverse["id"], "result": {"text": "hello"}}));
        let called: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(called["id"], 3);
        assert_eq!(called["result"]["content"][0]["text"], "HELLO");
        assert_eq!(called["result"]["isError"], false);

        send!(
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "nope"}})
        );
        let failed: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(failed["result"]["isError"], true);

        send!(json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"}));
        let unknown: Value =
            serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);

        host_write.shutdown().await.unwrap();
        drop(host_write);
        server.await.unwrap();
    }
}
