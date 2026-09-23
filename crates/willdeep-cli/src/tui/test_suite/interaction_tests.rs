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

/// 第一屏就得告诉用户从哪儿问路，否则命令表等于不存在。
#[test]
fn welcome_points_at_help_and_model_in_every_language() {
    for language in [Language::ZhCn, Language::En, Language::Ja] {
        let welcome = welcome_message(std::path::Path::new("/tmp/willdeep-rs"), language);
        assert!(welcome.contains("/help"), "{language:?} must mention /help");
        assert!(
            welcome.contains("/model"),
            "{language:?} must mention /model"
        );
    }
}

/// `/help` 曾经是一行写死的英文长串：中文界面看不懂，终端里也糊成一坨。
#[test]
fn help_is_localized_and_laid_out_one_command_per_line() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    let skills = SkillCatalog::default();

    assert!(app.handle_slash_command("/help", &skills));
    let help = app.transcript.last().expect("help").clone();

    let lines = help.lines().collect::<Vec<_>>();
    assert!(
        lines.len() >= 17,
        "every command needs its own line, got {}",
        lines.len()
    );
    assert!(lines[0].starts_with("System: "));
    assert!(
        !help.contains("prompts use Runtime by default"),
        "the old single-line English blob must be gone"
    );
    assert!(help.contains("命令一览"));
    assert!(help.contains("/model [模型名]"));
    assert!(help.contains("列出、筛选或切换当前模型"));
    for command in ["/help", "/compress", "/session", "/skills", "/clear"] {
        assert!(
            lines
                .iter()
                .any(|line| line.trim_start().starts_with(command)),
            "{command} must own a line"
        );
    }

    let mut english = App::new(Vec::new(), Language::En);
    assert!(english.handle_slash_command("/help", &skills));
    let english_help = english.transcript.last().expect("help").clone();
    assert!(english_help.contains("/model [model]"));
    assert!(
        !english_help.contains("命令一览"),
        "English help must not leak Chinese"
    );
}

/// 状态栏宽度有限，六位数的 token 读起来也费劲。
#[test]
fn token_counts_collapse_to_k_and_m_with_two_decimals() {
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1_000), "1.00K");
    assert_eq!(format_token_count(1_234), "1.23K");
    assert_eq!(format_token_count(994_999), "995.00K");
    // 四舍五入会越过 1000.00K 的都直接进位成 M。
    assert_eq!(format_token_count(999_999), "1.00M");
    assert_eq!(format_token_count(1_000_000), "1.00M");
    assert_eq!(format_token_count(1_234_567), "1.23M");
}

