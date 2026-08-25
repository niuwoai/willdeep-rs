//! 产品遥测：匿名上报使用数据到 `og.niuwoai.com`。
//!
//! 协议与字段清单是三端共用的契约，canonical 版本在 muchtoken 仓库的
//! `docs/client-telemetry.md`；Swift 端的对应实现是 `Xedit/Telemetry*.swift`。
//!
//! 这里只有枚举、短标识符和数值——没有任何字段能装下提示词、模型回复、
//! 文件路径、终端命令或原始错误。本地清洗是「不合规就丢弃」而不是
//! 「截断后凑合上报」：本地就拦下来的，连传输过程都不存在。
//!
//! CLI 与桌面端的节奏不同：桌面进程长驻，攒够 20 条或 30 秒发一批；CLI
//! 往往跑完一条命令就退出，所以这里改成「事件写本地队列文件，启动和退出
//! 各 flush 一次」。上次没发出去的事件躺在队列里，下次跑 CLI 时补发。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 上报目的地：some.im 的上游源站。
const TELEMETRY_ORIGIN: &str = "https://og.niuwoai.com";
const EVENTS_PATH: &str = "/api/v1/public/telemetry/events";
const FORGET_PATH: &str = "/api/v1/public/telemetry/forget";

const SCHEMA_VERSION: u32 = 1;
const APP_ID: &str = "willdeep-cli";

/// 队列上限；超了丢最旧的。遥测永远不该把磁盘吃满。
const MAX_QUEUED_EVENTS: usize = 500;
/// 单次请求最多带多少条（与服务端上限一致）。
const MAX_EVENTS_PER_REQUEST: usize = 100;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_TOKENS: u64 = 100_000_000;

/// 环境变量逃生舱：CI、容器镜像构建这类场景不该产生「用户」。
/// 置为 `1`/`true` 即整体关闭，优先级高于配置文件。
const DISABLE_ENV: &str = "WILLDEEP_TELEMETRY_DISABLED";

/// 事件名白名单。服务端有同名白名单，名单外的事件整条拒收。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventName {
    #[serde(rename = "app_session_started")]
    AppSessionStarted,
    #[serde(rename = "some_login_finished")]
    SomeLoginFinished,
    #[serde(rename = "model_selected")]
    ModelSelected,
    #[serde(rename = "ai_request_finished")]
    AiRequestFinished,
    #[serde(rename = "feature_used")]
    FeatureUsed,
    #[serde(rename = "app_error")]
    AppError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Succeeded,
    Failed,
    Cancelled,
}

/// 凭据来源——只记类型，永远不记密钥本身。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    SomeGateway,
    UserKey,
    Local,
    Unknown,
}

/// 一条待上报的事件。字段名与服务端 JSON 一一对应。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub event_name: EventName,
    pub schema_version: u32,
    pub occurred_at_ms: u64,
    pub install_id: String,
    pub app_id: String,
    pub app_version: String,
    pub os_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_source: Option<CredentialSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_count: Option<u64>,
}

impl Event {
    pub fn new(name: EventName, install_id: impl Into<String>) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            event_name: name,
            schema_version: SCHEMA_VERSION,
            occurred_at_ms: now_millis(),
            install_id: install_id.into(),
            app_id: APP_ID.to_owned(),
            app_version: willdeep_core::VERSION.to_owned(),
            os_name: os_name().to_owned(),
            os_version: None,
            arch: Some(arch().to_owned()),
            locale: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            credential_source: None,
            status: None,
            duration_ms: None,
            input_tokens: None,
            output_tokens: None,
            fallback_count: None,
            error_code: None,
            feature: None,
            surface: None,
            value_count: None,
        }
    }

    /// 把可疑字段就地清掉。入队前统一走一遍，保证队列里每条都符合协议。
    pub fn sanitized(mut self) -> Self {
        self.app_version = identifier(&self.app_version, 32).unwrap_or_else(|| "unknown".to_owned());
        self.os_version = self.os_version.as_deref().and_then(|v| identifier(v, 16));
        self.arch = self.arch.as_deref().and_then(|v| identifier(v, 16));
        self.locale = self.locale.as_deref().and_then(|v| identifier(v, 16));
        self.provider = self.provider.as_deref().and_then(|v| identifier(v, 48));
        self.requested_model = self.requested_model.as_deref().and_then(model_identifier);
        self.resolved_model = self.resolved_model.as_deref().and_then(model_identifier);
        self.error_code = self.error_code.as_deref().and_then(|v| identifier(v, 64));
        self.feature = self.feature.as_deref().and_then(|v| identifier(v, 64));
        self.surface = self.surface.as_deref().and_then(|v| identifier(v, 32));
        self.duration_ms = self.duration_ms.filter(|v| *v <= MAX_DURATION_MS);
        self.input_tokens = self.input_tokens.filter(|v| *v <= MAX_TOKENS);
        self.output_tokens = self.output_tokens.filter(|v| *v <= MAX_TOKENS);
        self.fallback_count = self.fallback_count.filter(|v| *v <= 255);
        self.value_count = self.value_count.filter(|v| *v <= 1_000_000);
        self
    }
}

