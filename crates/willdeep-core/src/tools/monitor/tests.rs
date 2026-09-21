use super::*;
// 下面几个辅助函数只给 Unix 上的 monitor 测试用（命令是 POSIX shell）。
#[cfg(unix)]
use crate::kernel::InterruptPolicy;

#[cfg(unix)]
struct AllowApprover;

#[cfg(unix)]
#[async_trait]
impl Approver for AllowApprover {
    async fn approve(&self, _description: &str, _always_allow_available: bool) -> ApprovalDecision {
        ApprovalDecision::AllowOnce
    }
}

#[test]
fn flood_guard_admits_thirty_events_in_any_sixty_seconds() {
    let mut guard = FloodGuard::default();
    let start = Instant::now();
    for index in 0..30 {
        assert!(guard.admit(start + Duration::from_millis(index)));
    }
    assert!(!guard.admit(start + Duration::from_secs(59)));
    // 第一条滑出窗口之后又能放行一条。
    assert!(guard.admit(start + Duration::from_secs(60)));
    assert!(!guard.admit(start + Duration::from_secs(60)));
}

#[test]
fn wake_throttle_allows_one_wake_per_thirty_seconds() {
    let mut throttle = WakeThrottle::default();
    let start = Instant::now();
    assert!(throttle.admit(start));
    assert!(!throttle.admit(start + Duration::from_secs(1)));
    assert!(!throttle.admit(start + Duration::from_secs(29)));
    assert!(throttle.admit(start + Duration::from_secs(30)));
}

#[test]
fn a_batch_closes_at_fifty_lines_and_ignores_blank_lines() {
    let mut batch = Batch::default();
    let now = Instant::now();
    assert!(!batch.push("   ".to_owned(), now));
    assert!(batch.deadline.is_none());
    for index in 0..49 {
        assert!(!batch.push(format!("line {index}"), now));
    }
    assert_eq!(batch.deadline, Some(now + BATCH_WINDOW));
    assert!(batch.push("line 49".to_owned(), now));
    assert_eq!(batch.take().len(), 50);
    assert!(batch.deadline.is_none());
    assert!(batch.push("x".repeat(MAX_BATCH_CHARS), now));
}

