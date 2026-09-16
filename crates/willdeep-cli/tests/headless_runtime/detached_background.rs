use super::*;

const JOB_OUTPUT: &str = "detached-job-finished";

/// 第一轮起一条后台命令，其余请求正常收尾。安全裁判的请求按内容识别，不占
/// 轮次序号——否则一次审批就会把后面的回复全部错位。
pub(super) fn response(body: &[u8], request_index: usize) -> String {
    let text = String::from_utf8_lossy(body);
    if text.contains("<verdict>") {
        return serde_json::json!({"choices":[{"message":{"content":"<verdict>YES</verdict>"},"finish_reason":"stop"}]}).to_string();
    }
    if request_index == 0 {
        // 命令行本身不能含完整标记：它会随工具调用回放进历史，断言就成了假绿。
        let arguments = serde_json::json!({
            "command": "sleep 1; printf 'detached-job-%s' finished",
            "run_in_background": true,
            "label": "detached regression",
        });
        return serde_json::json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"start_job","type":"function","function":{"name":"run_command","arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]}).to_string();
    }
    format!(
        r#"{{"choices":[{{"message":{{"content":"{MOCK_REPLY}","tool_calls":[]}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}}}"#
    )
}

/// 无头运行不能在后台命令跑完之前退出，结论要作为运行时事件再交给模型一轮。
///
/// 回归点：`run_in_background` 走的是落盘的脱离作业，而等待循环此前只看进程内
/// 注册表——第一轮一结束进程就退了，模型永远见不到这条命令的结果。
#[test]
fn local_run_waits_for_a_detached_job_and_wakes_the_model_with_its_result() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::BackgroundJobThenSuccess);
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
            "start the job in the background",
        ])
        .output()
        .unwrap();
    assert_success(&output, "headless run with a detached background job");
    let requests = provider.captured_requests.lock().unwrap().clone();
    let woke_with_result = requests
        .iter()
        .any(|request| request.to_string().contains(JOB_OUTPUT));
    assert!(
        woke_with_result,
        "作业结论必须在退出前交给模型；共 {} 次请求",
        requests.len()
    );
    // 合同 v1：模型看到的是原样框架，不是被内核整段转义过的 `&lt;…`。
    // `<runtime-events>` 给每条事件正文统一缩进两格，那是外层的排版。
    let woke = requests
        .iter()
        .map(|request| request.to_string())
        .find(|request| request.contains(JOB_OUTPUT))
        .unwrap();
    assert!(woke.contains("<background-task-notification>\\n  id: job_"));
    assert!(woke.contains("\\n  status: completed\\n  exit_code: 0\\n"));
    assert!(!woke.contains("&lt;background-task-notification"));

    // 已投递的作业留下领取标记，下一次续跑不会再讲一遍。
    let jobs = willdeep_core::DetachedJobStore::new(&home).list();
    assert_eq!(jobs.len(), 1);
    assert!(
        home.join("background-jobs")
            .join(&jobs[0].id)
            .join("delivered")
            .exists()
    );
}
