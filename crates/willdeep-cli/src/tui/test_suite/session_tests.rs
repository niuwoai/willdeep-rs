use super::*;

#[test]
fn session_picker_result_includes_content_snippet_without_embedded_lines() {
    let result = willdeep_runtime_protocol::SessionSearchResult {
        id: uuid::Uuid::new_v4(),
        title: "SSO\n设计".to_owned(),
        workspace: Some("/workspace".to_owned()),
        status: willdeep_runtime_protocol::SessionStatus::Idle,
        profile: None,
        model: None,
        updated_at: 1,
        message_count: 4,
        snippet: Some("RBAC\n数据权限".to_owned()),
        origin: willdeep_runtime_protocol::SessionOrigin::Runtime,
    };

    let line = session_picker_ui::session_picker_result_line(&result, true, Language::ZhCn, true);

    assert!(line.contains("SSO 设计"));
    assert!(line.contains("RBAC 数据权限"));
    assert!(line.contains("[当前]"));
    assert!(!line.contains('\n'));
}
#[test]
fn command_palette_session_item_queues_direct_switch() {
    let workspace = std::env::temp_dir().join(format!(
        "willdeep-palette-session-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&workspace).unwrap();
    let session = Session::new(workspace.clone(), None, "Unique history title");
    let store = SessionStore::new(workspace.join("home"));
    let registry = BackgroundTaskRegistry::default();
    let mut app = App::new(Vec::new(), Language::En);
    app.open_palette(&SkillCatalog::default(), &store, &session);
    for character in "uniquehistorytitle".chars() {
        app.handle_palette_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &registry,
        );
    }

    app.handle_palette_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &registry);

    let target = app.pending_session_switch.unwrap();
    assert_eq!(target.id, session.id.to_string());
    assert!(!target.archived);
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
fn renders_common_markdown_for_terminal() {
    let lines = render_assistant_markdown(
        "# Title\n- **bold** and `code`\n[Docs](https://example.com)",
        80,
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
fn renders_html_breaks_as_terminal_lines() {
    let lines = render_assistant_markdown("第一行<br>第二行<BR />第三行", 80);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(rendered, vec!["WillDeep: 第一行", "第二行", "第三行"]);
    assert!(rendered.iter().all(|line| !line.contains("<br")));
}
#[test]
fn renders_gfm_tables_with_cjk_width_and_wrapped_cells() {
    let lines = render_assistant_markdown(
        "说明：\n\n| 层级 | 说明 |\n|---|---|\n| 产品 | 官网、用户体系 |\n| SSO | 1. 登录<br>2. 同步权限 |",
        24,
    );
    let rendered_lines = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        rendered_lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 24)
    );
    let rendered = rendered_lines.join("\n");

    assert!(rendered.contains("层级"));
    assert!(rendered.contains("─┼─"));
    assert!(rendered.contains("产品"));
    assert!(rendered.contains("1. 登录"));
    assert!(rendered.contains("2. 同步权限"));
    assert!(!rendered.contains("|---|"));
    assert!(!rendered.contains("<br>"));
}
#[test]
fn renders_closed_thin_rules_around_and_between_table_rows() {
    let lines = render_assistant_markdown("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |", 80);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![
            "WillDeep: ",
            "┌───┬───┐",
            "│ A │ B │",
            "├───┼───┤",
            "│ 1 │ 2 │",
            "├───┼───┤",
            "│ 3 │ 4 │",
            "└───┴───┘",
        ]
    );
}
#[test]
fn recognizes_terminal_line_navigation_control_bytes() {
    assert_eq!(
        prompt_line_navigation_for_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        Some(PromptLineNavigation::Start)
    );
    assert_eq!(
        prompt_line_navigation_for_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        Some(PromptLineNavigation::End)
    );
    assert_eq!(
        prompt_line_navigation_for_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        None
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
#[test]
fn recognizes_clipboard_image_paste_shortcuts() {
    for key in [
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        KeyEvent::new(
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER),
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT),
    ] {
        assert!(is_clipboard_image_paste_key(key));
    }
    assert!(!is_clipboard_image_paste_key(KeyEvent::new(
        KeyCode::Char('v'),
        KeyModifiers::NONE,
    )));
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
async fn approval_keyboard_ignores_ime_text_and_supports_menu_and_shortcuts() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    let (menu_sender, menu_receiver) = oneshot::channel();
    app.approval = Some(("Run tests".to_owned(), true, menu_sender));

    app.handle_approval_key(KeyEvent::new(KeyCode::Char('中'), KeyModifiers::NONE));
    assert!(app.approval.is_some(), "IME text must not decide approval");
    app.handle_approval_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.approval_selected, 1);
    app.handle_approval_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(menu_receiver.await.unwrap(), ApprovalDecision::AlwaysAllow);

    let (allow_sender, allow_receiver) = oneshot::channel();
    app.approval = Some(("Run build".to_owned(), false, allow_sender));
    app.handle_approval_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT));
    assert_eq!(allow_receiver.await.unwrap(), ApprovalDecision::AllowOnce);

    let (deny_sender, deny_receiver) = oneshot::channel();
    app.approval = Some(("Run deploy".to_owned(), false, deny_sender));
    app.handle_approval_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
    assert_eq!(deny_receiver.await.unwrap(), ApprovalDecision::Deny);
}

