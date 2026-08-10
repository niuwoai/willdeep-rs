use super::*;

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
        let runtime = TuiRuntime {
            home: root.clone(),
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
            tx,
            rx,
        };

        assert!(
            handle_session_command(
                &format!("/session switch {}", target.id),
                &mut app,
                &mut current,
                &store,
                &runtime,
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
    fn focus_cycles_through_prompt_chat_and_sidebar() {
        let mut app = App::new(Vec::new(), Language::En);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPane::Chat);
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
        app.transcript_rect = Rect::new(0, 0, 80, 20);

        app.handle_mouse(85, 5, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Sidebar);
        app.handle_mouse(5, 22, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Prompt);
        app.handle_mouse(5, 5, &registry, &skills);
        assert_eq!(app.focus, FocusPane::Chat);
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
    fn sidebar_renders_runtime_agent_lifecycle_summary() {
        let mut app = App::new(Vec::new(), Language::En);
        app.runtime_agents
            .push(crate::daemon::tui_bridge::RemoteAgent {
                id: "abe596f8-940d-4629-9a82-339796029947".parse().unwrap(),
                parent_id: None,
                label: Some("root".to_owned()),
                background: false,
                profile: Some("editor".to_owned()),
                status: RuntimeStatus::Done,
                current_turn: 3,
                current_tool: None,
                total_tokens: Some(42),
                max_turns: None,
                token_budget: None,
                timeout_seconds: None,
                report: None,
            });
        app.runtime_agents
            .push(crate::daemon::tui_bridge::RemoteAgent {
                id: "bd9d3df1-d3c7-4b5c-8ad4-c515830b0ea8".parse().unwrap(),
                parent_id: Some("abe596f8-940d-4629-9a82-339796029947".parse().unwrap()),
                label: Some("inspect".to_owned()),
                background: true,
                profile: Some("scout".to_owned()),
                status: RuntimeStatus::Working,
                current_turn: 1,
                current_tool: Some("read_file".to_owned()),
                total_tokens: Some(9),
                max_turns: Some(8),
                token_budget: Some(32_000),
                timeout_seconds: Some(300),
                report: Some("found src/main.rs".to_owned()),
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
        assert!(rendered.contains("root · T3/- · - · 42t/- · -s"));
        assert!(rendered.contains("inspect · T1/8 · read_file · 9t/32000t · 300s"));
        assert!(rendered.contains("↳ bd9d3d · scout bg · working"));
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
            description: "run tests".to_owned(),
            always_allow_available: true,
        });
        assert_eq!(app.selected_remote_gate().unwrap().id(), interaction_id);

        let task_id = uuid::Uuid::new_v4();
        app.runtime_attention.clear();
        app.runtime_gates.clear();
        app.runtime_attention.push(AttentionItem {
            id: format!("runtime-task:{task_id}"),
            source: AttentionSource::BackgroundShell,
            title: "Runtime task".to_owned(),
            detail: String::new(),
            status: RuntimeStatus::Working,
            elapsed_millis: None,
        });
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
            vec!["WillDeep: [Runtime 12345678] restored answer"]
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
        app.attention_detail = None;
        assert!(app.attention_mark_read());
        assert!(app.attention_items().is_empty());
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
