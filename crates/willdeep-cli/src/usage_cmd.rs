//! `willdeep usage backfill`：把账本上线前 daemon 回合的用量从
//! `runtime/events.ndjson` 补进 `usage/YYYY-MM.jsonl`。
//!
//! 规范见 Xedit `docs/USAGE_LEDGER_DESIGN.md` §6.2，本仓库说明见
//! `docs/USAGE_LEDGER.md`。三道去重，保证反复跑结果不变：
//!
//! 1. 回填 id 由事件序号确定：`uuidv5(ns, "events.ndjson#<sequence>")`，
//!    账本里已有这个 id 就跳过；
//! 2. 实时记账的行带 `event_sequence`，这个序号已经出现在账本里的事件跳过；
//! 3. 所属任务还没结束（排队、执行中、等人）的事件跳过——那一轮正由运行中
//!    的 daemon 实时记账，它的账本行可能还在写线程的队列里没落盘。
//!
//! 事件日志十几 MB，一行一行流式读，不整份载入。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Deserialize;
use willdeep_core::types::Usage;
use willdeep_core::usage_ledger::{
    ClientKind, Execution, Outcome, UsageKind, UsageLedgerRecord, append_record, backfill_id,
    ledger_dir,
};

/// daemon 启动时自动回填的完成标记，记着回填到的最大事件序号。
pub(crate) const BACKFILL_MARKER: &str = ".backfill-v1.done";

#[derive(Clone, Debug, Subcommand)]
pub enum UsageAction {
    /// Backfill the local usage ledger from the Runtime event log.
    ///
    /// Reads `runtime/events.ndjson` and records every daemon model call that
    /// is not in the ledger yet. Safe to repeat: already recorded calls are
    /// skipped, so a second run changes nothing.
    Backfill {
        /// Print per-day call counts and token totals without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub(crate) fn run(action: UsageAction, home: &Path) -> Result<()> {
    match action {
        UsageAction::Backfill { dry_run } => {
            let plan = plan_backfill(home)?;
            print!("{}", render_report(&plan, dry_run));
            if !dry_run {
                write_backfill(home, &plan)?;
            }
            Ok(())
        }
    }
}

/// daemon 启动时跑一次。有标记就什么都不做；失败只警告，不挡启动。
///
/// 放在 daemon 开始接任务之前同步执行：这时没有任何回合在跑，事件日志里的
/// 每一条用量都来自上一个 Runtime，不会与实时记账抢同一个序号。
pub(crate) fn backfill_on_daemon_start(home: &Path) {
    if ledger_dir(home).join(BACKFILL_MARKER).exists() {
        return;
    }
    let result = plan_backfill(home).and_then(|plan| write_backfill(home, &plan));
    if let Err(error) = result {
        eprintln!("willdeep: usage ledger backfill skipped: {error:#}");
    }
}

#[derive(Debug, Default)]
pub(crate) struct BackfillPlan {
    pub records: Vec<UsageLedgerRecord>,
    /// 事件日志里的用量事件总数（含跳过的）。
    pub usage_events: usize,
    pub already_recorded: usize,
    pub recorded_live: usize,
    pub task_active: usize,
    pub max_sequence: u64,
}

#[derive(Deserialize)]
struct RuntimeEventLine {
    sequence: u64,
    timestamp: u64,
    kind: String,
    message: String,
}

#[derive(Deserialize)]
struct UsageEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct TaskRow {
    id: String,
    #[serde(default)]
    session_id: Option<uuid::Uuid>,
    #[serde(default)]
    turn_id: Option<uuid::Uuid>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    workspace: Option<PathBuf>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    origin_client: Option<String>,
}

#[derive(Deserialize)]
struct ModelRow {
    id: String,
    #[serde(default)]
    model: Option<String>,
}

/// 读 JSON 数组形式的 Runtime 状态文件；缺失就当空。
fn read_rows<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// 账本里已经有的 id，与实时行登记过的事件序号。坏行、不认识的行跳过。
fn existing_ledger(dir: &Path) -> Result<(HashSet<uuid::Uuid>, HashSet<u64>)> {
    let mut ids = HashSet::new();
    let mut live_sequences = HashSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((ids, live_sequences));
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let file = std::fs::File::open(&path)
            .with_context(|| format!("open ledger file {}", path.display()))?;
        for line in std::io::BufReader::new(file).split(b'\n') {
            let line = line?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if value["schema"] != willdeep_core::usage_ledger::SCHEMA {
                continue;
            }
            if let Some(id) = value["id"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
            {
                ids.insert(id);
            }
            if value["backfilled"] == false
                && let Some(sequence) = value["event_sequence"].as_u64()
            {
                live_sequences.insert(sequence);
            }
        }
    }
    Ok((ids, live_sequences))
}

fn task_is_finished(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "partial" | "failed" | "cancelled" | "interrupted")
    )
}

