use super::*;

#[test]
fn shared_profile_overrides_legacy_and_records_actual_cli_limits() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());
    let profile = willdeep_core::runtime_parameters::RuntimeParameters {
        max_turns: 17,
        input_suggestions: false,
        ..Default::default()
    };
    let path = home.join(willdeep_core::runtime_parameters::FILE_NAME);
    write_json(&path, serde_json::to_value(&profile).unwrap());
    let inspected = willdeep(&home)
        .args([
            "--config",
            path_text(&config),
            "config",
            "runtime",
            "--max-turns",
            "9",
        ])
        .output()
        .unwrap();
    assert_success(&inspected, "inspect shared runtime profile");
    let expected: willdeep_core::runtime_parameters::RuntimeParameters =
        serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(expected.max_turns, 9);
    assert!(!expected.input_suggestions);
    assert_eq!(
        provider.requests(),
        0,
        "config inspection cannot purchase a request"
    );
    let completed = willdeep(&home)
        .args([
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--max-turns",
            "9",
            "run",
            "--local",
            "--output",
            "json",
            "say hello",
        ])
        .output()
        .unwrap();
    assert_success(&completed, "run with shared runtime profile");
    let result: serde_json::Value = serde_json::from_slice(&completed.stdout).unwrap();
    let session = willdeep_core::SessionStore::new(&home)
        .load(result["session_id"].as_str().unwrap().parse().unwrap())
        .unwrap();
    let checkpoint = session.execution_checkpoint.unwrap();
    assert_eq!(checkpoint.runtime_parameters.as_ref(), Some(&expected));
    assert_eq!(
        checkpoint.runtime_parameters_sha256.as_deref(),
        Some(expected.fingerprint().as_str())
    );
    let before = provider.requests();
    write_json(
        &path,
        serde_json::json!({"schema":"willdeep.runtime-parameters.v1"}),
    );
    let refused = willdeep(&home)
        .args([
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "run",
            "--local",
            "--output",
            "json",
            "must not reach provider",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(
        provider.requests(),
        before,
        "invalid profile stops before any provider call"
    );
}
