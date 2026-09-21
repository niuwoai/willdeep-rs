use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::sink::append_record;
use super::*;
use crate::agent::{Agent, AgentConfig, AgentEvent, EventSink};
use crate::provider::{Provider, ProviderError, ProviderIdentity};
use crate::tools::{ApprovalMode, ToolRegistry};
use crate::types::{Completion, Message, ToolCall, ToolDefinition};

/// 规范 §5.2 的完整键集。多一个少一个都算破坏读端契约。
const SPEC_KEYS: &[&str] = &[
    "schema",
    "id",
    "ts",
    "client",
    "client_instance",
    "execution",
    "kind",
    "session_id",
    "turn_id",
    "task_id",
    "agent_id",
    "workspace",
    "provider",
    "model",
    "local",
    "input_tokens",
    "cache_read_tokens",
    "output_tokens",
    "total_tokens",
    "latency_ms",
    "outcome",
    "event_sequence",
    "backfilled",
];

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("willdeep-usage-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn read_lines(dir: &Path) -> Vec<serde_json::Value> {
    let mut files = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    files
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn full_record() -> UsageLedgerRecord {
    let mut record = UsageLedgerRecord::new(UsageKind::Main, Execution::Daemon);
    record.ts = 1_789_980_751_402;
    record.client = ClientKind::Tui;
    record.client_instance = Some("62a6264a-2604-4429-ae5f-b8d4a01128f4".into());
    record.session_id = Some(Uuid::new_v4());
    record.turn_id = Some(Uuid::new_v4().to_string());
    record.task_id = Some(Uuid::new_v4().to_string());
    record.workspace = Some(PathBuf::from("/Users/rocky/Sites/factify"));
    record.provider = Some("some.im".into());
    record.model = Some("deepseek-v4-pro".into());
    record.usage = Usage {
        input_tokens: Some(21_342),
        output_tokens: Some(1_036),
        total_tokens: Some(22_378),
        cache_read_tokens: Some(20_864),
    };
    record.latency_ms = Some(5_820);
    record.event_sequence = Some(80_756);
    record
}

#[test]
fn serialized_keys_are_exactly_the_spec_set_and_carry_no_content() {
    for record in [
        full_record(),
        UsageLedgerRecord::new(UsageKind::Auxiliary, Execution::InProcess),
    ] {
        let line = record.to_line().unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(keys, SPEC_KEYS.iter().copied().collect::<BTreeSet<_>>());
        for forbidden in [
            "content",
            "prompt",
            "messages",
            "arguments",
            "reasoning",
            "tool_calls",
            "text",
        ] {
            assert!(
                !keys.contains(forbidden),
                "{forbidden} must never be recorded"
            );
        }
        assert_eq!(value["schema"], SCHEMA);
        // 可选字段未知时输出 null，而不是省略。
        assert!(value.get("event_sequence").is_some());
        let parsed: UsageLedgerRecord = serde_json::from_slice(&line).unwrap();
        assert_eq!(parsed, record);
    }
}

#[test]
fn enums_and_timestamp_use_the_wire_spelling() {
    let value = serde_json::to_value(full_record()).unwrap();
    assert_eq!(value["ts"], "2026-09-21T08:52:31.402Z");
    assert_eq!(value["client"], "tui");
    assert_eq!(value["execution"], "daemon");
    assert_eq!(value["kind"], "main");
    assert_eq!(value["outcome"], "ok");
    let mut record = full_record();
    record.execution = Execution::InProcess;
    record.kind = UsageKind::Compression;
    record.outcome = Outcome::Cancelled;
    record.client = ClientKind::Unknown;
    let value = serde_json::to_value(record).unwrap();
    assert_eq!(value["execution"], "in_process");
    assert_eq!(value["kind"], "compression");
    assert_eq!(value["outcome"], "cancelled");
    assert_eq!(value["client"], "unknown");
    assert_eq!(full_record().month_file_name(), "2026-09.jsonl");
    assert_eq!(
        parse_ts("2026-09-21T08:52:31.402Z"),
        Some(1_789_980_751_402)
    );
    assert_eq!(parse_ts("2026-09-21T08:52:31Z"), Some(1_789_980_751_000));
    assert_eq!(format_ts(5), "1970-01-01T00:00:00.005Z");
    assert_eq!(parse_ts("2026-09-21T08:52:31.4x2Z"), None);
}

#[test]
fn origin_client_prefix_decides_the_client() {
    assert_eq!(
        ClientKind::parse_origin(Some("tui:62a6")),
        (ClientKind::Tui, Some("62a6".into()))
    );
    assert_eq!(
        ClientKind::parse_origin(Some("cli:abc")),
        (ClientKind::Cli, Some("abc".into()))
    );
    assert_eq!(
        ClientKind::parse_origin(Some("mobile:x")),
        (ClientKind::Mobile, Some("x".into()))
    );
    assert_eq!(
        ClientKind::parse_origin(Some("")),
        (ClientKind::Unknown, None)
    );
    assert_eq!(ClientKind::parse_origin(None), (ClientKind::Unknown, None));
    assert_eq!(
        ClientKind::parse_origin(Some("web:1")),
        (ClientKind::Unknown, Some("1".into()))
    );
}

#[test]
fn backfill_ids_are_deterministic_version_5_uuids() {
    assert_eq!(backfill_id(80_756), backfill_id(80_756));
    assert_ne!(backfill_id(80_756), backfill_id(80_757));
    assert_eq!(backfill_id(1).get_version_num(), 5);
}

#[test]
fn oversized_records_shed_descriptive_fields_but_keep_counts() {
    let mut record = full_record();
    record.workspace = Some(PathBuf::from("x".repeat(5_000)));
    let line = record.to_line().unwrap();
    assert!(line.len() < MAX_LINE_BYTES);
    let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
    assert!(value["workspace"].is_null());
    assert_eq!(value["input_tokens"], 21_342);
    assert_eq!(value["model"], "deepseek-v4-pro");
}

#[test]
fn sixteen_writers_append_to_one_file_without_interleaving() {
    let root = temp_root("concurrent");
    let dir = root.join("usage");
    let threads = (0..16)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for _ in 0..1_000 {
                    // 每个线程像一个独立进程那样各自打开文件追加。
                    append_record(&dir, &full_record_with_new_id()).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    let files = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(files, 1, "every record falls in the same month file");
    let lines = read_lines(&dir);
    assert_eq!(lines.len(), 16_000);
    let ids = lines
        .iter()
        .map(|line| line["id"].as_str().unwrap().to_owned())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 16_000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("2026-09.jsonl");
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn full_record_with_new_id() -> UsageLedgerRecord {
    let mut record = full_record();
    record.id = Uuid::new_v4();
    record
}

#[test]
fn the_shared_writer_thread_serializes_concurrent_submissions() {
    let root = temp_root("sink");
    let sink = UsageLedgerSink::spawn(root.join("usage"));
    let threads = (0..16)
        .map(|_| {
            let sink = sink.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    sink.submit(full_record_with_new_id());
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(sink.flush(Duration::from_secs(10)));
    assert_eq!(sink.written(), 800);
    assert_eq!(read_lines(&root.join("usage")).len(), 800);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unwritable_ledger_only_counts_failures() {
    let root = temp_root("unwritable");
    // 账本目录的位置被一个普通文件占着：建目录、开文件都会失败。
    let blocked = root.join("usage");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let sink = UsageLedgerSink::spawn(&blocked);
    sink.submit(full_record());
    sink.submit(full_record_with_new_id());
    assert!(sink.flush(Duration::from_secs(10)));
    assert_eq!(sink.written(), 0);
    assert_eq!(sink.failed(), 2);
    drop(sink);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dropping_the_sink_drains_what_was_already_submitted() {
    let root = temp_root("drain");
    let sink = UsageLedgerSink::spawn(root.join("usage"));
    for _ in 0..25 {
        sink.submit(full_record_with_new_id());
    }
    drop(sink);
    assert_eq!(read_lines(&root.join("usage")).len(), 25);
    std::fs::remove_dir_all(root).unwrap();
}

fn scope_at(dir: PathBuf, execution: Execution) -> (UsageLedgerScope, Arc<UsageLedgerSink>) {
    let sink = UsageLedgerSink::spawn(dir);
    let mut context = UsageLedgerContext::new(ClientKind::Cli, execution);
    context.session_id = Some(Uuid::new_v4());
    context.task_id = Some("task-1".into());
    (UsageLedgerScope::new(sink.clone(), context), sink)
}

#[test]
fn an_abandoned_call_is_recorded_as_cancelled() {
    let root = temp_root("abandoned");
    let (scope, sink) = scope_at(root.join("usage"), Execution::InProcess);
    drop(scope.begin(UsageKind::Main, None));
    let mut settled = scope
        .for_subagent(Uuid::nil())
        .begin(UsageKind::Subagent, None);
    settled.settle(
        Some(&Usage {
            input_tokens: Some(3),
            ..Usage::default()
        }),
        Outcome::Error,
    );
    settled.finish(Some(9));
    assert!(sink.flush(Duration::from_secs(10)));
    let lines = read_lines(&root.join("usage"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["outcome"], "cancelled");
    assert!(lines[0]["input_tokens"].is_null());
    assert_eq!(lines[1]["kind"], "subagent");
    assert_eq!(lines[1]["agent_id"], Uuid::nil().to_string());
    assert_eq!(lines[1]["event_sequence"], 9);
    assert_eq!(lines[1]["input_tokens"], 3);
    std::fs::remove_dir_all(root).unwrap();
}

/// 第一次回一个工具调用、第二次给终稿，两次都带 usage。
struct TwoStepProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for TwoStepProvider {
    fn ledger_identity(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity {
            provider: "some.im".into(),
            model: "deepseek-v4-flash".into(),
            local: false,
        })
    }

    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let usage = Some(Usage {
            input_tokens: Some(100 + call as u64),
            output_tokens: Some(10),
            total_tokens: Some(110 + call as u64),
            cache_read_tokens: Some(64),
        });
        Ok(if call == 0 {
            Completion {
                reasoning: None,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "list_directory".into(),
                    arguments: r#"{"path":"."}"#.into(),
                }],
                usage,
                finish_reason: Some("tool_calls".into()),
            }
        } else {
            Completion {
                reasoning: None,
                content: "done".into(),
                tool_calls: Vec::new(),
                usage,
                finish_reason: Some("stop".into()),
            }
        })
    }
}

/// 模拟 daemon 的事件宿主：每个事件领一个递增序号。
struct SequencedSink {
    next: AtomicU64,
    usage_sequences: Mutex<Vec<u64>>,
}

#[async_trait]
impl EventSink for SequencedSink {
    async fn emit(&self, _event: AgentEvent) {
        self.next.fetch_add(1, Ordering::SeqCst);
    }

    async fn emit_sequenced(&self, event: AgentEvent) -> Option<u64> {
        let sequence = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        if matches!(event, AgentEvent::Usage(_)) {
            self.usage_sequences.lock().unwrap().push(sequence);
        }
        Some(sequence)
    }
}

fn agent_config() -> AgentConfig {
    AgentConfig {
        max_turns: 4,
        system_prompt: String::new(),
        context_window: 32_000,
        token_budget: None,
    }
}

#[tokio::test]
async fn an_agent_run_writes_one_line_per_provider_request() {
    for execution in [Execution::InProcess, Execution::Daemon] {
        let root = temp_root("agent");
        let (scope, sink) = scope_at(root.join("usage"), execution);
        let events = Arc::new(SequencedSink {
            next: AtomicU64::new(100),
            usage_sequences: Mutex::new(Vec::new()),
        });
        let agent = Agent::new(
            Arc::new(TwoStepProvider {
                calls: AtomicUsize::new(0),
            }),
            ToolRegistry::new(&root, ApprovalMode::ReadOnly).unwrap(),
            agent_config(),
        )
        .with_event_sink(events.clone())
        .with_usage_ledger(scope);
        let outcome = agent.run("list and finish").await.unwrap();
        assert_eq!(outcome.final_text, "done");
        assert!(sink.flush(Duration::from_secs(10)));
        let lines = read_lines(&root.join("usage"));
        assert_eq!(lines.len(), 2, "one line per provider request");
        let expected = match execution {
            Execution::Daemon => "daemon",
            Execution::InProcess => "in_process",
        };
        let sequences = events.usage_sequences.lock().unwrap().clone();
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line["execution"], expected);
            assert_eq!(line["kind"], "main");
            assert_eq!(line["client"], "cli");
            assert_eq!(line["outcome"], "ok");
            assert_eq!(line["provider"], "some.im");
            assert_eq!(line["model"], "deepseek-v4-flash");
            assert_eq!(line["local"], false);
            assert_eq!(line["input_tokens"], 100 + index as u64);
            assert_eq!(line["cache_read_tokens"], 64);
            assert_eq!(line["task_id"], "task-1");
            assert_eq!(line["backfilled"], false);
            assert!(line["latency_ms"].is_u64());
            // 实时行带上宿主给 usage 事件的序号，回填据此去重。
            assert_eq!(line["event_sequence"], sequences[index]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn an_unwritable_ledger_never_fails_the_turn() {
    let root = temp_root("agent-unwritable");
    let blocked = root.join("usage");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let (scope, sink) = scope_at(blocked, Execution::InProcess);
    let agent = Agent::new(
        Arc::new(TwoStepProvider {
            calls: AtomicUsize::new(0),
        }),
        ToolRegistry::new(&root, ApprovalMode::ReadOnly).unwrap(),
        agent_config(),
    )
    .with_usage_ledger(scope);
    let outcome = agent.run("list and finish").await.unwrap();
    assert_eq!(outcome.final_text, "done");
    assert_eq!(outcome.input_tokens, 201);
    assert!(sink.flush(Duration::from_secs(10)));
    assert_eq!(sink.failed(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<Completion, ProviderError> {
        Err(ProviderError::EmptyResponse)
    }
}

#[tokio::test]
async fn auxiliary_providers_record_each_call_including_failures() {
    let root = temp_root("auxiliary");
    let (scope, sink) = scope_at(root.join("usage"), Execution::InProcess);
    let titler = scope.auxiliary(Arc::new(TwoStepProvider {
        calls: AtomicUsize::new(1),
    }));
    assert_eq!(
        titler.ledger_identity().map(|identity| identity.model),
        Some("deepseek-v4-flash".into())
    );
    titler.complete(&[Message::user("hi")], &[]).await.unwrap();
    let failing = scope.auxiliary(Arc::new(FailingProvider));
    assert!(failing.complete(&[Message::user("hi")], &[]).await.is_err());
    assert!(sink.flush(Duration::from_secs(10)));
    let lines = read_lines(&root.join("usage"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["kind"], "auxiliary");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["input_tokens"], 101);
    assert_eq!(lines[1]["outcome"], "error");
    assert!(lines[1]["model"].is_null());
    assert!(lines[1]["input_tokens"].is_null());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn compression_requests_are_recorded_as_compression() {
    let root = temp_root("compression");
    let (scope, sink) = scope_at(root.join("usage"), Execution::InProcess);
    let agent = Agent::new(
        Arc::new(TwoStepProvider {
            calls: AtomicUsize::new(1),
        }),
        ToolRegistry::new(&root, ApprovalMode::ReadOnly).unwrap(),
        agent_config(),
    )
    .with_usage_ledger(scope);
    let mut history = vec![Message::system("s")];
    for index in 0..40 {
        history.push(Message::user(format!(
            "question {index} {}",
            "x".repeat(400)
        )));
        history.push(Message::assistant(
            format!("answer {index} {}", "y".repeat(400)),
            Vec::new(),
        ));
    }
    let compressed = agent
        .compress_history_recorded(history, &mut |_| Ok(()))
        .await
        .unwrap();
    assert!(!compressed.is_empty());
    assert!(sink.flush(Duration::from_secs(10)));
    let lines = read_lines(&root.join("usage"));
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line["kind"] == "compression"));
    std::fs::remove_dir_all(root).unwrap();
}
