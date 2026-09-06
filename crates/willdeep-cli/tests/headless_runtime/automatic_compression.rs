use super::*;

#[test]
fn local_automatic_compression_preserves_constraints_batches_and_usage() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut contents = std::fs::read_to_string(&config).unwrap();
    contents.push_str("context_window = 32000\n");
    std::fs::write(&config, contents).unwrap();
    let _guard = TestGuard::new(root, home.clone());
    let id = uuid::Uuid::new_v4();
    let constraint =
        "Only inspect src/allowed.rs; never change protected.txt; preserve this exact constraint.";
    let mut messages = vec![serde_json::json!({"role":"user","content":constraint})];
    for index in 0..10 {
        messages.push(serde_json::json!({"role":"assistant","content":format!("old-inspection-{index}: {}", "historical evidence ".repeat(1000))}));
    }
    let arguments = serde_json::json!({"path":"src/allowed.rs","offset":17,"limit":3}).to_string();
    messages.push(serde_json::json!({"role":"assistant","content":"recent inspection","tool_calls":[{"id":"recent-read","name":"read_file","arguments":arguments}]}));
    messages.push(serde_json::json!({"role":"tool","content":"recent tool result","tool_call_id":"recent-read"}));
    for index in 0..3 {
        messages.push(
            serde_json::json!({"role":"assistant","content":format!("Recent observation {index}")}),
        );
    }
    write_json(
        &home.join("sessions").join(format!("{id}.json")),
        serde_json::json!({"version":1,"id":id,"title":"Automatic compression","workspace":workspace,"profile":"mock","created_at":1,"updated_at":1,"messages":messages}),
    );
    let output = willdeep(&home)
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
            "ndjson",
            "Continue the original inspection without changing any files",
        ])
        .output()
        .unwrap();
    assert_success(&output, "automatic threshold compression");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for kind in ["compression_started", "compression_completed", "completed"] {
        assert!(
            events.iter().any(|event| event["type"] == kind),
            "missing {kind}"
        );
    }
    let requests = provider.captured_requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "one automatic summary and one task request"
    );
    let summary_source = requests[0]["messages"].to_string();
    assert!(summary_source.contains("old-inspection-0"));
    let task = requests[1]["messages"].as_array().unwrap();
    assert!(
        task.iter()
            .any(|message| message["role"] == "user" && message["content"] == constraint)
    );
    assert!(
        !requests[1]["messages"]
            .to_string()
            .contains("old-inspection-0")
    );
    let call_index = task
        .iter()
        .position(|message| message["tool_calls"][0]["id"] == "recent-read")
        .expect("recent tool call preserved");
    assert_eq!(
        task[call_index]["tool_calls"][0]["function"]["arguments"],
        arguments
    );
    assert_eq!(task[call_index + 1]["role"], "tool");
    assert_eq!(task[call_index + 1]["tool_call_id"], "recent-read");
    assert_eq!(task[call_index + 1]["content"], "recent tool result");
    let session = willdeep_core::SessionStore::new(&home).load(id).unwrap();
    let checkpoint = session.execution_checkpoint.unwrap();
    assert_eq!((checkpoint.input_tokens, checkpoint.output_tokens), (10, 6));
    assert_eq!(session.manual_compression_usage.reported_calls, 0);
    assert!(
        session
            .messages
            .iter()
            .any(|message| message.role == willdeep_core::Role::User
                && message.content == constraint)
    );
}
