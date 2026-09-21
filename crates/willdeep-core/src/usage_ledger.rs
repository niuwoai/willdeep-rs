//! 本机用量账本 `willdeep.usage-ledger.v1`：**每次模型调用一行**，只追加。
//!
//! canonical 规范在 Xedit 仓库 `docs/USAGE_LEDGER_DESIGN.md`（§5 数据模型、§6.1
//! 实时写入、§7 不变量），本仓库的落地说明见 `docs/USAGE_LEDGER.md`。
//!
//! - 位置：`$WILLDEEP_HOME/usage/YYYY-MM.jsonl`，按记录的 UTC 月份分片。
//! - 每行一个 JSON 对象、`\n` 结尾、整行小于 4096 字节，用 `O_APPEND` 一次
//!   `write` 写完——daemon 与多个进程内 CLI 并发写同一文件不会交错。
//! - **可选字段一律输出、未知即 `null`，从不省略键。** 读端（Mac）两种都认，
//!   这里选定一种，便于按固定键集断言。
//! - 只记数，不记内容：记录类型里就没有提示词、回复、推理、工具参数这类字段。
//! - 记账是旁路：任何 IO 错误只打一条警告，从不上抛，不让回合失败或变慢。

mod scope;
mod sink;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::types::Usage;

pub use scope::{LedgeredProvider, ModelCall, UsageLedgerContext, UsageLedgerScope};
pub use sink::{EXIT_FLUSH_TIMEOUT, UsageLedgerSink, append_record, flush_all, shared_sink};

/// 行格式版本。读端遇到不认识的 schema 整行跳过。
pub const SCHEMA: &str = "willdeep.usage-ledger.v1";

/// 一行（含结尾换行）的硬上限，保证小于 `PIPE_BUF`，`O_APPEND` 单次写入原子。
pub const MAX_LINE_BYTES: usize = 4096;

/// 账本目录相对 `$WILLDEEP_HOME` 的名字。
pub const LEDGER_DIR: &str = "usage";

/// 回填 id 的 UUIDv5 命名空间。固定常量：同一条事件在任何机器、任何时候回填
/// 都得到同一个 id，这是回填可重放的前提。**永远不要改。**
pub const BACKFILL_NAMESPACE: Uuid = Uuid::from_u128(0x5d1c_7a42_3f0e_4b8e_9c61_2a7e_0b4d_91f3);

/// `$WILLDEEP_HOME/usage`。
pub fn ledger_dir(home: &Path) -> PathBuf {
    home.join(LEDGER_DIR)
}

/// 回填记录的确定性 id：`uuidv5(BACKFILL_NAMESPACE, "events.ndjson#<sequence>")`。
pub fn backfill_id(sequence: u64) -> Uuid {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(BACKFILL_NAMESPACE.as_bytes());
    hasher.update(format!("events.ndjson#{sequence}").as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}

/// 发起这次调用的前端，取自 Runtime 任务的 `origin_client` 前缀。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Cli,
    Tui,
    Mobile,
    Unknown,
}

impl ClientKind {
    /// `tui:<uuid>` → (`Tui`, `Some(uuid)`)；空值与不认识的前缀记 `Unknown`。
    pub fn parse_origin(origin: Option<&str>) -> (Self, Option<String>) {
        let Some(origin) = origin.map(str::trim).filter(|value| !value.is_empty()) else {
            return (Self::Unknown, None);
        };
        let (prefix, instance) = match origin.split_once(':') {
            Some((prefix, instance)) => (prefix, Some(instance.trim())),
            None => (origin, None),
        };
        let kind = match prefix.to_ascii_lowercase().as_str() {
            "cli" => Self::Cli,
            "tui" => Self::Tui,
            "mobile" => Self::Mobile,
            _ => Self::Unknown,
        };
        let instance = instance
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        (kind, instance)
    }
}

/// 回合在哪里执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    Daemon,
    InProcess,
}

/// 这次模型调用在做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// 主 Agent 的回合请求。
    Main,
    /// 子 Agent（Worker）的回合请求。
    Subagent,
    /// 上下文压缩摘要。
    Compression,
    /// 标题、下一句预测、路由分类、安全判官、看图兜底等辅助请求。
    Auxiliary,
}