#[derive(Serialize)]
struct UploadPayload<'a> {
    events: &'a [Event],
}

/// 校验短标识符；不合规返回 `None`（丢弃），不做「截断后凑合上报」。
///
/// 字符集与服务端 `^[A-Za-z0-9._:+_-]{1,96}$` 对齐：下划线在集合内
/// （`network_timeout`、`agent_chat` 这类维度名都用它），空格、中文、
/// 引号、换行都不在——一句自然语言过不了这一关。
pub fn identifier(raw: &str, max_length: usize) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > max_length {
        return None;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '+' | '-'))
    {
        return None;
    }
    Some(value.to_owned())
}

/// 模型名：允许 `/`（OpenRouter 风格的 vendor/model），但拒绝路径形状，
/// 免得文件路径伪装成模型名混进来。
pub fn model_identifier(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 96 {
        return None;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '+' | '-' | '/'))
    {
        return None;
    }
    if value.starts_with('/') || value.contains("//") || value.contains("..") {
        return None;
    }
    Some(value.to_owned())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => other,
    }
}

/// 判断这次请求用的是哪类凭据。只区分类型，永远不碰 key 本身。
pub fn credential_source(
    kind: willdeep_core::ProviderKind,
    base_url: &str,
    has_api_key: bool,
) -> CredentialSource {
    if matches!(kind, willdeep_core::ProviderKind::SomeIm) {
        return CredentialSource::SomeGateway;
    }
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".local") {
        return CredentialSource::Local;
    }
    if has_api_key {
        CredentialSource::UserKey
    } else {
        CredentialSource::Unknown
    }
}

/// Provider 的规范标识。**不上报用户在配置里起的 profile 名**——那是自由文本，
/// 里面什么都可能有；这里只出有限集里的值。
pub fn provider_label(kind: willdeep_core::ProviderKind) -> &'static str {
    match kind {
        willdeep_core::ProviderKind::SomeIm => "someim",
        willdeep_core::ProviderKind::Anthropic => "anthropic",
        willdeep_core::ProviderKind::OpenAiCompatible => "openai_compatible",
    }
}

/// 把一个 Agent 错误收敛成结构化错误码。刻意不带原始错误文案——
/// 那里面经常夹着路径、提示词片段甚至凭据。
pub fn error_code(error: &willdeep_core::AgentError) -> &'static str {
    use willdeep_core::AgentError;
    match error {
        AgentError::Provider(_) => "provider_error",
        AgentError::Tool(_) => "tool_error",
        AgentError::EmptyResponse => "empty_stream",
        AgentError::MaxTurns(_) => "max_turns",
        AgentError::TokenBudgetExceeded { .. } => "token_budget_exceeded",
        AgentError::Subagent(_) => "subagent_failed",
    }
}

/// `willdeep telemetry <action>`。
#[derive(Clone, Debug, clap::Subcommand)]
pub enum TelemetryAction {
    /// Show what is collected, whether it is on, and the anonymous identifier.
    Status,
    /// Ask the server to delete everything reported from this installation.
    Forget,
}

