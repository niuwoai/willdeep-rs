use super::*;
#[cfg(unix)]
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

struct AllowApprover;
#[async_trait]
impl Approver for AllowApprover {
    async fn approve(&self, _description: &str, _always_allow_available: bool) -> ApprovalDecision {
        ApprovalDecision::AllowOnce
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_schemas_are_loaded_on_demand_through_two_fixed_tools() {
    let script = r#"read init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}'
read initialized
read list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}'
read call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"pong"}]}}'
"#;
    let mut configs = BTreeMap::new();
    configs.insert(
        "mock".to_owned(),
        crate::mcp::McpServerConfig {
            command: Some("/bin/sh".to_owned()),
            args: vec!["-c".to_owned(), script.to_owned()],
            startup_timeout_seconds: 5,
            ..crate::mcp::McpServerConfig::default()
        },
    );
    let mcp = Arc::new(McpRegistry::connect(&configs).await.expect("connect MCP"));
    let root = std::env::temp_dir().join(format!("willdeep-mcp-tools-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("workspace");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_mcp(mcp)
        .with_approver(Arc::new(AllowApprover));
    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"list_mcp_tools".to_owned()));
    assert!(names.contains(&"call_mcp_tool".to_owned()));
    assert!(!names.iter().any(|name| name.starts_with("mcp__")));

    let listed = registry
        .execute(&ToolCall {
            id: "list".to_owned(),
            name: "list_mcp_tools".to_owned(),
            arguments: json!({"query":"echo"}).to_string(),
        })
        .await
        .expect("search MCP tools");
    assert!(listed.contains("mcp__mock__echo"));
    assert!(listed.contains("parameters"));
    let called = registry
        .execute(&ToolCall {
            id: "call".to_owned(),
            name: "call_mcp_tool".to_owned(),
            arguments: json!({"name":"mcp__mock__echo","arguments":{"text":"ping"}}).to_string(),
        })
        .await
        .expect("call MCP tool");
    assert!(called.contains("pong"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

struct AlwaysApprover(AtomicUsize);
#[async_trait]
impl Approver for AlwaysApprover {
    async fn approve(&self, _description: &str, available: bool) -> ApprovalDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        if available {
            ApprovalDecision::AlwaysAllow
        } else {
            ApprovalDecision::AllowOnce
        }
    }
}

struct AnswerApprover;
#[async_trait]
impl Approver for AnswerApprover {
    async fn approve(&self, _description: &str, _available: bool) -> ApprovalDecision {
        ApprovalDecision::Deny
    }
    async fn ask_user(&self, question: UserQuestion) -> Option<String> {
        assert_eq!(question.options, vec!["Rust", "Go"]);
        Some("Other <custom>".to_owned())
    }
}

fn workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("willdeep-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create fixture workspace");
    path
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn read_file_matches_swift_line_number_contract() {
    let root = workspace("read");
    std::fs::write(root.join("file.txt"), "one\ntwo\nthree\n").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
    let output = registry
        .read_file(ReadArgs {
            path: "file.txt".to_owned(),
            offset: Some(2),
            limit: Some(2),
            max_bytes: None,
        })
        .await
        .expect("read");
    assert_eq!(output, "     2  two\n     3  three\n");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn git_history_tools_are_bounded_read_only_and_path_scoped() {
    let root = workspace("git-history");
    git(&root, &["init"]);
    git(&root, &["config", "user.name", "WillDeep Test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(root.join("history.txt"), "first\nsecond\n").expect("first fixture");
    git(&root, &["add", "history.txt"]);
    git(&root, &["commit", "-m", "initial history"]);
    std::fs::write(root.join("history.txt"), "first\nchanged\n").expect("second fixture");
    git(&root, &["add", "history.txt"]);
    git(&root, &["commit", "-m", "update history"]);

    let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
    let log = registry
        .git_log(GitLogArgs {
            path: Some("history.txt".to_owned()),
            max_count: Some(1),
            author: Some("WillDeep Test".to_owned()),
            since: None,
        })
        .await
        .expect("git log");
    assert!(log.contains("update history"));
    assert!(!log.contains("initial history"));
    assert_eq!(log.lines().count(), 1);

    let blame = registry
        .git_blame(GitBlameArgs {
            path: "history.txt".to_owned(),
            start_line: None,
            end_line: None,
        })
        .await
        .expect("git blame");
    assert!(blame.contains("WillDeep Test"));
    assert!(blame.contains("first"));
    assert!(blame.contains("changed"));

    let invalid_range = registry
        .git_blame(GitBlameArgs {
            path: "history.txt".to_owned(),
            start_line: Some(3),
            end_line: Some(2),
        })
        .await;
    assert!(invalid_range.is_err());
    let escape = registry
        .git_blame(GitBlameArgs {
            path: "../../etc/passwd".to_owned(),
            start_line: None,
            end_line: None,
        })
        .await;
    assert!(matches!(escape, Err(ToolError::OutsideWorkspace(_))));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn existing_symlink_escape_is_rejected() {
    let root = workspace("escape");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
    let result = registry.resolve_existing("../../etc/passwd");
    assert!(matches!(result, Err(ToolError::OutsideWorkspace(_))));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn workspace_access_applies_exact_edit() {
    let root = workspace("edit");
    std::fs::write(root.join("file.txt"), "alpha beta").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess).expect("registry");
    registry
        .edit_file(EditArgs {
            path: "file.txt".to_owned(),
            old_string: "beta".to_owned(),
            new_string: "gamma".to_owned(),
            replace_all: None,
        })
        .await
        .expect("edit");
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).expect("read"),
        "alpha gamma"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn a_blocking_hook_stops_the_tool_before_it_runs() {
    // 引擎能拦是一回事，接没接上工具分发是另一回事。这条钉的是后者。
    let root = workspace("hook-gate");
    let target = root.join("file.txt");
    std::fs::write(&target, "unchanged").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_hooks(crate::hooks::HookRegistry::new(vec![crate::hooks::Hook {
            name: "change-ticket".to_owned(),
            event: crate::hooks::HookEvent::PreTool,
            command: "echo '缺少变更单编号' >&2; exit 1".to_owned(),
            blocking: true,
            timeout: std::time::Duration::from_secs(5),
            on_error: crate::hooks::HookFailure::Deny,
        }]));

    let result = registry
        .execute(&ToolCall {
            id: "write".to_owned(),
            name: "edit_file".to_owned(),
            arguments: serde_json::json!({
                "path": "file.txt",
                "old_string": "unchanged",
                "new_string": "changed"
            })
            .to_string(),
        })
        .await;

    let Err(ToolError::HookDenied(message)) = result else {
        panic!("hook 应当拦下这次调用：{result:?}");
    };
    assert!(message.contains("change-ticket"), "{message}");
    assert!(message.contains("缺少变更单编号"), "{message}");
    // 拦住的意思是文件没被动过，不是"改完了再报个错"。
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
}

#[tokio::test]
async fn an_observer_hook_sees_the_call_without_blocking_it() {
    let root = workspace("hook-audit");
    std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
    let log = root.join("audit.log");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_hooks(crate::hooks::HookRegistry::new(vec![crate::hooks::Hook {
            name: "audit".to_owned(),
            event: crate::hooks::HookEvent::PreTool,
            // Windows 上 hook 走 PowerShell，那里的 `cat` 是 Get-Content，不读 stdin。
            command: if cfg!(windows) {
                format!(
                    "[IO.File]::AppendAllText('{}', [Console]::In.ReadToEnd())",
                    log.display()
                )
            } else {
                format!("cat >> {}", log.display())
            },
            blocking: false,
            timeout: std::time::Duration::from_secs(5),
            on_error: crate::hooks::HookFailure::Deny,
        }]));

    let result = registry
        .execute(&ToolCall {
            id: "edit".to_owned(),
            name: "edit_file".to_owned(),
            arguments: serde_json::json!({
                "path": "file.txt",
                "old_string": "unchanged",
                "new_string": "changed"
            })
            .to_string(),
        })
        .await;

    assert!(result.is_ok(), "{result:?}");
    let recorded = std::fs::read_to_string(&log).expect("审计 hook 应当收到事件");
    assert!(recorded.contains("\"event\":\"pre_tool\""), "{recorded}");
    assert!(recorded.contains("edit_file"), "{recorded}");
}

#[tokio::test]
async fn read_only_policy_blocks_write_capable_tools_before_approval() {
    let root = workspace("read-only");
    std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::ReadOnly).expect("registry");
    let result = registry
        .execute(&ToolCall {
            id: "write".to_owned(),
            name: "edit_file".to_owned(),
            arguments: serde_json::json!({
                "path": "file.txt",
                "old_string": "unchanged",
                "new_string": "changed"
            })
            .to_string(),
        })
        .await;
    assert!(matches!(result, Err(ToolError::ReadOnlyPolicy(_))));
    assert_eq!(
        std::fs::read_to_string(root.join("file.txt")).expect("read"),
        "unchanged"
    );
    assert!(matches!(
        registry
            .approve_subagent_write_set(&["file.txt".to_owned()])
            .await,
        Err(ToolError::ReadOnlyPolicy(_))
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn always_allow_scope_for_a_post_is_the_registrable_domain() {
    let domain = |raw: &str| registrable_domain(&reqwest::Url::parse(raw).expect("url"));
    assert_eq!(domain("https://api.example.com/hooks/1"), "example.com");
    assert_eq!(domain("https://upload.EXAMPLE.com/x"), "example.com");
    // 后两段截法会把这个截成 co.uk，一条规则放行整个英国二级域。
    assert_eq!(domain("https://example.co.uk/x"), "example.co.uk");
    assert_eq!(domain("https://api.example.co.uk/x"), "example.co.uk");
    assert_eq!(domain("https://203.0.113.7:8443/x"), "203.0.113.7");
    assert_eq!(domain("https://[2001:db8::1]/x"), "2001:db8::1");
}

#[tokio::test]
async fn a_remembered_post_rule_covers_every_subdomain_of_one_registrable_domain() {
    let root = workspace("web-post-approval");
    let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_approver(approver.clone());
    let post = |raw: &str| reqwest::Url::parse(raw).expect("url");
    registry
        .require_web_post_approval(
            &post("https://api.example.com/hooks/1"),
            12,
            "application/json",
        )
        .await
        .expect("first POST is approved and remembered");
    assert_eq!(approver.0.load(Ordering::SeqCst), 1);
    // 同一注册域名下换了子域和路径，规则仍然命中，不再打断用户。
    registry
        .require_web_post_approval(
            &post("https://upload.example.com/files"),
            9_000,
            "text/plain",
        )
        .await
        .expect("same registrable domain reuses the stored rule");
    assert_eq!(approver.0.load(Ordering::SeqCst), 1);
    // 换个域名就得重新问一次。
    registry
        .require_web_post_approval(
            &post("https://api.other.com/hooks/1"),
            12,
            "application/json",
        )
        .await
        .expect("a different domain is approved on its own");
    assert_eq!(approver.0.load(Ordering::SeqCst), 2);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn read_only_mode_refuses_a_post_before_any_approval() {
    let root = workspace("web-post-read-only");
    let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(&root, ApprovalMode::ReadOnly)
        .expect("registry")
        .with_approver(approver.clone());
    let result = registry
        .require_web_post_approval(
            &reqwest::Url::parse("https://api.example.com/hooks/1").expect("url"),
            12,
            "application/json",
        )
        .await;
    assert!(matches!(result, Err(ToolError::ReadOnlyPolicy(_))));
    assert_eq!(approver.0.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn web_fetch_methods_are_limited_to_get_and_post() {
    assert!(matches!(parse_web_method(None), Ok(WebMethod::Get)));
    assert!(matches!(parse_web_method(Some(" ")), Ok(WebMethod::Get)));
    assert!(matches!(
        parse_web_method(Some("post")),
        Ok(WebMethod::Post)
    ));
    assert!(parse_web_method(Some("PUT")).is_err());
}

#[tokio::test]
async fn public_web_reads_skip_approval_outside_strict_mode() {
    let root = workspace("web-read-approval");
    // 默认 approver 是 DenyApprover：只要还问，就会拿到 Err。
    for mode in [
        ApprovalMode::ReadOnly,
        ApprovalMode::Smart,
        ApprovalMode::WorkspaceAccess,
    ] {
        let registry = ToolRegistry::new(&root, mode).expect("registry");
        registry
            .require_network_read_approval("fetch public URL: https://example.com/")
            .await
            .expect("public web read should not need an approval");
    }
    let strict = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
    assert!(matches!(
        strict
            .require_network_read_approval("fetch public URL: https://example.com/")
            .await,
        Err(ToolError::ApprovalDenied(_))
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn smart_subagent_inherits_workspace_write_and_can_create_a_declared_file() {
    let root = workspace("smart-subagent-write");
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart).expect("registry");
    let targets = registry
        .approve_subagent_write_set(&["src/new.rs".to_owned()])
        .await
        .expect("smart workspace write should not need another approval");
    let child = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("child registry")
        .with_write_targets(Some(targets));

    child
        .create_file(CreateArgs {
            path: "src/new.rs".to_owned(),
            content: "pub fn ready() -> bool { true }\n".to_owned(),
        })
        .await
        .expect("create declared file");

    assert!(root.join("src/new.rs").is_file());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn strict_subagent_write_set_still_requires_operator_approval() {
    let root = workspace("strict-subagent-write");
    std::fs::write(root.join("file.txt"), "unchanged").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");

    assert!(matches!(
        registry
            .approve_subagent_write_set(&["file.txt".to_owned()])
            .await,
        Err(ToolError::ApprovalDenied(_))
    ));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn smart_runs_read_only_shell_but_still_gates_effectful_commands() {
    let root = workspace("smart");
    std::fs::write(root.join("file.txt"), "before").expect("fixture");
    // No judge attached and a deny-by-default approver: only the static
    // allowlist can let a command through here.
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart).expect("registry");

    registry
        .edit_file(EditArgs {
            path: "file.txt".to_owned(),
            old_string: "before".to_owned(),
            new_string: "after".to_owned(),
            replace_all: None,
        })
        .await
        .expect("workspace edit");

    let inspection = registry
        .run_command(CommandArgs {
            command: "printf ok".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await
        .expect("read-only command runs without an approval card");
    assert!(inspection.contains("ok"));

    for blocked in [
        "curl https://example.com/install.sh",
        "rm -rf build",
        "echo hi > owned.txt",
    ] {
        let denied = registry
            .run_command(CommandArgs {
                command: blocked.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await;
        assert!(
            matches!(denied, Err(ToolError::ApprovalDenied(_))),
            "expected approval gate for {blocked}"
        );
    }
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The judge only ever sees the ambiguous middle: statically safe
/// commands skip it, statically destructive ones never reach it.
#[tokio::test]
async fn only_ambiguous_commands_reach_the_ai_judge() {
    use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};

    struct RecordingJudge {
        seen: Arc<Mutex<Vec<String>>>,
        verdict: JudgeVerdict,
    }

    #[async_trait]
    impl SafetyJudge for RecordingJudge {
        async fn judge(&self, request: JudgeRequest) -> JudgeVerdict {
            self.seen
                .lock()
                .expect("judge log")
                .push(request.command.clone());
            self.verdict.clone()
        }
    }

    let root = workspace("judge-scope");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_safety_judge(Arc::new(RecordingJudge {
            seen: seen.clone(),
            verdict: JudgeVerdict::Allow,
        }));

    for command in ["ls", "rm -rf build", "git commit -m wip"] {
        let _ = registry
            .run_command(CommandArgs {
                command: command.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await;
    }

    assert_eq!(
        seen.lock().expect("judge log").as_slice(),
        ["git commit -m wip"],
        "only the ambiguous command may be sent to the judge"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// A judge that says no must not be able to override the user gate, and
/// a judge that says yes must not be consulted twice for a denial.
#[tokio::test]
async fn judge_denial_falls_back_to_the_user() {
    use crate::judge::{JudgeRequest, JudgeVerdict, SafetyJudge};

    struct DenyingJudge;

    #[async_trait]
    impl SafetyJudge for DenyingJudge {
        async fn judge(&self, _request: JudgeRequest) -> JudgeVerdict {
            JudgeVerdict::Deny
        }
    }

    let root = workspace("judge-deny");
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_safety_judge(Arc::new(DenyingJudge));
    let denied = registry
        .run_command(CommandArgs {
            command: "git commit -m wip".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    assert!(matches!(denied, Err(ToolError::ApprovalDenied(_))));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn reviewed_child_never_sends_dangerous_or_sensitive_commands_to_the_judge() {
    struct RecordingJudge(Arc<Mutex<Vec<String>>>);

    #[async_trait]
    impl SafetyJudge for RecordingJudge {
        async fn judge(&self, request: JudgeRequest) -> JudgeVerdict {
            self.0.lock().expect("seen").push(request.command);
            JudgeVerdict::Allow
        }
    }

    let root = workspace("reviewed-child-boundary");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_reviewed_subagent_shell(true)
        .with_safety_judge(Arc::new(RecordingJudge(seen.clone())));

    let reviewed = registry
        .run_command(CommandArgs {
            command: "printf reviewed > result.txt".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    assert!(
        reviewed.is_ok(),
        "AI-approved bounded command: {reviewed:?}"
    );
    for command in [
        "rm -rf build",
        "cat ~/.ssh/id_ed25519",
        "cat .env",
        "printenv",
    ] {
        let denied = registry
            .run_command(CommandArgs {
                command: command.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await;
        let Err(ToolError::ApprovalDenied(message)) = denied else {
            panic!("reviewed child must refuse {command}");
        };
        assert!(
            message.contains(command),
            "denial must return exact command"
        );
        assert!(message.contains("target_command"));
    }
    assert_eq!(
        seen.lock().expect("seen").as_slice(),
        ["printf reviewed > result.txt"]
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn human_preapproval_is_exact_and_does_not_authorize_a_decorated_command() {
    let root = workspace("reviewed-child-human");
    let exact = "printf human > exact.txt";
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_reviewed_subagent_shell(true)
        .with_preapproved_commands([exact.to_owned()]);
    registry
        .run_command(CommandArgs {
            command: exact.to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await
        .expect("exact human-authorized command");
    let decorated = registry
        .run_command(CommandArgs {
            command: format!("{exact} && printf extra"),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    assert!(matches!(decorated, Err(ToolError::ApprovalDenied(_))));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn search_files_returns_literal_matches_with_rg_or_fallback() {
    let root = workspace("search");
    std::fs::write(root.join("sample.rs"), "fn alpha() {}\n").expect("fixture");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict).expect("registry");
    let output = registry
        .search_files(SearchArgs {
            query: "ALPHA".to_owned(),
            max_results: None,
        })
        .expect("search");
    assert!(output.contains("sample.rs:1:"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn html_cleanup_removes_executable_content() {
    let text = html_to_text("<h1>Hello &amp; world</h1><script>secret()</script><p>Body</p>");
    assert!(text.contains("Hello & world"));
    assert!(text.contains("Body"));
    assert!(!text.contains("secret"));
}

#[test]
fn private_addresses_are_not_public() {
    assert!(!is_public_ip("127.0.0.1".parse().expect("IPv4")));
    assert!(!is_public_ip("10.0.0.1".parse().expect("IPv4")));
    assert!(!is_public_ip("::1".parse().expect("IPv6")));
    assert!(is_public_ip("1.1.1.1".parse().expect("IPv4")));
}

#[test]
fn redirect_policy_recognizes_same_hostname_across_https_upgrade() {
    let http = reqwest::Url::parse("http://example.com/old").expect("http URL");
    let https = reqwest::Url::parse("https://EXAMPLE.com/new").expect("https URL");
    let other = reqwest::Url::parse("https://cdn.example.com/new").expect("other URL");
    assert!(same_hostname(&http, &https));
    assert!(!same_hostname(&https, &other));
}

#[test]
fn redirect_loop_key_ignores_client_side_fragments() {
    let first = reqwest::Url::parse("https://example.com/page#first").unwrap();
    let second = reqwest::Url::parse("https://example.com/page#second").unwrap();
    assert_eq!(redirect_key(&first), redirect_key(&second));
}

#[test]
fn chunked_web_response_stops_at_the_hard_byte_limit() {
    let mut output = vec![0; MAX_WEB_RESPONSE_BYTES - 2];
    append_web_chunk(&mut output, &[1, 2]).unwrap();
    assert_eq!(output.len(), MAX_WEB_RESPONSE_BYTES);
    let error = append_web_chunk(&mut output, &[3]).unwrap_err();
    assert!(error.to_string().contains("3 MiB"));
    assert_eq!(output.len(), MAX_WEB_RESPONSE_BYTES);
}

#[tokio::test]
async fn subagent_write_target_rejects_every_other_file() {
    let root = workspace("subagent-target");
    std::fs::write(root.join("allowed.txt"), "before").expect("allowed fixture");
    std::fs::write(root.join("other.txt"), "before").expect("other fixture");
    let target = root
        .join("allowed.txt")
        .canonicalize()
        .expect("canonical target");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_write_targets(Some(BTreeSet::from([target])));

    registry
        .edit_file(EditArgs {
            path: "allowed.txt".to_owned(),
            old_string: "before".to_owned(),
            new_string: "after".to_owned(),
            replace_all: None,
        })
        .await
        .expect("approved target");
    let denied = registry
        .edit_file(EditArgs {
            path: "other.txt".to_owned(),
            old_string: "before".to_owned(),
            new_string: "after".to_owned(),
            replace_all: None,
        })
        .await;
    assert!(matches!(denied, Err(ToolError::OutsideWorkspace(_))));
    assert_eq!(
        std::fs::read_to_string(root.join("other.txt")).expect("other"),
        "before"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The file-set write channel is the single-file channel generalized, so
/// the same three gates have to hold for a set: every declared file is
/// writable, and anything outside it is refused with a path back to the
/// parent rather than a bare denial.
#[tokio::test]
async fn subagent_file_set_allows_every_declared_file_and_nothing_else() {
    let root = workspace("subagent-file-set");
    for name in ["impl.rs", "test.rs", "other.rs"] {
        std::fs::write(root.join(name), "before").expect("fixture");
    }
    let targets = ["impl.rs", "test.rs"]
        .iter()
        .map(|name| root.join(name).canonicalize().expect("canonical"))
        .collect::<BTreeSet<_>>();
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_write_targets(Some(targets));

    for name in ["impl.rs", "test.rs"] {
        registry
            .edit_file(EditArgs {
                path: name.to_owned(),
                old_string: "before".to_owned(),
                new_string: "after".to_owned(),
                replace_all: None,
            })
            .await
            .unwrap_or_else(|error| panic!("declared file {name} must be writable: {error}"));
    }
    let denied = registry
        .edit_file(EditArgs {
            path: "other.rs".to_owned(),
            old_string: "before".to_owned(),
            new_string: "after".to_owned(),
            replace_all: None,
        })
        .await;
    let Err(ToolError::OutsideWorkspace(message)) = denied else {
        panic!("a file outside the declared set must be refused");
    };
    assert!(
        message.contains("dispatched again"),
        "the refusal must tell the worker how to widen its scope, got: {message}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("other.rs")).expect("other"),
        "before"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The worker-skill hint is deterministic and main-agent-only: a listing
/// that surfaces a worker-tier skill carries the dispatch recipe, and a
/// child — which cannot spawn — never sees it.
#[test]
fn list_skills_hints_worker_dispatch_only_for_the_main_agent() {
    let root = workspace("skill-hint");
    let dir = root.join(".willdeep/skills/convert");
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: convert\ndescription: convert images\ntier: worker\n---\n# Steps",
    )
    .expect("skill");

    let skills = Arc::new(crate::skills::SkillCatalog::discover(&root, &[]));
    let main = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_skills(skills.clone())
        .with_delegation_hints(true);
    let listing = main
        .list_skills(ListSkillsArgs { query: None })
        .expect("list");
    assert!(listing.contains("tier=worker"));
    assert!(
        listing.contains("<delegation-hint tier=\"worker\">") && listing.contains("convert"),
        "the recipe must ride the listing: {listing}"
    );

    let child = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_skills(skills);
    let listing = child
        .list_skills(ListSkillsArgs { query: None })
        .expect("list");
    assert!(
        !listing.contains("delegation-hint"),
        "a child cannot spawn, so the hint is noise for it"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The history trade composes its own git queries, so its gate is a shape,
/// not a literal. The shape has to hold in both directions: any read-only
/// git command runs, and everything else — including commands the static
/// classifier would happily wave through for the main agent — does not.
#[tokio::test]
async fn a_read_only_git_worker_composes_git_queries_and_nothing_else() {
    let root = workspace("git-shell");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_read_only_git_shell(true);
    for command in [
        "git status",
        "git log -p -3",
        "git show HEAD~1",
        "git diff a b",
    ] {
        registry
            .run_command(CommandArgs {
                command: command.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await
            .unwrap_or_else(|error| panic!("read-only git must run ({command}): {error}"));
    }
    for command in [
        "ls",
        "git push origin main",
        "git commit -m x",
        "cat /etc/hosts",
    ] {
        let denied = registry
            .run_command(CommandArgs {
                command: command.to_owned(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await;
        assert!(
            matches!(denied, Err(ToolError::ApprovalDenied(_))),
            "`{command}` is not a read-only git query and must be refused"
        );
    }
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// A symlink above the workspace must not turn an approved file into a
/// forbidden one.
///
/// This is the failure the live-fire range found first: on macOS the
/// worker's workspace sat under `/var/...` (a symlink to `/private/var`),
/// the approved target kept the `/var` spelling, and the edit path was
/// canonicalized to `/private/var` before the comparison. The worker sent
/// the correct one-line patch on its first turn and was refused every
/// time — with a message naming the very path it had asked for. Both
/// sides of that comparison have to be canonical.
#[cfg(unix)]
#[tokio::test]
async fn an_approved_target_reached_through_a_symlink_is_still_writable() {
    let root = workspace("write-target-symlink");
    std::fs::write(root.join("impl.rs"), "before").expect("fixture");
    let link = std::env::temp_dir().join(format!("willdeep-link-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(&root, &link).expect("symlink");

    // The uncanonicalized spelling: exactly what a worktree root reached
    // through a symlinked parent hands over.
    let registry = ToolRegistry::new(&link, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_write_targets(Some(BTreeSet::from([link.join("impl.rs")])));
    registry
        .edit_file(EditArgs {
            path: "impl.rs".to_owned(),
            old_string: "before".to_owned(),
            new_string: "after".to_owned(),
            replace_all: None,
        })
        .await
        .expect("an approved file stays writable through a symlinked workspace");
    assert_eq!(
        std::fs::read_to_string(root.join("impl.rs")).expect("impl"),
        "after"
    );
    std::fs::remove_file(&link).expect("cleanup link");
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// A worker with a verifier may run that verifier and nothing else — not
/// even a command the static classifier would happily wave through.
#[tokio::test]
async fn a_command_allowlisted_worker_runs_only_its_verifier() {
    let root = workspace("verifier-allowlist");
    let registry = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_command_allowlist(Some(HashSet::from(["echo verified".to_owned()])));
    registry
        .run_command(CommandArgs {
            command: "echo verified".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await
        .expect("the declared verifier must run");
    let denied = registry
        .run_command(CommandArgs {
            command: "ls".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    assert!(
        matches!(denied, Err(ToolError::ApprovalDenied(_))),
        "a read-only command outside the allowlist must still be refused"
    );
    // A decorated verifier is the common near-miss, and the refusal has to
    // name the exact command that would work — otherwise the worker guesses
    // again, and each guess costs a turn.
    let decorated = registry
        .run_command(CommandArgs {
            command: "echo verified 2>&1".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    let Err(ToolError::ApprovalDenied(message)) = decorated else {
        panic!("a decorated verifier is not the declared command");
    };
    assert!(
        message.contains("echo verified"),
        "the refusal must quote the command that is allowed, got: {message}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn only_test_and_build_shaped_commands_are_delegable() {
    assert_eq!(
        delegable_failure_profile("cargo test -p willdeep-core"),
        Some("test_fixer")
    );
    assert_eq!(delegable_failure_profile("pytest -q"), Some("test_fixer"));
    assert_eq!(
        delegable_failure_profile("cargo clippy --all-targets"),
        Some("build_fixer")
    );
    assert_eq!(delegable_failure_profile("make -j8"), Some("build_fixer"));
    // Not every failing command is a fixable local defect.
    assert_eq!(delegable_failure_profile("git push origin main"), None);
    assert_eq!(delegable_failure_profile("curl https://example.com"), None);
}

/// The delegation hint is the deterministic half of "make workers visible",
/// so it has to survive the real command path: appended on a failing build
/// command for the main agent, absent for a subagent that cannot spawn.
#[cfg(unix)]
#[tokio::test]
async fn a_failing_test_command_carries_a_delegation_hint_for_the_main_agent_only() {
    let root = workspace("delegable-failure");
    // `cargo test` outside any crate: statically safe, always fails, instant.
    let hinted = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .with_delegation_hints(true)
        .run_command(CommandArgs {
            command: "cargo test -p willdeep-core".to_owned(),
            timeout_seconds: Some(30),
            label: None,
            run_in_background: None,
            network: None,
        })
        .await
        .expect("run cargo test");
    assert!(
        hinted.contains("test_fixer") && hinted.contains("delegation-hint"),
        "the main agent must be offered the test_fixer worker, got: {hinted}"
    );
    let plain = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .expect("registry")
        .run_command(CommandArgs {
            command: "cargo test -p willdeep-core".to_owned(),
            timeout_seconds: Some(30),
            label: None,
            run_in_background: None,
            network: None,
        })
        .await
        .expect("run cargo test");
    assert!(
        !plain.contains("delegation-hint"),
        "a subagent cannot spawn anything, so it must not be told to delegate: {plain}"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn approved_background_command_returns_handle_and_publishes_completion() {
    let root = workspace("background-command");
    let background = Arc::new(BackgroundTaskRegistry::default());
    let mut events = background.subscribe();
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_approver(Arc::new(AllowApprover))
        .with_background_tasks(background.clone());
    let command = if cfg!(windows) {
        "Write-Output background-ok"
    } else {
        "printf background-ok"
    };
    let result = registry
        .run_command(CommandArgs {
            command: command.to_owned(),
            timeout_seconds: Some(10),
            label: Some("test command".to_owned()),
            run_in_background: Some(true),
            network: None,
        })
        .await
        .expect("start");
    assert!(result.contains("job_"));
    let event = events.recv().await.expect("completion");
    assert_eq!(event.snapshot.status, BackgroundTaskStatus::Completed);
    assert!(
        background
            .output(&event.snapshot.id, 20)
            .expect("output")
            .contains("background-ok")
    );
    let retried = background.retry(&event.snapshot.id).expect("retry command");
    let retried_event = events.recv().await.expect("retry completion");
    assert_eq!(retried_event.snapshot.id, retried);
    assert_eq!(
        retried_event.snapshot.status,
        BackgroundTaskStatus::Completed
    );
    assert!(
        background
            .output(&retried, 20)
            .unwrap()
            .contains("background-ok")
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn always_allow_is_persisted_and_reused_for_exact_signature() {
    let root = workspace("always-allow");
    let store = root.join("rules.json");
    let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_approver(approver.clone())
        .with_always_allow_store(store.clone())
        .expect("store");
    registry
        .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
        .await
        .expect("first");
    registry
        .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
        .await
        .expect("remembered");
    assert_eq!(approver.0.load(Ordering::SeqCst), 1);
    let reloaded = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_always_allow_store(store)
        .expect("reload");
    reloaded
        .require_rememberable_approval("run cargo test", "command-exact:cargo test".to_owned())
        .await
        .expect("persisted");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn ask_user_accepts_custom_answer_and_escapes_markup() {
    let root = workspace("ask-user");
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_approver(Arc::new(AnswerApprover));
    let answer = registry
        .ask_user(AskUserArgs {
            question: "Choose language".to_owned(),
            options: Some(vec!["Rust".to_owned(), "Go".to_owned()]),
            multi_select: Some(false),
        })
        .await
        .expect("answer");
    assert_eq!(answer, "<user_answer>Other &lt;custom&gt;</user_answer>");
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn command_always_allow_signature_is_exact_and_rejects_shell_composition() {
    assert_eq!(
        command_signature(" cargo   test --all ").as_deref(),
        Some("command-exact:cargo test --all")
    );
    assert_eq!(command_signature("cargo test && deploy"), None);
}

/// A stored rule is the command verbatim. If a command carrying a key
/// could be remembered, the key would live in `always-allow.json`
/// indefinitely — so such commands are approvable but never rememberable.
#[test]
fn commands_carrying_credentials_are_never_rememberable() {
    for command in [
        "MODEL_API_KEY=sk_live_0123456789abcdef ruby scripts/probe.rb",
        "curl -H Authorization: Bearer sk-0123456789abcdef https://example.com",
        "mysql --password hunter2 -e select 1",
        "deploy --token ghp_0123456789abcdef",
    ] {
        assert_eq!(
            command_signature(command),
            None,
            "credential-bearing command must not mint a rule: {command}"
        );
    }
    // The guard must not swallow ordinary commands that merely mention a
    // key-shaped word without a value.
    assert_eq!(
        command_signature("grep -r api_key src").as_deref(),
        Some("command-exact:grep -r api_key src")
    );
}

/// The macOS app writes into this same file (`AgentSharedAlwaysAllowStore`),
/// and Foundation's `JSONEncoder` does not spell JSON the way `serde_json`
/// does: it pretty-prints with two spaces and escapes forward slashes as
/// `\/`. Both are legal JSON, but "legal" is not the same as "we checked".
/// The bytes below are a verbatim capture of that encoder's output.
///
/// The second half is the part that actually matters: a rule minted by the
/// app must equal the signature minted here. Two normalizations that agree
/// on the format but disagree on the string would leave both apps writing
/// rules the other can never match — a shared file that shares nothing.
#[tokio::test]
async fn a_store_written_by_the_macos_app_loads_and_matches_here() {
    let root = workspace("always-allow-swift");
    let store = root.join("rules.json");
    let swift_encoded = "[\n  \"command-exact:cargo test --all\",\n  \
         \"command-exact:git push origin main\",\n  \
         \"command-exact:ls \\/tmp\\/data\"\n]";
    std::fs::write(&store, swift_encoded).expect("seed store");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    let approver = Arc::new(AlwaysApprover(AtomicUsize::new(0)));
    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_approver(approver.clone())
        .with_always_allow_store(store)
        .expect("app-written store must load");

    // `\/` has to arrive as `/`, or the escaped rules silently never match.
    let signature = command_signature("ls  /tmp/data").expect("signature");
    assert_eq!(signature, "command-exact:ls /tmp/data");
    registry
        .require_rememberable_approval("run ls", signature)
        .await
        .expect("rule pinned by the app is honored here");
    assert_eq!(
        approver.0.load(Ordering::SeqCst),
        0,
        "the operator already approved this in the other app; asking again is the bug"
    );

    // A wider command in the same family is a different rule: the app pins
    // families locally but publishes only the exact command, so nothing
    // here may widen beyond what was approved.
    registry
        .require_rememberable_approval(
            "run cargo",
            command_signature("cargo test --all -- --nocapture").expect("signature"),
        )
        .await
        .expect("approved");
    assert_eq!(approver.0.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn credential_rules_are_pruned_from_an_existing_store_on_load() {
    let root = workspace("always-allow-prune");
    let store = root.join("rules.json");
    let leaked = "command-exact:API_KEY=sk_live_0123456789abcdef ruby probe.rb";
    let clean = "command-exact:cargo test";
    std::fs::write(
        &store,
        serde_json::to_vec(&vec![leaked.to_owned(), clean.to_owned()]).expect("encode"),
    )
    .expect("seed store");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    let registry = ToolRegistry::new(&root, ApprovalMode::Strict)
        .expect("registry")
        .with_always_allow_store(store.clone())
        .expect("store");
    let rules = registry
        .always_allowed
        .lock()
        .expect("always allow rules")
        .clone();
    assert!(rules.contains(clean), "clean rule must survive");
    assert!(!rules.contains(leaked), "leaked rule must be dropped");

    // The rewrite is the point: the secret must be gone from disk, not
    // merely ignored in memory.
    let on_disk = std::fs::read_to_string(&store).expect("read store");
    assert!(!on_disk.contains("sk_live_0123456789abcdef"));
    assert!(on_disk.contains("cargo test"));
    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The old hard-coded `cargo test | grep` carve-out is gone; the general
/// classifier must still cover everything it used to allow, and must
/// still refuse everything it used to refuse.
#[test]
fn smart_mode_still_covers_the_former_test_pipeline_carve_out() {
    use crate::safety::{CommandSafety, classify};
    assert_eq!(
        classify("cargo test -p willdeep 2>&1 | grep -E 'FAILED|warning' | head -40"),
        CommandSafety::AlwaysSafe
    );
    assert_eq!(
        classify("cargo test --workspace"),
        CommandSafety::AlwaysSafe
    );
    assert_ne!(classify("cargo run"), CommandSafety::AlwaysSafe);
    assert_ne!(
        classify("cargo test | tee result.txt"),
        CommandSafety::AlwaysSafe
    );
    assert_ne!(
        classify("cargo test > result.txt"),
        CommandSafety::AlwaysSafe
    );
    assert_eq!(
        classify("cargo test && touch owned"),
        CommandSafety::AlwaysSafe
    );
    assert_ne!(classify("cargo test $(danger)"), CommandSafety::AlwaysSafe);
}

#[test]
fn verification_reporting_is_bounded_and_rejects_sensitive_commands() {
    let reported = Arc::new(Mutex::new(Vec::new()));
    let sink = reported.clone();
    let reporter: VerificationReporter = Arc::new(move |value| {
        sink.lock().unwrap().push(value);
    });
    report_verification(
        Some(&reporter),
        "cargo test --workspace",
        Some(1),
        VerificationStatus::Failed,
        &"失败".repeat(10_000),
        Some("before-command".into()),
    );
    report_verification(
        Some(&reporter),
        "API_KEY=secret cargo test",
        Some(0),
        VerificationStatus::Passed,
        "ok",
        None,
    );
    report_verification(
        Some(&reporter),
        "cargo build",
        Some(0),
        VerificationStatus::Passed,
        "ok",
        None,
    );

    let values = reported.lock().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].command, "cargo test --workspace");
    assert_eq!(values[0].snapshot_id.as_deref(), Some("before-command"));
    assert_eq!(values[0].exit_code, Some(1));
    assert!(values[0].summary.len() <= MAX_VERIFICATION_SUMMARY_BYTES);
    assert!(std::str::from_utf8(values[0].summary.as_bytes()).is_ok());
}

#[tokio::test]
async fn verification_evidence_is_recorded_without_an_external_reporter() {
    let root =
        std::env::temp_dir().join(format!("willdeep-local-evidence-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let tools = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .unwrap()
        .with_approver(Arc::new(AllowApprover))
        .with_verification_snapshot(|| Some("current".into()));
    for (script, expected) in [("exit 7\n", false), ("exit 0\n", true)] {
        std::fs::write(root.join("test"), script).unwrap();
        tools
            .run_command(CommandArgs {
                command: "ruby test".into(),
                timeout_seconds: None,
                label: None,
                run_in_background: None,
                network: None,
            })
            .await
            .unwrap();
        assert_eq!(
            tools.completion_has_current_evidence(Some("before")),
            expected
        );
    }
    assert_eq!(tools.verification_records.lock().unwrap().recent.len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_verification_launch_error_invalidates_previous_success() {
    let root =
        std::env::temp_dir().join(format!("willdeep-launch-evidence-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let tools = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .unwrap()
        .with_approver(Arc::new(AllowApprover))
        .with_verification_snapshot(|| Some("current".into()));
    std::fs::write(root.join("test"), "exit 0\n").unwrap();
    let command = || CommandArgs {
        command: "ruby test".into(),
        timeout_seconds: None,
        label: None,
        run_in_background: None,
        network: None,
    };
    tools.run_command(command()).await.unwrap();
    assert!(tools.completion_has_current_evidence(Some("current")));
    std::fs::remove_dir_all(root).unwrap();
    assert!(tools.run_command(command()).await.is_err());
    assert!(!tools.completion_has_current_evidence(Some("current")));
    assert_eq!(
        tools
            .verification_records
            .lock()
            .unwrap()
            .recent
            .last()
            .unwrap()
            .status,
        VerificationStatus::Failed
    );
}

#[tokio::test]
async fn a_successful_shell_wrapper_cannot_hide_a_failed_test() {
    let root = std::env::temp_dir().join(format!("willdeep-masked-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("revision"), "before").unwrap();
    std::fs::write(root.join("test"), "File.write('ran', 'yes'); exit 7\n").unwrap();
    let capture_root = root.clone();
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = records.clone();
    let tools = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .unwrap()
        .with_approver(Arc::new(AllowApprover))
        .with_verification_snapshot(move || {
            std::fs::read_to_string(capture_root.join("revision")).ok()
        })
        .with_verification_reporter(move |record| sink.lock().unwrap().push(record));
    let baseline = tools.verification_baseline();
    std::fs::write(root.join("revision"), "after").unwrap();
    // Windows PowerShell 5.1 没有 `||`；`; exit 0` 同样吞掉失败的退出码，也同样是带
    // shell 运算符、不算测试证据的形状。
    let masked = if cfg!(windows) {
        "ruby test; exit 0"
    } else {
        "ruby test || true"
    };
    tools
        .execute(&ToolCall {
            id: "masked-test".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({ "command": masked }).to_string(),
        })
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(root.join("ran")).unwrap(), "yes");
    assert!(records.lock().unwrap().is_empty());
    assert!(!tools.completion_has_current_evidence(baseline.as_deref()));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn verification_retains_start_revision_but_invalidates_changed_files() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-verification-revision-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("revision"), "before").unwrap();
    std::fs::write(root.join("test"), "File.write('revision', 'after')\n").unwrap();
    let capture_root = root.clone();
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = records.clone();
    let tools = ToolRegistry::new(&root, ApprovalMode::WorkspaceAccess)
        .unwrap()
        .with_approver(Arc::new(AllowApprover))
        .with_verification_snapshot(move || {
            std::fs::read_to_string(capture_root.join("revision")).ok()
        })
        .with_verification_reporter(move |record| sink.lock().unwrap().push(record));
    tools
        .execute(&ToolCall {
            id: "verify".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({"command":"ruby test"}).to_string(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("revision")).unwrap(),
        "after"
    );
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].snapshot_id.as_deref(), Some("before"));
    assert_eq!(records[0].status, VerificationStatus::Failed);
    assert_eq!(records[0].exit_code, Some(0));
    assert!(records[0].summary.contains("verification-invalidated"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn write_denial_hint_names_the_two_real_ways_out_and_rules_out_asking() {
    let spec = SandboxSpec::new(SandboxPolicy::WorkspaceWrite, [std::env::temp_dir()]);
    let hint = sandbox_denial_hint(&spec, false);
    assert!(hint.contains("full-access"), "{hint}");
    assert!(hint.contains("Shift+Tab"), "{hint}");
    assert!(hint.contains("agent.sandbox_writable_roots"), "{hint}");
    assert!(hint.contains("重启会话"), "{hint}");
    assert!(hint.contains("ask_user"), "{hint}");
    // 不再给「放宽工作区策略」这种没有落点的说法，模型会拿它去问一个改不了围栏的问题。
    assert!(!hint.contains("放宽工作区策略"), "{hint}");
}

#[test]
fn multiline_or_spaced_commands_are_not_mistaken_for_credentials() {
    let multiline = "cd sdk && .venv/bin/python -c \"\nimport pathlib\nassert pathlib.Path('README.md').exists()\nprint('ok')\n\"";
    assert!(!child_command_is_sensitive(multiline));
    assert!(!child_command_is_sensitive("pytest  -q\t tests/"));
    // 真带凭据的照旧拦下，多行也一样。
    assert!(child_command_is_sensitive(
        "curl \\\n  --token sk-abcdefghijklmnop0123 https://example.com"
    ));
    assert!(child_command_is_sensitive("API_KEY=secret-value-123 ./run"));
}

/// 子 Agent 跟随父会话的 full-access（rocky 2026-09-21 决定）：实时跟随，切回即失效；
/// 工种自己的收窄（只许跑 verifier）不因此放宽。
#[tokio::test]
async fn a_child_follows_the_parent_into_full_access_and_back_out() {
    let root = workspace("child-inherits-full-access");
    let parent = SharedApprovalMode::new(ApprovalMode::Smart);
    // 没有判官：原本判官判不了的命令只能被拒，正好看得出免审有没有生效。
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_reviewed_subagent_shell(true)
        .with_parent_approval_mode(parent.clone());
    let run = |command: &str| {
        let registry = &registry;
        let command = command.to_owned();
        async move {
            registry
                .run_command(CommandArgs {
                    command,
                    timeout_seconds: None,
                    label: None,
                    run_in_background: None,
                    network: None,
                })
                .await
        }
    };
    let undecidable = "echo \"$(printf inherited)\" > inherited.txt";

    assert_eq!(registry.approval_mode(), ApprovalMode::Smart);
    assert!(matches!(
        run(undecidable).await,
        Err(ToolError::ApprovalDenied(_))
    ));

    parent.set(ApprovalMode::FullAccess);
    assert_eq!(registry.approval_mode(), ApprovalMode::FullAccess);
    assert!(
        !registry.effective_sandbox().policy.is_enforcing(),
        "full access drops the fence, as for the main agent"
    );
    run(undecidable)
        .await
        .expect("full access: no review, no card");

    parent.set(ApprovalMode::Smart);
    assert_eq!(registry.approval_mode(), ApprovalMode::Smart);
    assert!(
        matches!(run(undecidable).await, Err(ToolError::ApprovalDenied(_))),
        "switching the parent back re-arms review"
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test]
async fn full_access_does_not_widen_a_verifier_only_child() {
    let root = workspace("child-full-access-allowlist");
    let parent = SharedApprovalMode::new(ApprovalMode::FullAccess);
    let registry = ToolRegistry::new(&root, ApprovalMode::Smart)
        .expect("registry")
        .with_command_allowlist(Some(HashSet::from(["echo verified".to_owned()])))
        .with_parent_approval_mode(parent);
    let other = registry
        .run_command(CommandArgs {
            command: "echo something else".to_owned(),
            timeout_seconds: None,
            label: None,
            run_in_background: None,
            network: None,
        })
        .await;
    assert!(matches!(other, Err(ToolError::ApprovalDenied(_))));
    std::fs::remove_dir_all(root).expect("cleanup");
}