/// 「没报缓存」和「一次没命中」必须区分，否则状态栏会天天挂着 0.00%。
#[test]
fn cache_rate_is_hidden_unless_the_provider_reported_it() {
    let silent = Usage {
        input_tokens: Some(1_000),
        output_tokens: Some(10),
        total_tokens: Some(1_010),
        cache_read_tokens: None,
    };
    assert_eq!(cache_hit_rate(&silent), None);

    let missed = Usage {
        cache_read_tokens: Some(0),
        ..silent.clone()
    };
    assert_eq!(cache_hit_rate(&missed), Some(0.0));

    let hit = Usage {
        cache_read_tokens: Some(750),
        ..silent.clone()
    };
    assert_eq!(cache_hit_rate(&hit), Some(75.0));

    let no_input = Usage {
        input_tokens: Some(0),
        cache_read_tokens: Some(0),
        ..silent
    };
    assert_eq!(cache_hit_rate(&no_input), None);
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
        kernel: willdeep_core::EventKernel::new(),
        kernel_store: willdeep_core::kernel_store::KernelStore::new(&root),
        detached_jobs: willdeep_core::DetachedJobStore::new(&root),
        context_window: 128_000,
        background_tasks: Arc::new(BackgroundTaskRegistry::default()),
        runtime_submit: crate::daemon::RuntimeSubmitOptions {
            workspace,
            profile: None,
            model: None,
            config: None,
        },
        provider_config: willdeep_core::provider::ProviderConfig::new(
            willdeep_core::provider::ProviderKind::OpenAiCompatible,
            willdeep_core::provider::ApiDialect::ChatCompletions,
            "https://provider.example/v1",
            "test-key",
            "test-model",
        ),
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
fn approval_actions_are_stacked_localized_and_selectable() {
    let allow = approval_action_text(ApprovalDecision::AllowOnce, Language::Ja, true);
    let always = approval_action_text(ApprovalDecision::AlwaysAllow, Language::Ja, false);
    let deny = approval_action_text(ApprovalDecision::Deny, Language::Ja, false);

    assert!(allow.contains("Y / Enter"));
    assert!(allow.starts_with('▶'));
    assert!(always.contains("常に許可"));
    assert!(deny.contains("N / Esc"));
    assert!(deny.contains("拒否"));
    assert_eq!(
        approval_action_style(ApprovalDecision::AllowOnce, true).bg,
        Some(Color::LightCyan)
    );
    assert_eq!(
        approval_action_style(ApprovalDecision::Deny, false).fg,
        Some(Color::LightRed)
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

    // 三行折行 + 前缀独占的一行 + 回复结束后留出的一行空白。
    assert_eq!(rendered_transcript_height(&entries, 10), 5);
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
fn command_menu_discovers_model_switching() {
    let mut app = App::new(Vec::new(), Language::En);
    app.input.insert("/mod");

    assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert_eq!(app.input.text(), "/model");
}

#[test]
fn exact_slash_command_enter_falls_through_for_immediate_execution() {
    for command in ["/model", "/compress", "/diff", "/session"] {
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert(command);

        assert!(!app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), command);
    }
}

#[test]
fn model_picker_filters_and_navigates_long_model_lists() {
    let mut app = App::new(Vec::new(), Language::En);
    app.open_model_picker("provider/model-000".to_owned());
    let mut models = (0..125)
        .map(|index| format!("provider/model-{index:03}"))
        .collect::<Vec<_>>();
    models.push("special/vision-alpha".to_owned());
    app.set_model_picker_result(Ok(models));

    for _ in 0..35 {
        assert!(matches!(
            app.handle_model_picker_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            ModelPickerAction::None
        ));
    }
    assert_eq!(app.model_picker.as_ref().unwrap().selected, 35);
    app.handle_model_picker_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(app.model_picker.as_ref().unwrap().selected, 45);

    for character in "vision".chars() {
        app.handle_model_picker_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    let picker = app.model_picker.as_ref().unwrap();
    assert_eq!(picker.filtered.len(), 1);
    assert_eq!(picker.models[picker.filtered[0]], "special/vision-alpha");
    assert!(matches!(
        app.handle_model_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModelPickerAction::Select(model) if model == "special/vision-alpha"
    ));
}
#[test]
fn command_menu_discovers_workspace_switching() {
    let mut app = App::new(Vec::new(), Language::En);
    // “Worker” 也会命中模型路由说明；用命令名的唯一前缀验证 workspace。
    app.input.insert("/works");

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
fn expanded_composer_toggles_without_losing_the_draft() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.input.insert("第一行\n第二行");
    app.focus = FocusPane::Chat;

    app.toggle_composer_expanded();
    assert!(app.composer_expanded);
    assert_eq!(app.focus, FocusPane::Prompt);
    assert_eq!(app.input.text(), "第一行\n第二行");

    app.toggle_composer_expanded();
    assert!(!app.composer_expanded);
    assert_eq!(app.input.text(), "第一行\n第二行");
}
#[test]
fn expanded_composer_owns_the_terminal_body() {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(composer_layout_constraints(true, 5, 3, 8))
        .split(Rect::new(0, 0, 100, 30));

    assert_eq!(areas[0].height, 0);
    assert_eq!(areas[1].height, 0);
    assert_eq!(areas[2].height, 0);
    assert_eq!(areas[3].height, 29);
    assert_eq!(areas[4].height, 1);
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
            retry_wait: None,
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
            retry_wait: None,
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
            retry_wait: None,
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
fn sidebar_group_headings_use_terminal_foreground_with_bold_contrast() {
    let style = sidebar::sidebar_group_heading_style();

    assert_eq!(style.fg, None);
    assert!(style.add_modifier.contains(Modifier::BOLD));
    assert!(!style.add_modifier.contains(Modifier::DIM));
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
        retry_wait: None,
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
        retry_wait: Some(willdeep_runtime_protocol::AgentRetryWait {
            attempt: 2,
            delay_ms: 1500,
        }),
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
    assert!(rendered.contains("Waiting to retry"));
    let detail =
        agent_worktree_ui::agent_detail_content(&app, &app.selected_runtime_agent().unwrap());
    assert!(detail.contains("Waiting to retry · 2 · 2s"));
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
        retry_wait: None,
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
        retry_wait: None,
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
    assert!(help.contains("Ctrl+L 链接与图片面板"));
    assert!(help.contains("Alt+V"));
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
    let text = colored_transcript_at_width(
        &["WillDeep: **Alpha** and alpha".to_owned()],
        Some("alpha"),
        80,
    );
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
fn session_picker_edits_query_navigates_and_selects_archived_session() {
    let current_id = uuid::Uuid::new_v4();
    let target_id = uuid::Uuid::new_v4();
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(current_id, SessionPickerRequest::default());

    assert!(matches!(
        app.handle_session_picker_key(KeyEvent::new(KeyCode::Char('登'), KeyModifiers::NONE)),
        SessionPickerAction::Refresh
    ));
    assert_eq!(app.session_picker.as_ref().unwrap().editor.text(), "登");

    app.set_session_picker_results(vec![
        willdeep_runtime_protocol::SessionSearchResult {
            id: current_id,
            title: "当前会话".to_owned(),
            workspace: Some("/workspace".to_owned()),
            status: willdeep_runtime_protocol::SessionStatus::Idle,
            profile: None,
            model: None,
            updated_at: 1,
            message_count: 2,
            snippet: None,
            origin: willdeep_runtime_protocol::SessionOrigin::Runtime,
        },
        willdeep_runtime_protocol::SessionSearchResult {
            id: target_id,
            title: "登录设计".to_owned(),
            workspace: Some("/workspace".to_owned()),
            status: willdeep_runtime_protocol::SessionStatus::Archived,
            profile: None,
            model: None,
            updated_at: 2,
            message_count: 8,
            snippet: Some("讨论 OAuth 登录".to_owned()),
            origin: willdeep_runtime_protocol::SessionOrigin::Runtime,
        },
    ]);
    assert!(matches!(
        app.handle_session_picker_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        SessionPickerAction::None
    ));
    match app.handle_session_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
        SessionPickerAction::Switch(target) => {
            assert_eq!(target.id, target_id.to_string());
            assert!(target.archived);
        }
        _ => panic!("expected Session switch"),
    }
}
#[test]
fn history_and_session_search_commands_open_the_same_panel() {
    // `/history` 不带参数：面板列最近会话，输入框留空。
    let bare = parse_session_picker_command("/history")
        .unwrap()
        .expect("/history opens the panel");
    assert!(bare.query.is_empty());
    assert!(bare.filters.is_empty());

    // `/session search` 带关键词与过滤器：关键词进输入框，过滤器随刷新走。
    let searched = parse_session_picker_command("/session search --status archived 登录设计")
        .unwrap()
        .expect("/session search opens the panel");
    assert_eq!(searched.query, "登录设计");
    assert_eq!(
        searched.filters,
        vec![("status".to_owned(), "archived".to_owned())]
    );

    // 两条命令进的是同一个面板，初始状态各按各的参数落位。
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(uuid::Uuid::new_v4(), searched);
    let picker = app.session_picker.as_ref().unwrap();
    assert_eq!(picker.editor.text(), "登录设计");
    assert_eq!(
        picker.filters,
        vec![("status".to_owned(), "archived".to_owned())]
    );

    // 不归它管的提示词必须原样放行，否则 `/session switch` 之类会被吞掉。
    assert!(
        parse_session_picker_command("/session switch 0")
            .unwrap()
            .is_none()
    );
    assert!(
        parse_session_picker_command("/historian")
            .unwrap()
            .is_none()
    );
    // 多打一个空格不该变成另一条命令。
    assert_eq!(
        parse_session_picker_command("/session  search  登录")
            .unwrap()
            .expect("extra spaces still open the panel")
            .query,
        "登录"
    );
    assert!(
        parse_session_picker_command("讲讲 /history")
            .unwrap()
            .is_none()
    );
    assert!(parse_session_picker_command("/history --wat x").is_err());
}
#[test]
fn session_picker_keeps_only_the_twenty_most_recent_results() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(uuid::Uuid::new_v4(), SessionPickerRequest::default());

    app.set_session_picker_results(recent_session_results(25));

    let picker = app.session_picker.as_ref().unwrap();
    assert_eq!(picker.results.len(), 20);
    // Runtime 已按更新时间倒序，截断只能砍掉尾巴，不能改动顺序。
    assert_eq!(picker.results.first().unwrap().title, "会话 0");
    assert_eq!(picker.results.last().unwrap().title, "会话 19");
    assert!(picker.truncated, "被截断时标题要显示 20+");
}
#[cfg(test)]
fn recent_session_results(count: u64) -> Vec<willdeep_runtime_protocol::SessionSearchResult> {
    (0..count)
        .map(|index| willdeep_runtime_protocol::SessionSearchResult {
            id: uuid::Uuid::new_v4(),
            title: format!("会话 {index}"),
            workspace: Some("/workspace".to_owned()),
            status: willdeep_runtime_protocol::SessionStatus::Idle,
            profile: None,
            model: None,
            updated_at: 100 - index,
            message_count: 1,
            snippet: None,
            origin: willdeep_runtime_protocol::SessionOrigin::Runtime,
        })
        .collect()
}
#[test]
fn session_picker_panel_renders_rows_and_click_enters_that_session() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(uuid::Uuid::new_v4(), SessionPickerRequest::default());
    let mut results = recent_session_results(25);
    let target = results[1].id;
    results[1].title = "登录设计".to_owned();
    app.set_session_picker_results(results);

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| session_picker_ui::render_session_picker(frame, &mut app))
        .unwrap();
    // 双宽字符在缓冲区里占两格，第二格是空白；比对前先把空格抹掉。
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .replace(' ', "");

    assert!(rendered.contains("历史会话（当前工作区）"), "{rendered}");
    // 25 条只留 20 条，标题必须写成 20+ 而不是谎称一共 20 条。
    assert!(rendered.contains("1/20+"), "标题缺少截断标记: {rendered}");
    assert!(
        rendered.contains("▶会话0"),
        "首行未选中或未渲染: {rendered}"
    );
    assert!(rendered.contains("登录设计"));

    // 点第二行 = 进那条会话：面板关闭，切换请求排给事件循环。
    let (row, _) = app.session_picker_hits[1];
    assert!(app.activate_session_picker_at(app.session_picker_rect.x + 2, row));
    assert_eq!(
        app.pending_session_switch.as_ref().unwrap().id,
        target.to_string()
    );
    assert!(app.session_picker.is_none());
}
/// 开一次 TUI 不敲字就关，此前磁盘上就多一条 0 消息会话。它们按更新时间
/// 排在最前，能把 20 个名额吃光，真正的历史一条都露不出来。
#[test]
fn empty_sessions_are_kept_out_of_the_panel_except_the_current_one() {
    let current = uuid::Uuid::new_v4();
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(current, SessionPickerRequest::default());

    let mut results = recent_session_results(3);
    // 当前会话也是空的（刚开的 TUI），但人得看得见自己在哪儿。
    results[0].id = current;
    results[0].message_count = 0;
    results[1].message_count = 0;
    results[2].message_count = 7;
    let survivor = results[2].id;
    app.set_session_picker_results(results);

    let listed = app
        .session_picker
        .as_ref()
        .unwrap()
        .results
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(listed, vec![current, survivor], "{listed:?}");
}

/// 过滤必须发生在截断之前，否则空会话先占满 20 个名额再被删掉，
/// 面板会剩下一张几乎空的列表，还谎称「20+」。
#[test]
fn empty_sessions_are_dropped_before_the_twenty_row_cap() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_session_picker(uuid::Uuid::new_v4(), SessionPickerRequest::default());

    let mut results = recent_session_results(25);
    for result in results.iter_mut().take(20) {
        result.message_count = 0;
    }
    app.set_session_picker_results(results);

    let picker = app.session_picker.as_ref().unwrap();
    assert_eq!(picker.results.len(), 5);
    assert!(!picker.truncated, "5 条真实会话不该被标成截断");
    assert!(picker.results.iter().all(|result| result.message_count > 0));
}