pub(crate) fn plan_backfill(home: &Path) -> Result<BackfillPlan> {
    let runtime = home.join("runtime");
    let tasks: HashMap<String, TaskRow> = read_rows::<TaskRow>(&runtime.join("tasks.json"))?
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    // 任务没记模型（沿用档案默认）时退到 Runtime 会话上的模型；子 Agent 的
    // 模型记在 agents.json。都没有就留空，不拿当前配置去猜历史。
    let model_of = |path: &Path| -> Result<HashMap<String, String>> {
        Ok(read_rows::<ModelRow>(path)?
            .into_iter()
            .filter_map(|row| row.model.map(|model| (row.id, model)))
            .collect())
    };
    let session_models = model_of(&runtime.join("sessions.json"))?;
    let agent_models = model_of(&runtime.join("agents.json"))?;
    let (existing_ids, live_sequences) = existing_ledger(&ledger_dir(home))?;

    let mut plan = BackfillPlan::default();
    let events_path = runtime.join("events.ndjson");
    let file = match std::fs::File::open(&events_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(plan),
        Err(error) => {
            return Err(error).with_context(|| format!("open {}", events_path.display()));
        }
    };
    for line in std::io::BufReader::new(file).split(b'\n') {
        let line = line.with_context(|| format!("read {}", events_path.display()))?;
        // 九成以上的事件与用量无关，先按子串筛一遍再解析。
        if !contains(&line, b"usage") {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<RuntimeEventLine>(&line) else {
            continue;
        };
        if event.kind != "task.output" {
            continue;
        }
        let Some((task_id, payload)) = event
            .message
            .strip_prefix("task_id=")
            .and_then(|rest| rest.split_once(' '))
        else {
            continue;
        };
        let Ok(usage) = serde_json::from_str::<UsageEvent>(payload) else {
            continue;
        };
        let kind = match usage.kind.as_str() {
            "usage" => UsageKind::Main,
            "subagent_usage" => UsageKind::Subagent,
            _ => continue,
        };
        plan.usage_events += 1;
        let id = backfill_id(event.sequence);
        if existing_ids.contains(&id) {
            plan.already_recorded += 1;
            continue;
        }
        if live_sequences.contains(&event.sequence) {
            plan.recorded_live += 1;
            continue;
        }
        let task = tasks.get(task_id);
        if task.is_some_and(|task| !task_is_finished(task.status.as_deref())) {
            plan.task_active += 1;
            continue;
        }
        let task = task.cloned().unwrap_or_default();
        let mut record = UsageLedgerRecord::new(kind, Execution::Daemon);
        record.id = id;
        record.ts = event.timestamp.saturating_mul(1_000);
        let (client, instance) = ClientKind::parse_origin(task.origin_client.as_deref());
        record.client = client;
        record.client_instance = instance;
        record.session_id = task.session_id;
        record.turn_id = task.turn_id.map(|id| id.to_string());
        record.task_id = Some(task_id.to_owned());
        record.workspace = task.workspace;
        record.agent_id = match kind {
            UsageKind::Subagent => usage.id.clone(),
            _ => task.agent_id,
        };
        record.model = match kind {
            UsageKind::Subagent => usage
                .id
                .as_ref()
                .and_then(|id| agent_models.get(id).cloned()),
            _ => task.model.or_else(|| {
                task.session_id
                    .and_then(|id| session_models.get(&id.to_string()).cloned())
            }),
        };
        record.usage = Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cache_read_tokens: usage.cache_read_tokens,
        };
        record.outcome = Outcome::Ok;
        record.event_sequence = Some(event.sequence);
        record.backfilled = true;
        plan.max_sequence = plan.max_sequence.max(event.sequence);
        plan.records.push(record);
    }
    Ok(plan)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

pub(crate) fn write_backfill(home: &Path, plan: &BackfillPlan) -> Result<()> {
    let dir = ledger_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for record in &plan.records {
        append_record(&dir, record).map_err(anyhow::Error::msg)?;
    }
    let previous = std::fs::read_to_string(dir.join(BACKFILL_MARKER))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value["max_sequence"].as_u64())
        .unwrap_or(0);
    let marker = serde_json::json!({
        "schema": "willdeep.usage-backfill.v1",
        "max_sequence": plan.max_sequence.max(previous),
    });
    std::fs::write(dir.join(BACKFILL_MARKER), format!("{marker}\n"))
        .with_context(|| format!("write {}", dir.join(BACKFILL_MARKER).display()))
}

