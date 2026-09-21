use super::*;

/// 结束通知的框架（请求 JSON 里换行被转义）。工具描述里也提到 `<monitor-ended>`，
/// 只认框架本身才不会假阳性。
#[cfg(unix)]
const ENDED_FRAME: &str = "<monitor-ended>\\n  id: mon_";

/// 第一轮起一个 monitor，其余请求正常收尾。安全裁判的请求按内容识别。
///
/// 命令每秒打印一行，第 3 行是失败行；按工具描述的用法先用
/// `grep --line-buffered` 过滤。命令行本身不能含完整的 `ERROR` 标记：它会随
/// 工具调用回放进历史，断言就成了假绿。
pub(super) fn response(body: &[u8], request_index: usize) -> String {
    let text = String::from_utf8_lossy(body);
    if text.contains("<verdict>") {
        return serde_json::json!({"choices":[{"message":{"content":"<verdict>YES</verdict>"},"finish_reason":"stop"}]}).to_string();
    }
    if request_index == 0 {
        let arguments = serde_json::json!({
            "command": "for i in 1 2 3 4 5 6; do if [ \"$i\" = 3 ]; then printf 'ERR%s: step %s failed\\n' OR \"$i\"; else printf 'step %s ok\\n' \"$i\"; fi; sleep 1; done | grep --line-buffered -E 'ERR|FAIL|Traceback'",
            "label": "watch the release log",
            "timeout_seconds": 60,
        });
        return serde_json::json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"start_monitor","type":"function","function":{"name":"monitor","arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]}).to_string();
    }
    format!(
        r#"{{"choices":[{{"message":{{"content":"{MOCK_REPLY}","tool_calls":[]}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}}}"#
    )
}

/// 无头运行里，monitor 的事件在命令还没结束时就交给模型，结束事件随后也到；
/// 进程在两者都投递之后才退出。
/// 仅 Unix：monitor 的命令是 POSIX shell（`for` 循环加 `grep --line-buffered`），Windows 上
/// Shell 工具走 PowerShell；monitor 自己的单元测试同样只在 Unix 上跑。
#[cfg(unix)]
#[test]
fn local_run_delivers_monitor_events_before_the_command_exits() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::MonitorThenSuccess);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());

    let output = willdeep(&home)
        .args([
            "run",
            "--local",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "watch the release log",
        ])
        .output()
        .unwrap();
    assert_success(&output, "headless run with a monitor");
    let requests = provider
        .captured_requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.to_string())
        .collect::<Vec<_>>();
    let event_index = requests
        .iter()
        .position(|request| request.contains("ERROR: step 3 failed"))
        .unwrap_or_else(|| panic!("no request carried the ERROR event: {requests:#?}"));
    let event_request = &requests[event_index];
    // 合同 v1：框架原样交给模型，不是被内核整段转义过的 `&lt;…`。
    assert!(event_request.contains("<monitor-event>\\n  id: mon_"));
    assert!(event_request.contains("\\n  label: watch the release log\\n  seq: 1\\n  lines: 1\\n"));
    assert!(!event_request.contains("&lt;monitor-event"));
    assert!(
        !event_request.contains(ENDED_FRAME),
        "the ERROR event must reach the model before the command exits"
    );
    let ended = requests[event_index + 1..]
        .iter()
        .find(|request| request.contains(ENDED_FRAME))
        .unwrap_or_else(|| panic!("the ended event never reached the model: {requests:#?}"));
    assert!(ended.contains("\\n  reason: exited\\n  exit_code: 0\\n"));
    assert!(ended.contains("\\n  events: 1\\n"));

    let monitors = std::fs::read_dir(home.join("monitors"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(monitors.len(), 1);
    assert_eq!(
        std::fs::read_to_string(monitors[0].path().join("stdout.log")).unwrap(),
        "ERROR: step 3 failed\n"
    );
}