/// 标题排第一位，Xedit 桥接的会话要有来源标记。
///
/// 此前这一行是「标题 · ID · 状态 · 条数」，而当标题清一色是 `New session`
/// 时，一屏里唯一在变的东西就是那八位十六进制——那不是列表，是让人用
/// UUID 认对话。ID 仍然留着，只是让位给标题。
#[test]
fn session_picker_line_leads_with_the_title_and_flags_bridged_sessions() {
    let bridged = willdeep_runtime_protocol::SessionSearchResult {
        id: uuid::Uuid::new_v4(),
        title: "代码提交一下".to_owned(),
        workspace: Some("/workspace".to_owned()),
        status: willdeep_runtime_protocol::SessionStatus::Idle,
        profile: None,
        model: None,
        updated_at: 1,
        message_count: 22,
        snippet: None,
        origin: willdeep_runtime_protocol::SessionOrigin::Xedit,
    };
    let line =
        session_picker_ui::session_picker_result_line(&bridged, false, Language::ZhCn, false);
    assert!(line.starts_with("  代码提交一下 [Xedit]"), "{line}");
    assert!(line.contains("22 条消息"), "{line}");
    assert!(
        line.contains(&bridged.id.simple().to_string()[..8]),
        "ID 仍要留在行里，出问题时人得靠它对日志: {line}"
    );

    // Runtime 来源是默认，不加噪音标记。
    let managed = willdeep_runtime_protocol::SessionSearchResult {
        origin: willdeep_runtime_protocol::SessionOrigin::Runtime,
        ..bridged.clone()
    };
    let line =
        session_picker_ui::session_picker_result_line(&managed, false, Language::ZhCn, false);
    assert!(!line.contains('['), "{line}");
}

