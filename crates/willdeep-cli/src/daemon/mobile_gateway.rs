//! 手机中继网关：Runtime Daemon 里的一个受限客户端。
//!
//! 手机经公网中继（`mobile-gateway.v1`）连到这里，看得到整个 Runtime——所有会话、
//! 所有待处理的审批与提问——但能做的写操作只有四件：发提示词、在已登记工作区里
//! 新建会话、停止当前轮次、批准 / 拒绝审批与回答提问。每个手机命令都映射到一个
//! 已有的 Runtime 操作，经 [`control_api::execute`] 分发，参数校验、幂等、Drain
//! 闸门和公共投影全部复用，不另写一套业务逻辑。
//!
//! 设计与取舍见 `docs/decisions/2026-09-23-daemon-mobile-relay.md`。

use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use willdeep_runtime_protocol::{ApiRequest, ApiResponse, MobileRelayEnabled, MobileRelayStatus};

use super::*;
use crate::mobile::{RelayCredentials, UnsafeCredentialPermissions};

mod commands;
mod projection;
#[cfg(test)]
mod tests;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// 手机端每 5 秒发一次 `session.list` 当心跳（旧版 15 秒）。一分钟没动静就当它走了：
/// 不再往中继推事件，省公网流量；回来时先补一份完整快照。
const PHONE_ACTIVITY_WINDOW: Duration = Duration::from_secs(60);
/// 会话改名、归档这类变化攒一攒再推快照。
const SNAPSHOT_DEBOUNCE: Duration = Duration::from_secs(2);

/// 只有手机会发的信封类型（与 macOS 桌面端 `AgentMobileGatewayPhonePresence.phoneCommandTypes`
/// 同一张表）。不在表里的——`ack`、`error`、`state.snapshot`、`message.append`，
/// 也就是中继回声或同一 room 里别的桌面端的回复——一律静默丢弃。不这样做的话，
/// 两个桌面端会把对方的 `ack` 当未知命令回 `error`，再把对方的 `error` 回一条
/// `error`，无限往返。
const PHONE_COMMANDS: &[&str] = &[
    "session.list",
    "session.create",
    "workspace.list",
    "capabilities.get",
    "push.register",
    "session.select",
    "message.send",
    "turn.stop",
    "tool.decide",
    "patch.decide",
    "diff.get",
    "job.kill",
    "file.read",
    "queue.update",
];

/// 网关能碰到的 Runtime 操作，**字面量**白名单。
///
/// 故意不写成「`.list` 结尾的都行」这种命名规则：规则会自动放行将来新加的操作，
/// 而手机上能做什么必须是逐条想清楚之后写进来的。删除 / 归档 / 改名会话、登记
/// 工作区、改审批档位与模型、派生子 Agent 都不在这里。
const RUNTIME_OPERATIONS: &[&str] = &[
    "session.list",
    "session.get",
    "session.create",
    "workspace.list",
    "task.list",
    "task.get",
    "approval.list",
    "approval.resolve",
    "question.list",
    "question.answer",
    "turn.submit",
    "turn.stop",
];

/// `mobile.enable` / `mobile.disable` 失败时交给调用方的话。闭集合、不含本机路径——
/// 控制面会把它原样透给 TUI，所以这里说的必须是用户能照做的事。
#[derive(Debug, thiserror::Error)]
pub(crate) enum MobileRelayError {
    #[error(
        "mobile relay credentials are readable by other users; run `chmod 600 $WILLDEEP_HOME/mobile-relay.toml` and retry"
    )]
    UnsafePermissions,
    #[error(
        "mobile relay credentials could not be read or saved; delete $WILLDEEP_HOME/mobile-relay.toml and retry to pair again"
    )]
    Credentials,
}

fn credential_error(error: anyhow::Error) -> MobileRelayError {
    if error
        .downcast_ref::<UnsafeCredentialPermissions>()
        .is_some()
    {
        return MobileRelayError::UnsafePermissions;
    }
    eprintln!("mobile relay credentials: {error:#}");
    MobileRelayError::Credentials
}

/// 中继开关与连接状态。挂在 [`ServerState`] 上，控制面的 `mobile.*` 读写它。
pub(super) struct MobileRelay {
    home: PathBuf,
    /// 网关任务要回头调用控制面，而 `ServerState` 又持有本结构——所以存弱引用，
    /// 在 `ServerState` 建好之后由 [`MobileRelay::bind`] 补上。
    server: std::sync::OnceLock<Weak<ServerState>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shared: Arc<RelayShared>,
}

