use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use futures_util::StreamExt;

const MOCK_REPLY: &str = "headless runtime reply";
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

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
    assert_eq!(provider.requests(), 1);
}

#[test]
fn public_api_spawns_and_waits_for_a_read_only_child_agent() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_waiting_root();
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
    assert_eq!(wait_envelope["data"]["label"], "external scout");
    assert_eq!(
        provider.requests(),
        2,
        "root and child must each reach Provider"
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

#[test]
fn background_supervisor_completes_work_and_kills_it_when_parent_disconnects() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test Workspace");

    let rejected = willdeep(&home)
        .args(["daemon", "background-supervisor"])
        .env_remove("WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR")
        .output()
        .expect("invoke untrusted background supervisor");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("background supervisor is an internal command")
    );

    let mut completed = spawn_background_supervisor(&home);
    let completed_liveness = send_supervisor_request(
        &mut completed,
        serde_json::json!({
            "command": supervisor_print_command(),
            "workspace": workspace,
            "timeout_seconds": 10
        }),
    );
    let completed = completed
        .wait_with_output()
        .expect("wait for completed background supervisor");
    drop(completed_liveness);
    assert_success(&completed, "completed background supervisor");
    let completed: serde_json::Value =
        serde_json::from_slice(&completed.stdout).expect("parse completed supervisor result");
    assert_eq!(completed["status"], "completed");
    assert!(
        completed["output"]
            .as_str()
            .is_some_and(|value| value.contains("supervisor-ok"))
    );

    let mut disconnected = spawn_background_supervisor(&home);
    let child_pid_path = workspace.join("supervisor-child.pid");
    let disconnected_liveness = send_supervisor_request(
        &mut disconnected,
        serde_json::json!({
            "command": supervisor_wait_command(),
            "workspace": workspace,
            "timeout_seconds": 60
        }),
    );
    wait_until(Duration::from_secs(5), || child_pid_path.exists());
    let child_pid = std::fs::read_to_string(&child_pid_path)
        .expect("read supervised child PID")
        .parse::<u32>()
        .expect("parse supervised child PID");
    let started = std::time::Instant::now();
    drop(disconnected_liveness);
    let disconnected = disconnected
        .wait_with_output()
        .expect("wait for disconnected background supervisor");
    assert_success(&disconnected, "disconnected background supervisor");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "parent disconnect must not wait for the 30 second Shell command"
    );
    let disconnected: serde_json::Value =
        serde_json::from_slice(&disconnected.stdout).expect("parse disconnected result");
    assert_eq!(disconnected["status"], "killed");
    #[cfg(unix)]
    wait_until(Duration::from_secs(5), || !process_exists(child_pid));
    std::fs::remove_dir_all(root).expect("remove supervisor test root");
}

fn spawn_background_supervisor(home: &Path) -> std::process::Child {
    willdeep(home)
        .args(["daemon", "background-supervisor"])
        .env("WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn background supervisor")
}

fn send_supervisor_request(
    child: &mut std::process::Child,
    request: serde_json::Value,
) -> std::process::ChildStdin {
    let payload = serde_json::to_vec(&request).expect("serialize supervisor request");
    let mut input = child.stdin.take().expect("background supervisor stdin");
    input
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .expect("write supervisor request length");
    input.write_all(&payload).expect("write supervisor request");
    input.flush().expect("flush supervisor request");
    input
}

#[cfg(unix)]
fn supervisor_print_command() -> &'static str {
    "printf supervisor-ok"
}

#[cfg(windows)]
fn supervisor_print_command() -> &'static str {
    "Write-Output 'supervisor-ok'"
}

#[cfg(unix)]
fn supervisor_wait_command() -> &'static str {
    "sleep 30 & child=$!; printf '%s' \"$child\" > supervisor-child.pid; wait \"$child\""
}

#[cfg(windows)]
fn supervisor_wait_command() -> &'static str {
    "$child = Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -PassThru; Set-Content -NoNewline supervisor-child.pid $child.Id; Wait-Process -Id $child.Id"
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("runtime/daemon.json")).expect("read Runtime state"),
    )
    .expect("parse Runtime state");
    let address = state["address"].as_str().expect("Runtime state address");
    let token = state["token"].as_str().expect("Runtime state token");
    let client =
        willdeep_runtime_client::RuntimeClient::new(format!("http://{address}"), token.to_owned())
            .expect("create Runtime event Client");
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
{language_line}
[providers.mock]
provider = "openai-compatible"
api = "chat-completions"
api_base = "{api_base}"
api_key = "integration-test-only"
model = "mock-model"
"#
    );
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

    fn start_waiting_root() -> Self {
        Self::start_with_mode(MockMode::WaitThenSuccess)
    }

    fn start_delayed(delay: Duration) -> Self {
        Self::start_with_mode(MockMode::DelayedSuccess(delay))
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
    Success,
    Status(u16),
    WaitThenSuccess,
    DelayedSuccess(Duration),
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock Provider");
        }
    }
}

#[derive(Clone)]
struct MockProviderState {
    mode: MockMode,
    requests: Arc<AtomicUsize>,
}

async fn mock_provider_response(
    axum::extract::State(state): axum::extract::State<MockProviderState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    serde_json::from_slice::<serde_json::Value>(&body).expect("parse Provider request JSON");
    let request_index = state.requests.fetch_add(1, Ordering::Relaxed);
    if let MockMode::DelayedSuccess(delay) = state.mode {
        tokio::time::sleep(delay).await;
    }
    let status = match state.mode {
        MockMode::Status(status) => status,
        MockMode::Success | MockMode::WaitThenSuccess | MockMode::DelayedSuccess(_) => 200,
    };
    let body = if mode_is_waiting_root(state.mode, request_index) {
        r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"ask_root","type":"function","function":{"name":"ask_user","arguments":"{\"question\":\"keep the root active?\",\"options\":[\"yes\"]}"}}]},"finish_reason":"tool_calls"}]}"#.to_owned()
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
    matches!(mode, MockMode::WaitThenSuccess) && request_index == 0
}