/// 轮次结束后的下一句预测：空输入框里 Tab 采用，只填入不发送。
#[test]
fn input_suggestion_is_accepted_with_tab_and_only_filled_in() {
    let mut app = App::new(Vec::new(), Language::En);
    let epoch = app.input_suggestion_epoch;
    assert!(app.adopt_input_suggestion(Some("run the tests again".to_owned()), epoch));
    assert_eq!(app.visible_input_suggestion(), Some("run the tests again"));

    assert!(app.accept_input_suggestion());
    assert_eq!(app.input.text(), "run the tests again");
    assert!(
        app.input_suggestion.is_none(),
        "accepted once, gone afterwards"
    );
    assert!(
        !app.accept_input_suggestion(),
        "Tab with text in the box is not ours to handle"
    );
}

/// 打字或 Esc 都放弃预测，删光了也不回来。
#[test]
fn typing_or_escape_dismisses_the_input_suggestion() {
    let mut app = App::new(Vec::new(), Language::En);
    let epoch = app.input_suggestion_epoch;
    assert!(app.adopt_input_suggestion(Some("commit it".to_owned()), epoch));

    app.edit_input(|input| input.insert("n"));
    assert!(app.visible_input_suggestion().is_none());
    assert!(app.input_suggestion.is_none());
    app.edit_input(|input| input.backspace());
    assert!(app.input.is_empty());
    assert!(
        app.visible_input_suggestion().is_none(),
        "a dismissed suggestion does not come back"
    );

    let epoch = app.input_suggestion_epoch;
    assert!(app.adopt_input_suggestion(Some("commit it".to_owned()), epoch));
    assert!(app.dismiss_input_suggestion());
    assert!(app.visible_input_suggestion().is_none());
    assert!(
        !app.dismiss_input_suggestion(),
        "Esc with nothing to dismiss falls through to its usual meaning"
    );
}