#[derive(Default)]
struct RelayShared {
    enabled: AtomicBool,
    connected: AtomicBool,
    /// 最近一次收到手机命令的 Unix 秒；0 表示从没收到过。
    last_phone_command_at: AtomicU64,
    relay_host: Mutex<Option<String>>,
}

impl MobileRelay {
    pub(super) fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            server: std::sync::OnceLock::new(),
            task: Mutex::new(None),
            shared: Arc::new(RelayShared::default()),
        }
    }

    pub(super) fn bind(&self, server: &Arc<ServerState>) {
        let _ = self.server.set(Arc::downgrade(server));
    }

    /// Daemon 启动时调用：上次开着就接着连。凭据有问题只记日志——中继是附属功能，
    /// 不能因为它拦住整个 Runtime 启动。
    pub(super) fn resume(&self) {
        match RelayCredentials::load(&self.home) {
            Ok(Some(credentials)) if credentials.enabled() => self.start(credentials),
            Ok(_) => {}
            Err(error) => eprintln!("mobile relay not resumed: {error:#}"),
        }
    }

    pub(super) fn status(&self) -> MobileRelayStatus {
        let last = self.shared.last_phone_command_at.load(Ordering::Acquire);
        let enabled = self.shared.enabled.load(Ordering::Acquire);
        MobileRelayStatus {
            enabled,
            // `abort()` 是异步生效的：关中继的那一刻，连接任务可能正在另一个线程上
            // 把 connected 写回 true。关了就一律报未连接。
            connected: enabled && self.shared.connected.load(Ordering::Acquire),
            phone_active: last != 0
                && now().saturating_sub(last) <= PHONE_ACTIVITY_WINDOW.as_secs(),
            last_phone_command_at: (last != 0).then_some(last),
            relay_host: self
                .shared
                .relay_host
                .lock()
                .ok()
                .and_then(|host| host.clone()),
        }
    }

    /// 打开中继（持久化）并返回配对 URL。已经连着的话只是再取一次 URL。
    pub(super) fn enable(&self) -> Result<MobileRelayEnabled, MobileRelayError> {
        let credentials =
            RelayCredentials::save_enabled(&self.home, true).map_err(credential_error)?;
        let pairing_url = credentials.pairing_url().map_err(credential_error)?;
        self.start(credentials);
        Ok(MobileRelayEnabled {
            status: self.status(),
            pairing_url,
        })
    }

    pub(super) fn disable(&self) -> Result<MobileRelayStatus, MobileRelayError> {
        // 从没开过就没有凭据文件，不必为了写一个 false 去生成 room 与 token。
        if RelayCredentials::load(&self.home)
            .map_err(credential_error)?
            .is_some()
        {
            RelayCredentials::save_enabled(&self.home, false).map_err(credential_error)?;
        }
        self.stop();
        Ok(self.status())
    }

    fn start(&self, credentials: RelayCredentials) {
        let Ok(mut task) = self.task.lock() else {
            return;
        };
        self.shared.enabled.store(true, Ordering::Release);
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let Some(server) = self.server.get().cloned() else {
            eprintln!("mobile relay requested before the Runtime finished starting");
            return;
        };
        if let Ok(mut host) = self.shared.relay_host.lock() {
            *host = credentials.relay_host();
        }
        *task = Some(tokio::spawn(run_gateway(
            server,
            credentials,
            self.shared.clone(),
        )));
    }

    fn stop(&self) {
        self.shared.enabled.store(false, Ordering::Release);
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
        self.shared.connected.store(false, Ordering::Release);
    }
}