/// 处理 `willdeep telemetry`。删除入口刻意做成一等命令而不是藏在 config 里：
/// 承诺了「可以删」就得有人找得到的地方能删。
pub async fn handle(action: TelemetryAction, home: &Path, enabled: bool) -> anyhow::Result<()> {
    let telemetry = Telemetry::new(enabled, home);
    match action {
        TelemetryAction::Status => {
            println!("enabled:  {}", telemetry.is_enabled());
            println!(
                "install:  {}",
                if telemetry.install_id().is_empty() {
                    "-"
                } else {
                    telemetry.install_id()
                }
            );
            println!("endpoint: {TELEMETRY_ORIGIN}{EVENTS_PATH}");
            println!("queued:   {}", telemetry.load_queue().len());
            println!();
            println!("Collected: app version, OS major version, CPU architecture, language;");
            println!("           provider/model per request, success, duration, token counts,");
            println!("           structured error codes; feature usage counts.");
            println!("Never collected: prompts, model replies, file names or paths, file");
            println!("           contents, terminal commands or output, screenshots, clipboard,");
            println!("           API keys, email or nickname, raw error text, hardware IDs.");
            println!();
            println!("Turn it off with `enabled = false` under [telemetry] in config.toml,");
            println!("or by setting {DISABLE_ENV}=1.");
        }
        TelemetryAction::Forget => {
            // 关掉遥测时 install_id 不再加载，但删除仍然要能做——按 HOME 里
            // 存着的那个 ID 删，否则「先关开关再想删」就永远删不掉了。
            let identity = Telemetry::new(true, home);
            if identity.install_id().is_empty() {
                println!("No anonymous identifier on this machine; nothing to delete.");
                return Ok(());
            }
            if identity.forget().await {
                println!("Deletion requested for {}.", identity.install_id());
            } else {
                println!(
                    "Could not reach the server. Local queue cleared; retry later to delete what was already reported."
                );
            }
        }
    }
    Ok(())
}

/// 进程级句柄。埋点散落在 harness / TUI 深处，一路把句柄传下去只会污染
/// 一堆与遥测无关的函数签名；没装过就是 `disabled()`，所有方法空操作。
static GLOBAL: OnceLock<Telemetry> = OnceLock::new();

/// 在 `run()` 拿到配置后装一次。重复调用只有第一次生效。
pub fn install(telemetry: Telemetry) {
    let _ = GLOBAL.set(telemetry);
}

pub fn global() -> &'static Telemetry {
    GLOBAL.get_or_init(Telemetry::disabled)
}

/// 尽力把队列发出去，超时就算了——遥测不该拖慢 CLI 退出。
pub async fn flush_before_exit() {
    let telemetry = global();
    if !telemetry.is_enabled() {
        return;
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), telemetry.flush()).await;
}

/// 遥测句柄。`enabled == false` 时所有方法都是空操作。
#[derive(Clone, Debug)]
pub struct Telemetry {
    enabled: bool,
    install_id: String,
    queue_path: PathBuf,
    endpoint: String,
    forget_endpoint: String,
}

impl Telemetry {
    /// 从配置与 `WILLDEEP_HOME` 建立句柄。
    ///
    /// `enabled` 来自配置文件，但 `WILLDEEP_TELEMETRY_DISABLED` 优先级更高：
    /// CI 和容器构建里跑 CLI 不该被算成用户。
    pub fn new(enabled: bool, home: &Path) -> Self {
        let enabled = enabled && !disabled_by_env();
        let install_id = if enabled {
            load_or_create_install_id(home)
        } else {
            String::new()
        };
        Self {
            enabled: enabled && !install_id.is_empty(),
            install_id,
            queue_path: home.join("telemetry-queue.json"),
            endpoint: format!("{TELEMETRY_ORIGIN}{EVENTS_PATH}"),
            forget_endpoint: format!("{TELEMETRY_ORIGIN}{FORGET_PATH}"),
        }
    }

    /// 全程关闭的句柄，给测试和 `--no-telemetry` 用。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            install_id: String::new(),
            queue_path: PathBuf::new(),
            endpoint: String::new(),
            forget_endpoint: String::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn install_id(&self) -> &str {
        &self.install_id
    }

