use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use futures_util::StreamExt;

#[path = "headless_runtime/automatic_compression.rs"]
mod automatic_compression;
#[path = "headless_runtime/background_supervisor.rs"]
mod background_supervisor;
#[path = "headless_runtime/detached_background.rs"]
mod detached_background;
#[path = "headless_runtime/foreground_recovery.rs"]
mod foreground_recovery;
#[path = "headless_runtime/local_partial.rs"]
mod local_partial;
#[path = "headless_runtime/monitor_events.rs"]
mod monitor_events;

const MOCK_REPLY: &str = "headless runtime reply";
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

/// 一条失败的工具，事后要能问出「哪条命令、为什么挂」。
///
/// 公共事件流对所有客户端脱敏（Web 桥接和手机中继都吃它），所以失败详情走本机的
/// `task.diagnostics`：同一份数据，两种口径——这条测试同时把两边都钉住。
#[test]
fn task_diagnostics_reports_the_failing_tool_that_events_redact() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_with_failing_tool();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let run = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "read a file that is not there",
        ])
        .output()
        .expect("run headless turn");
    assert_success(&run, "headless turn with a failing tool");

    let tasks = willdeep(&home)
        .args(["api", "task.list"])
        .output()
        .expect("list Runtime tasks");
    assert_success(&tasks, "task.list");
    let tasks: serde_json::Value = serde_json::from_slice(&tasks.stdout).expect("parse task.list");
    let task_id = tasks["data"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .and_then(|task| task["id"].as_str())
        .expect("one Runtime task")
        .to_owned();
    assert_eq!(
        tasks["data"][0]["prompt_excerpt"].as_str(),
        Some("read a file that is not there"),
        "task.list 必须带提示词摘要当任务标识"
    );

    let params = root.join("diagnostics.json");
    std::fs::write(&params, format!(r#"{{"id":"{task_id}"}}"#)).expect("write params");
    let diagnostics = willdeep(&home)
        .args([
            "api",
            "task.diagnostics",
            "--params-file",
            path_text(&params),
        ])
        .output()
        .expect("read task diagnostics");
    assert_success(&diagnostics, "task.diagnostics");
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&diagnostics.stdout).expect("parse task.diagnostics");
    let failed = diagnostics["data"]["failed_tools"]
        .as_array()
        .and_then(|tools| tools.first())
        .expect("one failed tool");
    assert_eq!(failed["name"], "read_file");
    assert!(
        failed["arguments"]
            .as_str()
            .expect("arguments")
            .contains("definitely-missing.txt"),
        "诊断必须说出是哪一条调用: {failed}"
    );
    assert!(
        !failed["output"].as_str().expect("output").trim().is_empty(),
        "诊断必须说出为什么失败"
    );

    // 同一次失败，公共事件流里不能出现参数或输出。
    let events = willdeep(&home)
        .args(["api", "event.list"])
        .output()
        .expect("list public events");
    assert_success(&events, "event.list");
    let events = String::from_utf8(events.stdout).expect("events are UTF-8");
    assert!(events.contains("tool_completed"), "事件流本身要有工具事件");
    assert!(
        !events.contains("definitely-missing.txt"),
        "公共事件流不得泄露工具参数"
    );

    guard.stop_daemon();
}

#[test]
fn explicit_verification_survives_runtime_restart_and_config_removal() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert_success(&init, "initialize verification workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let original = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        original.replace(
            "[agent]",
            "[agent]\nverification_commands = [\"cargo test --workspace\"]",
        ),
    )
    .unwrap();
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let run = |session: Option<&str>| {
        let mut command = willdeep(&home);
        command.args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
        ]);
        if let Some(session) = session {
            command.args(["--session", session]);
        }
        command.arg("complete the objective").output().unwrap()
    };
    let first = run(None);
    assert_eq!(
        first.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["type"], "partial");
    assert_eq!(first["stop_reason"], "unverified");
    let session = first["session_id"].as_str().unwrap();
    assert_eq!(provider.requests(), 3);
    let saved = willdeep_core::SessionStore::new(&home)
        .load(session.parse().unwrap())
        .unwrap();
    assert_eq!(
        saved.execution_checkpoint.unwrap().required_verifications,
        vec!["cargo test --workspace"]
    );
    assert_eq!(
        runtime_state(&home, "turns.json")[0]["metadata"]["status"],
        "partial"
    );

    let stop = willdeep(&home).args(["daemon", "stop"]).output().unwrap();
    assert_success(&stop, "stop Runtime before reconstructing executor");
    std::fs::write(&config, original).unwrap();
    let resumed = run(Some(session));
    assert_eq!(
        resumed.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["type"], "partial");
    assert_eq!(resumed["stop_reason"], "unverified");
    assert_eq!(resumed["session_id"], session);
    assert_eq!(provider.requests(), 6);

    let independent = run(None);
    assert_success(
        &independent,
        "new independent task has no configured verification contract",
    );
    let independent: serde_json::Value = serde_json::from_slice(&independent.stdout).unwrap();
    assert_eq!(independent["type"], "completed");
    assert_ne!(independent["session_id"], session);
    assert_eq!(provider.requests(), 7);
    guard.stop_daemon();
}

#[test]
fn partial_runtime_turn_returns_nonzero_and_can_continue() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::IncompleteThenSuccess);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let first = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "complete the objective",
        ])
        .output()
        .unwrap();
    assert_eq!(
        first.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(result["type"], "partial");
    assert_eq!(result["stop_reason"], "incomplete");
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .contains("partial evidence")
    );
    let session = result["session_id"].as_str().unwrap();
    let turns = runtime_state(&home, "turns.json");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["metadata"]["status"], "partial");
    assert!(turns[0]["metadata"]["message_end"].as_u64().unwrap() > 0);
    let agents = runtime_state(&home, "agents.json");
    let root_agent = agents
        .iter()
        .find(|agent| agent["parent_id"].is_null())
        .unwrap();
    assert_eq!(root_agent["status"], "partial");
    assert!(root_agent["completed_at"].as_u64().is_some());
    assert!(root_agent["current_tool"].is_null());
    assert_eq!(provider.requests(), 3);
    let second = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            session,
            "--output",
            "json",
            "continue the remaining work",
        ])
        .output()
        .unwrap();
    assert_success(&second, "continue partial Runtime turn");
    let result: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(result["type"], "completed");
    assert_eq!(result["text"], MOCK_REPLY);
    assert_eq!(provider.requests(), 4, "previous requests must not replay");
    guard.stop_daemon();
}

#[test]
fn web_partial_stream_closes_and_resume_preserves_partial_status() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::IncompleteThenSuccess);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let listen = free_loopback_address();
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let mut web = ChildGuard(Some(
        willdeep(&home)
            .args([
                "--web",
                "--listen",
                &listen,
                "--config",
                path_text(&config),
                "--workspace",
                path_text(&workspace),
            ])
            .spawn()
            .unwrap(),
    ));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = reqwest::Client::new();
        let base = format!("http://{listen}");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client
                    .get(format!("{base}/health"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        let response = client
            .post(format!("{base}/api/chat/stream"))
            .json(&serde_json::json!({
                "prompt": "finish all work", "workspace": workspace.canonicalize().unwrap(),
                "language": "en", "attachments": []
            }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let mut body = tokio::time::timeout(Duration::from_secs(15), response.text())
            .await
            .expect("partial stream must close")
            .unwrap();
        let mut terminal = None;
        while let Some(event) = pop_sse_json(&mut body) {
            assert_ne!(event["type"], "completed");
            if event["type"] == "partial" {
                terminal = Some(event);
            }
        }
        let terminal = terminal.expect("partial terminal event");
        assert!(
            terminal["text"]
                .as_str()
                .unwrap()
                .contains("partial evidence")
        );
        let session = terminal["session_id"].as_str().unwrap();
        let response = client
            .get(format!("{base}/api/sessions/{session}/stream?language=en"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let mut body = tokio::time::timeout(Duration::from_secs(10), response.text())
            .await
            .expect("resumed partial stream must close")
            .unwrap();
        let mut resumed_partial = false;
        while let Some(event) = pop_sse_json(&mut body) {
            assert_ne!(event["type"], "completed");
            assert_ne!(event["type"], "error");
            resumed_partial |= event["type"] == "partial";
        }
        assert!(resumed_partial);
        assert_eq!(provider.requests(), 3, "resume must not generate again");
    });
    web.stop();
    guard.stop_daemon();
}

#[test]
fn web_text_delta_arrives_before_provider_is_allowed_to_finish() {
    struct ReleaseOnDrop(Arc<AtomicBool>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::StreamingUntilReleased);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let listen = free_loopback_address();
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let mut web = ChildGuard(Some(
        willdeep(&home)
            .args([
                "--web",
                "--listen",
                &listen,
                "--config",
                path_text(&config),
                "--workspace",
                path_text(&workspace),
            ])
            .spawn()
            .unwrap(),
    ));
    let _release = ReleaseOnDrop(provider.release_stream.clone());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = reqwest::Client::new();
        let base = format!("http://{listen}");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !client.get(format!("{base}/health")).send().await.is_ok_and(|response| response.status().is_success()) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await.unwrap();
        let response = client.post(format!("{base}/api/chat/stream")).json(&serde_json::json!({
            "prompt":"answer", "workspace":workspace.canonicalize().unwrap(), "language":"en", "attachments":[]
        })).send().await.unwrap();
        assert!(response.status().is_success());
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                while let Some(event) = pop_sse_json(&mut buffer) {
                    assert_ne!(event["type"], "completed");
                    if event["type"] == "assistant_text_delta" {
                        assert_eq!(event["text"], "live prefix");
                        return;
                    }
                }
                let chunk = stream.next().await.expect("stream stays open").unwrap();
                buffer.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        }).await.expect("text must arrive while generation is still pending");
        provider.release_stream.store(true, Ordering::SeqCst);
        let mut completed = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(chunk) = stream.next().await {
                buffer.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
                while let Some(event) = pop_sse_json(&mut buffer) {
                    if event["type"] == "completed" {
                        assert_eq!(event["text"], "live prefix");
                        completed = true;
                    }
                }
            }
        }).await.expect("finished generation closes the stream");
        assert!(completed);
    });
    web.stop();
    guard.stop_daemon();
}