impl Drop for MobileRelay {
    fn drop(&mut self) {
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ConnectionEnd {
    Disconnected,
    Shutdown,
}

async fn run_gateway(
    server: Weak<ServerState>,
    credentials: RelayCredentials,
    shared: Arc<RelayShared>,
) {
    let Some((events, mut shutdown)) = server
        .upgrade()
        .map(|server| (server.events.clone(), server.shutdown.subscribe()))
    else {
        return;
    };
    let mut gateway = Gateway::new(server, shared.clone());
    loop {
        if *shutdown.borrow() {
            return;
        }
        match credentials.websocket_request() {
            Ok(request) => {
                if let Ok((socket, _)) = tokio_tungstenite::connect_async(request).await {
                    shared.connected.store(true, Ordering::Release);
                    let end = gateway.serve(socket, &events, &mut shutdown).await;
                    shared.connected.store(false, Ordering::Release);
                    if end == ConnectionEnd::Shutdown {
                        return;
                    }
                }
            }
            Err(error) => eprintln!("mobile relay request: {error}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// 手机发来的一条信封。
pub(super) struct PhoneEnvelope {
    pub(super) id: Option<String>,
    pub(super) kind: String,
    pub(super) session_id: Option<String>,
    pub(super) payload: Value,
}

impl PhoneEnvelope {
    /// 解析得出来、且类型是手机命令才返回；其余（回声、别的桌面端的回复）都是 `None`。
    pub(super) fn parse(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let kind = value.get("type")?.as_str()?;
        if !PHONE_COMMANDS.contains(&kind) {
            return None;
        }
        // `workspace.list` 既是手机的命令名，也是桌面端回复的类型名。带着
        // `payload.workspaces` 的是回复（自己的回声或别的桌面端），当命令回了就是
        // 两边互相回复的死循环。
        if kind == "workspace.list" && value.pointer("/payload/workspaces").is_some() {
            return None;
        }
        let string = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            id: string("id"),
            kind: kind.to_owned(),
            session_id: string("session_id"),
            payload: value.get("payload").cloned().unwrap_or(Value::Null),
        })
    }
}

/// 调用 Runtime 操作失败时给手机的话。控制面的错误文案本来就是对外脱敏过的。
#[derive(Debug)]
pub(super) struct CommandError(pub(super) String);

impl CommandError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 进程内调用一次 Runtime 操作。只认 [`RUNTIME_OPERATIONS`] 里的名字。
pub(super) async fn call<T: serde::de::DeserializeOwned>(
    server: &ServerState,
    operation: &'static str,
    params: Value,
    request_id: Option<uuid::Uuid>,
) -> Result<T, CommandError> {
    if !RUNTIME_OPERATIONS.contains(&operation) {
        return Err(CommandError::new(format!(
            "operation is not available to the mobile relay: {operation}"
        )));
    }
    let mut request = ApiRequest::new(operation, params);
    if let Some(request_id) = request_id {
        request.request_id = request_id;
    }
    match control_api::execute(server, request).await.body {
        ApiResponse::Ok { data, .. } => serde_json::from_value(data).map_err(|error| {
            eprintln!("mobile relay could not decode {operation}: {error}");
            CommandError::new("internal Runtime error")
        }),
        ApiResponse::Error { error, .. } => Err(CommandError(error.message)),
    }
}

/// 一条还没落进 Core Session 文件的消息（见 [`Gateway::tails`]）。
#[derive(Clone, Debug)]
pub(super) struct TailMessage {
    pub(super) id: String,
    pub(super) role: &'static str,
    pub(super) content: String,
    pub(super) created_at: u64,
    /// 所在轮次结束的时刻。过了宽限期还没在历史里出现（轮次失败、被回退）就丢掉。
    pub(super) settled_at: Option<Instant>,
}

/// 一条会话的历史投影缓存。键是会话摘要里的 (updated_at, message_count)：摘要本身
/// 按文件 mtime 缓存，5 秒一次的心跳因此不会反复解析大会话文件。
pub(super) struct HistoryCache {
    pub(super) key: (u64, usize),
    pub(super) items: Vec<Value>,
}

/// 一条中继连接背后的网关状态。跨重连保留：手机选中的会话、实时尾巴和缓存都不该
/// 因为中继抖一下就丢掉。
pub(super) struct Gateway {
    server: Weak<ServerState>,
    shared: Arc<RelayShared>,
    /// 手机发起的轮次记在谁名下（`mobile:<进程随机 id>`），用量账本认得这个前缀。
    pub(super) origin_client: String,
    /// 手机当前选中的会话，快照里的 `active_session_id`。
    pub(super) selected: Option<uuid::Uuid>,
    phone_seen: Option<Instant>,
    pub(super) snapshot_dirty: bool,
    /// 进行中轮次推过去的用户回显与助手消息。Core Session 文件在轮次结束时才落盘，
    /// 而 Android 每 5 秒拿快照整体替换对话列表——没有这段，刚推过去的消息会被下一次
    /// 心跳冲掉。
    pub(super) tails: HashMap<uuid::Uuid, Vec<TailMessage>>,
    pub(super) task_sessions: HashMap<uuid::Uuid, Option<uuid::Uuid>>,
    pub(super) history: HashMap<uuid::Uuid, HistoryCache>,
}

impl Gateway {
    fn new(server: Weak<ServerState>, shared: Arc<RelayShared>) -> Self {
        Self {
            server,
            shared,
            origin_client: crate::client_identity(crate::Surface::Mobile).to_owned(),
            selected: None,
            phone_seen: None,
            snapshot_dirty: false,
            tails: HashMap::new(),
            task_sessions: HashMap::new(),
            history: HashMap::new(),
        }
    }

    fn phone_active(&self) -> bool {
        self.phone_seen
            .is_some_and(|seen| seen.elapsed() <= PHONE_ACTIVITY_WINDOW)
    }

    /// 记一次手机命令。返回 true 表示手机是从「不在场」回来的，欠它一份完整快照。
    fn record_phone_command(&mut self) -> bool {
        let returning = !self.phone_active();
        self.phone_seen = Some(Instant::now());
        self.shared
            .last_phone_command_at
            .store(now(), Ordering::Release);
        returning
    }

    async fn serve<S>(
        &mut self,
        socket: tokio_tungstenite::WebSocketStream<S>,
        events: &EventLog,
        shutdown: &mut watch::Receiver<bool>,
    ) -> ConnectionEnd
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let (mut output, mut input) = socket.split();
        let mut live = events.subscribe();
        // 新连接上的手机得重新报到；在那之前不往中继推任何东西。
        self.phone_seen = None;
        let mut tick = tokio::time::interval(SNAPSHOT_DEBOUNCE);
        loop {
            let outgoing = tokio::select! {
                _ = shutdown.changed() => {
                    let _ = output.send(WebSocketMessage::Close(None)).await;
                    return ConnectionEnd::Shutdown;
                }
                event = live.recv() => match event {
                    Ok(event) => self.on_runtime_event(event).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.snapshot_dirty = true;
                        Vec::new()
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return ConnectionEnd::Shutdown;
                    }
                },
                incoming = input.next() => match incoming {
                    Some(Ok(WebSocketMessage::Text(text))) => self.on_phone_text(&text).await,
                    Some(Ok(WebSocketMessage::Ping(payload))) => {
                        if output.send(WebSocketMessage::Pong(payload)).await.is_err() {
                            return ConnectionEnd::Disconnected;
                        }
                        Vec::new()
                    }
                    Some(Ok(WebSocketMessage::Close(_))) | None | Some(Err(_)) => {
                        return ConnectionEnd::Disconnected;
                    }
                    _ => Vec::new(),
                },
                _ = tick.tick() => self.on_tick().await,
            };
            for envelope in outgoing {
                if output
                    .send(WebSocketMessage::Text(envelope.to_string().into()))
                    .await
                    .is_err()
                {
                    return ConnectionEnd::Disconnected;
                }
            }
        }
    }

    pub(super) async fn on_phone_text(&mut self, text: &str) -> Vec<Value> {
        let Some(envelope) = PhoneEnvelope::parse(text) else {
            return Vec::new();
        };
        let Some(server) = self.server.upgrade() else {
            return Vec::new();
        };
        let returning = self.record_phone_command();
        let mut outgoing = self.handle_command(&server, envelope).await;
        // 刚回来的手机欠一份完整快照；命令本身已经回了快照的就不再补。
        if returning
            && !outgoing
                .iter()
                .any(|envelope| envelope["type"] == "state.snapshot")
        {
            outgoing.push(self.snapshot(&server, None).await);
        }
        outgoing
    }

    async fn on_runtime_event(&mut self, event: RuntimeEvent) -> Vec<Value> {
        let Some(server) = self.server.upgrade() else {
            return Vec::new();
        };
        // 状态（尾巴、缓存、脏标记）照常更新，只是手机不在场时不往外推。
        let outgoing = self.translate(&server, event).await;
        if self.phone_active() {
            outgoing
        } else {
            Vec::new()
        }
    }

    async fn on_tick(&mut self) -> Vec<Value> {
        if !self.snapshot_dirty || !self.phone_active() {
            return Vec::new();
        }
        let Some(server) = self.server.upgrade() else {
            return Vec::new();
        };
        vec![self.snapshot(&server, None).await]
    }
}
