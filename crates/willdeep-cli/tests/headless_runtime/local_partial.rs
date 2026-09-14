use super::*;

#[test]
fn local_partial_returns_nonzero_and_keeps_resumable_json() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start_with_mode(MockMode::IncompleteThenSuccess);
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
            "complete the objective",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["type"], "partial");
    assert_eq!(result["stop_reason"], "incomplete");
    let id = result["session_id"].as_str().unwrap();
    let store = willdeep_core::SessionStore::new(&home);
    assert_eq!(
        store
            .load(id.parse().unwrap())
            .unwrap()
            .execution_checkpoint
            .unwrap()
            .status,
        willdeep_core::checkpoint::CheckpointStatus::Partial
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
            id,
            "--output",
            "json",
            "continue the remaining work",
        ])
        .output()
        .unwrap();
    assert_success(&resumed, "resume partial local execution");
    let completed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(completed["type"], "completed");
    assert_eq!(completed["session_id"], id);
    assert_eq!(provider.requests(), 4);
}