    pub fn event(&self, name: EventName) -> Event {
        Event::new(name, &self.install_id)
    }

    /// 记录一个事件：清洗后写入本地队列文件。不发网络请求。
    pub fn record(&self, event: Event) {
        if !self.enabled {
            return;
        }
        let mut queue = self.load_queue();
        queue.push(event.sanitized());
        if queue.len() > MAX_QUEUED_EVENTS {
            let overflow = queue.len() - MAX_QUEUED_EVENTS;
            queue.drain(0..overflow);
        }
        self.save_queue(&queue);
    }

    /// 把队列发出去。失败保留队列——`event_id` 是服务端唯一键，重发不会
    /// 产生重复行。整个过程不打断 CLI：任何错误都只是「下次再发」。
    pub async fn flush(&self) {
        if !self.enabled {
            return;
        }
        let queue = self.load_queue();
        if queue.is_empty() {
            return;
        }
        let batch: Vec<Event> = queue.iter().take(MAX_EVENTS_PER_REQUEST).cloned().collect();
        if self.send(&batch).await {
            let remaining: Vec<Event> = queue.into_iter().skip(batch.len()).collect();
            self.save_queue(&remaining);
        }
    }

    /// 请求服务端删除这个安装已上报的全部数据，并清掉本地队列。
    pub async fn forget(&self) -> bool {
        if self.install_id.is_empty() {
            return false;
        }
        let _ = std::fs::remove_file(&self.queue_path);
        let Ok(client) = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() else {
            return false;
        };
        let body = serde_json::json!({ "install_id": self.install_id });
        match client.post(&self.forget_endpoint).json(&body).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }

    async fn send(&self, events: &[Event]) -> bool {
        let Ok(client) = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() else {
            return false;
        };
        let payload = UploadPayload { events };
        let response = client
            .post(&self.endpoint)
            .header(
                "User-Agent",
                format!("some.im/willdeep-cli-{} ({})", willdeep_core::VERSION, os_name()),
            )
            .json(&payload)
            .send()
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                // 4xx（除 429）是协议问题，重发也不会变好；当作已处理丢掉这批，
                // 免得一条坏事件把队列永久卡死。
                status.is_success()
                    || (status.is_client_error() && status.as_u16() != 429)
            }
            Err(_) => false,
        }
    }

    fn load_queue(&self) -> Vec<Event> {
        let Ok(contents) = std::fs::read_to_string(&self.queue_path) else {
            return Vec::new();
        };
        match serde_json::from_str::<Vec<Event>>(&contents) {
            Ok(events) => events,
            Err(_) => {
                // 队列文件坏了就丢掉重来：遥测数据没有留着修的价值。
                let _ = std::fs::remove_file(&self.queue_path);
                Vec::new()
            }
        }
    }

    fn save_queue(&self, events: &[Event]) {
        if events.is_empty() {
            let _ = std::fs::remove_file(&self.queue_path);
            return;
        }
        if let Some(parent) = self.queue_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = serde_json::to_string(events) {
            let _ = std::fs::write(&self.queue_path, serialized);
        }
    }
}

