use super::*;

fn tool(name: &str, id: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
}

pub(super) async fn response(
    state: MockProviderState,
    body: axum::body::Bytes,
    index: usize,
) -> axum::response::Response {
    let value = match index {
        0 => tool(
            "spawn_agent",
            "foreground-child",
            serde_json::json!({"profile":"generalist","prompt":"Create result.txt exactly once","run_in_background":false,"task":{"goal":"write one marker","write_files":["result.txt"]}}),
        ),
        1 => tool(
            "create_file",
            "foreground-write",
            serde_json::json!({"path":"result.txt","content":"once"}),
        ),
        2 => {
            while !state.release_stream.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            serde_json::json!({"choices":[{"message":{"content":MOCK_REPLY},"finish_reason":"stop"}]})
        }
        3 | 7 => tool(
            "list_agent_recoveries",
            "find-original",
            serde_json::json!({}),
        ),
        4 | 8 => {
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let content = request["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap();
            let list: serde_json::Value = serde_json::from_str(content).unwrap();
            assert_eq!(list["agents"].as_array().unwrap().len(), 1);
            assert_eq!(list["agents"][0]["completed_report_available"], index == 8);
            tool(
                "resume_agent",
                "resume-original",
                serde_json::json!({"agent_id":list["agents"][0]["agent_id"]}),
            )
        }
        _ => {
            serde_json::json!({"choices":[{"message":{"content":MOCK_REPLY},"finish_reason":"stop"}]})
        }
    };
    use axum::response::IntoResponse;
    axum::Json(value).into_response()
}

#[test]
fn interrupted_foreground_child_is_discovered_resumed_and_reported_without_replay() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::ForegroundRecovery);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let trigger = root.join("interrupt-now");
    let controller = root.join("interrupt.rb");
    write_private_text(
        &controller,
        r#"
require 'json'
repository, home, workspace, config, binary, trigger = ARGV
require File.join(repository, 'scripts/lib/agent_eval_process')
captured = nil
observer = lambda do
  next false unless File.exist?(trigger)
  children = Dir.glob(File.join(home, 'workers/sessions/*.json'))
  parents = Dir.glob(File.join(home, 'sessions/*.json'))
  next false unless children.size == 1 && parents.size == 1
  child = JSON.parse(File.read(children.first))
  checkpoint = child['execution_checkpoint']
  next false unless checkpoint && checkpoint['status'] == 'running' && checkpoint['pending_call_ids'] == []
  next false unless child['messages'].any? { |message| message['role'] == 'tool' && message['tool_call_id'] == 'foreground-write' }
  next false unless File.read(File.join(workspace, 'result.txt')) == 'once'
  captured = {parent: File.basename(parents.first, '.json'), child: child['id']}
  true
end
env = {'WILLDEEP_HOME' => home}
%w[WILLDEEP_API_BASE WILLDEEP_API_KEY WILLDEEP_CONFIG WILLDEEP_LANGUAGE WILLDEEP_MODEL].each { |key| env[key] = nil }
command = [binary, 'run', '--local', '--config', config, '--workspace', workspace, '--full-auto', '--output', 'json', 'Delegate a foreground child to create result.txt once']
code, timeout, _, injected = AgentEvalProcess.run(command, env, workspace, 30, home, interrupt_when: observer)
puts JSON.generate({code: code, timeout: timeout, injected: injected, captured: captured})
"#,
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let process = Command::new("ruby")
        .args([
            path_text(&controller),
            path_text(&repository),
            path_text(&home),
            path_text(&workspace),
            path_text(&config),
            env!("CARGO_BIN_EXE_willdeep"),
            path_text(&trigger),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut controller = ChildGuard(Some(process));
    wait_until(Duration::from_secs(20), || provider.requests() == 3);
    std::fs::write(&trigger, "interrupt at durable boundary").unwrap();
    let interrupted = controller.0.take().unwrap().wait_with_output().unwrap();
    assert_success(&interrupted, "interrupt foreground child controller");
    let evidence: serde_json::Value = serde_json::from_slice(&interrupted.stdout).unwrap();
    assert_eq!(evidence["injected"], true);
    assert_eq!(evidence["timeout"], false);
    assert!(evidence["code"].is_null());
    let parent = evidence["captured"]["parent"].as_str().unwrap();
    let child = evidence["captured"]["child"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    provider.release_stream.store(true, Ordering::SeqCst);
    for (prompt, expected_requests) in [
        (
            "Continue the original task; recover the interrupted child",
            7,
        ),
        (
            "Retrieve the completed child report without executing it again",
            10,
        ),
    ] {
        let output = willdeep(&home)
            .args([
                "run",
                "--local",
                "--config",
                path_text(&config),
                "--workspace",
                path_text(&workspace),
                "--session",
                parent,
                "--full-auto",
                "--output",
                "json",
                prompt,
            ])
            .output()
            .unwrap();
        assert_success(&output, "recover foreground child through model tools");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["type"], "completed");
        assert_eq!(result["session_id"], parent);
        assert_eq!(provider.requests(), expected_requests);
        assert_eq!(
            std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
            "once"
        );
        let saved = willdeep_core::SessionStore::new(home.join("workers"))
            .load(child)
            .unwrap();
        assert_eq!(
            saved
                .messages
                .iter()
                .flat_map(|message| &message.tool_calls)
                .filter(|call| call.id == "foreground-write")
                .count(),
            1
        );
    }
}