/// 请求结局。失败但已计费（带 usage）的请求同样记一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
}

/// 账本的一行。字段与 JSON 键逐一对应 canonical 规范 §5.2。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageLedgerRecord {
    pub schema: String,
    pub id: Uuid,
    /// 调用**完成**时刻，Unix 毫秒；序列化为 RFC 3339 UTC 毫秒精度（`…T08:52:31.402Z`）。
    #[serde(serialize_with = "serialize_ts", deserialize_with = "deserialize_ts")]
    pub ts: u64,
    pub client: ClientKind,
    pub client_instance: Option<String>,
    pub execution: Execution,
    pub kind: UsageKind,
    pub session_id: Option<Uuid>,
    pub turn_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub local: bool,
    /// `input_tokens`（含缓存命中）/ `cache_read_tokens` / `output_tokens` /
    /// `total_tokens`。协议没报就是 `null`，不估算。
    #[serde(flatten)]
    pub usage: Usage,
    pub latency_ms: Option<u64>,
    pub outcome: Outcome,
    pub event_sequence: Option<u64>,
    pub backfilled: bool,
}

impl UsageLedgerRecord {
    /// 一条空白记录：新 id、当前时刻、其余未知。调用方再按需填字段。
    pub fn new(kind: UsageKind, execution: Execution) -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            id: Uuid::new_v4(),
            ts: now_millis(),
            client: ClientKind::Unknown,
            client_instance: None,
            execution,
            kind,
            session_id: None,
            turn_id: None,
            task_id: None,
            agent_id: None,
            workspace: None,
            provider: None,
            model: None,
            local: false,
            usage: Usage::default(),
            latency_ms: None,
            outcome: Outcome::Ok,
            event_sequence: None,
            backfilled: false,
        }
    }

    /// 记录所属的月份分片文件名（按 UTC）：`2026-09.jsonl`。
    pub fn month_file_name(&self) -> String {
        format!("{}.jsonl", &format_ts(self.ts)[..7])
    }

    /// 序列化成一整行（含结尾 `\n`），保证小于 [`MAX_LINE_BYTES`]。
    ///
    /// 超长（理论上不会出现，除非工作区路径或模型名离谱地长）时按「越不要紧
    /// 越先丢」的顺序把可选的描述字段置空再试；计数字段永远保留。
    pub fn to_line(&self) -> Option<Vec<u8>> {
        let mut record = self.clone();
        for step in 0..=4 {
            match step {
                0 => {}
                1 => record.workspace = None,
                2 => {
                    record.client_instance = None;
                    record.turn_id = None;
                    record.agent_id = None;
                }
                3 => record.task_id = None,
                _ => {
                    record.provider = None;
                    record.model = None;
                }
            }
            let mut line = serde_json::to_vec(&record).ok()?;
            line.push(b'\n');
            if line.len() < MAX_LINE_BYTES {
                return Some(line);
            }
        }
        None
    }
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// Unix 毫秒 → `YYYY-MM-DDTHH:MM:SS.mmmZ`。
pub fn format_ts(millis: u64) -> String {
    let seconds = crate::session::format_iso8601(millis / 1_000);
    // format_iso8601 以 `Z` 结尾，把毫秒插在它前面。
    format!("{}.{:03}Z", seconds.trim_end_matches('Z'), millis % 1_000)
}

/// `format_ts` 的反向；也接受不带毫秒的 `…:SSZ`。
pub fn parse_ts(text: &str) -> Option<u64> {
    let text = text.trim();
    let body = text.strip_suffix('Z')?;
    let (seconds, fraction) = match body.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (body, None),
    };
    let base = crate::session::parse_iso8601_utc(&format!("{seconds}Z"))?;
    let millis = match fraction {
        None => 0,
        Some(fraction)
            if !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let padded = format!("{fraction:0<3}");
            padded[..3].parse::<u64>().ok()?
        }
        Some(_) => return None,
    };
    Some(base * 1_000 + millis)
}

fn serialize_ts<S: Serializer>(millis: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format_ts(*millis))
}

fn deserialize_ts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_ts(&text).ok_or_else(|| serde::de::Error::custom(format!("invalid ledger ts {text:?}")))
}
