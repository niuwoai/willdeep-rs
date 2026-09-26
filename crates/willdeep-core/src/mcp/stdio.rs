//! stdio 传输：子进程的 stdout 由一个常驻读任务独占。
//!
//! 为什么不是「发一条、同步读到对上 id 为止」：插件 MCP 服务会在任何时候往
//! stdout 写一条**服务端发给宿主的请求**（反向请求，见 [`HostRequestHandler`]），
//! 不只在宿主正等着它回话的时候。短剧工坊的本机 HTTP 入口在工作线程里出图，
//! 这时宿主这边可能根本没有在途的 stdio 请求——同步读法会让这条反向请求一直
//! 躺在管道里，插件那头等到超时。
//!
//! 所以读任务常驻：响应按 id 交给等它的请求，反向请求起一个任务处理完再把响应
//! 写回 stdin，通知直接丢。写 stdin 的只有两处（请求、反向请求的响应），共用
//! 一把锁，行不会交错。
//!
//! 超时语义与 macOS 宿主（`AgentMCPBridge.waitForResponse`）一致：衡量的是
//! 「服务端多久没动静」。宿主替它处理反向请求的那几十秒不算——处理期间不计时，
//! 处理完从头计，否则 10 秒超时的插件出一张图就必然超时。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, oneshot, watch};

use super::{HostRequestError, HostRequestHandler, McpError, McpServerConfig};

/// 单次反向请求的处理上限，与 macOS 宿主同值。出一张图通常几十秒，带参考图要
/// 先上传；十分钟之外只可能是上游挂住了，不能让等它的请求永远不超时。
pub const HOST_REQUEST_MAX_SECONDS: u64 = 600;

struct Shared {
    stdin: Mutex<ChildStdin>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<Value>>>,
    closed: AtomicBool,
    /// 正在处理的反向请求数。大于 0 时等待中的请求不计超时。
    host_requests_in_flight: AtomicUsize,
    /// 每次反向请求开始、结束都拨一下：等待方据此把超时从头计。
    activity: watch::Sender<u64>,
}