#[test]
fn run_uses_persistent_runtime_and_continues_the_session() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let first = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "first runtime turn",
        ])
        .output()
        .expect("run first headless turn");
    assert_success(&first, "first headless turn");
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("parse first completion JSON");
    assert_eq!(first_json["type"], "completed");
    assert_eq!(first_json["text"], MOCK_REPLY);
    let session_id = first_json["session_id"]
        .as_str()
        .expect("completion session id");

    let second = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            session_id,
            "--output",
            "json",
            "second runtime turn",
        ])
        .output()
        .expect("continue headless Session");
    assert_success(&second, "continued headless turn");
    let second_json: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("parse second completion JSON");
    assert_eq!(second_json["session_id"], session_id);
    assert_eq!(second_json["text"], MOCK_REPLY);

    let turns = willdeep(&home)
        .args(["session", "turns", session_id])
        .output()
        .expect("list persistent Runtime turns");
    assert_success(&turns, "list Runtime turns");
    let turn_lines = String::from_utf8(turns.stdout)
        .expect("turn output is UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    assert_eq!(turn_lines, 2, "both turns must be durable Runtime records");
    assert_eq!(provider.requests(), 2, "each turn must reach the Provider");

    guard.stop_daemon();
}

#[test]
fn web_sse_disconnect_resumes_the_same_runtime_turn_without_resubmission() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_delayed(Duration::from_secs(5));
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let listen = free_loopback_address();
    let canonical_workspace = workspace.canonicalize().expect("canonical test Workspace");
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let web = willdeep(&home)
        .args([
            "--web",
            "--listen",
            &listen,
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
        ])
        .spawn()
        .expect("start isolated Web server");
    let mut web = ChildGuard(Some(web));
    let base = format!("http://{listen}");
    let runtime = tokio::runtime::Runtime::new().expect("create async test Runtime");

    runtime.block_on(async {
        let client = reqwest::Client::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client
                    .get(format!("{base}/health"))
                    .send()
                    .await
                    .is_ok_and(|response| response.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("Web server becomes healthy");

        let response = client
            .post(format!("{base}/api/chat/stream"))
            .header("accept", "text/event-stream")
            .json(&serde_json::json!({
                "prompt": "survive a browser refresh",
                "workspace": canonical_workspace,
                "language": "en",
                "attachments": []
            }))
            .send()
            .await
            .expect("submit Web Runtime Turn");
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            panic!("submit Web Runtime Turn failed with {status}: {body}");
        }
        let mut initial_stream = response.bytes_stream();
        let mut initial_buffer = String::new();
        let submitted = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(value) = pop_sse_json(&mut initial_buffer)
                    && value["type"] == "submitted"
                {
                    break value;
                }
                let chunk = initial_stream
                    .next()
                    .await
                    .expect("initial SSE remains open")
                    .expect("read initial SSE chunk");
                initial_buffer.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await
        .expect("receive submitted event");
        let session_id = submitted["session_id"]
            .as_str()
            .expect("submitted Session ID")
            .to_owned();
        let turn_id = submitted["turn_id"]
            .as_str()
            .expect("submitted Turn ID")
            .to_owned();
        let cursor = submitted["cursor"].as_u64().expect("submitted cursor");
        drop(initial_stream);

        // 这一步只在 Turn 还活着时成立，所以既要给每次请求单独设超时（一个卡住的
        // 请求不能吃掉整个等待预算），也要在 Turn 提前失败时立刻带着真实错误退出，
        // 而不是空转到超时报一个什么都没说的 Elapsed。
        let active_turn_id = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let sessions = client
                    .get(format!("{base}/api/sessions"))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status);
                let sessions = match sessions {
                    Ok(response) => response
                        .json::<Vec<serde_json::Value>>()
                        .await
                        .expect("decode Web Sessions"),
                    // 慢一次不算失败，下一轮再问；真出不来由外层超时兜底。
                    Err(_) => Vec::new(),
                };
                if let Some(active_turn_id) = sessions
                    .iter()
                    .find(|session| session["id"] == session_id)
                    .and_then(|session| session["active_turn_id"].as_str())
                {
                    break active_turn_id.to_owned();
                }
                // 只有确实读到终态才判失败；`unknown` 表示这一刻没读到状态本身，
                // 不能拿它当作 Turn 已经结束的证据。
                let status = runtime_turn_status(&home, &turn_id);
                assert!(
                    matches!(status.as_str(), "queued" | "running" | "unknown"),
                    "Turn must stay in flight until the Web client reattaches, but the Runtime reports {}",
                    runtime_turn_report(&home)
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Session never exposed its active Turn; Runtime reports {}",
                runtime_turn_report(&home)
            )
        });
        assert_eq!(active_turn_id, turn_id);

        let response = client
            .get(format!(
                "{base}/api/sessions/{session_id}/stream?after={cursor}&language=en"
            ))
            .header("accept", "text/event-stream")
            .send()
            .await
            .expect("resume Web Runtime Turn");
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            panic!("resume Web Runtime Turn failed with {status}: {body}");
        }
        let mut resumed_stream = response.bytes_stream();
        let mut resumed_buffer = String::new();
        let completed = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(value) = pop_sse_json(&mut resumed_buffer)
                    && value["type"] == "completed"
                {
                    break value;
                }
                let chunk = resumed_stream
                    .next()
                    .await
                    .expect("resumed SSE remains open")
                    .expect("read resumed SSE chunk");
                resumed_buffer.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await
        .expect("resumed stream reaches completion");
        assert_eq!(completed["session_id"], session_id);
        assert_eq!(completed["turn_id"], turn_id);
        assert_eq!(completed["text"], MOCK_REPLY);

        let detail = client
            .get(format!("{base}/api/sessions/{session_id}"))
            .send()
            .await
            .expect("load completed Web Session")
            .json::<serde_json::Value>()
            .await
            .expect("decode completed Web Session");
        let assistant_messages = detail["messages"]
            .as_array()
            .expect("Session messages")
            .iter()
            .filter(|message| message["role"] == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0]["content"], MOCK_REPLY);
    });

    assert_eq!(
        provider.requests(),
        1,
        "SSE recovery must attach to the existing Turn instead of submitting again"
    );
    web.stop();
    guard.stop_daemon();
}

#[test]
fn web_compress_command_is_served_by_the_harness_instead_of_the_provider() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config_with_language(&config, provider.api_base(), Some("en"));
    let listen = free_loopback_address();
    let canonical_workspace = workspace.canonicalize().expect("canonical test Workspace");
    let mut guard = TestGuard::new(root.clone(), home.clone());
    let web = willdeep(&home)
        .args([
            "--web",
            "--listen",
            &listen,
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
        ])
        .spawn()
        .expect("start isolated Web server");
    let mut web = ChildGuard(Some(web));
    let base = format!("http://{listen}");
    let runtime = tokio::runtime::Runtime::new().expect("create async test Runtime");

    runtime.block_on(async {
        let client = reqwest::Client::new();
        await_web_health(&client, &base).await;

        let first = web_chat_turn(&client, &base, &canonical_workspace, None, "first web turn").await;
        assert_eq!(first["text"], MOCK_REPLY);
        let session_id = first["session_id"]
            .as_str()
            .expect("completed Session ID")
            .to_owned();
        assert_eq!(
            provider.requests(),
            1,
            "an ordinary Web prompt must reach the Provider"
        );

        let compressed = web_chat_turn(
            &client,
            &base,
            &canonical_workspace,
            Some(&session_id),
            "/compress",
        )
        .await;
        assert_eq!(compressed["session_id"], session_id);
        let compressed_text = compressed["text"].as_str().expect("completed text");
        assert!(
            matches!(
                compressed_text,
                "Context compressed" | "Context is too short to compress"
            ),
            "Web /compress must be answered by the harness compression branch, got {compressed_text:?}"
        );
        assert_eq!(
            provider.requests(),
            1,
            "Web /compress must never reach the Provider as an ordinary prompt"
        );

        let detail = client
            .get(format!("{base}/api/sessions/{session_id}"))
            .send()
            .await
            .expect("load compressed Web Session")
            .json::<serde_json::Value>()
            .await
            .expect("decode compressed Web Session");
        let messages = detail["messages"].as_array().expect("Session messages");
        assert_eq!(
            messages.len(),
            2,
            "Web /compress must not append a Turn to the Session history"
        );
        assert!(
            messages
                .iter()
                .all(|message| message["content"] != "/compress"),
            "Web /compress must never be persisted as a user message"
        );
        assert!(
            messages
                .iter()
                .all(|message| message["content"] != compressed_text),
            "the compression confirmation is a Harness reply, not Session history"
        );
    });

    web.stop();
    guard.stop_daemon();
}