/// 晚到的预测对不上世代号就丢；在跑、已打字、有附件时也不落地。
#[test]
fn late_input_suggestions_are_dropped_when_the_world_moved_on() {
    let mut app = App::new(Vec::new(), Language::En);
    let stale_epoch = app.input_suggestion_epoch;
    app.begin_turn(false, "working".to_owned());
    assert!(
        !app.adopt_input_suggestion(Some("stale".to_owned()), stale_epoch),
        "a new turn turns the page"
    );
    app.finish_turn();

    let epoch = app.input_suggestion_epoch;
    app.input.insert("half-typed");
    assert!(!app.adopt_input_suggestion(Some("late".to_owned()), epoch));
    app.input.take();

    assert!(
        !app.adopt_input_suggestion(None, epoch),
        "the model saw no obvious next step: nothing to show"
    );
    assert!(app.visible_input_suggestion().is_none());

    assert!(app.adopt_input_suggestion(Some("fresh".to_owned()), epoch));
    assert_eq!(app.visible_input_suggestion(), Some("fresh"));

    app.begin_turn(false, "working".to_owned());
    assert!(app.visible_input_suggestion().is_none());
    assert!(app.input_suggestion.is_none(), "cleared, not merely hidden");
}

/// `/workspace` 面板：打开时光标落在当前工作区上，输入即过滤，`Enter` 交出
/// 选中的工作区。此前切换要人把 UUID 从聊天记录里抄回输入框。
#[test]
fn workspace_picker_starts_on_the_current_workspace_filters_and_selects() {
    let current = workspace_fixture("当前项目", "/Users/rocky/Sites/willdeep-rs", true);
    let other = workspace_fixture("tokenhub", "/Users/rocky/Sites/tokenhub", false);
    let other_id = other.id;
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_workspace_picker(vec![other.clone(), current.clone()], current.root.clone());

    // 列表里第二条才是当前工作区，光标必须落在它身上。
    let picker = app.workspace_picker.as_ref().expect("panel is open");
    assert_eq!(picker.selected, 1);
    assert_eq!(picker.filtered.len(), 2);

    // 按名字过滤，一条都不用记 ID。
    for character in "token".chars() {
        assert!(matches!(
            app.handle_workspace_picker_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE
            )),
            WorkspacePickerAction::None
        ));
    }
    let picker = app.workspace_picker.as_ref().unwrap();
    assert_eq!(picker.filtered.len(), 1);
    assert_eq!(picker.workspaces[picker.filtered[0]].name, "tokenhub");

    match app.handle_workspace_picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
        WorkspacePickerAction::Select(id) => assert_eq!(id, other_id),
        _ => panic!("expected a Workspace selection"),
    }

    // 路径也能过滤：人手上有时只有一个目录。
    app.open_workspace_picker(vec![other, current.clone()], current.root.clone());
    for character in "Sites/willdeep".chars() {
        app.handle_workspace_picker_key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::NONE,
        ));
    }
    let picker = app.workspace_picker.as_ref().unwrap();
    assert_eq!(picker.filtered.len(), 1);
    assert_eq!(picker.workspaces[picker.filtered[0]].id, current.id);

    // Esc 关掉面板，不切任何东西。
    assert!(matches!(
        app.handle_workspace_picker_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        WorkspacePickerAction::Close
    ));
}