#[tokio::test]
async fn mouse_can_resolve_approval_and_single_choice_question() {
    let registry = BackgroundTaskRegistry::default();
    let skills = SkillCatalog::default();
    let mut app = App::new(Vec::new(), Language::En);
    let (approval_sender, approval_receiver) = oneshot::channel();
    app.approval = Some(("Run tests".to_owned(), true, approval_sender));
    app.approval_rect = Rect::new(10, 10, 60, 9);
    app.approval_action_hits = vec![
        (Rect::new(11, 15, 58, 1), ApprovalDecision::AllowOnce),
        (Rect::new(11, 16, 58, 1), ApprovalDecision::AlwaysAllow),
        (Rect::new(11, 17, 58, 1), ApprovalDecision::Deny),
    ];
    app.handle_mouse(35, 16, &registry, &skills);
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
    runtime_ui::apply_runtime_events(&mut app, vec![event.clone()], &mut session, &store).unwrap();
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

/// 线上复现：撞轮次上限的 Runtime 轮次以 `partial` 收尾，TUI 只认 `completed`，
/// 于是整轮一个字不显示、「工作中」一直挂着，直到快照对账强行复位。
#[test]
fn runtime_partial_turn_shows_final_text_and_finishes() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-partial-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime partial");
    let mut app = App::new(Vec::new(), Language::En);
    let task_id = uuid::Uuid::new_v4();
    let session_id = session.id;
    let output = |sequence: u64, value: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id={task_id} {value}"),
        visible: true,
        session_id: Some(session_id),
    };
    let events = vec![
        output(1, serde_json::json!({"type":"turn_started","turn":1})),
        output(
            2,
            serde_json::json!({"type":"assistant_text","text":"Now remove the   unused\nimports"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert!(app.running);
    assert!(app.transient_thought.is_none());
    assert_eq!(
        app.transcript,
        vec!["WillDeep: Now remove the   unused\nimports"],
        "mid-turn narration lands in the chat as soon as it is final"
    );

    let events = vec![
        output(
            3,
            serde_json::json!({"type":"tool_requested","name":"edit_file"}),
        ),
        output(
            4,
            serde_json::json!({"type":"assistant_text_delta","text":"Now "}),
        ),
        output(
            5,
            serde_json::json!({"type":"assistant_text_delta","text":"update"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert_eq!(
        app.transcript.get(1).map(String::as_str),
        Some("· … edit_file"),
        "the tool call gets its own chat row"
    );
    assert_eq!(app.transient_thought.as_deref(), Some("Now update"));

    let events = vec![
        output(
            6,
            serde_json::json!({"type":"partial","stop_reason":"max_turns","turns":64,"text":"turn limit reached"}),
        ),
        crate::daemon::RemoteRuntimeEvent {
            sequence: 7,
            kind: "task.partial".to_owned(),
            message: format!("task_id={task_id} session_id={session_id}"),
            visible: true,
            session_id: Some(session_id),
        },
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert!(!app.running);
    assert!(app.transient_thought.is_none());
    assert_eq!(
        app.transcript
            .iter()
            .filter(|line| *line == "WillDeep: turn limit reached")
            .count(),
        1
    );
    assert_eq!(
        transcript(&store.load(session.id).unwrap().messages),
        vec![
            "WillDeep: Now remove the   unused\nimports",
            "WillDeep: turn limit reached"
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 工具调用直接进聊天区：发起时占一行，完成后原地改成 ✓/✗，不再只藏在活动区。
#[test]
fn runtime_tool_calls_land_in_chat_and_settle_in_place() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-tool-rows-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime tool rows");
    let mut app = App::new(Vec::new(), Language::En);
    let task_id = uuid::Uuid::new_v4();
    let session_id = session.id;
    let output = |sequence: u64, value: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id={task_id} {value}"),
        visible: true,
        session_id: Some(session_id),
    };
    let events = vec![
        output(1, serde_json::json!({"type":"turn_started","turn":1})),
        output(
            2,
            serde_json::json!({"type":"tool_requested","name":"run_command","detail":"cargo test -p"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert_eq!(app.transcript, vec!["· … run_command · cargo test -p"]);

    let events = vec![
        output(
            3,
            serde_json::json!({"type":"tool_completed","name":"run_command","is_error":true,"detail":"cargo test -p"}),
        ),
        output(
            4,
            serde_json::json!({"type":"tool_requested","name":"read_file"}),
        ),
        // 完成事件不带摘要也要能对上发起行。
        output(
            5,
            serde_json::json!({"type":"tool_completed","name":"read_file","is_error":false}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert_eq!(
        app.transcript,
        vec!["· ✗ run_command · cargo test -p", "· ✓ read_file"]
    );
    assert!(app.running, "tool rows do not end the turn");
    assert!(
        session.messages.is_empty(),
        "tool rows are display only, never model history"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 收尾只补聊天区还没见过的部分：撞轮次上限时守护进程把提示拼在最后一段前面，
/// 只补那句提示；正常收尾与最后一段一模一样就不再落第二遍。会话镜像同样只补差量。
#[test]
fn runtime_reply_only_adds_what_narration_has_not_shown() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-reply-dedupe-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime reply dedupe");
    let mut app = App::new(Vec::new(), Language::En);
    let task_id = uuid::Uuid::new_v4();
    let session_id = session.id;
    let output = |sequence: u64, value: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id={task_id} {value}"),
        visible: true,
        session_id: Some(session_id),
    };
    let events = vec![
        output(
            1,
            serde_json::json!({"type":"assistant_text","text":"Now update the log line"}),
        ),
        output(
            2,
            serde_json::json!({"type":"partial","stop_reason":"max_turns","turns":64,"text":"⚠ turn limit\n\nNow update the log line"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    let replies = |app: &App| {
        app.transcript
            .iter()
            .filter(|line| line.starts_with("WillDeep: "))
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        replies(&app),
        vec![
            "WillDeep: Now update the log line",
            "WillDeep: ⚠ turn limit"
        ]
    );
    assert_eq!(
        transcript(&store.load(session.id).unwrap().messages),
        vec![
            "WillDeep: Now update the log line",
            "WillDeep: ⚠ turn limit"
        ]
    );
    assert!(!app.running);

    let events = vec![
        output(3, serde_json::json!({"type":"turn_started","turn":1})),
        output(
            4,
            serde_json::json!({"type":"assistant_text","text":"All done."}),
        ),
        output(
            5,
            serde_json::json!({"type":"completed","stop_reason":"finished","turns":1,"text":"All done."}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert_eq!(
        replies(&app),
        vec![
            "WillDeep: Now update the log line",
            "WillDeep: ⚠ turn limit",
            "WillDeep: All done."
        ]
    );
    assert_eq!(
        transcript(&store.load(session.id).unwrap().messages),
        vec![
            "WillDeep: Now update the log line",
            "WillDeep: ⚠ turn limit",
            "WillDeep: All done."
        ]
    );
    assert!(!app.running);
    std::fs::remove_dir_all(root).unwrap();
}

/// 思考型模型正文常为空：Runtime 发来的思维链增量落在临时行，工具一调就清掉，
/// 既不进记录也不进会话镜像。
#[test]
fn runtime_reasoning_delta_shows_as_transient_thought_only() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-reasoning-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime reasoning");
    let mut app = App::new(Vec::new(), Language::En);
    let task_id = uuid::Uuid::new_v4();
    let session_id = session.id;
    let output = |sequence: u64, value: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id={task_id} {value}"),
        visible: true,
        session_id: Some(session_id),
    };
    let events = vec![
        output(1, serde_json::json!({"type":"turn_started","turn":1})),
        output(
            2,
            serde_json::json!({"type":"reasoning_delta","text":"先看日志"}),
        ),
        output(
            3,
            serde_json::json!({"type":"reasoning_delta","text":"，再改"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert!(app.running);
    assert_eq!(app.transient_thought.as_deref(), Some("先看日志，再改"));
    assert_eq!(app.transient_label(), "thinking");
    assert!(
        app.transcript.is_empty(),
        "reasoning never lands in the chat"
    );
    assert!(session.messages.is_empty());

    let events = vec![output(
        4,
        serde_json::json!({"type":"tool_requested","name":"read_file"}),
    )];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert!(app.transient_thought.is_none());
    assert_eq!(app.transcript, vec!["· … read_file"]);
    std::fs::remove_dir_all(root).unwrap();
}

/// 插话送进了任务却没赶上下一次调模型：Runtime 以 `steer_undelivered` 交回，
/// TUI 说一声并排队，本轮结束后照常发出，一句话都不丢。
#[test]
fn runtime_hands_back_undelivered_steering_and_the_tui_requeues_it() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-steer-undelivered-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime steer");
    let mut app = App::new(Vec::new(), Language::En);
    let task_id = uuid::Uuid::new_v4();
    let session_id = session.id;
    let output = |sequence: u64, value: serde_json::Value| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id={task_id} {value}"),
        visible: true,
        session_id: Some(session_id),
    };
    let events = vec![
        output(1, serde_json::json!({"type":"turn_started","turn":1})),
        output(
            2,
            serde_json::json!({"type":"steer_undelivered","text":"先别删，只改 handler"}),
        ),
        output(
            3,
            serde_json::json!({"type":"completed","stop_reason":"finished","turns":1,"text":"done"}),
        ),
    ];
    runtime_ui::apply_runtime_events(&mut app, events, &mut session, &store).unwrap();
    assert!(!app.running);
    assert_eq!(app.queued_prompts.len(), 1);
    assert_eq!(app.queued_prompts[0].text, "先别删，只改 handler");
    assert!(
        app.transcript
            .iter()
            .any(|line| line.starts_with("System: The last message missed this turn")),
        "{:?}",
        app.transcript
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_partial_terminal_event_alone_releases_busy_state() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-partial-terminal-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "runtime partial terminal");
    let mut app = App::new(Vec::new(), Language::En);
    app.ensure_runtime_turn();
    assert!(app.running);
    let event = crate::daemon::RemoteRuntimeEvent {
        sequence: 1,
        kind: "turn.partial".to_owned(),
        message: format!("session_id={} turn_id={}", session.id, uuid::Uuid::new_v4()),
        visible: true,
        session_id: Some(session.id),
    };
    runtime_ui::apply_runtime_events(&mut app, vec![event], &mut session, &store).unwrap();
    assert!(!app.running);
    assert!(!app.runtime_turn);
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
                serde_json::json!({"type":"subagent_retry_wait","id":child,"attempt":2,"delay_ms":1500}),
            ),
            event(
                6,
                serde_json::json!({"type":"subagent_completed","id":child,"status":"completed"}),
            ),
        ],
        &mut session,
        &store,
    )
    .unwrap();
    assert_eq!(app.runtime_event_cursor, 6);
    assert!(
        app.progress_log
            .iter()
            .any(|line| line.contains("87654321") && line.contains("Waiting to retry 2 · 2s"))
    );
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

/// 聊天区里只有三种东西：工具行、回答、本轮账目。轮次号、task_id 这类运行时
/// 标识仍只在活动区。
#[test]
fn runtime_chat_renders_tool_rows_then_the_assistant_answer() {
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

    // 工具行在前，回答之后跟一行本轮账目。这一轮没有用量事件，所以只报耗时——
    // 不印一个像「真的用了 0 个 token」的 0。
    assert_eq!(app.transcript.len(), 3, "{:?}", app.transcript);
    assert_eq!(app.transcript[0], "· … read_file");
    assert_eq!(app.transcript[1], "WillDeep: 真实的 AI 回复");
    assert!(
        app.transcript[2].starts_with("── turn finished · total "),
        "{}",
        app.transcript[2]
    );
    assert!(app.transcript[2].ends_with("your turn ──"));
    assert!(!app.transcript[2].contains("in 0"));
    assert!(
        !app.running,
        "the completed Runtime output must end the busy state"
    );
    assert!(app.last_elapsed.is_some());
    // 轮次号、task_id 这类运行时标识仍只在活动区；分隔线里的「turn finished /
    // your turn」是给人看的话，不算泄漏。
    assert!(app.transcript.iter().all(|line| {
        !line.contains("turn 9") && !line.contains("task_id") && !line.contains(&task.to_string())
    }));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_terminal_failure_stops_the_busy_indicator() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-failure-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "failed runtime");
    let mut app = App::new(Vec::new(), Language::En);
    app.begin_turn(true, "Submitted to Runtime".to_owned());
    let event = crate::daemon::RemoteRuntimeEvent {
        sequence: 1,
        kind: "task.failed".to_owned(),
        message: format!("task_id={} error=provider", uuid::Uuid::new_v4()),
        visible: true,
        session_id: Some(session.id),
    };

    runtime_ui::apply_runtime_events(&mut app, vec![event], &mut session, &store).unwrap();

    assert!(!app.running);
    assert!(app.last_elapsed.is_some());
    assert!(
        app.transcript
            .iter()
            .any(|line| line.contains("Runtime task failed"))
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
    assert!(is_selection_copy_key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
    assert!(is_selection_copy_key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::NONE
    )));
}

#[test]
fn internal_chat_selection_copies_unicode_display_columns() {
    let rows = vec!["你好吗".to_owned(), "second".to_owned()];
    assert_eq!(selected_text(&rows, (0, 2), (1, 3)), "好吗\nsec");
    assert_eq!(quote_selected_text("第一行\n第二行"), "> 第一行\n> 第二行");
}

/// Regression: `Line::styled` records the colour on the line, leaving its
/// spans raw. Flattening to characters while reading only `span.style` threw
/// the whole `You:` / `Error:` transcript palette away and everything came
/// out in the terminal's default foreground.
#[test]
fn wrapping_preserves_line_level_styles_not_just_span_styles() {
    let wrapped = wrap_styled_text(
        colored_transcript_at_width(
            &["You: 初始化".to_owned(), "Error: boom".to_owned()],
            None,
            40,
        ),
        40,
    );
    let colour_of = |row: usize| {
        wrapped.lines[row]
            .spans
            .iter()
            .map(|span| wrapped.lines[row].style.patch(span.style).fg)
            .collect::<Vec<_>>()
    };

    assert!(
        colour_of(0).iter().all(|fg| *fg == Some(Color::Cyan)),
        "user lines stay cyan after wrapping: {:?}",
        colour_of(0)
    );
    assert!(
        colour_of(1).iter().all(|fg| *fg == Some(Color::Red)),
        "error lines stay red after wrapping: {:?}",
        colour_of(1)
    );
}

/// The dialog has to read as its own region: the row under the cursor gets a
/// highlight bar, the free-text field is picked out, and the key hints dim.
#[test]
fn question_modal_highlights_the_cursor_row_and_dims_the_hints() {
    let request = UserQuestion {
        question: "现在提交首次 commit 吗？".to_owned(),
        options: vec!["提交".to_owned(), "暂不提交".to_owned()],
        multi_select: false,
    };
    let (tx, _rx) = oneshot::channel();
    let dialog = AskDialog {
        request,
        selected: 1,
        checked: vec![false; 2],
        answer: PromptEditor::default(),
        sender: tx,
    };
    let content = "现在提交首次 commit 吗？\n\n  ( ) 提交\n▶ (*) 暂不提交\n\n其他答案: \n↑/↓ 选择";
    let lines = question_lines(&dialog, content);

    assert_eq!(lines.len(), 7);
    assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
    // Row 2 is the first option, row 3 the cursor row.
    assert_eq!(lines[2].style.bg, None);
    assert_eq!(lines[3].style.bg, Some(Color::LightCyan));
    assert!(lines[3].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(lines[5].style.fg, Some(Color::LightYellow));
    assert_eq!(lines[6].style.fg, Some(Color::Gray));
}

/// The complaint was that the dialog did not read as its own region: its
/// text fell back to the terminal's default colour and chat showed through.
/// Render it for real and check the panel is opaque and covers what was
/// underneath.
#[test]
fn modal_panel_renders_opaque_over_whatever_was_underneath() {
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

    let area = Rect::new(0, 0, 40, 10);
    let mut buffer = Buffer::empty(area);
    // Stand in for the transcript sitting under the modal.
    Paragraph::new("chat chat chat chat chat chat chat chat").render(area, &mut buffer);
    assert!(
        buffer_text(&buffer).contains("chat"),
        "precondition: chat is on screen"
    );

    let popup = Rect::new(4, 2, 32, 6);
    Clear.render(modal_halo(popup, area), &mut buffer);
    Block::default()
        .style(MODAL_PANEL)
        .render(modal_halo(popup, area), &mut buffer);
    Paragraph::new("现在提交首次 commit 吗？")
        .block(modal_block("智能体提问".to_owned(), Color::LightCyan))
        .render(popup, &mut buffer);
    let halo = modal_halo(popup, area);
    // Without this, the trailing cell of every CJK grapheme in the title and
    // the question is reset to the terminal background — a solid-looking
    // panel with transparent holes punched through the Chinese text.
    assert!(
        (halo.x..halo.x + halo.width).any(|x| buffer[(x, halo.y + 1)].bg == Color::Reset),
        "precondition: wide graphemes leave gaps before sealing"
    );
    seal_modal_background(&mut buffer, halo);

    // Every cell of the panel and its halo carries the panel background.
    for y in halo.y..halo.y + halo.height {
        for x in halo.x..halo.x + halo.width {
            assert_eq!(
                buffer[(x, y)].bg,
                MODAL_BG,
                "cell ({x},{y}) is not part of an opaque panel"
            );
        }
    }
    // Rows the modal covers no longer show the transcript underneath.
    for y in halo.y..halo.y + halo.height {
        let row = (halo.x..halo.x + halo.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>();
        assert!(!row.contains("chat"), "row {y} still leaks chat: {row}");
    }
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

#[test]
fn modals_span_most_of_the_terminal_so_chat_never_sits_beside_them() {
    // A narrow centered popup left transcript rows visible to either side,
    // which is what made the dialog look tangled with the chat.
    let wide = modal_width(Rect::new(0, 0, 200, 50));
    assert_eq!(wide, 110, "capped so it does not become unreadably wide");
    let typical = modal_width(Rect::new(0, 0, 100, 50));
    assert_eq!(typical, 92, "leaves a 4-column gutter each side");
    let narrow = modal_width(Rect::new(0, 0, 30, 20));
    assert_eq!(narrow, 30, "never wider than the terminal");
}

#[test]
fn styled_chat_wrap_and_selection_highlight_share_visual_rows() {
    let source = Text::from(Line::from(vec![
        Span::styled("abc", Style::default().fg(Color::Cyan)),
        Span::styled("中文", Style::default().add_modifier(Modifier::BOLD)),
    ]));
    let mut wrapped = wrap_styled_text(source, 5);
    assert_eq!(text_rows(&wrapped), vec!["abc中", "文"]);

    highlight_text_selection(&mut wrapped, (0, 3), (0, 5));
    let highlighted = wrapped.lines[0]
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(MODAL_BG))
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(highlighted, "中");
}

#[test]
fn dragging_chat_text_enters_internal_selection_mode() {
    let mut app = App::new(vec!["hello world".to_owned()], Language::En);
    app.transcript_rect = Rect::new(0, 0, 20, 6);
    app.transcript_rows = vec!["hello world".to_owned()];
    app.transcript_render_offset = 0;

    assert!(!app.handle_chat_selection_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 1,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.handle_chat_selection_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 5,
        row: 1,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.selection_mode);
    assert_eq!(app.selected_chat_text(), "ello");
    assert!(!app.native_selection_mode);
}

#[test]
fn native_selection_mode_has_an_explicit_state_and_exit() {
    let mut app = App::new(Vec::new(), Language::En);
    app.chat_selection = Some(ChatSelection {
        anchor: ChatSelectionPoint { row: 0, column: 0 },
        head: ChatSelectionPoint { row: 0, column: 1 },
    });

    app.enter_native_selection_mode();
    assert!(app.selection_mode);
    assert!(app.native_selection_mode);
    assert!(app.chat_selection.is_none());
    assert_eq!(app.focus, FocusPane::Chat);

    app.exit_selection_mode();
    assert!(!app.selection_mode);
    assert!(!app.native_selection_mode);
}

#[test]
fn quoting_chat_selection_preserves_the_existing_draft() {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.input.insert("我的补充");
    app.transcript_rows = vec!["第一行".to_owned(), "第二行".to_owned()];
    app.chat_selection = Some(ChatSelection {
        anchor: ChatSelectionPoint { row: 0, column: 0 },
        head: ChatSelectionPoint { row: 1, column: 5 },
    });
    app.selection_mode = true;

    app.quote_chat_selection();

    assert_eq!(app.input.text(), "我的补充\n\n> 第一行\n> 第二行");
    assert_eq!(app.focus, FocusPane::Prompt);
    assert!(!app.selection_mode);
    assert!(app.chat_selection.is_none());
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
        "/model",
        "/model qwen3-coder",
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

/// 本轮在跑时按 Enter 曾经什么都不做：命令发不出去，也没有任何提示。
/// 现在按输入的性质分三路——本地命令立即执行，提示词排队，会改状态的命令说明原因。
#[test]
fn a_running_turn_no_longer_swallows_every_enter() {
    for command in [
        "/help",
        "/clear",
        "/sidebar off",
        "/skills",
        "/history 登录",
        // 换模型只作用于下一轮：当场收下，本轮结束再切。
        "/model",
        "/model qwen3-coder",
    ] {
        assert_eq!(busy_input(command), BusyInput::RunNow, "{command}");
    }
    // `/session search` 只是查询，`/session switch` 会换掉当前会话。
    assert_eq!(busy_input("/session search 登录"), BusyInput::RunNow);
    assert_eq!(busy_input("/session switch abc"), BusyInput::Refuse);
    for prompt in ["继续重构这个模块", "/local 跑一下测试", "/runtime 修一下"] {
        assert_eq!(busy_input(prompt), BusyInput::Queue, "{prompt}");
    }
    for command in ["/compress", "/daemon upgrade", "/diff"] {
        assert_eq!(busy_input(command), BusyInput::Refuse, "{command}");
    }
}

/// 一条失败的 Inbox 项此前只写 `Status: Failed`，打开详情等于什么都没说。
#[test]
fn failed_task_details_name_the_command_and_the_reason() {
    let diagnostics = willdeep_runtime_protocol::RuntimeTaskDiagnostics {
        task: willdeep_runtime_protocol::RuntimeTask {
            origin_client: None,
            id: uuid::Uuid::new_v4(),
            session_id: None,
            turn_id: None,
            agent_id: None,
            event_start_sequence: 1,
            status: willdeep_runtime_protocol::TaskStatus::Failed,
            workspace: Some("/workspace".to_owned()),
            profile: None,
            prompt_excerpt: None,
            created_at: 1,
            started_at: Some(1),
            completed_at: Some(2),
            exit_code: Some(101),
            failure_domain: Some(willdeep_runtime_protocol::FailureDomain::Tool),
        },
        failure: Some("task_id=abc exit_code=101 error=harness exited".to_owned()),
        failed_tools: vec![willdeep_runtime_protocol::RuntimeToolFailure {
            sequence: 7,
            name: "run_command".to_owned(),
            arguments: Some(r#"{"command":"cargo build --release"}"#.to_owned()),
            output: Some("error[E0425]: cannot find value `x`".to_owned()),
        }],
    };

    let text = format_task_diagnostics(&diagnostics, Language::ZhCn).expect("failure details");

    assert!(text.contains("退出码: 101"));
    assert!(text.contains("失败域: Tool"));
    assert!(text.contains("error=harness exited"));
    assert!(!text.contains("task_id=abc"), "task_id 详情里已经有了");
    assert!(text.contains("run_command"));
    assert!(text.contains("command: cargo build --release"), "{text}");
    assert!(text.contains("error[E0425]"));

    // 没有失败痕迹时不挂一个空标题。
    let clean = willdeep_runtime_protocol::RuntimeTaskDiagnostics {
        task: willdeep_runtime_protocol::RuntimeTask {
            origin_client: None,
            exit_code: None,
            failure_domain: None,
            ..diagnostics.task.clone()
        },
        failure: None,
        failed_tools: Vec::new(),
    };
    assert!(format_task_diagnostics(&clean, Language::ZhCn).is_none());
}

#[test]
fn command_completion_offers_the_runtime_controls() {
    let matches = command_catalog::command_candidates(Language::En);
    for command in ["/daemon", "/history", "/model", "/webapp"] {
        assert!(
            matches.iter().any(|(name, _)| *name == command),
            "{command} missing from the completion catalog"
        );
    }
    // help_text 内部断言用法签名与说明一一对齐；漏改一边会把说明贴到别的命令上。
    for language in [Language::ZhCn, Language::En, Language::Ja] {
        assert!(command_catalog::help_text(language).contains("/history"));
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
fn working_summary_distinguishes_progress_waiting_and_silence() {
    let recent = format_working_summary(
        Language::ZhCn,
        true,
        "Runtime · 正在使用 read_file",
        Duration::from_secs(5),
        Duration::from_secs(2),
    );
    assert!(recent.contains("正在使用 read_file"));
    assert!(recent.contains("已运行 5.0s"));

    let waiting = format_working_summary(
        Language::ZhCn,
        true,
        "Runtime · 正在使用 read_file",
        Duration::from_secs(18),
        Duration::from_secs(9),
    );
    assert!(waiting.contains("等待 Runtime / 模型返回"));

    let silent = format_working_summary(
        Language::ZhCn,
        true,
        "Runtime · 正在使用 read_file",
        Duration::from_secs(48),
        Duration::from_secs(31),
    );
    assert!(silent.contains("暂未收到新事件"));
    assert!(silent.contains("已等待 31s"));

    let long = format_working_summary(
        Language::ZhCn,
        true,
        "Runtime · 正在使用 run_command",
        Duration::from_secs(293),
        Duration::from_secs(2),
    );
    assert!(long.contains("已运行 4.9m"), "got: {long}");
}

/// 过了 120 秒的读数换分钟、过了 120 分钟换小时，都保留一位小数；
/// 120 以内维持各显示点原有的秒精度。
#[test]
fn elapsed_span_switches_units_past_two_minutes() {
    assert_eq!(format_elapsed_span(5.0, 1), "5.0s");
    assert_eq!(format_elapsed_span(31.0, 0), "31s");
    assert_eq!(format_elapsed_span(120.0, 1), "120.0s");
    assert_eq!(format_elapsed_span(293.2, 1), "4.9m");
    assert_eq!(format_elapsed_span(3_600.0, 1), "60.0m");
    assert_eq!(format_elapsed_span(9_000.0, 0), "2.5h");
}

#[test]
fn active_runtime_snapshot_restores_busy_state_after_reconnect() {
    let current_session = uuid::Uuid::new_v4();
    let task = crate::daemon::tui_bridge::RemoteTask {
        id: uuid::Uuid::new_v4(),
        session_id: Some(current_session),
        turn_id: Some(uuid::Uuid::new_v4()),
        agent_id: Some(uuid::Uuid::new_v4()),
        status: willdeep_runtime_protocol::TaskStatus::Running,
        profile: None,
        created_at: unix_now(),
        started_at: Some(unix_now()),
        completed_at: None,
        exit_code: None,
        failure_domain: None,
    };
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.runtime_event_cursor = 100;

    assert!(!app.observe_runtime_tasks(std::slice::from_ref(&task), current_session, Some(100)));

    assert!(app.running);
    assert!(app.runtime_turn);
    assert!(app.turn_started.is_some());
    assert!(app.activity_line.contains("重新连接 Runtime"));

    let mut other_session_app = App::new(Vec::new(), Language::ZhCn);
    other_session_app.observe_runtime_tasks(&[task], uuid::Uuid::new_v4(), Some(100));
    assert!(
        !other_session_app.running,
        "another session's task must not mark this chat as busy"
    );
}

fn running_remote_task(session: uuid::Uuid) -> crate::daemon::tui_bridge::RemoteTask {
    crate::daemon::tui_bridge::RemoteTask {
        id: uuid::Uuid::new_v4(),
        session_id: Some(session),
        turn_id: Some(uuid::Uuid::new_v4()),
        agent_id: Some(uuid::Uuid::new_v4()),
        status: willdeep_runtime_protocol::TaskStatus::Running,
        profile: None,
        created_at: unix_now(),
        started_at: Some(unix_now()),
        completed_at: None,
        exit_code: None,
        failure_domain: None,
    }
}

/// 快照在任务还在跑时拍下，却在 `turn.completed` 复位界面之后才送到：
/// 它的序号落后于本地事件游标，不能凭它再开一个永远结束不了的轮次。
#[test]
fn stale_runtime_snapshot_does_not_start_a_phantom_turn() {
    let session = uuid::Uuid::new_v4();
    let task = running_remote_task(session);
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.runtime_event_cursor = 4783;

    assert!(!app.observe_runtime_tasks(std::slice::from_ref(&task), session, Some(4781)));
    assert!(
        !app.running,
        "a snapshot older than the consumed events must not mark the chat busy"
    );

    // Runtime 不可达的空序号同样不算数。
    assert!(!app.observe_runtime_tasks(std::slice::from_ref(&task), session, None));
    assert!(!app.running);

    // 序号追平游标的快照才有资格恢复忙碌状态。
    assert!(!app.observe_runtime_tasks(std::slice::from_ref(&task), session, Some(4783)));
    assert!(app.running);
}

/// 界面显示 Runtime 轮次在跑，新鲜快照却连续几份都没有本会话的活动任务：
/// 攒够次数就该去向 Runtime 求证；中途只要见到任务或者拿到旧快照，计数归零。
#[test]
fn repeated_fresh_snapshots_without_tasks_flag_a_stale_runtime_turn() {
    let session = uuid::Uuid::new_v4();
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.runtime_event_cursor = 10;
    app.ensure_runtime_turn();
    assert!(app.running && app.runtime_turn);

    for _ in 1..STALE_RUNTIME_TURN_SNAPSHOTS {
        assert!(!app.observe_runtime_tasks(&[], session, Some(10)));
    }
    // 旧快照和不可达快照都不推进计数。
    assert!(!app.observe_runtime_tasks(&[], session, Some(9)));
    assert!(!app.observe_runtime_tasks(&[], session, None));
    assert!(
        app.observe_runtime_tasks(&[], session, Some(11)),
        "enough fresh empty snapshots must ask the Runtime to confirm"
    );
    assert!(
        app.running,
        "flagging alone must not reset the turn; the Runtime confirms first"
    );

    // 看到活动任务就重新计数。
    let task = running_remote_task(session);
    assert!(!app.observe_runtime_tasks(std::slice::from_ref(&task), session, Some(12)));
    assert_eq!(app.stale_runtime_turn_snapshots, 0);
    assert!(!app.observe_runtime_tasks(&[], session, Some(12)));

    // 本地轮次不归 Runtime 管，快照里没任务是正常的。
    let mut local = App::new(Vec::new(), Language::ZhCn);
    local.begin_turn(false, String::new());
    for _ in 0..(STALE_RUNTIME_TURN_SNAPSHOTS + 1) {
        assert!(!local.observe_runtime_tasks(&[], session, Some(1)));
    }
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

/// 压缩反馈此前只有进程内轮次认，Runtime 托管会话整条丢掉——状态栏的占用只
/// 跟着 `usage` 走，而 usage 要请求成功才回来。于是压缩前的真实体量从来没上过
/// 屏：一次真实故障里，用户盯着压缩后的 4.6 万 token，实际送出去的是 94 万。
#[test]
fn runtime_compression_events_reach_the_status_bar_and_the_progress_line() {
    let root = std::env::temp_dir().join(format!(
        "willdeep-tui-runtime-compression-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = SessionStore::new(&root);
    let mut session = Session::new(root.clone(), None, "compression test");
    let mut app = App::new(Vec::new(), Language::En);
    let session_id = session.id;
    let output = |sequence: u64, payload: &str| crate::daemon::RemoteRuntimeEvent {
        sequence,
        kind: "task.output".to_owned(),
        message: format!("task_id=12345678-0000-0000-0000-000000000000 {payload}"),
        visible: true,
        session_id: Some(session_id),
    };

    runtime_ui::apply_runtime_events(
        &mut app,
        vec![output(
            1,
            r#"{"type":"compression_started","estimated_tokens":944662}"#,
        )],
        &mut session,
        &store,
    )
    .unwrap();
    assert_eq!(
        app.context_tokens, 944_662,
        "the pre-compaction size is the one that blows the window"
    );

    runtime_ui::apply_runtime_events(
        &mut app,
        vec![output(
            2,
            r#"{"type":"compression_completed","estimated_tokens":45998,"dropped_messages":3}"#,
        )],
        &mut session,
        &store,
    )
    .unwrap();
    assert_eq!(app.context_tokens, 45_998);
    assert!(
        app.progress_log
            .iter()
            .any(|line| line.contains("Context compressed") && line.contains('3')),
        "compression must leave a progress line naming the dropped messages: {:?}",
        app.progress_log
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn modal_colours_do_not_come_from_the_theme_palette() {
    // 0–15 号会被终端主题改写；弹窗与选区必须用色立方里的固定色。
    for colour in [MODAL_BG, MODAL_FG] {
        match colour {
            Color::Indexed(index) => assert!(index >= 16, "{colour:?} is a theme palette slot"),
            other => panic!("{other:?} is not a fixed 256-colour index"),
        }
    }
}

#[test]
fn clicking_outside_the_chat_after_selecting_releases_the_click_for_focus() {
    let mut app = App::new(vec!["hello world".to_owned()], Language::En);
    app.transcript_rect = Rect::new(0, 0, 20, 6);
    app.transcript_rows = vec!["hello world".to_owned()];
    app.transcript_render_offset = 0;
    for (kind, column) in [
        (MouseEventKind::Down(MouseButton::Left), 2),
        (MouseEventKind::Drag(MouseButton::Left), 5),
        (MouseEventKind::Up(MouseButton::Left), 5),
    ] {
        app.handle_chat_selection_mouse(MouseEvent {
            kind,
            column,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(app.selection_mode);

    // 点在聊天区下方（输入框的位置）：不能被选区吞掉，得交给正常的点击处理去切焦点。
    let consumed = app.handle_chat_selection_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row: 10,
        modifiers: KeyModifiers::NONE,
    });
    assert!(!consumed);
    assert!(!app.selection_mode);
    assert!(app.chat_selection.is_none());
}

#[test]
fn typing_in_selection_mode_releases_it_and_moves_focus_to_the_prompt() {
    let mut app = App::new(Vec::new(), Language::En);
    app.enter_native_selection_mode();
    app.release_selection_for_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(!app.selection_mode && !app.native_selection_mode);
    assert_eq!(app.focus, FocusPane::Prompt);

    // 翻页这类非输入键只退出选区，不抢焦点。
    app.enter_native_selection_mode();
    app.release_selection_for_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert!(!app.selection_mode);
    assert_eq!(app.focus, FocusPane::Chat);
}

#[test]
fn model_switch_during_a_turn_is_recorded_not_applied() {
    let mut app = App::new(Vec::new(), Language::En);
    app.running = true;
    let message = super::model_commands::defer_model_switch(&mut app, " glm-5 ").unwrap();
    assert_eq!(app.pending_model.as_deref(), Some("glm-5"));
    assert!(message.contains("glm-5"), "{message}");
    // 同一轮里再换一次，以最后一次为准。
    super::model_commands::defer_model_switch(&mut app, "deepseek-v4-flash").unwrap();
    assert_eq!(app.pending_model.as_deref(), Some("deepseek-v4-flash"));
    assert!(super::model_commands::defer_model_switch(&mut app, "  ").is_err());
    assert_eq!(app.pending_model.as_deref(), Some("deepseek-v4-flash"));
}

#[test]
fn runtime_versions_compare_with_release_candidates() {
    use super::app_state::version_is_older;
    assert!(version_is_older("0.78.0-rc28", "0.78.0-rc29"));
    assert!(
        version_is_older("0.78.0-rc9", "0.78.0-rc10"),
        "rc 按数字比，不按字符串"
    );
    assert!(version_is_older("0.78.0-rc33", "0.79.0-rc1"));
    assert!(
        version_is_older("0.78.0-rc33", "0.78.0"),
        "正式版晚于同号 rc"
    );
    assert!(!version_is_older("0.79.0-rc1", "0.78.0-rc33"));
    assert!(!version_is_older("0.79.0-rc1", "0.79.0-rc1"));
    // 拿不准就不升。
    assert!(!version_is_older("dev", "0.79.0-rc1"));
    assert!(!version_is_older("0.78.0-beta1", "0.79.0-rc1"));
    assert!(!version_is_older("0.78", "0.79.0-rc1"));
}

#[test]
fn only_an_older_runtime_is_queued_for_one_automatic_upgrade() {
    let mut app = App::new(Vec::new(), Language::En);
    app.observe_runtime_version(Some("0.21.0-rc62".to_owned()));
    assert!(app.runtime_auto_upgrade_pending, "older Runtime → try once");

    // 事件循环发起后标记已试；同一进程再见到旧 Runtime 不再重试。
    app.runtime_auto_upgrade_pending = false;
    app.runtime_auto_upgrade_tried = true;
    app.observe_runtime_version(Some("0.21.0-rc65".to_owned()));
    assert!(!app.runtime_auto_upgrade_pending);

    // 比客户端新的 Runtime 只警告，绝不降级。
    let mut app = App::new(Vec::new(), Language::En);
    app.observe_runtime_version(Some("999.0.0".to_owned()));
    assert!(app.stale_runtime_version().is_some());
    assert!(!app.runtime_auto_upgrade_pending);
}