async fn await_web_health(client: &reqwest::Client, base: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .get(format!("{base}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("Web server becomes healthy");
}

async fn web_chat_turn(
    client: &reqwest::Client,
    base: &str,
    workspace: &Path,
    session_id: Option<&str>,
    prompt: &str,
) -> serde_json::Value {
    let response = client
        .post(format!("{base}/api/chat/stream"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "prompt": prompt,
            "session_id": session_id,
            "workspace": workspace,
            "language": "en",
            "attachments": []
        }))
        .send()
        .await
        .expect("submit Web chat Turn");
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        panic!("Web chat Turn '{prompt}' failed with {status}: {body}");
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(value) = pop_sse_json(&mut buffer) {
                if value["type"] == "error" {
                    panic!("Web chat Turn '{prompt}' reported {}", value["message"]);
                }
                if value["type"] == "completed" {
                    break value;
                }
                continue;
            }
            let chunk = stream
                .next()
                .await
                .expect("Web chat SSE remains open")
                .expect("read Web chat SSE chunk");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Web chat Turn '{prompt}' reaches completion"))
}

#[test]
fn runtime_provider_failure_preserves_the_documented_exit_code() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_with_status(503);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());

    let output = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "provider failure",
        ])
        .output()
        .expect("run failing Provider turn");

    assert_eq!(
        output.status.code(),
        Some(3),
        "Provider failures returned through Runtime must keep exit code 3; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 503 属于该重发的那一类，所以这里看到的是重试预算跑满（willdeep-core 的
    // MAX_ATTEMPTS）而不是一次就走。重试改变的只是敲门次数——一直不成，
    // 对外仍旧是同一个退出码 3。
    assert_eq!(provider.requests(), 3);
}

#[test]
fn public_api_spawns_and_waits_for_a_read_only_child_agent() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_with_mode(MockMode::WaitRootThenRetryChild);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut session = willdeep_core::Session::new(workspace.clone(), None, "external spawn");
    session.config = Some(config.clone());
    willdeep_core::SessionStore::new(&home)
        .save(&mut session)
        .expect("persist Core Session fixture");
    let _guard = TestGuard::new(root.clone(), home.clone());

    let root_turn = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            &session.id.to_string(),
            "wait for user input",
        ])
        .output()
        .expect("start waiting root Agent");
    assert_eq!(
        root_turn.status.code(),
        Some(4),
        "ask_user must leave the Runtime Turn waiting; stderr:\n{}",
        String::from_utf8_lossy(&root_turn.stderr)
    );

    let editor_params = root.join("editor-spawn.json");
    write_json(
        &editor_params,
        serde_json::json!({
            "session_id": session.id,
            "prompt": "edit a file",
            "profile": "editor"
        }),
    );
    let editor = willdeep(&home)
        .args([
            "api",
            "agent.spawn",
            "--params-file",
            path_text(&editor_params),
        ])
        .output()
        .expect("reject external editor spawn");
    assert!(!editor.status.success());
    let editor_envelope: serde_json::Value =
        serde_json::from_slice(&editor.stdout).expect("parse editor rejection envelope");
    assert_eq!(editor_envelope["error"]["code"], "invalid_request");

    let spawn_params = root.join("scout-spawn.json");
    write_json(
        &spawn_params,
        serde_json::json!({
            "session_id": session.id,
            "prompt": "inspect the repository structure",
            "profile": "scout",
            "label": "external scout"
        }),
    );
    let spawn = willdeep(&home)
        .args([
            "api",
            "agent.spawn",
            "--params-file",
            path_text(&spawn_params),
        ])
        .output()
        .expect("spawn external scout");
    assert_success(&spawn, "spawn external scout");
    let spawn_envelope: serde_json::Value =
        serde_json::from_slice(&spawn.stdout).expect("parse spawn envelope");
    assert_eq!(spawn_envelope["data"]["status"], "queued");
    assert_eq!(spawn_envelope["data"]["profile"], "scout");
    let child_id = spawn_envelope["data"]["id"]
        .as_str()
        .expect("spawned Agent ID");

    let async_runtime = tokio::runtime::Runtime::new().unwrap();
    let client = runtime_client(&home);
    let child_uuid = child_id.parse::<uuid::Uuid>().unwrap();
    wait_until(Duration::from_secs(10), || {
        let agents = async_runtime.block_on(client.agents()).unwrap();
        let willdeep_runtime_protocol::ApiResponse::Ok { data, .. } = agents else {
            panic!("agent.list failed");
        };
        assert!(
            data.iter()
                .filter(|agent| agent.parent_id.is_none())
                .all(|agent| agent.retry_wait.is_none()),
            "child retry must not mark the root as waiting"
        );
        data.iter()
            .find(|agent| agent.id == child_uuid)
            .is_some_and(|agent| {
                agent.retry_wait.as_ref().is_some_and(|wait| {
                    assert_eq!(
                        agent.status,
                        willdeep_runtime_protocol::AgentStatus::Running
                    );
                    assert_eq!(wait.attempt, 1);
                    assert_eq!(wait.delay_ms, 2000);
                    true
                })
            })
    });
    wait_until(Duration::from_secs(10), || {
        if provider.requests() != 3 {
            return false;
        }
        let willdeep_runtime_protocol::ApiResponse::Ok { data, .. } =
            async_runtime.block_on(client.agent(child_uuid)).unwrap()
        else {
            panic!("agent.get failed");
        };
        data.retry_wait.is_none() && data.status == willdeep_runtime_protocol::AgentStatus::Running
    });
    provider.release_stream.store(true, Ordering::SeqCst);

    let wait_params = root.join("agent-wait.json");
    write_json(
        &wait_params,
        serde_json::json!({"id": child_id, "timeout_ms": 10_000}),
    );
    let wait = willdeep(&home)
        .args([
            "api",
            "agent.wait",
            "--params-file",
            path_text(&wait_params),
        ])
        .output()
        .expect("wait for external scout");
    assert_success(&wait, "wait for external scout");
    let wait_envelope: serde_json::Value =
        serde_json::from_slice(&wait.stdout).expect("parse Agent wait envelope");
    assert_eq!(wait_envelope["data"]["id"], child_id);
    assert_eq!(wait_envelope["data"]["status"], "completed");
    assert!(wait_envelope["data"]["retry_wait"].is_null());
    assert_eq!(wait_envelope["data"]["label"], "external scout");
    let persisted = willdeep_core::SessionStore::new(home.join("workers"))
        .load(child_id.parse().expect("child UUID"))
        .expect("persisted Worker history");
    assert_eq!(
        persisted.execution_checkpoint.unwrap().status,
        willdeep_core::checkpoint::CheckpointStatus::Completed
    );
    assert!(
        persisted
            .messages
            .iter()
            .any(|message| message.role == willdeep_core::Role::Assistant)
    );
    assert_eq!(
        provider.requests(),
        3,
        "root reaches Provider once; child retries its one 429 exactly once"
    );
}