fn temp_root(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("willdeep-monitor-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

#[cfg(unix)]
fn monitored_registry(
    root: &Path,
    kernel: &EventKernel,
) -> (ToolRegistry, Arc<BackgroundTaskRegistry>) {
    let background = Arc::new(BackgroundTaskRegistry::default());
    let tools = ToolRegistry::new(root, ApprovalMode::Strict)
        .unwrap()
        .with_approver(Arc::new(AllowApprover))
        .with_background_tasks(background.clone())
        .with_monitor_events(kernel.clone(), uuid::Uuid::nil(), root.join("monitors"));
    (tools, background)
}

#[cfg(unix)]
async fn start(tools: &ToolRegistry, command: &str, timeout: Option<u64>) -> String {
    let result = tools
        .monitor(MonitorArgs {
            command: command.to_owned(),
            label: "watch the build".to_owned(),
            timeout_seconds: timeout,
        })
        .await
        .expect("monitor starts");
    result
        .split_whitespace()
        .find(|word| word.starts_with("mon_"))
        .expect("monitor id")
        .trim_end_matches('.')
        .to_owned()
}

#[cfg(unix)]
async fn wait_for_body(kernel: &EventKernel, needle: &str) -> KernelEventView {
    for _ in 0..200 {
        if let Some(event) = kernel.snapshot().into_iter().find(|event| {
            event
                .body
                .as_deref()
                .is_some_and(|body| body.contains(needle))
        }) {
            return KernelEventView {
                body: event.body.unwrap_or_default(),
                dedup_key: event.dedup_key.unwrap_or_default(),
                interrupt: event.interrupt,
            };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "no kernel event contained {needle:?}: {:#?}",
        kernel.snapshot()
    );
}

#[cfg(unix)]
struct KernelEventView {
    body: String,
    dedup_key: String,
    interrupt: InterruptPolicy,
}

#[cfg(unix)]
#[tokio::test]
async fn lines_reach_the_kernel_while_the_command_is_still_running() {
    let root = temp_root("stream");
    let kernel = EventKernel::new();
    let (tools, background) = monitored_registry(&root, &kernel);
    let id = start(
        &tools,
        "printf 'ok 1\\n'; printf 'to-stderr\\n' >&2; sleep 1; printf 'ERR%s: boom\\n' OR; sleep 2; exit 1",
        Some(30),
    )
    .await;
    assert!(id.starts_with("mon_") && id.len() == 10, "{id}");

    let first = wait_for_body(&kernel, "ok 1").await;
    assert_eq!(first.dedup_key, format!("monitor:{id}:1"));
    assert_eq!(first.interrupt, InterruptPolicy::YieldAtBoundary);
    assert!(first.body.starts_with("<monitor-event>\n"));
    assert!(!first.body.contains("to-stderr"));

    let error = wait_for_body(&kernel, "ERROR: boom").await;
    assert_eq!(error.dedup_key, format!("monitor:{id}:2"));
    // 30 秒内的第二个事件不再唤醒，只入队等回合边界。
    assert_eq!(error.interrupt, InterruptPolicy::Enqueue);
    assert_eq!(
        background.snapshots()[0].status,
        BackgroundTaskStatus::Running,
        "the ERROR event must arrive before the command exits"
    );
    assert!(background.take_monitor_signals() >= 2);
    let progress = tools.monitor_output(&id, Some(5)).expect("monitor output");
    assert!(progress.contains("status: running") && progress.contains("ERROR: boom"));

    let ended = wait_for_body(&kernel, "<monitor-ended>").await;
    assert_eq!(ended.dedup_key, format!("monitor:{id}:ended"));
    assert!(ended.body.contains("reason: exited\nexit_code: 1\n"));
    assert!(ended.body.contains("events: 2\n"));
    let log = root.join("monitors").join(&id);
    assert!(
        ended
            .body
            .contains(&log.join("stdout.log").display().to_string())
    );
    for _ in 0..100 {
        if background.snapshots()[0].status != BackgroundTaskStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        background.snapshots()[0].status,
        BackgroundTaskStatus::Failed
    );
    assert_eq!(
        std::fs::read_to_string(log.join("stdout.log")).unwrap(),
        "ok 1\nERROR: boom\n"
    );
    assert_eq!(
        std::fs::read_to_string(log.join("stderr.log")).unwrap(),
        "to-stderr\n"
    );
    // 结束通知只由监视器自己发，注册表不再排一条 background-task-notification。
    assert!(background.drain_pending().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_flooding_command_is_stopped_after_thirty_events() {
    let root = temp_root("flood");
    let kernel = EventKernel::new();
    let (tools, background) = monitored_registry(&root, &kernel);
    let id = start(
        &tools,
        "i=0; while true; do echo \"spam $i\"; i=$((i+1)); done",
        Some(60),
    )
    .await;
    let ended = wait_for_body(&kernel, "<monitor-ended>").await;
    assert!(
        ended.body.contains("reason: flooded\nexit_code: unknown\n"),
        "{}",
        ended.body
    );
    assert!(ended.body.contains("events: 30\n"));
    let events = kernel
        .snapshot()
        .into_iter()
        .filter(|event| event.kind == "monitor.event")
        .count();
    assert_eq!(events, 30);
    assert!(
        kernel
            .snapshot()
            .iter()
            .any(|event| event.dedup_key.as_deref() == Some(&format!("monitor:{id}:ended")))
    );
    for _ in 0..100 {
        if background.snapshots()[0].status != BackgroundTaskStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        background.snapshots()[0].status,
        BackgroundTaskStatus::Killed
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn kill_job_stops_a_monitor_and_a_deadline_times_it_out() {
    let root = temp_root("kill");
    let kernel = EventKernel::new();
    let (tools, _background) = monitored_registry(&root, &kernel);
    let id = start(&tools, "sleep 30", None).await;
    tools
        .kill_job(JobIDArgs { job_id: id.clone() })
        .await
        .expect("kill requested");
    let ended = wait_for_body(
        &kernel,
        &format!("id: {id}\nlabel: watch the build\nreason: killed"),
    )
    .await;
    assert!(ended.body.contains("exit_code: unknown\n"));

    let id = start(&tools, "sleep 30", Some(1)).await;
    let ended = wait_for_body(
        &kernel,
        &format!("id: {id}\nlabel: watch the build\nreason: timed_out"),
    )
    .await;
    assert!(ended.body.contains("events: 0\n"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn monitor_needs_its_event_sink_and_uses_the_command_gate() {
    let root = temp_root("gate");
    let plain = ToolRegistry::new(&root, ApprovalMode::Strict).unwrap();
    assert!(
        plain
            .definitions()
            .iter()
            .all(|tool| tool.name != "monitor")
    );

    let kernel = EventKernel::new();
    // 默认审批器拒绝一切：monitor 与 run_command 同一道闸，拒了就不启动。
    let denied = ToolRegistry::new(&root, ApprovalMode::Strict)
        .unwrap()
        .with_monitor_events(kernel.clone(), uuid::Uuid::nil(), root.join("monitors"));
    assert!(
        denied
            .definitions()
            .iter()
            .any(|tool| tool.name == "monitor")
    );
    let result = denied
        .execute(&ToolCall {
            id: "m".into(),
            name: "monitor".into(),
            arguments: json!({"command": "echo hi", "label": "x"}).to_string(),
        })
        .await;
    assert!(
        matches!(result, Err(ToolError::ApprovalDenied(_))),
        "{result:?}"
    );
    assert!(kernel.snapshot().is_empty());

    let read_only = ToolRegistry::new(&root, ApprovalMode::ReadOnly)
        .unwrap()
        .with_monitor_events(kernel.clone(), uuid::Uuid::nil(), root.join("monitors"));
    let result = read_only
        .execute(&ToolCall {
            id: "m".into(),
            name: "monitor".into(),
            arguments: json!({"command": "echo hi", "label": "x"}).to_string(),
        })
        .await;
    assert!(
        matches!(result, Err(ToolError::ReadOnlyPolicy(_))),
        "{result:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