#[derive(Default)]
struct DayTotals {
    calls: u64,
    subagent_calls: u64,
    input: u64,
    cached: u64,
    output: u64,
}

pub(crate) fn render_report(plan: &BackfillPlan, dry_run: bool) -> String {
    let mut days: BTreeMap<String, DayTotals> = BTreeMap::new();
    for record in &plan.records {
        let totals = days.entry(local_day(record.ts / 1_000)).or_default();
        totals.calls += 1;
        if record.kind == UsageKind::Subagent {
            totals.subagent_calls += 1;
        }
        totals.input += record.usage.input_tokens.unwrap_or(0);
        totals.cached += record.usage.cache_read_tokens.unwrap_or(0);
        totals.output += record.usage.output_tokens.unwrap_or(0);
    }
    let mut out = String::new();
    out.push_str("day (local)\tcalls\tsubagent\tinput\tcached\toutput\n");
    let mut total = DayTotals::default();
    for (day, totals) in &days {
        out.push_str(&format!(
            "{day}\t{}\t{}\t{}\t{}\t{}\n",
            totals.calls, totals.subagent_calls, totals.input, totals.cached, totals.output
        ));
        total.calls += totals.calls;
        total.subagent_calls += totals.subagent_calls;
        total.input += totals.input;
        total.cached += totals.cached;
        total.output += totals.output;
    }
    out.push_str(&format!(
        "total\t{}\t{}\t{}\t{}\t{}\n",
        total.calls, total.subagent_calls, total.input, total.cached, total.output
    ));
    out.push_str(&format!(
        "{} {} of {} usage events; skipped {} already in the ledger, {} recorded live, {} from unfinished tasks\n",
        if dry_run { "would backfill" } else { "backfilled" },
        plan.records.len(),
        plan.usage_events,
        plan.already_recorded,
        plan.recorded_live,
        plan.task_active,
    ));
    out
}

/// Unix 秒 → 本机时区的 `YYYY-MM-DD`。取不到本地偏移（非 Unix）时按 UTC。
fn local_day(seconds: u64) -> String {
    let shifted = seconds as i64 + local_offset_seconds(seconds);
    willdeep_core::format_iso8601(shifted.max(0) as u64)[..10].to_owned()
}

#[cfg(unix)]
// `tm_gmtoff` 是 `c_long`：64 位平台上已是 i64，32 位上不是。
#[allow(clippy::unnecessary_cast)]
fn local_offset_seconds(seconds: u64) -> i64 {
    let time = seconds as libc::time_t;
    // SAFETY: localtime_r only writes the caller-owned `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&time, &mut tm) };
    if converted.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn local_offset_seconds(_seconds: u64) -> i64 {
    0
}

#[cfg(test)]
mod tests;