#[test]
fn root_retry_wait_is_observable_through_public_runtime_api() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::RetryRoot);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let child = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "retry root request",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = ChildGuard(Some(child));
    wait_until(Duration::from_secs(10), || provider.requests() > 0);
    let client = runtime_client(&home);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut root_id = None;
    wait_until(Duration::from_secs(10), || {
        let willdeep_runtime_protocol::ApiResponse::Ok { data, .. } =
            runtime.block_on(client.agents()).unwrap()
        else {
            panic!("agent.list failed");
        };
        let Some(agent) = data.iter().find(|agent| agent.parent_id.is_none()) else {
            return false;
        };
        root_id = Some(agent.id);
        agent
            .retry_wait
            .as_ref()
            .is_some_and(|wait| wait.attempt == 1 && wait.delay_ms == 2000)
    });
    let root_id = root_id.unwrap();
    wait_until(Duration::from_secs(10), || {
        if provider.requests() != 2 {
            return false;
        }
        let willdeep_runtime_protocol::ApiResponse::Ok { data, .. } =
            runtime.block_on(client.agent(root_id)).unwrap()
        else {
            panic!("agent.get failed");
        };
        data.retry_wait.is_none() && data.status == willdeep_runtime_protocol::AgentStatus::Running
    });
    provider.release_stream.store(true, Ordering::SeqCst);
    wait_until(Duration::from_secs(10), || {
        process.0.as_mut().unwrap().try_wait().unwrap().is_some()
    });
    let result = process.0.take().unwrap().wait_with_output().unwrap();
    assert_success(&result, "root Provider retry");
    let willdeep_runtime_protocol::ApiResponse::Ok { data, .. } =
        runtime.block_on(client.agent(root_id)).unwrap()
    else {
        panic!("final agent.get failed");
    };
    assert_eq!(
        data.status,
        willdeep_runtime_protocol::AgentStatus::Completed
    );
    assert!(data.retry_wait.is_none());
    assert_eq!(provider.requests(), 2);
}

#[test]
fn local_ndjson_exposes_completed_foreground_child_for_evaluation() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::DelegateThenSuccess);
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
            "ndjson",
            "Delegate a read-only diagnosis then report the result",
        ])
        .output()
        .unwrap();
    assert_success(&output, "local delegated evaluation output");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let (index, started) = events
        .iter()
        .enumerate()
        .find(|(_, event)| event["type"] == "subagent_started")
        .expect("real child start event");
    assert_eq!(started["profile"], "scout");
    assert_eq!(started["background"], false);
    let terminal = events[index + 1..]
        .iter()
        .find(|event| event["type"] == "subagent_completed" && event["id"] == started["id"])
        .expect("same child completion event");
    assert_eq!(terminal["status"], "completed");
    let final_event = events.last().unwrap();
    assert_eq!(final_event["type"], "completed");
    assert!(final_event["session_id"].as_str().is_some());
    assert_eq!(
        provider.requests(),
        3,
        "root delegation, child response and parent integration"
    );
    let resumed = willdeep(&home)
        .args([
            "run",
            "--local",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            final_event["session_id"].as_str().unwrap(),
            "--output",
            "json",
            "continue the existing local session",
        ])
        .output()
        .unwrap();
    assert_success(&resumed, "resume local evaluation session");
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["type"], "completed");
    assert_eq!(resumed["session_id"], final_event["session_id"]);
    assert_eq!(provider.requests(), 4);
}

#[test]
fn local_compression_bridge_preserves_seeded_constraint_then_resumes() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let id = uuid::Uuid::new_v4();
    let constraint = "Fix sum_to. Change only src/lib.rs; preserve all other files and run tests.";
    let mut messages = vec![serde_json::json!({"role":"user", "content":constraint})];
    for index in 0..10 {
        messages.push(serde_json::json!({"role":"assistant", "content":format!("Historical inspection {index}; no changes made.")}));
    }
    // Same minimal serialized fixture shape used by the Ruby evaluator.
    write_json(
        &home.join("sessions").join(format!("{id}.json")),
        serde_json::json!({"version":1,"id":id,"title":"Compression evaluation","workspace":workspace,"profile":null,"created_at":1,"updated_at":1,"messages":messages}),
    );
    let mut command = willdeep(&home)
        .args([
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--resume",
            &id.to_string(),
            "--no-tui",
            "--json",
            "--web-input-json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    command
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"prompt":"/compress","attachments":[]}"#)
        .unwrap();
    let output = command.wait_with_output().unwrap();
    assert_success(&output, "explicit evaluation compression");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "compression_completed")
    );
    let store = willdeep_core::SessionStore::new(&home);
    let compressed = store.load(id).unwrap();
    assert_eq!(compressed.compression_generation, 1);
    assert_eq!(compressed.manual_compression_usage.reported_calls, 1);
    assert_eq!(compressed.manual_compression_usage.input_tokens, 5);
    assert_eq!(compressed.manual_compression_usage.output_tokens, 3);
    assert!(compressed.messages.len() < 11);
    assert!(
        compressed
            .messages
            .iter()
            .any(|message| message.role == willdeep_core::Role::User
                && message.content == constraint)
    );
    let resumed = willdeep(&home)
        .args([
            "run",
            "--local",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            &id.to_string(),
            "--output",
            "json",
            "Continue the original task under its original constraints",
        ])
        .output()
        .unwrap();
    assert_success(&resumed, "resume compressed evaluation");
    let reloaded_usage = store.load(id).unwrap().manual_compression_usage;
    assert_eq!(reloaded_usage.reported_calls, 1);
    assert_eq!(
        (reloaded_usage.input_tokens, reloaded_usage.output_tokens),
        (5, 3)
    );
    assert!(
        store
            .load(id)
            .unwrap()
            .messages
            .iter()
            .any(|message| message.role == willdeep_core::Role::User
                && message.content == constraint)
    );
    assert_eq!(
        provider.requests(),
        2,
        "one actual summary and one resumed request"
    );
}

/// 中断控制器是 `scripts/lib/agent_eval_process.rb`：进程组、fd 3/4 私有管道、
/// 按组 KILL 都只有 POSIX 有，评测脚本本身也只在 Linux 上跑。
#[cfg(unix)]
#[test]
fn local_interrupted_write_resumes_without_replay() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::CheckpointThenWait);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let controller = root.join("interrupt.rb");
    write_private_text(
        &controller,
        r#"
require 'json'
require File.join(ARGV.shift, 'scripts/lib/agent_eval_process')
require File.expand_path('agent_eval_recovery', File.dirname($LOADED_FEATURES.find { |p| p.end_with?('/agent_eval_process.rb') }))
home, workspace, config, binary = ARGV
observed = nil
observer = lambda { observed = AgentEvalRecovery.boundary(home, workspace); !observed.nil? }
env = {'WILLDEEP_HOME' => home}
%w[WILLDEEP_API_BASE WILLDEEP_API_KEY WILLDEEP_CONFIG WILLDEEP_LANGUAGE WILLDEEP_MODEL].each { |key| env[key] = nil }
command = [binary, 'run', '--local', '--config', config, '--workspace', workspace, '--full-auto', '--output', 'json', 'Append the marker exactly once.']
code, timeout, elapsed, injected = AgentEvalProcess.run(command, env, workspace, 20, home, interrupt_when: observer)
post = observed && AgentEvalRecovery.boundary(home, workspace, id: observed[:session_id])
puts JSON.generate({code: code, timeout: timeout, injected: injected, observed: observed, boundary_preserved: post == observed && !post.nil?})
"#,
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("ruby")
        .args([
            path_text(&controller),
            path_text(&repository),
            path_text(&home),
            path_text(&workspace),
            path_text(&config),
            env!("CARGO_BIN_EXE_willdeep"),
        ])
        .output()
        .unwrap();
    assert_success(&output, "interrupt controller");
    let evidence: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(evidence["injected"], true, "{evidence}");
    assert_eq!(evidence["timeout"], false, "{evidence}");
    assert_eq!(evidence["boundary_preserved"], true, "{evidence}");
    assert!(evidence["code"].is_null());
    let id = evidence["observed"]["session_id"].as_str().unwrap();
    provider.release_stream.store(true, Ordering::SeqCst);
    let resumed = willdeep(&home)
        .args([
            "run",
            "--local",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            id,
            "--output",
            "json",
            "Continue without repeating the completed marker write",
        ])
        .output()
        .unwrap();
    assert_success(&resumed, "resume interrupted marker write");
    let result: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(result["session_id"], id);
    assert_eq!(
        std::fs::read_to_string(workspace.join("progress.log")).unwrap(),
        "checkpoint-once\n"
    );
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("sessions").join(format!("{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["execution_checkpoint"]["status"], "completed");
    let marker_calls = saved["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .filter(|call| call["id"] == "marker-once")
        .count();
    assert_eq!(marker_calls, 1);
    assert!(
        (3..=4).contains(&provider.requests()),
        "initial request, local safety judge and resume; interruption may precede the next HTTP request"
    );
}

#[test]
fn public_retry_restores_child_after_daemon_and_parent_harness_restart() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::WaitRootsForRecoveredChild);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut session = willdeep_core::Session::new(workspace.clone(), None, "restore child");
    session.config = Some(config.clone());
    willdeep_core::SessionStore::new(&home)
        .save(&mut session)
        .unwrap();
    let _guard = TestGuard::new(root.clone(), home.clone());
    let root_turn = || {
        willdeep(&home)
            .args([
                "run",
                "--config",
                path_text(&config),
                "--workspace",
                path_text(&workspace),
                "--session",
                &session.id.to_string(),
                "wait for user input",
            ])
            .output()
            .unwrap()
    };
    assert_eq!(root_turn().status.code(), Some(4));
    let params = root.join("spawn.json");
    write_json(
        &params,
        serde_json::json!({"session_id":session.id,"prompt":"inspect without edits","profile":"scout"}),
    );
    let spawn = willdeep(&home)
        .args(["api", "agent.spawn", "--params-file", path_text(&params)])
        .output()
        .unwrap();
    assert_success(&spawn, "spawn persisted child");
    let envelope: serde_json::Value = serde_json::from_slice(&spawn.stdout).unwrap();
    let id = envelope["data"]["id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = runtime_client(&home);
    wait_until(
        Duration::from_secs(10),
        || matches!(rt.block_on(client.agent(id)).unwrap(), willdeep_runtime_protocol::ApiResponse::Ok { data, .. } if data.status == willdeep_runtime_protocol::AgentStatus::Completed),
    );
    let willdeep_runtime_protocol::ApiResponse::Ok { data: old, .. } =
        rt.block_on(client.agent(id)).unwrap()
    else {
        panic!("missing child")
    };
    assert_eq!(provider.requests(), 2);
    assert_success(
        &willdeep(&home).args(["daemon", "stop"]).output().unwrap(),
        "stop original Runtime",
    );
    assert_eq!(root_turn().status.code(), Some(4));
    let client = runtime_client(&home);
    let response = rt
        .block_on(client.retry_agent(id, uuid::Uuid::new_v4()))
        .unwrap();
    assert!(
        matches!(response, willdeep_runtime_protocol::ApiResponse::Ok { .. }),
        "{response:?}"
    );
    wait_until(
        Duration::from_secs(10),
        || matches!(rt.block_on(client.agent(id)).unwrap(), willdeep_runtime_protocol::ApiResponse::Ok { data, .. } if data.task_id != old.task_id && data.parent_id == old.parent_id && data.status == willdeep_runtime_protocol::AgentStatus::Completed),
    );
    assert_eq!(
        provider.requests(),
        4,
        "two parent turns and two executions of the same restored child"
    );
    let worker = willdeep_core::SessionStore::new(home.join("workers"))
        .load(id)
        .unwrap();
    assert_eq!(
        worker
            .messages
            .iter()
            .filter(|message| message.role == willdeep_core::Role::Assistant
                && message.content == MOCK_REPLY)
            .count(),
        2
    );
}

#[test]
fn daemon_upgrade_drains_active_work_and_hands_off_without_loss() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_delayed(Duration::from_secs(3));
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());

    let active = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "active during handoff",
        ])
        .spawn()
        .expect("start active Runtime turn");
    wait_until(Duration::from_secs(10), || provider.requests() == 1);

    let async_runtime = tokio::runtime::Runtime::new().expect("create async test Runtime");
    let mut event_stream = async_runtime.block_on(open_idle_event_stream(&home));

    let before = daemon_status(&home);
    let old_pid = status_value(&before, "pid");
    let upgrade = willdeep(&home)
        .args(["daemon", "upgrade", "--force", "--timeout", "15"])
        .spawn()
        .expect("start Runtime handoff");
    wait_until(Duration::from_secs(5), || {
        daemon_status(&home).starts_with("draining\t")
    });

    let rejected = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "must wait for replacement runtime",
        ])
        .output()
        .expect("submit while draining");
    assert!(
        !rejected.status.success(),
        "draining Runtime must reject new work"
    );
    assert_eq!(
        provider.requests(),
        1,
        "rejected work must not reach Provider"
    );

    let active = active
        .wait_with_output()
        .expect("wait for active Runtime turn");
    assert_success(&active, "active turn during Runtime handoff");
    let upgrade = upgrade
        .wait_with_output()
        .expect("wait for Runtime handoff");
    assert_success(&upgrade, "Runtime handoff");
    async_runtime.block_on(assert_event_stream_closed(&mut event_stream));
    let after = daemon_status(&home);
    assert!(after.starts_with("running\t"));
    assert_ne!(status_value(&after, "pid"), old_pid);

    let replacement = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "replacement runtime turn",
        ])
        .output()
        .expect("run turn on replacement Runtime");
    assert_success(&replacement, "replacement Runtime turn");
    assert_eq!(provider.requests(), 2);
}