fn disabled_by_env() -> bool {
    match std::env::var(DISABLE_ENV) {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

/// 读取或生成匿名安装 ID。存在 `WILLDEEP_HOME/telemetry-install-id`，
/// 是随机 UUID，不从任何硬件标识推导；删掉这个文件等价于重置。
fn load_or_create_install_id(home: &Path) -> String {
    let path = home.join("telemetry-install-id");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let trimmed = contents.trim();
        if Uuid::parse_str(trimmed).is_ok() {
            return trimmed.to_ascii_lowercase();
        }
    }
    let fresh = Uuid::new_v4().to_string();
    if std::fs::create_dir_all(home).is_ok() && std::fs::write(&path, &fresh).is_ok() {
        return fresh;
    }
    // 写不进去（只读 HOME、容器里没挂卷）就不上报：一个每次都变的 ID
    // 会把一台机器算成无数个新装机，比没有数据更糟。
    String::new()
}

/// 一次 Agent 回合的遥测累计值。
pub struct TurnTelemetry {
    started: std::time::Instant,
    provider: &'static str,
    requested_model: String,
    credential_source: CredentialSource,
}

impl TurnTelemetry {
    pub fn start(
        kind: willdeep_core::ProviderKind,
        base_url: &str,
        model: &str,
        has_api_key: bool,
    ) -> Self {
        Self {
            started: std::time::Instant::now(),
            provider: provider_label(kind),
            requested_model: model.to_owned(),
            credential_source: credential_source(kind, base_url, has_api_key),
        }
    }

    /// 回合结束：按结果生成事件并写入队列。
    pub fn finish(
        self,
        telemetry: &Telemetry,
        outcome: Result<(u64, u64), &willdeep_core::AgentError>,
    ) {
        if !telemetry.is_enabled() {
            return;
        }
        let mut event = telemetry.event(EventName::AiRequestFinished);
        event.provider = Some(self.provider.to_owned());
        event.requested_model = Some(self.requested_model.clone());
        event.resolved_model = Some(self.requested_model);
        event.credential_source = Some(self.credential_source);
        event.duration_ms = Some(self.started.elapsed().as_millis() as u64);
        match outcome {
            Ok((input_tokens, output_tokens)) => {
                event.status = Some(Status::Succeeded);
                event.input_tokens = (input_tokens > 0).then_some(input_tokens);
                event.output_tokens = (output_tokens > 0).then_some(output_tokens);
            }
            Err(error) => {
                event.status = Some(Status::Failed);
                event.error_code = Some(error_code(error).to_owned());
            }
        }
        telemetry.record(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let path = std::env::temp_dir().join(format!("willdeep-telemetry-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// 红线用例：自由文本（提示词、路径、命令、原始错误）进不了事件字段。
    #[test]
    fn free_text_is_dropped() {
        let mut event = Event::new(EventName::AiRequestFinished, Uuid::new_v4().to_string());
        event.provider = Some("帮我把这个函数重构一下".to_owned());
        event.requested_model = Some("/Users/rocky/Sites/Xedit/AppState.swift".to_owned());
        event.resolved_model = Some("rm -rf /tmp/build && make".to_owned());
        event.error_code = Some("dial tcp 1.2.3.4:443: i/o timeout".to_owned());
        event.feature = Some("用户在写一封邮件".to_owned());

        let clean = event.sanitized();
        assert!(clean.provider.is_none());
        assert!(clean.requested_model.is_none());
        assert!(clean.resolved_model.is_none());
        assert!(clean.error_code.is_none());
        assert!(clean.feature.is_none());
    }

    #[test]
    fn well_formed_values_survive() {
        let mut event = Event::new(EventName::AiRequestFinished, Uuid::new_v4().to_string());
        event.provider = Some("anthropic".to_owned());
        event.requested_model = Some("claude-opus-5".to_owned());
        event.resolved_model = Some("deepseek/deepseek-v3".to_owned());
        event.error_code = Some("http_429".to_owned());
        event.feature = Some("agent_chat".to_owned());

        let clean = event.sanitized();
        assert_eq!(clean.provider.as_deref(), Some("anthropic"));
        assert_eq!(clean.requested_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(clean.resolved_model.as_deref(), Some("deepseek/deepseek-v3"));
        assert_eq!(clean.error_code.as_deref(), Some("http_429"));
        assert_eq!(clean.feature.as_deref(), Some("agent_chat"));
    }

    /// 事件维度名大量用下划线；字符集漏掉 `_` 会让这些值被静默丢弃。
    #[test]
    fn underscore_is_allowed() {
        assert_eq!(
            identifier("network_timeout", 64).as_deref(),
            Some("network_timeout")
        );
        assert_eq!(identifier("agent chat", 64), None);
        assert_eq!(identifier("帮我写代码", 64), None);
    }

    #[test]
    fn model_identifier_rejects_paths() {
        assert_eq!(
            model_identifier("deepseek/deepseek-v3").as_deref(),
            Some("deepseek/deepseek-v3")
        );
        assert_eq!(model_identifier("/Users/rocky/a.txt"), None);
        assert_eq!(model_identifier("a//b"), None);
        assert_eq!(model_identifier("../etc/passwd"), None);
    }

    /// 编码出来的 JSON key 必须与服务端协议逐字对齐，且不存在能装下
    /// 提示词、路径、命令的字段。
    #[test]
    fn encoded_payload_matches_server_contract() {
        let mut event = Event::new(EventName::AiRequestFinished, Uuid::new_v4().to_string());
        event.status = Some(Status::Succeeded);
        event.credential_source = Some(CredentialSource::SomeGateway);
        event.duration_ms = Some(4210);

        let value = serde_json::to_value(&event).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object["event_name"], "ai_request_finished");
        assert_eq!(object["app_id"], "willdeep-cli");
        assert_eq!(object["os_name"], os_name());
        assert_eq!(object["status"], "succeeded");
        assert_eq!(object["credential_source"], "some_gateway");
        assert_eq!(object["schema_version"], 1);
        assert!(!object.contains_key("prompt"));
        assert!(!object.contains_key("content"));
        assert!(!object.contains_key("path"));
    }

    #[test]
    fn disabled_telemetry_records_nothing() {
        let home = temp_home();
        let telemetry = Telemetry::new(false, &home);
        telemetry.record(Event::new(EventName::AppSessionStarted, "x"));
        assert!(!home.join("telemetry-queue.json").exists());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn enabled_telemetry_queues_and_bounds() {
        let home = temp_home();
        let telemetry = Telemetry::new(true, &home);
        assert!(telemetry.is_enabled());
        for _ in 0..(MAX_QUEUED_EVENTS + 10) {
            telemetry.record(telemetry.event(EventName::FeatureUsed));
        }
        let queued = telemetry.load_queue();
        assert_eq!(queued.len(), MAX_QUEUED_EVENTS);
        std::fs::remove_dir_all(&home).ok();
    }

    /// 安装 ID 必须稳定：每次跑都换一个会把一台机器算成无数个新装机。
    #[test]
    fn install_id_is_stable() {
        let home = temp_home();
        let first = Telemetry::new(true, &home).install_id().to_owned();
        let second = Telemetry::new(true, &home).install_id().to_owned();
        assert_eq!(first, second);
        assert!(Uuid::parse_str(&first).is_ok());
        std::fs::remove_dir_all(&home).ok();
    }

    /// 入队前先清洗：落盘文件里也不该出现那句中文。
    #[test]
    fn record_sanitizes_before_queueing() {
        let home = temp_home();
        let telemetry = Telemetry::new(true, &home);
        let mut event = telemetry.event(EventName::FeatureUsed);
        event.feature = Some("用户写了一封邮件".to_owned());
        telemetry.record(event);

        let raw = std::fs::read_to_string(home.join("telemetry-queue.json")).unwrap();
        assert!(!raw.contains("用户写了一封邮件"));
        let queued = telemetry.load_queue();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].feature.is_none());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn credential_source_classification() {
        use willdeep_core::ProviderKind;
        assert_eq!(
            credential_source(ProviderKind::SomeIm, "https://some.im/v1", true),
            CredentialSource::SomeGateway
        );
        assert_eq!(
            credential_source(
                ProviderKind::OpenAiCompatible,
                "http://127.0.0.1:11434/v1",
                false
            ),
            CredentialSource::Local
        );
        assert_eq!(
            credential_source(ProviderKind::Anthropic, "https://api.anthropic.com", true),
            CredentialSource::UserKey
        );
        assert_eq!(
            credential_source(ProviderKind::Anthropic, "https://api.anthropic.com", false),
            CredentialSource::Unknown
        );
    }

    /// 上报的 provider 只出有限集里的值，不是用户在配置里起的 profile 名。
    #[test]
    fn provider_label_is_canonical() {
        use willdeep_core::ProviderKind;
        assert_eq!(provider_label(ProviderKind::SomeIm), "someim");
        assert_eq!(provider_label(ProviderKind::Anthropic), "anthropic");
        assert_eq!(
            provider_label(ProviderKind::OpenAiCompatible),
            "openai_compatible"
        );
    }
}
