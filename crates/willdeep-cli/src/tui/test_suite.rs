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
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: "Tests".to_owned(),
                status: BackgroundTaskStatus::Failed,
                elapsed_millis: 50,
                exit_code: Some(1),
                output_bytes: 10,
            },
            BackgroundTaskSnapshot {
                id: "agent_working".to_owned(),
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
                kind: willdeep_core::BackgroundTaskKind::Shell,
                label: "Failed tests".to_owned(),
                status: BackgroundTaskStatus::Failed,
                elapsed_millis: 100,
                exit_code: Some(1),
                output_bytes: 10,
            },
            BackgroundTaskSnapshot {
                id: "job_done".to_owned(),
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
}