#[test]
fn daemon_restart_recovers_persisted_execution_resources_exactly_once() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let runtime_home = home.join("runtime");
    let workspace = root.join("workspace");
    let child_worktree = root.join("child-worktree");
    std::fs::create_dir_all(&runtime_home).expect("create test Runtime home");
    std::fs::create_dir_all(&workspace).expect("create test Workspace");
    std::fs::create_dir_all(&child_worktree).expect("create child Worktree");
    let task_id = uuid::Uuid::new_v4();
    let root_agent_id = uuid::Uuid::new_v4();
    let child_agent_id = uuid::Uuid::new_v4();
    let spawn_agent_id = uuid::Uuid::new_v4();
    let stop_command_id = uuid::Uuid::new_v4();
    let spawn_command_id = uuid::Uuid::new_v4();
    let tool_id = uuid::Uuid::new_v4();
    let background_tool_id = uuid::Uuid::new_v4();
    let private_prompt = "private pending spawn prompt";

    write_json(
        &runtime_home.join("agents.json"),
        serde_json::json!([
            persisted_agent(
                root_agent_id,
                None,
                task_id,
                &workspace,
                "completed",
                false,
                None
            ),
            persisted_agent(
                child_agent_id,
                Some(root_agent_id),
                task_id,
                &child_worktree,
                "running",
                true,
                Some("willdeep/restart-recovery")
            ),
            persisted_agent(
                spawn_agent_id,
                Some(root_agent_id),
                task_id,
                &workspace,
                "queued",
                false,
                None
            )
        ]),
    );
    write_json(
        &runtime_home.join("agent-commands.json"),
        serde_json::json!([
            {
                "id": stop_command_id,
                "task_id": task_id,
                "agent_id": child_agent_id,
                "kind": "stop",
                "status": "pending",
                "created_at": 2,
                "resolved_at": null,
                "error": null,
                "message": null,
                "profile": null,
                "label": null,
                "model": null
            },
            {
                "id": spawn_command_id,
                "task_id": task_id,
                "agent_id": spawn_agent_id,
                "kind": "spawn",
                "status": "pending",
                "created_at": 3,
                "resolved_at": null,
                "error": null,
                "message": private_prompt,
                "profile": "scout",
                "label": "private label",
                "model": "private model"
            }
        ]),
    );
    write_json(
        &runtime_home.join("tools.json"),
        serde_json::json!([
            {
                "id": tool_id,
                "session_id": null,
                "turn_id": null,
                "task_id": task_id,
                "agent_id": child_agent_id,
                "name": "edit_file",
                "status": "running",
                "started_at_ms": 1000,
                "completed_at_ms": null
            },
            {
                "id": background_tool_id,
                "session_id": null,
                "turn_id": null,
                "task_id": task_id,
                "agent_id": root_agent_id,
                "name": "background_shell:job_restart",
                "status": "running",
                "started_at_ms": 1001,
                "completed_at_ms": null
            }
        ]),
    );
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let started = willdeep(&home)
        .args(["daemon", "start"])
        .output()
        .expect("start Runtime from interrupted resource snapshot");
    assert_success(&started, "start Runtime from interrupted resource snapshot");
    wait_until(Duration::from_secs(10), || {
        daemon_status(&home).starts_with("running\t")
    });

    let agents = read_json_array(&runtime_home.join("agents.json"));
    let child = object_with_id(&agents, child_agent_id);
    assert_eq!(child["status"], "interrupted");
    assert_eq!(
        child["workspace"],
        child_worktree.to_string_lossy().as_ref()
    );
    assert_eq!(child["worktree_branch"], "willdeep/restart-recovery");
    assert_eq!(child["dedicated_worktree"], true);
    let spawn = object_with_id(&agents, spawn_agent_id);
    assert_eq!(spawn["status"], "failed");
    assert!(
        spawn["error"]
            .as_str()
            .is_some_and(|error| error.contains("before external Agent spawn was applied"))
    );

    let commands = read_json_array(&runtime_home.join("agent-commands.json"));
    assert!(
        commands
            .iter()
            .all(|command| command["status"] == "rejected")
    );
    assert!(commands.iter().all(|command| {
        command["message"].is_null()
            && command["profile"].is_null()
            && command["label"].is_null()
            && command["model"].is_null()
    }));
    let tools = read_json_array(&runtime_home.join("tools.json"));
    assert_eq!(object_with_id(&tools, tool_id)["status"], "interrupted");
    assert_eq!(
        object_with_id(&tools, background_tool_id)["status"],
        "interrupted"
    );

    let first_events = read_ndjson_values(&runtime_home.join("events.ndjson"));
    assert_eq!(event_kind_count(&first_events, "agent.command_rejected"), 2);
    assert_eq!(event_kind_count(&first_events, "agent.spawn_rejected"), 1);
    assert_eq!(event_kind_count(&first_events, "agent.interrupted"), 1);
    assert_eq!(event_kind_count(&first_events, "tool.interrupted"), 2);
    let event_log = std::fs::read_to_string(runtime_home.join("events.ndjson"))
        .expect("read recovered Runtime events");
    assert!(!event_log.contains(private_prompt));
    assert!(!event_log.contains(child_worktree.to_string_lossy().as_ref()));

    guard.stop_daemon();
    let restarted = willdeep(&home)
        .args(["daemon", "start"])
        .output()
        .expect("restart recovered Runtime");
    assert_success(&restarted, "restart recovered Runtime");
    guard.daemon_stopped = false;
    wait_until(Duration::from_secs(10), || {
        daemon_status(&home).starts_with("running\t")
    });
    let second_events = read_ndjson_values(&runtime_home.join("events.ndjson"));
    assert_eq!(
        second_events.len(),
        first_events.len() + 2,
        "restart adds only daemon.stopped and daemon.started lifecycle events"
    );
    for kind in [
        "agent.command_rejected",
        "agent.spawn_rejected",
        "agent.interrupted",
        "tool.interrupted",
    ] {
        assert_eq!(
            event_kind_count(&second_events, kind),
            event_kind_count(&first_events, kind),
            "a second Runtime process must not repeat {kind} recovery events"
        );
    }
    guard.stop_daemon();
}