/// 面板一行里必须有名字和路径——一屏 UUID 不是列表。
#[test]
fn workspace_picker_line_leads_with_the_name_and_path() {
    let workspace = workspace_fixture("tokenhub", "/Users/rocky/Sites/tokenhub", false);
    let line = workspace_picker_ui::workspace_picker_line(&workspace, true, Language::En, true);
    assert!(line.starts_with("▶ tokenhub"));
    assert!(line.contains("/Users/rocky/Sites/tokenhub"));
    assert!(line.contains("[current]"));
    assert!(
        !line.contains(&workspace.id.to_string()),
        "the panel is for choosing, not for reading UUIDs: {line}"
    );
}

fn workspace_fixture(name: &str, root: &str, active: bool) -> crate::daemon::RuntimeWorkspace {
    crate::daemon::RuntimeWorkspace {
        schema: 1,
        id: uuid::Uuid::new_v4(),
        name: name.to_owned(),
        root: std::path::PathBuf::from(root),
        access: crate::daemon::WorkspaceAccess::Smart,
        provider_profile: None,
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        created_at: 0,
        updated_at: 0,
        active,
    }
}

/// `/new` 不是 `/clear`：前者换会话，后者只擦屏幕。主循环接手 `/new`，
/// 兜底处理器不能把它报成未知命令。
#[test]
fn new_command_passes_through_to_the_main_loop() {
    let mut app = App::new(Vec::new(), Language::En);
    let skills = SkillCatalog::default();
    assert!(
        !app.handle_slash_command("/new", &skills),
        "/new is handled by the main loop"
    );
    assert!(app.transcript.is_empty());
    assert!(
        crate::tui::command_catalog::command_candidates(Language::En)
            .into_iter()
            .any(|(command, _)| command == "/new"),
        "/new must be listed in /help and the completion menu"
    );
}