pub(super) struct StdioConnection {
    child: StdMutex<Child>,
    shared: Arc<Shared>,
    /// 同一连接上的请求串行，与 macOS 宿主一致：排队的时间不算进超时。
    turn: Mutex<()>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for StdioConnection {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl StdioConnection {
    pub(super) fn spawn(
        server: &str,
        config: &McpServerConfig,
        handler: Option<Arc<dyn HostRequestHandler>>,
    ) -> Result<Self, McpError> {
        let program = config.command.as_deref().unwrap_or_default();
        let mut command = Command::new(program);
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
        let (activity, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            stdin: Mutex::new(stdin),
            pending: StdMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            host_requests_in_flight: AtomicUsize::new(0),
            activity,
        });
        let reader = tokio::spawn(read_loop(
            server.to_owned(),
            stdout,
            shared.clone(),
            handler,
        ));
        Ok(Self {
            child: StdMutex::new(child),
            shared,
            turn: Mutex::new(()),
            next_id: AtomicU64::new(1),
            reader,
        })
    }

    /// 进程还活着、stdout 还没读到头。
    pub(super) fn is_alive(&self) -> bool {
        if self.shared.closed.load(Ordering::SeqCst) {
            return false;
        }
        match self.child.lock() {
            Ok(mut child) => matches!(child.try_wait(), Ok(None)),
            Err(_) => false,
        }
    }

    pub(super) async fn request(
        &self,
        server: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let _turn = self.turn.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, mut receiver) = oneshot::channel();
        self.pending().insert(id, sender);
        // 读任务可能恰好在插入之前收尾：那时它清不到这一条，这里自己兜住。
        if self.shared.closed.load(Ordering::SeqCst) {
            self.pending().remove(&id);
            return Err(McpError::Exited(server.to_owned()));
        }
        let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        if let Err(error) = self.write(&message).await {
            self.pending().remove(&id);
            return Err(error);
        }
        let mut activity = self.shared.activity.subscribe();
        let mut deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                answer = &mut receiver => {
                    let value = answer.map_err(|_| McpError::Exited(server.to_owned()))?;
                    if let Some(error) = value.get("error") {
                        return Err(McpError::Remote(error.to_string()));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                changed = activity.changed() => {
                    if changed.is_ok() {
                        deadline = tokio::time::Instant::now() + timeout;
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    if self.shared.host_requests_in_flight.load(Ordering::SeqCst) > 0 {
                        deadline = tokio::time::Instant::now() + timeout;
                        continue;
                    }
                    self.pending().remove(&id);
                    return Err(McpError::Timeout(server.to_owned()));
                }
            }
        }
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.write(&json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    async fn write(&self, message: &Value) -> Result<(), McpError> {
        write_line(&self.shared, message).await
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Value>>> {
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

async fn write_line(shared: &Shared, message: &Value) -> Result<(), McpError> {
    let line = format!("{}\n", serde_json::to_string(message)?);
    let mut stdin = shared.stdin.lock().await;
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_loop(
    server: String,
    mut stdout: BufReader<ChildStdout>,
    shared: Arc<Shared>,
    handler: Option<Arc<dyn HostRequestHandler>>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // 不是 JSON 的行（服务端误把日志写到了 stdout）跳过，不拖垮整条连接。
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                shared
                    .host_requests_in_flight
                    .fetch_add(1, Ordering::SeqCst);
                shared
                    .activity
                    .send_modify(|tick| *tick = tick.wrapping_add(1));
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                tokio::spawn(answer_host_request(
                    server.clone(),
                    shared.clone(),
                    handler.clone(),
                    id,
                    method,
                    params,
                ));
            }
            // 通知：资源变更之类，宿主目前不订阅推送，丢掉。
            (Some(_), None) => {}
            (None, Some(id)) => {
                if let Some(id) = id.as_u64()
                    && let Some(sender) = shared
                        .pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&id)
                {
                    let _ = sender.send(message);
                }
            }
            (None, None) => {}
        }
    }
    shared.closed.store(true, Ordering::SeqCst);
    // 丢掉全部等待者：它们各自收到 RecvError，报「服务端退出」而不是等到超时。
    shared
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

async fn answer_host_request(
    server: String,
    shared: Arc<Shared>,
    handler: Option<Arc<dyn HostRequestHandler>>,
    id: Value,
    method: String,
    params: Value,
) {
    let outcome = match &handler {
        // 用户手工配置的 MCP 服务没有经过插件清单的权限声明与安装批准，
        // 一律回 method not found。
        None => Err(HostRequestError::method_not_found(
            "Host requests are only available to plugin MCP servers.",
        )),
        Some(handler) if !handler.methods().iter().any(|item| item == &method) => Err(
            HostRequestError::method_not_found(format!("Method not found: {method}")),
        ),
        Some(handler) => match tokio::time::timeout(
            Duration::from_secs(HOST_REQUEST_MAX_SECONDS),
            handler.handle(&method, params),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(HostRequestError {
                code: HostRequestError::TIMED_OUT,
                message: format!(
                    "Host request {method} did not finish within {HOST_REQUEST_MAX_SECONDS} seconds."
                ),
            }),
        },
    };
    let response = match outcome {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => {
            // 结构化一行：服务名、方法、错误码。错误正文可能带上游回的内容，
            // 但不会带凭据——处理器只回稳定的错误码与说明。
            eprintln!(
                "warning: mcp_host_request_failed server={server} method={method} code={}",
                error.code
            );
            json!({"jsonrpc":"2.0","id":id,"error":{"code":error.code,"message":error.message}})
        }
    };
    // 写失败（服务端刚好退出）只能放弃：等它的一方会因为进程退出收到错误。
    let _ = write_line(&shared, &response).await;
    shared
        .host_requests_in_flight
        .fetch_sub(1, Ordering::SeqCst);
    shared
        .activity
        .send_modify(|tick| *tick = tick.wrapping_add(1));
}