fn persisted_agent(
    id: uuid::Uuid,
    parent_id: Option<uuid::Uuid>,
    task_id: uuid::Uuid,
    workspace: &Path,
    status: &str,
    dedicated_worktree: bool,
    worktree_branch: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "parent_id": parent_id,
        "task_id": task_id,
        "label": "test Agent",
        "background": parent_id.is_some(),
        "workspace": workspace,
        "root_workspace": null,
        "worktree_branch": worktree_branch,
        "dedicated_worktree": dedicated_worktree,
        "worktree_merged_review_id": null,
        "worktree_merged_child_snapshot_id": null,
        "worktree_merged_at": null,
        "worktree_quarantined_at": null,
        "profile": null,
        "model": null,
        "status": status,
        "current_turn": 1,
        "current_tool": null,
        "input_tokens": null,
        "output_tokens": null,
        "total_tokens": null,
        "max_turns": null,
        "token_budget": null,
        "timeout_seconds": null,
        "report": null,
        "created_at": 1,
        "updated_at": 1,
        "completed_at": null,
        "error": null
    })
}

fn read_json_array(path: &Path) -> Vec<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).expect("read Runtime JSON array"))
        .expect("parse Runtime JSON array")
}

fn read_ndjson_values(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read Runtime NDJSON")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse Runtime NDJSON line"))
        .collect()
}

fn object_with_id(values: &[serde_json::Value], id: uuid::Uuid) -> &serde_json::Value {
    values
        .iter()
        .find(|value| value["id"] == id.to_string())
        .unwrap_or_else(|| panic!("missing Runtime object {id}"))
}

fn event_kind_count(events: &[serde_json::Value], kind: &str) -> usize {
    events.iter().filter(|event| event["kind"] == kind).count()
}

async fn open_idle_event_stream(home: &Path) -> willdeep_runtime_client::NdjsonEventStream {
    let client = runtime_client(home);
    let mut stream = client
        .stream_events(0, 1_000, None)
        .await
        .expect("open Runtime NDJSON event stream");

    loop {
        match tokio::time::timeout(
            Duration::from_millis(100),
            stream.next::<willdeep_runtime_protocol::RuntimeEvent>(),
        )
        .await
        {
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) => panic!("Runtime event stream ended before handoff"),
            Ok(Err(error)) => panic!("read Runtime event stream before handoff: {error}"),
            Err(_) => return stream,
        }
    }
}

fn runtime_client(home: &Path) -> willdeep_runtime_client::RuntimeClient {
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("runtime/daemon.json")).expect("read Runtime state"),
    )
    .expect("parse Runtime state");
    let address = state["address"].as_str().expect("Runtime state address");
    let token = state["token"].as_str().expect("Runtime state token");
    willdeep_runtime_client::RuntimeClient::new(format!("http://{address}"), token.to_owned())
        .expect("create Runtime Client")
}

async fn assert_event_stream_closed(stream: &mut willdeep_runtime_client::NdjsonEventStream) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match stream
                .next::<willdeep_runtime_protocol::RuntimeEvent>()
                .await
                .expect("read Runtime event stream during handoff")
            {
                Some(_) => {}
                None => return,
            }
        }
    })
    .await
    .expect("old Runtime must close its NDJSON event stream during handoff");
}

fn willdeep(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_willdeep"));
    command
        .env("WILLDEEP_HOME", home)
        .env_remove("WILLDEEP_API_BASE")
        .env_remove("WILLDEEP_API_KEY")
        .env_remove("WILLDEEP_CONFIG")
        .env_remove("WILLDEEP_LANGUAGE")
        .env_remove("WILLDEEP_MODEL");
    command
}

fn process_test_guard() -> MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 同一条会话第二次 ensure，参数变了也必须能跑。
///
/// `ensure_runtime_session` 曾经拿**会话 ID 当幂等键**，而幂等记录存的是整个
/// params 的指纹。于是任何一个会变的字段——Provider、模型、标题——改一次，
/// 下一次 ensure 就撞 `request_id was already used with different operation
/// params`；记录是持久化的，撞上之后这条会话**永久**发不出 ensure，TUI 和 Web
/// 一起哑掉。Web 侧两个调用点传的 profile 本来就不一样，一直在踩。
#[test]
fn re_ensuring_a_session_with_changed_params_is_not_an_idempotency_conflict() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let first = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "first turn without an explicit profile",
        ])
        .output()
        .expect("run first headless turn");
    assert_success(&first, "first headless turn");
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("parse first completion JSON");
    let session_id = first_json["session_id"]
        .as_str()
        .expect("completion session id")
        .to_owned();

    // 第二轮显式带上 --profile：ensure 的 params 与第一轮不同。
    let second = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            &session_id,
            "--profile",
            "mock",
            "--output",
            "json",
            "second turn with an explicit profile",
        ])
        .output()
        .expect("continue headless Session");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        !stderr.contains("request_id was already used"),
        "changed ensure params must not collide with the previous request id: {stderr}"
    );
    assert_success(&second, "continued headless turn with a changed profile");
    let second_json: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("parse second completion JSON");
    assert_eq!(second_json["session_id"], session_id);

    guard.stop_daemon();
}

/// 体验基线第 11 项的事件级用例：两轮跑完后「回到第 1 步」——对话截到第 1 步的
/// 末尾，文件按第 2 步开始前拍的检查点恢复，被盖掉的原件进回收区，用户仓库的
/// git 状态一根毫毛不动。
#[test]
fn rewind_restores_conversation_and_files_to_an_earlier_turn() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(workspace.join("src")).expect("create test workspace");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    git(&["init", "--quiet", "-b", "main"]);
    std::fs::write(workspace.join("src/lib.rs"), "v1\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "init"]);
    let head = git(&["rev-parse", "HEAD"]);

    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let run_turn = |session: Option<&str>, prompt: &str| {
        let mut args = vec![
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
        ];
        if let Some(session) = session {
            args.extend(["--session", session]);
        }
        args.push(prompt);
        let output = willdeep(&home)
            .args(&args)
            .output()
            .expect("run headless turn");
        assert_success(&output, prompt);
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse completion JSON");
        json["session_id"]
            .as_str()
            .expect("completion session id")
            .to_owned()
    };

    let session_id = run_turn(None, "first step");
    // 第 1 步「做」的事：改了一个文件、新建了一个。第 2 步开始前的检查点要把这个状态拍下来。
    std::fs::write(workspace.join("src/lib.rs"), "v2\n").unwrap();
    std::fs::write(workspace.join("scratch.txt"), "after step one\n").unwrap();
    assert_eq!(run_turn(Some(&session_id), "second step"), session_id);
    // 第 2 步「做」的事：再改、再新建。回退要把这些全撤掉。
    std::fs::write(workspace.join("src/lib.rs"), "v3\n").unwrap();
    std::fs::write(workspace.join("scratch.txt"), "after step two\n").unwrap();
    std::fs::write(workspace.join("later.txt"), "only in step two\n").unwrap();

    let session = uuid::Uuid::parse_str(&session_id).expect("session uuid");
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let client = runtime_client(&home);
    let mut turns = runtime
        .block_on(client.turns(session))
        .expect("list turns")
        .into_result()
        .expect("turn list");
    turns.sort_by_key(|turn| turn.queue_sequence);
    assert_eq!(turns.len(), 2);
    assert!(
        turns
            .iter()
            .all(|turn| turn.status == willdeep_runtime_protocol::TurnStatus::Completed)
    );
    let first_end = turns[0].message_end.expect("first turn boundary");
    assert!(
        turns[1].workspace_checkpoint.is_some(),
        "the second turn must have captured a workspace checkpoint: {:?}",
        turns[1]
    );
    assert_eq!(
        git(&["for-each-ref", "refs/willdeep"]),
        "",
        "checkpoints never touch the user's own repository"
    );

    let rewind = willdeep(&home)
        .args([
            "daemon",
            "rewind-session",
            &session_id,
            "--through-turn",
            &turns[0].id.to_string(),
            "--restore-workspace",
        ])
        .output()
        .expect("rewind session");
    assert_success(&rewind, "rewind to the first turn");
    let stdout = String::from_utf8_lossy(&rewind.stdout);
    assert!(
        stdout.contains(&format!("rewound\tmessages={first_end}\tdropped_turns=1")),
        "{stdout}"
    );
    assert!(stdout.contains("restored\tsrc/lib.rs"), "{stdout}");
    assert!(stdout.contains("restored\tscratch.txt"), "{stdout}");
    assert!(stdout.contains("removed\tlater.txt"), "{stdout}");

    // 文件回到第 2 步开始前；第 2 步的产物在回收区，一个没丢。
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/lib.rs")).unwrap(),
        "v2\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("scratch.txt")).unwrap(),
        "after step one\n"
    );
    assert!(!workspace.join("later.txt").exists());
    let recovery = stdout
        .lines()
        .find_map(|line| line.strip_prefix("workspace\t"))
        .and_then(|line| {
            line.split('\t')
                .find_map(|field| field.strip_prefix("recovery="))
        })
        .expect("recovery path in the rewind report");
    let recovery = PathBuf::from(recovery);
    assert!(
        recovery.starts_with(home.join("runtime/recovery")),
        "{}",
        recovery.display()
    );
    assert_eq!(
        std::fs::read_to_string(recovery.join("src/lib.rs")).unwrap(),
        "v3\n"
    );
    assert_eq!(
        std::fs::read_to_string(recovery.join("later.txt")).unwrap(),
        "only in step two\n"
    );
    assert_eq!(
        git(&["rev-parse", "HEAD"]),
        head,
        "HEAD is not moved by a rewind"
    );

    // 对话截到第 1 步末尾，第 2 步的轮次记录没了，会话不再持有执行检查点。
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("sessions").join(format!("{session_id}.json")))
            .expect("read stored Session"),
    )
    .expect("parse stored Session");
    assert_eq!(stored["messages"].as_array().map(Vec::len), Some(first_end));
    assert!(stored["execution_checkpoint"].is_null());
    let remaining = runtime
        .block_on(client.turns(session))
        .expect("list turns after rewind")
        .into_result()
        .expect("turn list after rewind");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, turns[0].id);

    // 回退之后会话照常能续：第三轮从第 1 步末尾接着跑。
    assert_eq!(
        run_turn(Some(&session_id), "third step after rewind"),
        session_id
    );
    let events = read_ndjson_values(&home.join("runtime/events.ndjson"));
    assert_eq!(event_kind_count(&events, "session.rewound"), 1);

    guard.stop_daemon();
}

