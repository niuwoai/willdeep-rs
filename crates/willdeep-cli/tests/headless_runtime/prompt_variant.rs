use super::*;

const ADDED_RULE: &str = "- Lead every answer with the conclusion.";

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `willdeep prompt` 给出写变体所需的一切：父版本号与段原文。
fn main_tone_variant(home: &Path, parent_bundle: Option<&str>) -> serde_json::Value {
    let sections = willdeep(home)
        .args(["prompt", "sections", "--role", "main"])
        .output()
        .unwrap();
    assert_success(&sections, "prompt sections");
    let bundle = stdout_text(&sections)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("bundle id on the role line")
        .to_owned();
    let show = willdeep(home)
        .args(["prompt", "show", "main", "tone"])
        .output()
        .unwrap();
    assert_success(&show, "prompt show");
    let original = stdout_text(&show);
    serde_json::json!({
        "schema": "willdeep.prompt-variant.v1",
        "id": "tone-conclusion-first",
        "role": "main",
        "section": "tone",
        "parent_bundle": parent_bundle.unwrap_or(&bundle),
        "text": format!("{}\n{ADDED_RULE}", original.trim_end_matches('\n')),
    })
}

/// 候选提示词只在 `run --local` 里生效，而且真的进了发给模型的 system 消息；
/// 过期或经 daemon 的调用直接报错，一个请求都不发。
#[test]
fn a_prompt_variant_reaches_the_model_only_through_a_valid_local_run() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let run = |variant: &Path, local: bool| {
        let mut command = willdeep(&home);
        command.env("WILLDEEP_PROMPT_VARIANT", variant).arg("run");
        if local {
            command.arg("--local");
        }
        command
            .args([
                "--config",
                path_text(&config),
                "--workspace",
                path_text(&workspace),
                "--output",
                "json",
                "say hello",
            ])
            .output()
            .unwrap()
    };

    let stale = root.join("stale.json");
    write_json(&stale, main_tone_variant(&home, Some("main@000000000000")));
    let refused = run(&stale, true);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("not a valid prompt variant"), "{stderr}");
    assert!(stderr.contains("stale"), "{stderr}");
    assert_eq!(provider.requests(), 0, "an invalid variant never runs");

    let valid = root.join("valid.json");
    write_json(&valid, main_tone_variant(&home, None));
    let check = willdeep(&home)
        .args(["prompt", "check", path_text(&valid)])
        .output()
        .unwrap();
    assert_success(&check, "prompt check");
    assert!(stdout_text(&check).contains(&format!("+ {ADDED_RULE}")));

    let through_daemon = run(&valid, false);
    assert!(!through_daemon.status.success());
    assert!(
        String::from_utf8_lossy(&through_daemon.stderr)
            .contains("only honored by `willdeep run --local`")
    );
    assert_eq!(provider.requests(), 0);

    let local = run(&valid, true);
    assert_success(&local, "local run with a prompt variant");
    let requests = provider.captured_requests.lock().unwrap().clone();
    let system = requests[0]["messages"][0]["content"]
        .as_str()
        .expect("system message text");
    assert!(
        system.contains(ADDED_RULE),
        "variant text reaches the model"
    );
    assert!(
        system.contains("Stable tool contract:"),
        "other sections stay"
    );
}

/// `prompt propose` 走会话主模型：模型答不出合规 JSON 时两次请求后放弃、
/// 不写文件、退出码非零；候选数超过 3 直接拒绝，一个请求都不发。
#[test]
fn prompt_propose_asks_the_configured_model_and_writes_nothing_invalid() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root.clone(), home.clone());
    let candidates = root.join("candidates.json");
    write_json(
        &candidates,
        serde_json::json!({"candidates": [{
            "target": {"role": "main", "bundle": null, "section": "tool_rules:edit"},
            "signal": "tool_failed:edit_file/edit_text_not_found",
            "evidence": {"count": 6, "rate": null, "examples": []},
            "suggestion": "strengthen the read-before-edit rule"
        }]}),
    );
    let out = root.join("variants");
    let propose = |count: &str| {
        willdeep(&home)
            .args([
                "--config",
                path_text(&config),
                "prompt",
                "propose",
                "--candidates",
                path_text(&candidates),
                "--count",
                count,
                "--out-dir",
                path_text(&out),
            ])
            .output()
            .unwrap()
    };

    let too_many = propose("4");
    assert!(!too_many.status.success());
    assert!(String::from_utf8_lossy(&too_many.stderr).contains("--count must be between 1 and 3"));
    assert_eq!(provider.requests(), 0);

    let discarded = propose("1");
    assert!(!discarded.status.success());
    assert!(
        stdout_text(&discarded).contains("was discarded"),
        "{}",
        stdout_text(&discarded)
    );
    assert_eq!(provider.requests(), 2, "one draft and one repair round");
    let requests = provider.captured_requests.lock().unwrap().clone();
    let user = requests[1]["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("rejected by the structure gate"), "{user}");
    assert!(
        !out.join("main-tool-failed-edit-file-edit-text-not-found-p1.json")
            .exists()
    );
}
