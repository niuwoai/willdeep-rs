# willdeep-runtime-client

WillDeep Runtime 控制面的 Rust SDK。Runtime 是一个本机常驻的 daemon
（`willdeep daemon start`），持有会话、轮次、Agent、审批和 Diff 审查的状态机；
`willdeep` 的 CLI、TUI 和 Web 桥接用的就是这个 crate——每个稳定操作一个类型化方法，
共享 [`willdeep-runtime-protocol`](https://crates.io/crates/willdeep-runtime-protocol) 的
DTO 与响应信封，外加一条可按游标续传的 NDJSON 事件流。

```toml
[dependencies]
willdeep-runtime-client = "0.78.0-rc1"
willdeep-runtime-protocol = "0.78.0-rc1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## 连接

Runtime 只监听回环地址或本机 socket，并要求它启动时写进
`$WILLDEEP_HOME/runtime/daemon.json`（缺省 `~/.willdeep`，文件 `0600`）的随机 Token。
读这个文件，有本机传输就优先用本机传输：

```rust
use willdeep_runtime_client::RuntimeClient;
use willdeep_runtime_protocol::ApiResponse;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("WILLDEEP_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".willdeep"));
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("runtime/daemon.json"))?)?;
    let token = state["token"].as_str().unwrap_or_default();
    let client = match state["local_transport"]["path"].as_str() {
        #[cfg(unix)]
        Some(socket) => RuntimeClient::new_unix_socket(socket, token)?,
        _ => RuntimeClient::new(
            format!("http://{}", state["address"].as_str().unwrap_or_default()),
            token,
        )?,
    };
    match client.capabilities(None).await? {
        ApiResponse::Ok { data, meta } => println!(
            "runtime {} · protocol {} · {} operations",
            meta.server_version,
            data.protocol_version,
            data.operations.len()
        ),
        ApiResponse::Error { error, .. } => eprintln!("runtime refused: {error}"),
    }
    Ok(())
}
```

`RuntimeClient::new` 只接受 `http://127.0.0.1:…`、`http://[::1]:…`、`http://localhost:…`，
别的地址一律 `ClientError::UnsafeEndpoint`：控制面不是给远程用的，远程要走
你自己的网关和鉴权。

## 提交一轮、跟到结束

```bash
willdeep daemon start
cargo run -p willdeep-runtime-client --example submit_turn -- /path/to/workspace "总结这个项目的风险"
cargo run -p willdeep-runtime-client --example tail_events -- 0
```

`examples/submit_turn.rs`：创建会话、`turn.submit`、从提交前的事件游标开始流式读
`task.output`，遇到该轮的 `turn.completed` / `turn.partial` / `turn.failed` 停下。
`examples/tail_events.rs`：从任意游标尾随事件流，流被关掉（比如 `willdeep daemon upgrade`
交接）就沿最后一个序号重连。

## 覆盖的操作

会话（创建 / 搜索 / 改名 / 换模型 / 换审批档 / fork / 归档 / 删除 / 导出）、Agent（派生 /
提示 / 插话 / 等待 / 停止 / 重试）、任务与轮次（提交 / 停止 / 诊断）、审批与提问、
事件（分页与 NDJSON 流）、Diff（快照 / 内容 / 审阅 / 验证 / 归属 / 提交预览 / 撤销）、
worktree（审查 / 合并 / 审计 / 隔离）、工具与产物。操作语义、错误码和限制见
[`docs/RUNTIME_CONTROL_API.md`](https://github.com/niuwoai/willdeep-rs/blob/main/docs/RUNTIME_CONTROL_API.md)。

## 错误与版本

- `ClientError` 区分传输失败（`Http`）、HTTP 状态（`HttpStatus`）、信封解析失败
  （`InvalidResponse`）与 NDJSON 流的三种损坏；`status_code()` 给诊断用。
  业务层的拒绝在 `ApiResponse::Error` 里，带稳定 `ErrorCode` 与 `retryable`。
- crate 版本跟随 `willdeep` 发行版本（同一个 workspace、同一天发）；线上契约是
  `willdeep_runtime_protocol::PROTOCOL_VERSION`，连接后用 `capabilities` 协商，
  只调服务端声明过的操作。
- Runtime 升级用 `willdeep daemon upgrade` 排空交接：旧进程先拒收新工作、跑完手上的，
  新进程换一套 Token 与传输身份。长连接客户端应在流关闭后重新读 `daemon.json` 再沿游标续传。