/// 会话标题走两级：提交那一刻确定性派生，第一轮回复落地后再花一次便宜调用
/// 摘要。这条用例把两级都钉住，同时把「多打的那一次 Provider 请求」显式写进
/// 断言——它是这个特性的真实成本，不该藏在别的用例的计数里。
#[test]
fn first_turn_titles_the_session_with_one_extra_provider_call() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_titling_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let run = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "重构订单模块的库存扣减",
        ])
        .output()
        .expect("run titled headless turn");
    assert_success(&run, "titled headless turn");
    let completion: serde_json::Value =
        serde_json::from_slice(&run.stdout).expect("parse completion JSON");
    let session_id = completion["session_id"]
        .as_str()
        .expect("completion session id");

    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("sessions").join(format!("{session_id}.json")))
            .expect("read stored Session"),
    )
    .expect("parse stored Session");
    // 模型（这里是 mock）给出的摘要生效，并落锁到 `summarized`——落锁是
    // 「下一轮不会再花一次调用」的唯一凭据。
    assert_eq!(stored["title"], MOCK_REPLY);
    assert_eq!(stored["title_source"], "summarized");
    assert_eq!(
        provider.requests(),
        2,
        "one turn plus exactly one title summary"
    );

    // 第二轮不再摘要：标题已经锁死，Provider 只该被打一次。
    let again = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            session_id,
            "--output",
            "json",
            "继续",
        ])
        .output()
        .expect("continue titled Session");
    assert_success(&again, "continued titled turn");
    assert_eq!(
        provider.requests(),
        3,
        "a locked title must not buy another summary"
    );
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("sessions").join(format!("{session_id}.json")))
            .expect("re-read stored Session"),
    )
    .expect("parse stored Session again");
    assert_eq!(stored["title"], MOCK_REPLY);

    guard.stop_daemon();
}

fn write_private_config(path: &Path, api_base: String) {
    write_private_config_with_language(path, api_base, None);
}

fn write_private_config_with_language(path: &Path, api_base: String, language: Option<&str>) {
    let language_line = language
        .map(|value| format!("language = \"{value}\"\n"))
        .unwrap_or_default();
    let contents = format!(
        r#"version = 1
default_provider = "mock"

[agent]
max_turns = 4
approval = "smart"
# 这些用例数的是「一轮打几次 Provider」。自动标题会在会话第一轮多打一次，
# 那是它设计上的成本，但会把这里的断言变成在数两件事。标题链路本身由
# `first_turn_titles_the_session_with_one_extra_provider_call` 单独盯。
auto_title = false
{language_line}
[providers.mock]
provider = "openai-compatible"
api = "chat-completions"
api_base = "{api_base}"
api_key = "integration-test-only"
model = "mock-model"
"#
    );
    write_private_text(path, &contents);
}

fn write_private_text(path: &Path, contents: &str) {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .expect("create private test config")
        .write_all(contents.as_bytes())
        .expect("write test config");
}

/// 与 [`write_private_config`] 同一份配置，只是把自动标题打开。
fn write_titling_config(path: &Path, api_base: String) {
    let contents = format!(
        r#"version = 1
default_provider = "mock"

[agent]
max_turns = 4
approval = "smart"
auto_title = true

[providers.mock]
provider = "openai-compatible"
api = "chat-completions"
api_base = "{api_base}"
api_key = "integration-test-only"
model = "mock-model"
"#
    );
    write_private_text(path, &contents);
}

fn write_json(path: &Path, value: serde_json::Value) {
    std::fs::write(
        path,
        serde_json::to_vec(&value).expect("serialize test JSON"),
    )
    .expect("write test JSON");
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path is UTF-8")
}

fn temporary_root() -> PathBuf {
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    base.join(format!("wdhl-{}", uuid::Uuid::new_v4().simple()))
}

fn free_loopback_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve Web test port");
    let address = listener.local_addr().expect("Web test address");
    drop(listener);
    address.to_string()
}

fn pop_sse_json(buffer: &mut String) -> Option<serde_json::Value> {
    let lf = buffer.find("\n\n");
    let crlf = buffer.find("\r\n\r\n");
    let (index, width) = match (lf, crlf) {
        (None, None) => return None,
        (Some(index), None) => (index, 2),
        (None, Some(index)) => (index, 4),
        (Some(lf), Some(crlf)) if lf <= crlf => (lf, 2),
        (Some(_), Some(crlf)) => (crlf, 4),
    };
    let frame = buffer[..index].to_owned();
    buffer.drain(..index + width);
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return pop_sse_json(buffer);
    }
    serde_json::from_str(&data).ok()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("condition was not met within {timeout:?}");
}

/// 持久化的 Turn 状态（`queued` / `running` / `completed` / `failed` ...）。
/// 读不到就返回 `unknown`：这是给失败诊断用的，不该自己再制造一个 panic。
fn runtime_turn_status(home: &Path, turn_id: &str) -> String {
    runtime_state(home, "turns.json")
        .iter()
        .find(|turn| turn["metadata"]["id"] == turn_id)
        .and_then(|turn| turn["metadata"]["status"].as_str())
        .unwrap_or("unknown")
        .to_owned()
}

/// Runtime 侧 Session 与 Turn 的状态摘要，用于把"等不到活跃 Turn"变成一条能直接
/// 定位的失败信息，而不是一个光秃秃的超时。
fn runtime_turn_report(home: &Path) -> String {
    let sessions = runtime_state(home, "sessions.json")
        .iter()
        .map(|session| {
            format!(
                "session {} status={} active_turn={} last_error={}",
                session["id"], session["status"], session["active_turn_id"], session["last_error"]
            )
        })
        .collect::<Vec<_>>();
    let turns = runtime_state(home, "turns.json")
        .iter()
        .map(|turn| {
            format!(
                "turn {} status={} attempts={} error={}",
                turn["metadata"]["id"],
                turn["metadata"]["status"],
                turn["metadata"]["attempts"],
                turn["metadata"]["error"]
            )
        })
        .collect::<Vec<_>>();
    format!("[{}] [{}]", sessions.join("; "), turns.join("; "))
}

fn runtime_state(home: &Path, name: &str) -> Vec<serde_json::Value> {
    std::fs::read(home.join("runtime").join(name))
        .ok()
        .and_then(|data| serde_json::from_slice::<Vec<serde_json::Value>>(&data).ok())
        .unwrap_or_default()
}

