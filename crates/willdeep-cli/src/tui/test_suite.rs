use super::*;

#[cfg(test)]
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn goal_command_enriches_future_prompts() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();

        assert!(app.handle_slash_command("/goal ship the CLI", &skills));
        let enriched = app.enrich_prompt("continue", &skills);

        assert!(enriched.contains("<goal>\nship the CLI\n</goal>"));
        assert!(enriched.ends_with("continue"));
        assert!(app.handle_slash_command("/goal off", &skills));
        assert_eq!(app.enrich_prompt("continue", &skills), "continue");
    }

    #[test]
    fn unknown_slash_command_is_handled_locally() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();

        assert!(app.handle_slash_command("/wat", &skills));
        assert!(app.transcript.last().unwrap().contains("unknown command"));
    }

    #[test]
    fn delegated_webapp_command_is_not_rejected_by_the_fallback_handler() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();

        assert!(!app.handle_slash_command("/webapp", &skills));
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn ordinary_prompt_is_not_treated_as_command() {
        let mut app = App::new(Vec::new(), Language::En);
        assert!(!app.handle_slash_command("please inspect /docs", &SkillCatalog::default()));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_tools() {
        let mut a = ToolActivity::default();
        a.requested("read_file");
        a.completed("read_file", true);
        assert!(a.summary(Language::En).contains("1 failed"));
    }

    #[test]
    fn command_menu_discovers_webapp() {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("/web");
        let matches = app.command_matches();
        assert!(matches.iter().any(|(command, _)| *command == "/webapp"));
    }

    #[test]
    fn welcome_mentions_workspace_without_entering_model_history() {
        let welcome = welcome_message(std::path::Path::new("/tmp/willdeep-rs"), Language::ZhCn);
        assert!(welcome.starts_with("WillDeep:"));
        assert!(welcome.contains("willdeep-rs"));
    }
    #[test]
    fn loading_another_session_replaces_transient_chat_state() {
        let workspace = std::env::temp_dir();
        let mut target = Session::new(workspace, None, "target");
        target.messages = vec![
            Message::user("target question"),
            Message::assistant("target answer", Vec::new()),
        ];
        target.attention_read.insert("read-item".to_owned());
        target.goal = Some("persisted target goal".to_owned());
        let mut app = App::new(vec!["old session output".to_owned()], Language::En);
        app.attachments.push(DraftAttachment {
            message: MessageAttachment::Text {
                name: "old.txt".to_owned(),
                content: "old".to_owned(),
            },
        });
        app.transient_thought = Some("old thought".to_owned());

        app.load_session(&target);

        assert_eq!(
            app.transcript,
            vec!["You: target question", "WillDeep: target answer"]
        );
        assert!(app.attachments.is_empty());
        assert!(app.transient_thought.is_none());
        assert!(app.attention_read.contains("read-item"));
        assert_eq!(app.goal.as_deref(), Some("persisted target goal"));
        assert_eq!(app.focus, FocusPane::Prompt);
    }
    #[tokio::test]
    async fn session_switch_replaces_the_open_session_without_restarting_tui() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-session-switch-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = SessionStore::new(&root);
        let mut current = Session::new(workspace.clone(), None, "current");
        store.save(&mut current).unwrap();
        let mut target = Session::new(workspace.clone(), None, "target");
        target.messages.push(Message::user("restored question"));
        target
            .messages
            .push(Message::assistant("restored answer", Vec::new()));
        store.save(&mut target).unwrap();
        let mut app = App::new(vec!["old transcript".to_owned()], Language::En);
        app.runtime_event_cursor = 17;
        let (tx, rx) = mpsc::unbounded_channel();
        target.profile = Some("target-provider".to_owned());
        target.model = Some("target-model".to_owned());
        target.config = Some(root.join("target-config.toml"));
        store.save(&mut target).unwrap();
        let mut runtime = TuiRuntime {
            home: root.clone(),
            notifier: crate::notify::Notifier::disabled(),
            skills: Arc::new(SkillCatalog::default()),
            relay_bridge: RelayBridge::new(),
            context_window: 128_000,
            background_tasks: Arc::new(BackgroundTaskRegistry::default()),
            runtime_submit: crate::daemon::RuntimeSubmitOptions {
                workspace,
                profile: None,
                model: None,
                config: None,
            },
            local_workspace: root.join("workspace"),
            tx,
            rx,
        };

        assert!(
            handle_session_command(
                &format!("/session switch {}", target.id),
                &mut app,
                &mut current,
                &store,
                &mut runtime,
            )
            .await
            .unwrap()
        );
        assert_eq!(current.id, target.id);
        assert_eq!(
            app.transcript[..2],
            ["You: restored question", "WillDeep: restored answer"]
        );
        assert_eq!(store.load(target.id).unwrap().runtime_event_cursor, 17);
        assert_eq!(
            runtime.runtime_submit.profile.as_deref(),
            Some("target-provider")
        );
        assert_eq!(
            runtime.runtime_submit.model.as_deref(),
            Some("target-model")
        );
        assert_eq!(runtime.runtime_submit.config, target.config);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn approval_shortcuts_are_colored_and_localized() {
        let lines = approval_content("run command", true, Language::Ja);
        let actions = lines.last().unwrap();
        assert_eq!(actions.spans[0].content, " Y ");
        assert_eq!(actions.spans[0].style.bg, Some(Color::Yellow));
        assert!(
            actions
                .spans
                .iter()
                .any(|span| span.content.contains("常に許可"))
        );
        let deny = actions
            .spans
            .iter()
            .find(|span| span.content == " N ")
            .unwrap();
        assert_eq!(deny.style.bg, Some(Color::Red));
        assert!(
            actions
                .spans
                .iter()
                .any(|span| span.content.contains("拒否"))
        );
    }
    #[test]
    fn long_paste_is_attachment_and_deletable() {
        let mut a = App::new(Vec::new(), Language::En);
        a.handle_paste("one\ntwo".to_owned());
        assert_eq!(a.attachments.len(), 1);
        a.delete_selected_attachment();
        assert!(a.attachments.is_empty());
    }
    #[test]
    fn cjk_wraps() {
        assert_eq!(visual_lines("中文", 2), 2);
    }
    #[test]
    fn transcript_height_uses_ratatui_word_wrapping() {
        let entries = vec!["WillDeep: 12345 12345 12345".to_owned()];

        assert_eq!(rendered_transcript_height(&entries, 10), 4);
        assert_eq!(visual_lines(&entries.join("\n"), 10), 3);
    }
    #[test]
    fn skill_menu_filters_and_inserts_selected_skill() {
        let workspace =
            std::env::temp_dir().join(format!("willdeep-tui-skill-menu-{}", uuid::Uuid::new_v4()));
        let skill_dir = workspace.join(".willdeep/skills/image-processing");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Image Processing\ndescription: Edit images\n---\n# Instructions",
        )
        .unwrap();
        let skills = SkillCatalog::discover(&workspace, &[]);
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("use $image-pro");

        assert!(app.handle_skill_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &skills));
        assert_eq!(app.input.text(), "use $image-processing ");

        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn command_menu_filters_and_inserts_without_executing() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.input.insert("/com");

        assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), "/compress");
        assert!(app.transcript.is_empty());
    }
    #[test]
    fn command_menu_discovers_session_management() {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("/sess");

        assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), "/session ");
        assert!(app.transcript.is_empty());
    }
    #[test]
    fn command_menu_discovers_workspace_switching() {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("/work");

        assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), "/workspace ");
        assert!(app.transcript.is_empty());
    }
    #[test]
    fn command_menu_discovers_diff_review() {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("/dif");

        assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), "/diff");
    }
    #[test]
    fn side_by_side_diff_pairs_replacements_and_preserves_cjk_width() {
        let rows = diff_side_by_side_rows("@@ -1 +1 @@\n-old 中文\n+new 中文\n context", 43);

        assert_eq!(rows.len(), 3);
        assert!(rows[1].contains("-old 中文"));
        assert!(rows[1].contains("+new 中文"));
        assert_eq!(UnicodeWidthStr::width(rows[1].as_str()), 43);
    }
    #[test]
    fn diff_rendering_expands_tabs_and_escapes_terminal_control_characters() {
        let lines = diff_review_lines("+\tmodel\u{1b}[2J\u{7}", None);
        let rendered = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "+   model\\u{1b}[2J\\u{7}");
        assert!(!rendered.chars().any(char::is_control));

        let rows = diff_side_by_side_rows("-\told\n+\tnew", 43);
        assert!(rows[0].contains("-   old"));
        assert!(rows[0].contains("+   new"));
        assert!(!rows[0].chars().any(char::is_control));
    }
    #[test]
    fn diff_area_cycles_without_skipping_a_scope() {
        use crate::daemon::diff_review::DiffArea;

        assert!(matches!(
            next_diff_area(DiffArea::Combined),
            DiffArea::Staged
        ));
        assert!(matches!(
            next_diff_area(DiffArea::Staged),
            DiffArea::Unstaged
        ));
        assert!(matches!(
            next_diff_area(DiffArea::Unstaged),
            DiffArea::Combined
        ));
    }
    #[test]
    fn session_fork_options_select_turn_provider_model_and_title() {
        let turn_id = uuid::Uuid::new_v4();
        let options = session_commands::parse_fork_options(&format!(
            "--through {turn_id} --profile research --model qwen3-max investigate branch"
        ))
        .unwrap();
        assert_eq!(options.through_turn_id, Some(turn_id));
        assert_eq!(options.provider_profile.as_deref(), Some("research"));
        assert_eq!(options.model.as_deref(), Some("qwen3-max"));
        assert_eq!(options.title.as_deref(), Some("investigate branch"));
        assert!(session_commands::parse_fork_options("--model").is_err());
        assert!(session_commands::parse_fork_options("--unknown value").is_err());
    }
    #[test]
    fn session_search_options_combine_structured_filters_and_text() {
        let parameters = session_commands::parse_search_options(
            "--status idle --profile research --model qwen3 --after 10 --before 20 durable answer",
        )
        .unwrap();
        assert!(parameters.contains(&("status".to_owned(), "idle".to_owned())));
        assert!(parameters.contains(&("profile".to_owned(), "research".to_owned())));
        assert!(parameters.contains(&("model".to_owned(), "qwen3".to_owned())));
        assert!(parameters.contains(&("updated_after".to_owned(), "10".to_owned())));
        assert!(parameters.contains(&("updated_before".to_owned(), "20".to_owned())));
        assert!(parameters.contains(&("q".to_owned(), "durable answer".to_owned())));
        assert!(session_commands::parse_search_options("--status").is_err());
    }
    #[test]
    fn sidebar_navigation_wraps_and_toggles_sections() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.sidebar_move(-1);
        assert_eq!(app.focus, FocusPane::Sidebar);
        assert_eq!(app.sidebar_selected, 3);

        app.sidebar_toggle();
        assert!(!app.sidebar_expanded[3]);
        app.sidebar_move(1);
        assert_eq!(app.sidebar_selected, 0);
    }
    #[test]
    fn focus_cycles_through_prompt_chat_activity_and_sidebar() {
        let mut app = App::new(Vec::new(), Language::En);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPane::Chat);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPane::Activity);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPane::Sidebar);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPane::Prompt);
    }
    #[test]
    fn clicking_sidebar_focuses_it_and_clicking_prompt_restores_prompt_focus() {
        let mut app = App::new(Vec::new(), Language::En);
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        app.sidebar_rect = Rect::new(80, 0, 20, 30);
        app.prompt_rect = Rect::new(0, 20, 80, 8);
        app.transcript_rect = Rect::new(0, 0, 80, 18);
        app.activity_rect = Rect::new(0, 18, 80, 2);

        app.handle_mouse(85, 5, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Sidebar);
        app.handle_mouse(5, 22, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Prompt);
        app.handle_mouse(5, 5, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Chat);
        app.handle_mouse(5, 18, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Activity);
    }
    #[test]
    fn clicking_sidebar_hits_toggles_sections_and_opens_task_detail() {
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_rect = Rect::new(80, 0, 20, 30);
        app.sidebar_hits = vec![(2, SidebarHit::Section(1)), (5, SidebarHit::Attention(0))];
        app.background_tasks.push(BackgroundTaskSnapshot {
            id: "job_test".to_owned(),
            agent_id: None,
            kind: willdeep_core::BackgroundTaskKind::Shell,
            label: "Run tests".to_owned(),
            status: BackgroundTaskStatus::Completed,
            elapsed_millis: 1200,
            settled_millis: Some(0),
            exit_code: Some(0),
            output_bytes: 12,
        });

        app.handle_mouse(85, 2, &registry, &skills);
        assert_eq!(app.sidebar_selected, 1);
        assert!(!app.sidebar_expanded[1]);
        app.handle_mouse(85, 5, &registry, &skills);
        assert_eq!(
            app.task_detail
                .as_ref()
                .map(|detail| detail.snapshot.id.as_str()),
            Some("job_test")
        );
    }
    #[test]
    fn sidebar_wheel_scrolls_content_without_changing_selected_section() {
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_selected = 2;
        app.sidebar_scroll_by(3);
        assert_eq!(app.sidebar_selected, 2);
        assert_eq!(app.sidebar_scroll, 3);
        assert!(app.sidebar_manual_scroll);
    }

    #[test]
    fn sidebar_render_clamps_extreme_manual_scroll_without_underflow() {
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_manual_scroll = true;
        app.sidebar_scroll = usize::MAX;
        let now = unix_now();
        app.runtime_agents
            .extend((0..8).map(|index| crate::daemon::tui_bridge::RemoteAgent {
                id: uuid::Uuid::new_v4(),
                parent_id: None,
                label: Some(format!("agent-{index}")),
                background: true,
                profile: Some("scout".to_owned()),
                model: None,
                status: RuntimeStatus::Done,
                current_turn: 1,
                current_tool: None,
                total_tokens: None,
                max_turns: None,
                token_budget: None,
                timeout_seconds: None,
                report: None,
                workspace: PathBuf::from("/workspace"),
                worktree_branch: None,
                dedicated_worktree: false,
                created_at: now - 5,
                completed_at: Some(now - 1),
            }));
        let backend = ratatui::backend::TestBackend::new(24, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| sidebar::render_sidebar(frame, &mut app, frame.area()))
            .unwrap();

        assert_ne!(app.sidebar_scroll, usize::MAX);
        assert!(app.sidebar_hits.iter().all(|(row, _)| *row < 6));
    }

    #[test]
    fn sidebar_renders_runtime_agent_lifecycle_summary() {
        let mut app = App::new(Vec::new(), Language::En);
        let now = unix_now();
        app.runtime_agents
            .push(crate::daemon::tui_bridge::RemoteAgent {
                id: "abe596f8-940d-4629-9a82-339796029947".parse().unwrap(),
                parent_id: None,
                label: Some("root".to_owned()),
                background: false,
                profile: Some("editor".to_owned()),
                model: Some("root-model".to_owned()),
                status: RuntimeStatus::Done,
                current_turn: 3,
                current_tool: None,
                total_tokens: Some(42),
                max_turns: None,
                token_budget: None,
                timeout_seconds: None,
                report: None,
                workspace: PathBuf::from("/workspace"),
                worktree_branch: None,
                dedicated_worktree: false,
                created_at: now - 1,
                completed_at: Some(now),
            });
        app.runtime_agents
            .push(crate::daemon::tui_bridge::RemoteAgent {
                id: "bd9d3df1-d3c7-4b5c-8ad4-c515830b0ea8".parse().unwrap(),
                parent_id: Some("abe596f8-940d-4629-9a82-339796029947".parse().unwrap()),
                label: Some("inspect".to_owned()),
                background: true,
                profile: Some("scout".to_owned()),
                model: Some("scout-model".to_owned()),
                status: RuntimeStatus::Working,
                current_turn: 1,
                current_tool: Some("read_file".to_owned()),
                total_tokens: Some(9),
                max_turns: Some(8),
                token_budget: Some(32_000),
                timeout_seconds: Some(300),
                report: Some("found src/main.rs".to_owned()),
                workspace: PathBuf::from("/worktrees/agent"),
                worktree_branch: Some("willdeep/agent-test".to_owned()),
                dedicated_worktree: true,
                created_at: now - 5,
                completed_at: None,
            });
        app.runtime_tools
            .push(willdeep_runtime_protocol::RuntimeTool {
                id: uuid::Uuid::new_v4(),
                session_id: None,
                turn_id: None,
                task_id: uuid::Uuid::new_v4(),
                agent_id: uuid::Uuid::new_v4(),
                name: "read_file".to_owned(),
                status: willdeep_runtime_protocol::ToolStatus::Running,
                started_at_ms: 1,
                completed_at_ms: None,
            });
        app.runtime_artifacts
            .push(willdeep_runtime_protocol::RuntimeArtifact {
                id: uuid::Uuid::new_v4(),
                kind: willdeep_runtime_protocol::ArtifactKind::WorkspaceChange,
                session_id: None,
                turn_id: None,
                task_id: uuid::Uuid::new_v4(),
                agent_id: uuid::Uuid::new_v4(),
                title: "edit_file workspace changes".to_owned(),
                source_id: "diff-1".to_owned(),
                item_count: 1,
                created_at: 1,
            });
        let backend = ratatui::backend::TestBackend::new(80, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                sidebar::render_sidebar(frame, &mut app, area);
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Runtime agents · 2"));
        assert!(rendered.contains("abe596 · editor · done"));
        assert!(rendered.contains("root · root-model · took 1s"));
        assert!(rendered.contains("T3/- · - · 42t/- · -s"));
        assert!(rendered.contains("inspect · scout-model · running for"));
        assert!(rendered.contains("T1/8 · read_file · 9t/32000t · 300s"));
        assert!(rendered.contains("↳ bd9d3d · scout bg · working"));
        assert!(rendered.contains("Tools: 1 · Running: 1 · Artifacts: 1"));
        assert!(rendered.contains("read_file · Running"));
        let footer = (0..80)
            .map(|x| terminal.backend().buffer()[(x, 30)].symbol())
            .collect::<String>();
        let version = format!("WillDeep v{}", willdeep_core::VERSION);
        assert!(footer.contains(&version));
        let version_x = 1 + (80 - 2 - version.chars().count() as u16);
        assert_eq!(
            terminal.backend().buffer()[(version_x, 30)].fg,
            Color::DarkGray
        );
        assert_eq!(
            app.selected_runtime_agent().unwrap().label.as_deref(),
            Some("root")
        );
        app.runtime_agent_move(1);
        assert_eq!(
            app.selected_runtime_agent().unwrap().label.as_deref(),
            Some("inspect")
        );
        app.runtime_agent_move(1);
        assert_eq!(app.runtime_agent_selected, 0);
    }

    #[test]
    fn sidebar_drops_long_finished_agents_and_keeps_selection_on_the_visible_ones() {
        let mut app = App::new(Vec::new(), Language::En);
        let now = unix_now();
        let stale_root = crate::daemon::tui_bridge::RemoteAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: None,
            label: Some("stale-root".to_owned()),
            background: false,
            profile: None,
            model: None,
            status: RuntimeStatus::Done,
            current_turn: 0,
            current_tool: None,
            total_tokens: None,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            report: None,
            workspace: PathBuf::from("/workspace"),
            worktree_branch: None,
            dedicated_worktree: false,
            created_at: now - 47_148,
            completed_at: Some(now - 43_200),
        };
        let live_child = crate::daemon::tui_bridge::RemoteAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: Some(uuid::Uuid::new_v4()),
            label: Some("live-child".to_owned()),
            background: true,
            profile: Some("scout".to_owned()),
            model: Some("scout-model".to_owned()),
            status: RuntimeStatus::Working,
            current_turn: 1,
            created_at: now - 5,
            completed_at: None,
            ..stale_root.clone()
        };
        app.runtime_agents.push(stale_root);
        app.runtime_agents.push(live_child);

        let backend = ratatui::backend::TestBackend::new(80, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| sidebar::render_sidebar(frame, &mut app, frame.area()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Runtime agents · 1 (+1 finished)"));
        assert!(rendered.contains("live-child"));
        assert!(!rendered.contains("stale-root"));
        assert_eq!(
            app.selected_runtime_agent().unwrap().label.as_deref(),
            Some("live-child")
        );
    }

    #[test]
    fn agent_detail_filters_tool_timeline_and_diff_by_agent() {
        let mut app = App::new(Vec::new(), Language::En);
        let agent_id = uuid::Uuid::new_v4();
        let other_id = uuid::Uuid::new_v4();
        let agent = crate::daemon::tui_bridge::RemoteAgent {
            id: agent_id,
            parent_id: None,
            label: Some("reader".to_owned()),
            background: false,
            profile: Some("reader".to_owned()),
            model: Some("detail-model".to_owned()),
            status: RuntimeStatus::Done,
            current_turn: 2,
            current_tool: None,
            total_tokens: Some(55),
            max_turns: Some(8),
            token_budget: Some(10_000),
            timeout_seconds: Some(120),
            report: Some("detail report".to_owned()),
            workspace: PathBuf::from("/workspace"),
            worktree_branch: None,
            dedicated_worktree: false,
            created_at: 1,
            completed_at: Some(2),
        };
        for (owner, name) in [(agent_id, "read_file"), (other_id, "git_status")] {
            app.runtime_tools
                .push(willdeep_runtime_protocol::RuntimeTool {
                    id: uuid::Uuid::new_v4(),
                    session_id: None,
                    turn_id: None,
                    task_id: uuid::Uuid::new_v4(),
                    agent_id: owner,
                    name: name.to_owned(),
                    status: willdeep_runtime_protocol::ToolStatus::Completed,
                    started_at_ms: 10,
                    completed_at_ms: Some(25),
                });
        }
        for (owner, title) in [(agent_id, "reader changes"), (other_id, "other changes")] {
            app.runtime_artifacts
                .push(willdeep_runtime_protocol::RuntimeArtifact {
                    id: uuid::Uuid::new_v4(),
                    kind: willdeep_runtime_protocol::ArtifactKind::WorkspaceChange,
                    session_id: None,
                    turn_id: None,
                    task_id: uuid::Uuid::new_v4(),
                    agent_id: owner,
                    title: title.to_owned(),
                    source_id: uuid::Uuid::new_v4().to_string(),
                    item_count: 2,
                    created_at: 1,
                });
        }

        let content = agent_worktree_ui::agent_detail_content(&app, &agent);
        assert!(content.contains("Tool timeline (1)"));
        assert!(content.contains("read_file · 15ms"));
        assert!(!content.contains("git_status"));
        assert!(content.contains("Diff summary (1)"));
        assert!(content.contains("reader changes"));
        assert!(!content.contains("other changes"));
        assert!(content.contains("detail report"));
    }
    #[test]
    fn agent_detail_scroll_is_bounded_to_the_wrapped_content() {
        let content = (0..20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            agent_worktree_ui::agent_detail_scroll_offset(&content, 20, 5, usize::MAX),
            15
        );
        assert_eq!(
            agent_worktree_ui::agent_detail_scroll_offset(&content, 20, 30, usize::MAX),
            0
        );
    }
    #[test]
    fn terminal_agent_detail_exposes_clickable_retry_model_and_diff_actions() {
        let mut app = App::new(Vec::new(), Language::En);
        app.agent_detail = Some(crate::daemon::tui_bridge::RemoteAgent {
            id: uuid::Uuid::new_v4(),
            parent_id: None,
            label: Some("editor".to_owned()),
            background: true,
            profile: Some("editor".to_owned()),
            model: Some("old-model".to_owned()),
            status: RuntimeStatus::Failed,
            current_turn: 2,
            current_tool: None,
            total_tokens: Some(55),
            max_turns: Some(8),
            token_budget: Some(10_000),
            timeout_seconds: Some(120),
            report: Some("failed report".to_owned()),
            workspace: PathBuf::from("/worktree"),
            worktree_branch: Some("willdeep/editor".to_owned()),
            dedicated_worktree: true,
            created_at: 1,
            completed_at: Some(2),
        });
        let backend = ratatui::backend::TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| agent_worktree_ui::render_agent_overlays(frame, &mut app))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("[R Retry]"));
        assert!(rendered.contains("[M Change model]"));
        assert!(rendered.contains("[W View Diff]"));
        for expected in [
            AgentDetailAction::Retry,
            AgentDetailAction::RetryWithModel,
            AgentDetailAction::ReviewWorktree,
        ] {
            let (rect, _) = app
                .agent_detail_action_rects
                .iter()
                .find(|(_, action)| *action == expected)
                .expect("action rect");
            assert_eq!(app.agent_detail_action_at(rect.x, rect.y), Some(expected));
        }
    }

    #[test]
    fn agent_command_prefill_never_overwrites_an_existing_draft() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        let id = uuid::Uuid::new_v4();
        app.input.insert("保留我的草稿");
        prefill_agent_command(
            &mut app,
            id,
            AgentDetailAction::RetryWithModel,
            Language::ZhCn,
        );
        assert_eq!(app.input.text(), "保留我的草稿");
        assert!(app.notice.as_deref().unwrap().contains("已有草稿"));

        app.input.take();
        prefill_agent_command(
            &mut app,
            id,
            AgentDetailAction::RetryWithModel,
            Language::ZhCn,
        );
        assert_eq!(app.input.text(), format!("/agent retry {id} --model "));
        assert_eq!(app.focus, FocusPane::Prompt);
    }
    #[test]
    fn help_opens_globally_but_question_mark_remains_typable_in_a_prompt() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        assert!(app.handle_help_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
        assert!(app.help_visible);
        assert!(app.handle_help_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.help_visible);

        app.input.insert("这是什么");
        assert!(!app.handle_help_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)));
    }
    #[test]
    fn help_documents_current_focus_and_sidebar_shortcuts() {
        assert_eq!(focus_label(FocusPane::Sidebar, Language::ZhCn), "状态栏");
        assert_eq!(focus_label(FocusPane::Activity, Language::ZhCn), "活动");
        let help = help_content(Language::ZhCn);
        assert!(help.contains("Ctrl+W"));
        assert!(help.contains("Enter 详情"));
        assert!(help.contains("K 停止"));
        assert!(help.contains("R 重试"));
        assert!(help.contains("M 已读"));
        assert!(help.contains("Ctrl+F"));
        assert!(help.contains("Ctrl+P"));
    }
    #[test]
    fn chat_search_filters_cycles_and_scrolls_to_matching_entries() {
        let mut app = App::new(
            vec![
                "You: first".to_owned(),
                "WillDeep: Alpha result".to_owned(),
                "You: middle".to_owned(),
                "WillDeep: alpha again".to_owned(),
            ],
            Language::En,
        );
        app.transcript_width = 40;
        app.viewport_height = 2;
        app.search = Some(SearchState::default());
        for character in "ALPHA".chars() {
            app.handle_search_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let search = app.search.as_ref().unwrap();
        assert_eq!(search.matches, vec![1, 3]);
        assert_eq!(search.selected, 0);
        assert!(!app.follow_bottom);

        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().selected, 1);
        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.search.as_ref().unwrap().selected, 0);
    }
    #[test]
    fn chat_search_highlights_matches_without_removing_markdown_styles() {
        let text = colored_transcript(&["WillDeep: **Alpha** and alpha".to_owned()], Some("alpha"));
        let highlighted = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.bg == Some(Color::Yellow))
            .collect::<Vec<_>>();

        assert_eq!(highlighted.len(), 2);
        assert!(highlighted[0].style.add_modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn command_palette_fuzzy_filters_and_inserts_a_command() {
        let workspace = std::env::temp_dir().join(format!(
            "willdeep-palette-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let session = Session::new(workspace.clone(), None, "Palette test");
        let registry = BackgroundTaskRegistry::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.workspace = Some(workspace.clone());
        let store = SessionStore::new(workspace.join("home"));
        app.open_palette(&SkillCatalog::default(), &store, &session);
        for character in "cmp".chars() {
            app.handle_palette_key(
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                &registry,
            );
        }

        let palette = app.palette.as_ref().unwrap();
        let labels = palette
            .filtered
            .iter()
            .map(|index| palette.items[*index].label.as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"/compress"));
        app.handle_palette_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &registry);
        assert_eq!(app.input.text(), "/compress");
        assert!(app.palette.is_none());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn workspace_file_palette_is_bounded_and_skips_heavy_directories() {
        let workspace = std::env::temp_dir().join(format!(
            "willdeep-palette-files-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(workspace.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(workspace.join("target/ignored"), "large").unwrap();

        let files = workspace_files(&workspace, 10);
        assert_eq!(files, vec!["src/main.rs"]);
        assert_eq!(fuzzy_score("smr", "src/main.rs"), Some(7));
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn transient_thought_is_single_line_and_bounded() {
        let value = compact_thought(&format!("first\n{}", "x".repeat(300)));
        assert!(!value.contains('\n'));
        assert!(value.chars().count() <= 181);
    }
    #[test]
    fn renders_common_markdown_for_terminal() {
        let lines = render_assistant_markdown(
            "# Title\n- **bold** and `code`\n[Docs](https://example.com)",
        );
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("■ Title"));
        assert!(rendered.contains("• bold and code"));
        assert!(rendered.contains("Docs (https://example.com)"));
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }
    #[test]
    fn encodes_clipboard_rgba_as_deletable_image() {
        let value = encode_clipboard_image(1, 1, vec![255, 0, 0, 255]).unwrap();
        assert!(matches!(
            value.message,
            MessageAttachment::Image {
                width: 1,
                height: 1,
                ..
            }
        ));
    }
    #[tokio::test]
    async fn ask_dialog_accepts_custom_text() {
        let mut app = App::new(Vec::new(), Language::En);
        let (sender, receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: false,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        });
        for value in "Other".chars() {
            app.handle_question_key(KeyEvent::new(KeyCode::Char(value), KeyModifiers::NONE));
        }
        app.handle_question_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(receiver.await.expect("answer").as_deref(), Some("Other"));
    }
    #[tokio::test]
    async fn ask_dialog_supports_multiple_selected_options() {
        let mut app = App::new(Vec::new(), Language::En);
        let (sender, receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: true,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        });
        app.handle_question_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(receiver.await.expect("answer").as_deref(), Some("A, B"));
    }
    #[tokio::test]
    async fn mouse_can_resolve_approval_and_single_choice_question() {
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        let mut app = App::new(Vec::new(), Language::En);
        let (approval_sender, approval_receiver) = oneshot::channel();
        app.approval = Some(("Run tests".to_owned(), true, approval_sender));
        app.approval_rect = Rect::new(10, 10, 60, 9);
        app.handle_mouse(35, 18, &registry, &skills);
        assert_eq!(
            approval_receiver.await.unwrap(),
            ApprovalDecision::AlwaysAllow
        );

        let (question_sender, question_receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: false,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender: question_sender,
        });
        app.question_rect = Rect::new(10, 10, 60, 10);
        app.question_hits = vec![(13, 0), (14, 1)];
        app.handle_mouse(20, 14, &registry, &skills);
        assert_eq!(question_receiver.await.unwrap().as_deref(), Some("B"));
    }

    #[test]
    fn runtime_attention_selects_remote_gate_and_task_actions() {
        let mut app = App::new(Vec::new(), Language::En);
        let interaction_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        app.runtime_attention.push(AttentionItem {
            id: format!("runtime-interaction:{interaction_id}"),
            source: AttentionSource::Approval,
            title: "Runtime approval".to_owned(),
            detail: "run tests".to_owned(),
            status: RuntimeStatus::WaitingApproval,
            elapsed_millis: None,
        });
        app.runtime_gates.push(crate::daemon::RemoteGate::Approval {
            id: interaction_id,
            task_id,
            description: "run tests".to_owned(),
            always_allow_available: true,
        });
        assert_eq!(app.selected_remote_gate().unwrap().id(), interaction_id);

        app.runtime_attention.clear();
        app.runtime_attention.push(AttentionItem {
            id: format!("runtime-task:{task_id}"),
            source: AttentionSource::BackgroundShell,
            title: "Runtime task".to_owned(),
            detail: String::new(),
            status: RuntimeStatus::Working,
            elapsed_millis: None,
        });
        assert_eq!(app.selected_remote_gate().unwrap().id(), interaction_id);
        assert_eq!(app.selected_runtime_task_id(), Some(task_id));
    }

    #[test]
    fn runtime_events_resume_by_cursor_without_duplicate_chat_rows() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-runtime-events-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "runtime test");
        let mut app = App::new(Vec::new(), Language::En);
        let event = crate::daemon::RemoteRuntimeEvent {
            sequence: 7,
            kind: "task.output".to_owned(),
            message: concat!(
                "task_id=12345678-0000-0000-0000-000000000000 ",
                "{\"type\":\"completed\",\"text\":\"restored answer\"}"
            )
            .to_owned(),
            visible: true,
            session_id: Some(session.id),
        };
        runtime_ui::apply_runtime_events(&mut app, vec![event.clone()], &mut session, &store)
            .unwrap();
        runtime_ui::apply_runtime_events(&mut app, vec![event], &mut session, &store).unwrap();
        assert_eq!(app.runtime_event_cursor, 7);
        assert_eq!(
            app.transcript
                .iter()
                .filter(|line| line.contains("restored answer"))
                .count(),
            1
        );
        assert_eq!(store.load(session.id).unwrap().runtime_event_cursor, 7);
        let restored = store.load(session.id).unwrap();
        assert_eq!(restored.messages.len(), 1);
        assert_eq!(
            transcript(&restored.messages),
            vec!["WillDeep: restored answer"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_chat_ignores_events_owned_by_another_session() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-runtime-event-isolation-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "current");
        let mut app = App::new(Vec::new(), Language::En);
        let event = crate::daemon::RemoteRuntimeEvent {
            sequence: 1,
            kind: "task.output".to_owned(),
            message: format!(
                "task_id={} {}",
                uuid::Uuid::new_v4(),
                serde_json::json!({"type":"completed","text":"private other answer"})
            ),
            visible: true,
            session_id: Some(uuid::Uuid::new_v4()),
        };
        runtime_ui::apply_runtime_events(&mut app, vec![event], &mut session, &store).unwrap();
        assert!(app.transcript.is_empty());
        assert!(session.messages.is_empty());
        assert_eq!(app.runtime_event_cursor, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_managed_session_reloads_core_history_at_turn_terminal() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-runtime-managed-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        let mut persisted = Session::new(root.clone(), None, "runtime managed");
        persisted.runtime_managed = true;
        store.save(&mut persisted).unwrap();
        let mut visible = persisted.clone();
        let mut app = App::new(Vec::new(), Language::En);
        let task_id = uuid::Uuid::new_v4();
        let output = crate::daemon::RemoteRuntimeEvent {
            sequence: 1,
            kind: "task.output".to_owned(),
            message: format!(
                "task_id={task_id} {}",
                serde_json::json!({"type":"completed","text":"answer"})
            ),
            visible: true,
            session_id: Some(visible.id),
        };
        let terminal = crate::daemon::RemoteRuntimeEvent {
            sequence: 2,
            kind: "turn.completed".to_owned(),
            message: format!(
                "session_id={} turn_id={} task_id={task_id}",
                visible.id,
                uuid::Uuid::new_v4()
            ),
            visible: true,
            session_id: Some(visible.id),
        };

        runtime_ui::apply_runtime_events(&mut app, vec![output], &mut visible, &store).unwrap();
        assert!(visible.messages.is_empty());
        assert!(store.load(visible.id).unwrap().messages.is_empty());

        persisted.messages = vec![
            Message::user("question"),
            Message::assistant("answer", Vec::new()),
        ];
        store.save(&mut persisted).unwrap();
        runtime_ui::apply_runtime_events(&mut app, vec![terminal], &mut visible, &store).unwrap();
        assert_eq!(
            transcript(&visible.messages),
            vec!["You: question", "WillDeep: answer"]
        );
        assert_eq!(visible.runtime_event_cursor, 2);
        assert_eq!(
            app.transcript
                .iter()
                .filter(|line| line.contains("answer"))
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn runtime_events_render_child_agent_activity() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-child-events-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "child events");
        let mut app = App::new(Vec::new(), Language::En);
        let task = "12345678-0000-0000-0000-000000000000";
        let child = "87654321-0000-0000-0000-000000000000";
        let event = |sequence, payload: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
            sequence,
            kind: "task.output".to_owned(),
            message: format!("task_id={task} {payload}"),
            visible: true,
            session_id: Some(session.id),
        };
        runtime_ui::apply_runtime_events(
            &mut app,
            vec![
                event(
                    1,
                    serde_json::json!({"type":"subagent_started","id":child,"profile":"scout"}),
                ),
                event(
                    2,
                    serde_json::json!({"type":"subagent_turn_started","id":child,"turn":1}),
                ),
                event(
                    3,
                    serde_json::json!({"type":"subagent_tool_requested","id":child,"name":"read_file"}),
                ),
                event(
                    4,
                    serde_json::json!({"type":"subagent_tool_completed","id":child,"name":"read_file","is_error":false}),
                ),
                event(
                    5,
                    serde_json::json!({"type":"subagent_completed","id":child,"status":"completed"}),
                ),
            ],
            &mut session,
            &store,
        )
        .unwrap();
        assert_eq!(app.runtime_event_cursor, 5);
        assert_eq!(app.tools.requested, 1);
        assert_eq!(app.tools.completed, 1);
        assert!(
            app.transcript.is_empty(),
            "runtime rounds, agent ids and tool activity belong in the status panel, not chat"
        );
        assert!(
            app.progress_log
                .iter()
                .any(|line| line.contains("87654321"))
        );
        assert!(
            app.progress_log
                .iter()
                .any(|line| line.contains("completed"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_chat_only_renders_the_assistant_answer() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-tui-runtime-chat-content-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "clean runtime chat");
        let mut app = App::new(Vec::new(), Language::En);
        let task = uuid::Uuid::new_v4();
        let event = |sequence, payload: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
            sequence,
            kind: "task.output".to_owned(),
            message: format!("task_id={task} {payload}"),
            visible: true,
            session_id: Some(session.id),
        };

        runtime_ui::apply_runtime_events(
            &mut app,
            vec![
                event(1, serde_json::json!({"type":"turn_started","turn":9})),
                event(
                    2,
                    serde_json::json!({"type":"tool_requested","name":"read_file"}),
                ),
                event(
                    3,
                    serde_json::json!({"type":"completed","text":"真实的 AI 回复"}),
                ),
            ],
            &mut session,
            &store,
        )
        .unwrap();

        assert_eq!(app.transcript, vec!["WillDeep: 真实的 AI 回复"]);
        assert!(app.transcript.iter().all(|line| {
            !line.contains("turn") && !line.contains("task_id") && !line.contains(&task.to_string())
        }));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn mouse_can_toggle_and_submit_multi_choice_question() {
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        let mut app = App::new(Vec::new(), Language::En);
        let (sender, receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: true,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        });
        app.question_rect = Rect::new(10, 10, 60, 10);
        app.question_hits = vec![(13, 0), (14, 1)];

        app.handle_mouse(20, 13, &registry, &skills);
        assert!(app.question.as_ref().unwrap().checked[0]);
        app.handle_mouse(60, 18, &registry, &skills);
        assert_eq!(receiver.await.unwrap().as_deref(), Some("A"));
    }
    #[test]
    fn mouse_can_place_cursor_in_chat_search() {
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        let mut app = App::new(Vec::new(), Language::En);
        let mut search = SearchState::default();
        search.editor.insert("abc");
        app.search = Some(search);
        app.search_rect = Rect::new(10, 2, 40, 3);

        app.handle_mouse(12, 3, &registry, &skills);
        app.handle_search_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().editor.text(), "aXbc");
    }
    #[test]
    fn wrapped_question_offsets_mouse_option_rows() {
        assert_eq!(question_option_row(10, "123456789", 4, 0), 15);
        assert_eq!(question_option_row(10, "123456789", 4, 1), 16);
    }
    #[test]
    fn attention_inbox_merges_human_gates_and_background_work() {
        let mut app = App::new(Vec::new(), Language::En);
        let (approval_sender, _approval_receiver) = oneshot::channel();
        app.approval = Some(("Run release".to_owned(), true, approval_sender));
        let (question_sender, _question_receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose target".to_owned(),
                options: vec!["A".to_owned()],
                multi_select: false,
            },
            selected: 0,
            checked: vec![false],
            answer: PromptEditor::default(),
            sender: question_sender,
        });
        app.background_tasks.extend([
            BackgroundTaskSnapshot {
                id: "job_failed".to_owned(),
                agent_id: None,
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: "Tests".to_owned(),
                status: BackgroundTaskStatus::Failed,
                elapsed_millis: 50,
                settled_millis: Some(0),
                exit_code: Some(1),
                output_bytes: 10,
            },
            BackgroundTaskSnapshot {
                id: "agent_working".to_owned(),
                agent_id: None,
                kind: willdeep_core::BackgroundTaskKind::Subagent,
                label: "Scout".to_owned(),
                status: BackgroundTaskStatus::Running,
                elapsed_millis: 20,
                settled_millis: None,
                exit_code: None,
                output_bytes: 0,
            },
        ]);

        let items = app.attention_items();
        assert_eq!(
            items.iter().map(|item| item.status).collect::<Vec<_>>(),
            vec![
                RuntimeStatus::WaitingApproval,
                RuntimeStatus::WaitingAnswer,
                RuntimeStatus::Failed,
                RuntimeStatus::Working,
            ]
        );
    }

    /// Inbox 是活动面板：顺利收尾的任务过一会儿自己走，
    /// 失败的赖着不走——那是还等着人处理的。
    #[test]
    fn attention_inbox_recycles_settled_tasks_but_keeps_failures() {
        let mut app = App::new(Vec::new(), Language::En);
        let task =
            |id: &str, status: BackgroundTaskStatus, settled_millis: u64| BackgroundTaskSnapshot {
                id: id.to_owned(),
                agent_id: None,
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: id.to_owned(),
                status,
                elapsed_millis: 50,
                settled_millis: Some(settled_millis),
                exit_code: Some(0),
                output_bytes: 0,
            };
        app.background_tasks.extend([
            task("job_fresh", BackgroundTaskStatus::Completed, 5_000),
            task("job_stale", BackgroundTaskStatus::Completed, 600_000),
            task("job_failed", BackgroundTaskStatus::Failed, 600_000),
        ]);

        let ids = app
            .attention_items()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"job_fresh".to_owned()));
        assert!(ids.contains(&"job_failed".to_owned()));
        assert!(!ids.contains(&"job_stale".to_owned()));
    }

    #[test]
    fn attention_inbox_navigates_opens_details_and_marks_terminal_items_read() {
        let registry = BackgroundTaskRegistry::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_selected = 1;
        app.background_tasks.extend([
            BackgroundTaskSnapshot {
                id: "job_failed".to_owned(),
                agent_id: None,
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: "Failed tests".to_owned(),
                status: BackgroundTaskStatus::Failed,
                elapsed_millis: 100,
                settled_millis: Some(0),
                exit_code: Some(1),
                output_bytes: 10,
            },
            BackgroundTaskSnapshot {
                id: "job_done".to_owned(),
                agent_id: None,
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: "Finished build".to_owned(),
                status: BackgroundTaskStatus::Completed,
                elapsed_millis: 50,
                settled_millis: Some(0),
                exit_code: Some(0),
                output_bytes: 10,
            },
        ]);

        assert_eq!(app.selected_attention().unwrap().id, "job_failed");
        app.attention_activate(&registry);
        assert_eq!(app.task_detail.as_ref().unwrap().snapshot.id, "job_failed");
        app.task_detail = None;
        assert!(app.attention_mark_read());
        assert_eq!(app.selected_attention().unwrap().id, "job_done");
        app.attention_move(-1);
        assert_eq!(app.selected_attention().unwrap().id, "job_done");
        assert!(app.attention_mark_read());
        assert!(app.attention_items().is_empty());
    }
    #[test]
    fn workspace_attention_opens_exact_detail_and_can_be_marked_read() {
        let registry = BackgroundTaskRegistry::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.workspace_attention.push(AttentionItem {
            id: "diff-review:abc".to_owned(),
            source: AttentionSource::DiffReview,
            title: "2 changed files ready for review".to_owned(),
            detail: "M src/a.rs · ?? docs/b.md".to_owned(),
            status: RuntimeStatus::WaitingApproval,
            elapsed_millis: None,
        });

        app.attention_activate(&registry);
        let detail = app.attention_detail.as_ref().expect("detail");
        assert_eq!(detail.id, "diff-review:abc");
        assert!(detail.detail.contains("src/a.rs"));
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| sidebar::render_attention_detail(frame, &mut app))
            .unwrap();
        assert_ne!(app.attention_diff_rect, Rect::default());
        assert_ne!(app.attention_allow_rect, Rect::default());
        assert_ne!(app.attention_deny_rect, Rect::default());
        assert_eq!(
            app.diff_attention_action_at(app.attention_diff_rect.x, app.attention_diff_rect.y),
            Some(DiffAttentionAction::Open)
        );
        assert_eq!(
            app.diff_attention_action_at(app.attention_allow_rect.x, app.attention_allow_rect.y),
            Some(DiffAttentionAction::Accept)
        );
        assert_eq!(
            app.diff_attention_action_at(app.attention_deny_rect.x, app.attention_deny_rect.y),
            Some(DiffAttentionAction::Reject)
        );
        app.attention_detail = None;
        assert!(app.attention_mark_read());
        assert!(app.attention_items().is_empty());
    }

    #[test]
    fn diff_attention_keyboard_shortcuts_are_captured_by_the_modal() {
        assert_eq!(
            diff_attention_action_for_key(KeyCode::Char('d')),
            Some(DiffAttentionAction::Open)
        );
        assert_eq!(
            diff_attention_action_for_key(KeyCode::Enter),
            Some(DiffAttentionAction::Open)
        );
        assert_eq!(
            diff_attention_action_for_key(KeyCode::Char('Y')),
            Some(DiffAttentionAction::Accept)
        );
        assert_eq!(
            diff_attention_action_for_key(KeyCode::Char('n')),
            Some(DiffAttentionAction::Reject)
        );
        assert_eq!(diff_attention_action_for_key(KeyCode::Char('x')), None);
    }

    #[test]
    fn text_selection_mode_uses_escape_or_ctrl_s_without_treating_copy_as_exit() {
        assert!(selection_mode_exit_key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE
        )));
        assert!(selection_mode_exit_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL
        )));
        assert!(!selection_mode_exit_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
    }

    #[test]
    fn diff_review_consumes_every_mouse_event_and_routes_wheel_to_the_modal() {
        assert_eq!(
            diff_review_mouse_action(true, MouseEventKind::ScrollUp),
            Some(DiffReviewMouseAction::ScrollUp)
        );
        assert_eq!(
            diff_review_mouse_action(true, MouseEventKind::ScrollDown),
            Some(DiffReviewMouseAction::ScrollDown)
        );
        assert_eq!(
            diff_review_mouse_action(true, MouseEventKind::Down(MouseButton::Left)),
            Some(DiffReviewMouseAction::Consume)
        );
        assert_eq!(
            diff_review_mouse_action(true, MouseEventKind::Moved),
            Some(DiffReviewMouseAction::Consume)
        );
        assert_eq!(
            diff_review_mouse_action(false, MouseEventKind::ScrollDown),
            None
        );
    }

    #[test]
    fn mouse_click_inserts_command_candidate() {
        let registry = BackgroundTaskRegistry::default();
        let skills = SkillCatalog::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("/com");
        app.command_rect = Rect::new(0, 0, 60, 4);
        app.command_hits = vec![(1, 0)];

        app.handle_mouse(5, 1, &registry, &skills);
        assert_eq!(app.input.text(), "/compress");
    }

    /// A second approval arriving mid-dialog used to overwrite the first,
    /// dropping its oneshot sender — the harness read that as a Deny the
    /// user never saw, and the turn died without explanation.
    #[test]
    fn a_second_approval_queues_instead_of_silently_denying_the_first() {
        let mut app = App::new(Vec::new(), Language::En);
        let (first_tx, mut first_rx) = oneshot::channel();
        let (second_tx, mut second_rx) = oneshot::channel();

        assert!(
            app.enqueue_approval(("run command: cargo build".to_owned(), true, first_tx)),
            "the first approval is shown immediately"
        );
        assert!(
            !app.enqueue_approval(("run command: git push".to_owned(), false, second_tx)),
            "the second approval waits its turn"
        );
        assert_eq!(app.approval_queue.len(), 1);
        // Neither sender has been resolved yet.
        assert!(first_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());

        app.resolve_approval(|_| ApprovalDecision::AllowOnce);
        assert_eq!(first_rx.try_recv(), Ok(ApprovalDecision::AllowOnce));
        // The queued one is promoted right away, not after the next event.
        assert!(app.approval.is_some());
        assert!(app.approval_queue.is_empty());
        assert_eq!(
            app.approval.as_ref().map(|(text, _, _)| text.as_str()),
            Some("run command: git push")
        );

        app.resolve_approval(|_| ApprovalDecision::Deny);
        assert_eq!(second_rx.try_recv(), Ok(ApprovalDecision::Deny));
        assert!(app.approval.is_none());
    }

    /// Switching sessions must not leave a harness parked forever, and must
    /// say so rather than dropping the channel on the floor.
    #[test]
    fn switching_sessions_denies_pending_approvals_visibly() {
        let mut app = App::new(Vec::new(), Language::En);
        let (tx, mut rx) = oneshot::channel();
        app.enqueue_approval(("run command: rm -rf build".to_owned(), false, tx));

        app.discard_pending_approvals();

        assert_eq!(rx.try_recv(), Ok(ApprovalDecision::Deny));
        assert!(app.approval.is_none());
        assert!(app.notice.is_some(), "the denial must be reported");
    }

    /// An arriving approval writes an activity line, so a user watching the
    /// progress column sees why the turn stopped moving.
    #[test]
    fn an_arriving_approval_reports_itself_in_the_activity_log() {
        let mut app = App::new(Vec::new(), Language::En);
        let (tx, _rx) = oneshot::channel();
        app.enqueue_approval((
            "call hub API\ncommand: curl https://example.com".to_owned(),
            false,
            tx,
        ));
        assert!(
            app.progress_log
                .iter()
                .any(|line| line.contains("Waiting for you") && line.contains("call hub API")),
            "progress log missing the approval line: {:?}",
            app.progress_log
        );
    }

    #[test]
    fn approval_title_reports_the_queue_depth() {
        assert_eq!(approval_title(Language::En, 0), "Approval required");
        assert_eq!(
            approval_title(Language::En, 2),
            "Approval required · more 2"
        );
    }

    /// Questions pop on arrival too, queue the same way, and must never
    /// clobber the draft the user was typing in the main input.
    #[test]
    fn questions_queue_and_preserve_the_draft_prompt() {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("half-written prompt");

        let (first_tx, mut first_rx) = oneshot::channel();
        let (second_tx, mut second_rx) = oneshot::channel();
        let dialog = |question: &str, sender| AskDialog {
            request: UserQuestion {
                question: question.to_owned(),
                options: vec!["a".to_owned(), "b".to_owned()],
                multi_select: false,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        };

        assert!(app.enqueue_question(dialog("which branch?", first_tx)));
        assert!(!app.enqueue_question(dialog("which remote?", second_tx)));
        assert_eq!(
            app.input.text(),
            "half-written prompt",
            "a popping question must not eat the draft"
        );
        assert!(first_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());

        app.handle_question_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(first_rx.try_recv(), Ok(Some("a".to_owned())));
        assert_eq!(
            app.question.as_ref().map(|d| d.request.question.as_str()),
            Some("which remote?"),
            "the queued question is promoted immediately"
        );

        app.handle_question_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(second_rx.try_recv(), Ok(None));
        assert!(app.question.is_none());
    }

    /// The status sidebar is a lookup surface, not a permanent one: it
    /// starts hidden and `/sidebar` brings it back.
    #[test]
    fn sidebar_starts_hidden_and_the_command_toggles_it() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();
        assert!(!app.sidebar_visible, "the sidebar must start hidden");

        assert!(app.handle_slash_command("/sidebar", &skills));
        assert!(app.sidebar_visible);
        assert!(app.handle_slash_command("/sidebar", &skills));
        assert!(!app.sidebar_visible, "a second /sidebar hides it again");

        // Explicit forms.
        assert!(app.handle_slash_command("/sidebar on", &skills));
        assert!(app.sidebar_visible);
        assert!(app.handle_slash_command("/sidebar off", &skills));
        assert!(!app.sidebar_visible);
        assert_eq!(
            app.focus,
            FocusPane::Prompt,
            "hiding the sidebar must not leave focus stranded on it"
        );

        // A bad argument reports usage instead of silently toggling.
        app.sidebar_visible = false;
        assert!(app.handle_slash_command("/sidebar sideways", &skills));
        assert!(!app.sidebar_visible);
        assert!(app.transcript.iter().any(|line| line.contains("usage:")));
    }

    /// Many Inbox rows have no action left except "stop showing me this" —
    /// a Runtime task that was interrupted days ago, for instance. Dismiss
    /// must work, must persist, and must refuse running items.
    #[test]
    fn inbox_items_can_be_dismissed_but_running_ones_cannot() {
        let mut app = App::new(Vec::new(), Language::En);
        app.runtime_attention.push(AttentionItem {
            id: "runtime-task:dead".to_owned(),
            source: AttentionSource::BackgroundShell,
            title: "Runtime task".to_owned(),
            detail: "Status: Interrupted".to_owned(),
            status: RuntimeStatus::Failed,
            elapsed_millis: Some(319_129_000),
        });
        app.runtime_attention.push(AttentionItem {
            id: "runtime-task:live".to_owned(),
            source: AttentionSource::BackgroundShell,
            title: "Runtime task".to_owned(),
            detail: String::new(),
            status: RuntimeStatus::Working,
            elapsed_millis: Some(1_000),
        });
        assert_eq!(app.attention_items().len(), 2);

        assert!(
            app.attention_dismiss("runtime-task:dead"),
            "a settled item must be dismissible"
        );
        let remaining = app.attention_items();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "runtime-task:live");
        assert!(
            app.attention_detail.is_none(),
            "dismissing closes the detail popup"
        );

        assert!(
            !app.attention_dismiss("runtime-task:live"),
            "a running item must not be hidden — it is the only handle on it"
        );
        assert_eq!(app.attention_items().len(), 1);
    }

    /// Failed background tasks are worth keeping around — but not forever.
    /// A command that failed yesterday is noise crowding out what actually
    /// needs attention today.
    #[test]
    fn background_tasks_are_recycled_by_status_and_age() {
        use crate::tui::sidebar::background_task_visible;
        use willdeep_core::{BackgroundTaskKind, BackgroundTaskSnapshot, BackgroundTaskStatus};

        let task = |status, settled_millis| BackgroundTaskSnapshot {
            id: "task".to_owned(),
            agent_id: None,
            kind: BackgroundTaskKind::Shell,
            label: "job".to_owned(),
            status,
            elapsed_millis: 10,
            settled_millis,
            exit_code: None,
            output_bytes: 0,
        };

        const MINUTE: u64 = 60_000;
        const HOUR: u64 = 60 * MINUTE;

        // Running tasks never expire.
        assert!(background_task_visible(&task(
            BackgroundTaskStatus::Running,
            None
        )));
        // Success: gone after a minute.
        assert!(background_task_visible(&task(
            BackgroundTaskStatus::Completed,
            Some(30_000)
        )));
        assert!(!background_task_visible(&task(
            BackgroundTaskStatus::Completed,
            Some(2 * MINUTE)
        )));
        // Failure: still visible hours later, gone after a day.
        for status in [
            BackgroundTaskStatus::Failed,
            BackgroundTaskStatus::TimedOut,
            BackgroundTaskStatus::Killed,
        ] {
            assert!(
                background_task_visible(&task(status.clone(), Some(6 * HOUR))),
                "{status:?} must survive six hours"
            );
            assert!(
                !background_task_visible(&task(status.clone(), Some(25 * HOUR))),
                "{status:?} must be recycled after a day"
            );
        }
    }

    /// `/daemon` and `/webapp` are handled by the main loop, not by
    /// `handle_slash_command`, so they must be declared as pass-through —
    /// otherwise they would be reported as unknown commands.
    #[test]
    fn runtime_and_webapp_commands_pass_through_to_the_main_loop() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();
        for command in [
            "/daemon",
            "/daemon upgrade",
            "/webapp",
            "/webapp stop",
            "/webapp status",
        ] {
            assert!(
                !app.handle_slash_command(command, &skills),
                "{command} must be handled by the main loop"
            );
        }
        assert!(
            app.transcript.is_empty(),
            "pass-through commands must not write an error line: {:?}",
            app.transcript
        );
        // A genuinely unknown command still reports itself.
        assert!(app.handle_slash_command("/nope", &skills));
        assert!(
            app.transcript
                .iter()
                .any(|line| line.contains("unknown command"))
        );
    }

    #[test]
    fn command_completion_offers_the_runtime_controls() {
        let matches = command_catalog::command_candidates(Language::En);
        for command in ["/daemon", "/webapp"] {
            assert!(
                matches.iter().any(|(name, _)| *name == command),
                "{command} missing from the completion catalog"
            );
        }
    }

    /// The TUI is only a front end. A Runtime started days ago keeps
    /// executing tools with its own approval policy, so a version mismatch
    /// must be visible — `willdeep --version` alone proves nothing about
    /// what actually runs commands.
    #[test]
    fn a_stale_runtime_version_is_announced_once_and_stays_flagged() {
        let mut app = App::new(Vec::new(), Language::En);
        assert_eq!(app.stale_runtime_version(), None, "no Runtime, no warning");

        app.observe_runtime_version(Some("0.21.0-rc62".to_owned()));
        assert_eq!(app.stale_runtime_version(), Some("0.21.0-rc62"));
        let warnings = |app: &App| {
            app.transcript
                .iter()
                .filter(|line| line.contains("does not match client"))
                .count()
        };
        assert_eq!(warnings(&app), 1);

        // Repeated snapshots keep the flag but must not spam the transcript.
        app.observe_runtime_version(Some("0.21.0-rc62".to_owned()));
        app.observe_runtime_version(Some("0.21.0-rc62".to_owned()));
        assert_eq!(warnings(&app), 1);
        assert!(app.stale_runtime_version().is_some());

        // A matching Runtime clears the warning entirely.
        app.observe_runtime_version(Some(willdeep_core::VERSION.to_owned()));
        assert_eq!(app.stale_runtime_version(), None);
        assert_eq!(warnings(&app), 1, "no new line for a healthy Runtime");

        // Handing off to another stale Runtime warns again.
        app.observe_runtime_version(Some("0.21.0-rc65".to_owned()));
        assert_eq!(warnings(&app), 2);
    }

    #[test]
    fn switching_sessions_drops_pending_questions_visibly() {
        let mut app = App::new(Vec::new(), Language::En);
        let (tx, mut rx) = oneshot::channel();
        app.enqueue_question(AskDialog {
            request: UserQuestion {
                question: "which branch?".to_owned(),
                options: Vec::new(),
                multi_select: false,
            },
            selected: 0,
            checked: Vec::new(),
            answer: PromptEditor::default(),
            sender: tx,
        });

        app.discard_pending_questions();

        assert_eq!(rx.try_recv(), Ok(None));
        assert!(app.question.is_none());
        assert!(app.notice.is_some(), "the drop must be reported");
    }

    #[test]
    fn ordinary_prompts_default_to_runtime_with_explicit_local_escape() {
        assert_eq!(
            prompt_execution("fix the tests"),
            PromptExecution::Runtime("fix the tests".to_owned())
        );
        assert_eq!(
            prompt_execution("/runtime inspect logs"),
            PromptExecution::Runtime("inspect logs".to_owned())
        );
        assert_eq!(
            prompt_execution("/local inspect process"),
            PromptExecution::Local("inspect process".to_owned())
        );
    }
}
