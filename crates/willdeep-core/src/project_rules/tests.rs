use super::*;

fn workspace() -> PathBuf {
    let root = std::env::temp_dir().join(format!("rule-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    root
}

fn call(path: &str) -> ToolCall {
    ToolCall {
        id: "write".to_owned(),
        name: "create_file".to_owned(),
        arguments: serde_json::json!({"path":path,"content":"new"}).to_string(),
    }
}

#[test]
fn long_rules_are_complete_and_directory_overrides_have_sources() {
    let root = workspace();
    let long = format!(
        "{}\nMUST PRESERVE CONFIG AT THE END",
        "rule\n".repeat(2_000)
    );
    std::fs::write(root.join("AGENTS.md"), &long).unwrap();
    std::fs::write(root.join("src/AGENTS.md"), "Use the src convention").unwrap();
    std::fs::write(root.join("src/nested/CLAUDE.md"), "Nested exception").unwrap();
    std::fs::write(root.join("other/AGENTS.md"), "Unrelated rule").unwrap();
    let mut rules = ProjectRules::new(&root).unwrap();
    assert!(rules.render().contains(&long));
    assert!(rules.before_call(&call("src/nested/new.rs")).unwrap());
    let rendered = rules.render();
    assert!(rendered.contains("Source: src/AGENTS.md"));
    assert!(rendered.contains("Source: src/nested/CLAUDE.md"));
    assert!(
        rendered.find("Use the src convention").unwrap()
            < rendered.find("Nested exception").unwrap()
    );
    assert!(!rendered.contains("Unrelated rule"));
    assert!(!rules.before_call(&call("src/nested/new.rs")).unwrap());
}

#[test]
fn changed_and_new_rules_are_refreshed_before_a_repeat_action() {
    let root = workspace();
    let mut rules = ProjectRules::new(&root).unwrap();
    rules.before_call(&call("src/nested/new.rs")).unwrap();
    std::fs::write(root.join("src/AGENTS.md"), "New constraint").unwrap();
    assert!(rules.before_call(&call("src/nested/new.rs")).unwrap());
    assert!(rules.render().contains("New constraint"));
    std::fs::remove_file(root.join("src/AGENTS.md")).unwrap();
    assert!(rules.refresh().unwrap());
    assert!(!rules.render().contains("New constraint"));
}

#[test]
fn opaque_shell_paths_discover_rules_in_the_workspace() {
    let root = workspace();
    std::fs::write(root.join("other/AGENTS.md"), "Only other uses this rule").unwrap();
    let mut rules = ProjectRules::new(&root).unwrap();
    assert!(
        rules
            .before_call(&ToolCall {
                name: "run_command".to_owned(),
                ..call(".")
            })
            .unwrap()
    );
    assert!(rules.render().contains("Source: other/AGENTS.md"));
}

#[cfg(unix)]
#[test]
fn outside_rule_symlinks_are_not_loaded_as_project_instructions() {
    let root = workspace();
    let outside = workspace();
    std::fs::write(outside.join("rules.md"), "outside").unwrap();
    std::os::unix::fs::symlink(outside.join("rules.md"), root.join("AGENTS.md")).unwrap();
    assert!(ProjectRules::new(&root).is_err());
}

#[test]
fn opaque_tools_observe_rules_even_in_ignored_and_hidden_directories() {
    let root = workspace();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".gitignore"), "other/\n").unwrap();
    std::fs::write(root.join(".ignore"), "src/\n").unwrap();
    std::fs::create_dir_all(root.join(".private")).unwrap();
    for directory in ["other", "src/nested", ".private"] {
        std::fs::write(root.join(directory).join("AGENTS.md"), directory).unwrap();
    }
    std::fs::write(root.join(".git/AGENTS.md"), "Git internals are not rules").unwrap();
    for name in ["run_command", "spawn_agent", "call_mcp_tool", "mcp__test"] {
        let mut rules = ProjectRules::new(&root).unwrap();
        assert!(
            rules
                .before_call(&ToolCall {
                    name: name.into(),
                    ..call(".")
                })
                .unwrap()
        );
        let rendered = rules.render();
        for directory in ["other", "src/nested", ".private"] {
            assert!(
                rendered.contains(&format!("Source: {directory}/AGENTS.md")),
                "{name} omitted {directory}"
            );
        }
        assert!(!rendered.contains("Git internals are not rules"));
        assert!(
            !rules
                .before_call(&ToolCall {
                    name: name.into(),
                    ..call(".")
                })
                .unwrap()
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

struct ScopedWriter {
    requests: std::sync::atomic::AtomicUsize,
    path: PathBuf,
}

#[async_trait::async_trait]
impl crate::provider::Provider for ScopedWriter {
    async fn complete(
        &self,
        messages: &[crate::Message],
        _: &[crate::types::ToolDefinition],
    ) -> Result<crate::types::Completion, crate::provider::ProviderError> {
        let request = self
            .requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if request == 1 {
            assert!(
                !self.path.exists(),
                "new directory instructions must be observed before the write"
            );
            assert!(messages[0].content.contains("Nested exception"));
        }
        Ok(crate::types::Completion {
            reasoning: None,
            content: "done".to_owned(),
            tool_calls: if request < 2 {
                vec![call("src/nested/new.rs")]
            } else {
                Vec::new()
            },
            finish_reason: Some("stop".to_owned()),
            usage: None,
        })
    }
}

#[tokio::test]
async fn a_new_scope_defers_the_write_until_the_model_has_seen_its_rules() {
    let root = workspace();
    std::fs::write(root.join("src/nested/AGENTS.md"), "Nested exception").unwrap();
    let path = root.join("src/nested/new.rs");
    let agent = crate::Agent::new(
        std::sync::Arc::new(ScopedWriter {
            requests: 0.into(),
            path: path.clone(),
        }),
        crate::ToolRegistry::new(&root, crate::ApprovalMode::WorkspaceAccess).unwrap(),
        crate::AgentConfig {
            max_turns: 4,
            system_prompt: "test".to_owned(),
            context_window: 32_000,
            token_budget: None,
        },
    );
    agent.run("write file").await.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "new");
}