fn daemon_status(home: &Path) -> String {
    let output = willdeep(home)
        .args(["daemon", "status"])
        .output()
        .expect("query Runtime status");
    assert_success(&output, "query Runtime status");
    String::from_utf8(output.stdout)
        .expect("Runtime status is UTF-8")
        .trim()
        .to_owned()
}

fn status_value<'a>(status: &'a str, name: &str) -> &'a str {
    status
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("missing {name} in Runtime status: {status}"))
}

struct TestGuard {
    root: PathBuf,
    home: PathBuf,
    daemon_stopped: bool,
}

struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

impl TestGuard {
    fn new(root: PathBuf, home: PathBuf) -> Self {
        Self {
            root,
            home,
            daemon_stopped: false,
        }
    }

    fn stop_daemon(&mut self) {
        let _ = willdeep(&self.home).args(["daemon", "stop"]).output();
        self.daemon_stopped = true;
    }
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        if !self.daemon_stopped {
            let _ = willdeep(&self.home).args(["daemon", "stop"]).output();
        }
        if thread::panicking() {
            eprintln!(
                "preserving failed Headless Runtime test at {}",
                self.root.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct MockProvider {
    captured_requests: Arc<Mutex<Vec<serde_json::Value>>>,
    release_stream: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockProvider {
    fn start() -> Self {
        Self::start_with_mode(MockMode::Success)
    }

    fn start_with_status(status: u16) -> Self {
        Self::start_with_mode(MockMode::Status(status))
    }

    fn start_delayed(delay: Duration) -> Self {
        Self::start_with_mode(MockMode::DelayedSuccess(delay))
    }

    fn start_with_failing_tool() -> Self {
        Self::start_with_mode(MockMode::FailingToolThenSuccess)
    }

    fn start_with_mode(mode: MockMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock Provider");
        listener
            .set_nonblocking(true)
            .expect("configure mock Provider");
        let address = listener.local_addr().expect("mock Provider address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_requests = Arc::clone(&requests);
        let captured_requests = Arc::new(Mutex::new(Vec::new()));
        let worker_captured = captured_requests.clone();
        let release_stream = Arc::new(AtomicBool::new(false));
        let worker_release = release_stream.clone();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("create mock Provider Runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("adopt mock Provider socket");
                let app = axum::Router::new()
                    .route(
                        "/v1/chat/completions",
                        axum::routing::post(mock_provider_response),
                    )
                    .with_state(MockProviderState {
                        captured_requests: worker_captured,
                        release_stream: worker_release,
                        mode,
                        requests: worker_requests,
                    });
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        while !worker_stop.load(Ordering::Relaxed) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .expect("serve mock Provider");
            });
        });
        Self {
            captured_requests,
            release_stream,
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn api_base(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
enum MockMode {
    // 这两种只给经 `scripts/lib/agent_eval_process.rb` 中断的测试用，那两条仅 Unix。
    #[cfg_attr(not(unix), allow(dead_code))]
    ForegroundRecovery,
    WaitRootsForRecoveredChild,
    #[cfg_attr(not(unix), allow(dead_code))]
    CheckpointThenWait,
    DelegateThenSuccess,
    WaitRootThenRetryChild,
    RetryRoot,
    StreamingUntilReleased,
    Success,
    IncompleteThenSuccess,
    Status(u16),
    DelayedSuccess(Duration),
    /// 第一轮请求一个注定失败的工具（读不存在的文件），之后正常收尾。
    FailingToolThenSuccess,
    /// 第一轮起一条 `run_in_background` 命令，之后正常收尾。
    BackgroundJobThenSuccess,
    /// 第一轮起一个 `monitor`，之后正常收尾。
    MonitorThenSuccess,
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.release_stream.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock Provider");
        }
    }
}

#[derive(Clone)]
struct MockProviderState {
    captured_requests: Arc<Mutex<Vec<serde_json::Value>>>,
    release_stream: Arc<AtomicBool>,
    mode: MockMode,
    requests: Arc<AtomicUsize>,
}

async fn mock_provider_response(
    axum::extract::State(state): axum::extract::State<MockProviderState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    state.captured_requests.lock().unwrap().push(
        serde_json::from_slice::<serde_json::Value>(&body).expect("parse Provider request JSON"),
    );
    let request_index = state.requests.fetch_add(1, Ordering::Relaxed);
    if matches!(state.mode, MockMode::ForegroundRecovery) {
        return foreground_recovery::response(state, body, request_index).await;
    }
    if matches!(state.mode, MockMode::CheckpointThenWait) && request_index == 2 {
        while !state.release_stream.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    if matches!(
        state.mode,
        MockMode::WaitRootThenRetryChild | MockMode::RetryRoot
    ) {
        let first = usize::from(matches!(state.mode, MockMode::WaitRootThenRetryChild));
        if request_index == first {
            return axum::response::Response::builder()
                .status(429)
                .header("retry-after", "2")
                .body(axum::body::Body::from("rate limited"))
                .unwrap();
        }
        if request_index == first + 1 {
            while !state.release_stream.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    if matches!(state.mode, MockMode::StreamingUntilReleased) {
        let stream = futures_util::stream::unfold(
            (0, state.release_stream),
            |(step, release)| async move {
                let frame = match step {
                    0 => {
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"live prefix\"},\"finish_reason\":null}]}\n\n"
                    }
                    1 => {
                        while !release.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                    }
                    _ => return None,
                };
                Some((
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        frame.as_bytes(),
                    )),
                    (step + 1, release),
                ))
            },
        );
        return axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream))
            .unwrap();
    }
    if let MockMode::DelayedSuccess(delay) = state.mode {
        tokio::time::sleep(delay).await;
    }
    let status = match state.mode {
        MockMode::Status(status) => status,
        MockMode::Success
        | MockMode::ForegroundRecovery
        | MockMode::WaitRootsForRecoveredChild
        | MockMode::CheckpointThenWait
        | MockMode::DelegateThenSuccess
        | MockMode::WaitRootThenRetryChild
        | MockMode::RetryRoot
        | MockMode::StreamingUntilReleased
        | MockMode::IncompleteThenSuccess
        | MockMode::DelayedSuccess(_)
        | MockMode::FailingToolThenSuccess
        | MockMode::BackgroundJobThenSuccess
        | MockMode::MonitorThenSuccess => 200,
    };
    let body = if matches!(state.mode, MockMode::CheckpointThenWait) && request_index == 0 {
        let command = r#"ruby -e 'File.open("progress.log", "a") { |file| file.write("checkpoint-once\n") }'"#;
        serde_json::json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"marker-once","type":"function","function":{"name":"run_command","arguments":serde_json::json!({"command":command}).to_string()}}]},"finish_reason":"tool_calls"}]}).to_string()
    } else if matches!(state.mode, MockMode::CheckpointThenWait) && request_index == 1 {
        serde_json::json!({"choices":[{"message":{"content":"<verdict>YES</verdict>"},"finish_reason":"stop"}]}).to_string()
    } else if matches!(state.mode, MockMode::DelegateThenSuccess) && request_index == 0 {
        serde_json::json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"delegate-diagnosis","type":"function","function":{"name":"spawn_agent","arguments":serde_json::json!({"profile":"scout","prompt":"Inspect the task without editing files and report your diagnosis","run_in_background":false}).to_string()}}]},"finish_reason":"tool_calls"}]}).to_string()
    } else if matches!(state.mode, MockMode::FailingToolThenSuccess) && request_index == 0 {
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"read_missing","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"definitely-missing.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#.to_owned()
    } else if matches!(state.mode, MockMode::BackgroundJobThenSuccess) {
        detached_background::response(&body, request_index)
    } else if matches!(state.mode, MockMode::MonitorThenSuccess) {
        monitor_events::response(&body, request_index)
    } else if mode_is_waiting_root(state.mode, request_index) {
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"ask_root","type":"function","function":{"name":"ask_user","arguments":"{\"question\":\"keep the root active?\",\"options\":[\"yes\"]}"}}]},"finish_reason":"tool_calls"}]}"#.to_owned()
    } else if matches!(state.mode, MockMode::IncompleteThenSuccess) && request_index < 3 {
        serde_json::json!({
            "choices": [{"message": {"content": "partial evidence", "tool_calls": []}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        }).to_string()
    } else if status == 200 {
        format!(
            r#"{{"choices":[{{"message":{{"content":"{MOCK_REPLY}","tool_calls":[]}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}}}"#
        )
    } else {
        r#"{"error":"temporary failure"}"#.to_owned()
    };
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::from_u16(status).expect("valid mock Provider status"),
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn mode_is_waiting_root(mode: MockMode, request_index: usize) -> bool {
    (matches!(mode, MockMode::WaitRootThenRetryChild) && request_index == 0)
        || (matches!(mode, MockMode::WaitRootsForRecoveredChild) && matches!(request_index, 0 | 2))
}
