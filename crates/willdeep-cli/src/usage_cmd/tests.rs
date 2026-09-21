use super::*;

const TASK: &str = "2187366a-ee7f-41db-b4d1-1e92cfd11d5c";
const ACTIVE_TASK: &str = "3207b34d-4a00-4dfd-bfb7-f01942305dfb";
const SESSION: &str = "4758346f-2ee8-4196-ba7c-5083ae22822e";
const CHILD: &str = "24d74d11-bbb0-4b3f-93cb-22370696beac";

fn fixture_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("willdeep-backfill-{}", uuid::Uuid::new_v4()));
    let runtime = home.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let tasks = serde_json::json!([
        {
            "id": TASK, "session_id": SESSION,
            "turn_id": "1cf6a23b-3a84-4a67-b569-9ed0818f1f2c",
            "agent_id": "f812da33-45e0-45f9-8a6d-371562d9f3a8",
            "status": "completed", "workspace": "/work/factify",
            "profile": null, "model": null, "origin_client": "tui:62a6264a",
            "prompt_excerpt": "must never reach the ledger"
        },
        {
            "id": ACTIVE_TASK, "session_id": null, "status": "running",
            "workspace": "/work/other", "model": "glm-5", "origin_client": null
        }
    ]);
    std::fs::write(runtime.join("tasks.json"), tasks.to_string()).unwrap();
    let sessions = serde_json::json!([{ "id": SESSION, "model": "deepseek-v4-pro" }]);
    std::fs::write(runtime.join("sessions.json"), sessions.to_string()).unwrap();
    let agents = serde_json::json!([{ "id": CHILD, "model": "someim-32b" }]);
    std::fs::write(runtime.join("agents.json"), agents.to_string()).unwrap();
    let event = |sequence: u64, task: &str, payload: serde_json::Value| {
        serde_json::json!({
            "sequence": sequence, "timestamp": 1_789_980_751_u64 + sequence,
            "kind": "task.output", "message": format!("task_id={task} {payload}")
        })
        .to_string()
    };
    let lines = [
        serde_json::json!({"sequence":1,"timestamp":1_789_980_700_u64,"kind":"task.queued","message":format!("task_id={TASK}")}).to_string(),
        event(2, TASK, serde_json::json!({"input_tokens":16394,"output_tokens":176,"total_tokens":16570,"cache_read_tokens":16000,"type":"usage"})),
        event(3, TASK, serde_json::json!({"type":"assistant_text","text":"usage is fine"})),
        event(4, TASK, serde_json::json!({"id":CHILD,"input_tokens":953,"output_tokens":139,"total_tokens":1092,"type":"subagent_usage"})),
        event(5, TASK, serde_json::json!({"input_tokens":10,"output_tokens":1,"total_tokens":11,"type":"usage"})),
        event(6, ACTIVE_TASK, serde_json::json!({"input_tokens":7,"output_tokens":1,"total_tokens":8,"type":"usage"})),
        "{not json usage".to_owned(),
    ];
    std::fs::write(runtime.join("events.ndjson"), lines.join("\n") + "\n").unwrap();
    home
}

fn ledger_bytes(home: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = std::fs::read_dir(ledger_dir(home))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn ledger_lines(home: &Path) -> Vec<serde_json::Value> {
    ledger_bytes(home)
        .into_iter()
        .flat_map(|(_, bytes)| {
            String::from_utf8(bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

#[test]
fn backfill_joins_tasks_and_is_byte_identical_when_repeated() {
    let home = fixture_home();
    let plan = plan_backfill(&home).unwrap();
    assert_eq!(plan.usage_events, 4);
    assert_eq!(
        plan.task_active, 1,
        "the running task is left to live recording"
    );
    write_backfill(&home, &plan).unwrap();
    let first = ledger_bytes(&home);
    let lines = ledger_lines(&home);
    assert_eq!(lines.len(), 3);

    let main = &lines[0];
    assert_eq!(main["id"], backfill_id(2).to_string());
    assert_eq!(main["ts"], "2026-09-21T08:52:33.000Z");
    assert_eq!(main["client"], "tui");
    assert_eq!(main["client_instance"], "62a6264a");
    assert_eq!(main["execution"], "daemon");
    assert_eq!(main["kind"], "main");
    assert_eq!(main["session_id"], SESSION);
    assert_eq!(main["task_id"], TASK);
    assert_eq!(main["agent_id"], "f812da33-45e0-45f9-8a6d-371562d9f3a8");
    assert_eq!(main["workspace"], "/work/factify");
    assert_eq!(
        main["model"], "deepseek-v4-pro",
        "falls back to the session model"
    );
    assert!(main["provider"].is_null());
    assert_eq!(main["input_tokens"], 16394);
    assert_eq!(main["cache_read_tokens"], 16000);
    assert_eq!(main["event_sequence"], 2);
    assert_eq!(main["backfilled"], true);
    assert!(!main.to_string().contains("must never reach the ledger"));

    let child = &lines[1];
    assert_eq!(child["kind"], "subagent");
    assert_eq!(child["agent_id"], CHILD);
    assert_eq!(child["model"], "someim-32b");
    assert!(child["cache_read_tokens"].is_null());

    let marker = std::fs::read_to_string(ledger_dir(&home).join(BACKFILL_MARKER)).unwrap();
    assert!(marker.contains("\"max_sequence\":5"));

    // 第二次：全部命中已有 id，一行不写，账本逐字节不变。
    let again = plan_backfill(&home).unwrap();
    assert!(again.records.is_empty());
    assert_eq!(again.already_recorded, 3);
    write_backfill(&home, &again).unwrap();
    assert_eq!(ledger_bytes(&home), first);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn events_already_recorded_live_are_not_backfilled() {
    let home = fixture_home();
    // daemon 实时记过序号 5 的那次调用（id 是随机的，只能靠 event_sequence 认出来）。
    let mut live = UsageLedgerRecord::new(UsageKind::Main, Execution::Daemon);
    live.ts = 1_789_980_756_000;
    live.event_sequence = Some(5);
    append_record(&ledger_dir(&home), &live).unwrap();
    let plan = plan_backfill(&home).unwrap();
    assert_eq!(plan.recorded_live, 1);
    let sequences = plan
        .records
        .iter()
        .filter_map(|record| record.event_sequence)
        .collect::<Vec<_>>();
    assert_eq!(sequences, vec![2, 4]);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn dry_run_reports_per_day_totals() {
    let home = fixture_home();
    let plan = plan_backfill(&home).unwrap();
    let report = render_report(&plan, true);
    assert!(report.contains("\t3\t1\t17357\t16000\t316\n"), "{report}");
    assert!(report.contains("would backfill 3 of 4 usage events"));
    assert!(!ledger_dir(&home).exists(), "dry run writes nothing");
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn daemon_start_backfills_once() {
    let home = fixture_home();
    backfill_on_daemon_start(&home);
    assert_eq!(ledger_lines(&home).len(), 3);
    // 标记在：删掉账本也不会再跑，自动回填只做一次。
    std::fs::remove_file(ledger_dir(&home).join("2026-09.jsonl")).unwrap();
    backfill_on_daemon_start(&home);
    assert!(ledger_bytes(&home).is_empty());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_home_without_runtime_files_backfills_nothing() {
    let home = std::env::temp_dir().join(format!("willdeep-backfill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let plan = plan_backfill(&home).unwrap();
    assert!(plan.records.is_empty());
    backfill_on_daemon_start(&home);
    assert!(ledger_dir(&home).join(BACKFILL_MARKER).exists());
    std::fs::remove_dir_all(home).unwrap();
}
