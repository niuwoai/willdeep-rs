use super::*;

fn point(step: usize, snippet: &str, files: bool) -> crate::daemon::RewindPoint {
    crate::daemon::RewindPoint {
        step,
        turn_id: (step > 0).then(uuid::Uuid::new_v4),
        snippet: snippet.to_owned(),
        completed_at: Some(1_000 + step as u64),
        can_restore_workspace: files,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn app_with_points(points: Vec<crate::daemon::RewindPoint>) -> App {
    let mut app = App::new(Vec::new(), Language::ZhCn);
    app.open_rewind_picker(points);
    app
}

#[test]
fn the_picker_opens_on_the_latest_step_and_confirms_before_rewinding() {
    let points = vec![
        point(0, "", true),
        point(1, "修登录\n第二行", true),
        point(2, "补测试", false),
    ];
    let latest = points[2].turn_id;
    let mut app = app_with_points(points);
    assert_eq!(
        app.rewind_picker.as_ref().unwrap().selected,
        2,
        "默认停在最近的一步"
    );

    // 列表阶段：上下移动、Enter 只是进入确认，不会直接回退。
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Up)),
        RewindPickerAction::None
    ));
    assert_eq!(app.rewind_picker.as_ref().unwrap().selected, 1);
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Down)),
        RewindPickerAction::None
    ));
    assert_eq!(app.rewind_picker.as_ref().unwrap().selected, 2);
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Enter)),
        RewindPickerAction::None
    ));
    assert!(app.rewind_picker.as_ref().unwrap().confirming);

    // 确认阶段的 Esc 是返回列表，不是关掉面板。
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Esc)),
        RewindPickerAction::None
    ));
    assert!(!app.rewind_picker.as_ref().unwrap().confirming);
    assert!(app.rewind_picker.is_some());

    // 这一步没有文件检查点：Enter 也只回对话。
    app.handle_rewind_picker_key(key(KeyCode::Enter));
    match app.handle_rewind_picker_key(key(KeyCode::Enter)) {
        RewindPickerAction::Rewind {
            step,
            through_turn_id,
            restore_workspace,
        } => {
            assert_eq!(step, 2);
            assert_eq!(through_turn_id, latest);
            assert!(
                !restore_workspace,
                "no checkpoint means conversation only, whatever key was pressed"
            );
        }
        _ => panic!("Enter in the confirm stage must rewind"),
    }

    // 回退动作交给事件循环去关面板；列表阶段的 Esc 则直接关掉。
    app.rewind_picker = None;
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Esc)),
        RewindPickerAction::None
    ));
    let mut app = app_with_points(vec![point(0, "", true), point(1, "x", true)]);
    assert!(matches!(
        app.handle_rewind_picker_key(key(KeyCode::Esc)),
        RewindPickerAction::Close
    ));
}

#[test]
fn a_step_with_a_checkpoint_offers_files_and_v_keeps_them() {
    let mut app = app_with_points(vec![point(0, "", true), point(1, "修登录", true)]);
    app.handle_rewind_picker_key(key(KeyCode::Enter));
    match app.handle_rewind_picker_key(key(KeyCode::Char('v'))) {
        RewindPickerAction::Rewind {
            restore_workspace,
            step,
            ..
        } => {
            assert_eq!(step, 1);
            assert!(!restore_workspace);
        }
        _ => panic!("v must rewind the conversation only"),
    }
    let mut app = app_with_points(vec![point(0, "", true), point(1, "修登录", true)]);
    app.handle_rewind_picker_key(key(KeyCode::Up));
    app.handle_rewind_picker_key(key(KeyCode::Enter));
    match app.handle_rewind_picker_key(key(KeyCode::Char('c'))) {
        RewindPickerAction::Rewind {
            restore_workspace,
            step,
            through_turn_id,
        } => {
            assert_eq!(step, 0);
            assert_eq!(through_turn_id, None, "the beginning has no boundary turn");
            assert!(restore_workspace);
        }
        _ => panic!("c must rewind conversation and files"),
    }
}

#[test]
fn rows_and_confirmation_lines_say_what_will_happen() {
    let first = point(1, "修登录\n第二行", true);
    let line = rewind_ui::rewind_point_line(&first, Language::ZhCn, true);
    assert_eq!(line, "▶ 第 1 步 · 文件✓ · 修登录 第二行");
    let beginning = rewind_ui::rewind_point_line(&point(0, "", false), Language::En, false);
    assert_eq!(beginning, "  beginning · chat only");

    let confirm = rewind_ui::rewind_confirm_line(&first, 2, Language::ZhCn);
    assert!(
        confirm.starts_with(
            "回到第 1 步 · 丢弃之后的 2 步 · [Enter] 对话与文件 · [v] 仅对话 · [Esc] 返回"
        ),
        "{confirm}"
    );
    let confirm = rewind_ui::rewind_confirm_line(&point(3, "x", false), 1, Language::En);
    assert!(
        confirm.contains("conversation only (no file checkpoint for this step)"),
        "{confirm}"
    );
    assert!(!confirm.contains("[v]"), "{confirm}");
}

/// 测试终端给每个宽字符留两格，第二格是空白；比对时把空白全部抹掉，只看字符顺序。
fn compact(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

#[test]
fn the_picker_renders_its_rows_and_footer() {
    let mut app = app_with_points(vec![
        point(0, "", true),
        point(1, "修登录", true),
        point(2, "补测试", false),
    ]);
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_rewind_picker(frame, &mut app))
        .unwrap();
    let rendered = compact(&terminal);
    assert!(
        rendered.contains("回到第N步·3/3·↑/↓·Enter·Esc"),
        "{rendered}"
    );
    assert!(rendered.contains("开头·文件✓"), "{rendered}");
    assert!(rendered.contains("第1步·文件✓·修登录"), "{rendered}");
    assert!(rendered.contains("▶第2步·仅对话·补测试"), "{rendered}");
    assert!(rendered.contains("之后的步骤会被丢弃"), "{rendered}");

    // 换一块新画布：TestBackend 按差异回写，宽字符的第二格会残留上一帧的字，
    // 那是测试后端的事，不是面板的。
    app.handle_rewind_picker_key(key(KeyCode::Enter));
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| render_rewind_picker(frame, &mut app))
        .unwrap();
    let rendered = compact(&terminal);
    assert!(
        rendered.contains("回到第2步·丢弃之后的1步·[Enter]仅对话"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("[v]"),
        "a step without a checkpoint offers no file option: {rendered}"
    );
}

#[test]
fn the_summary_line_reports_dropped_turns_and_file_changes() {
    let session = willdeep_runtime_protocol::RuntimeSession {
        id: uuid::Uuid::new_v4(),
        root_agent_id: uuid::Uuid::new_v4(),
        workspace: Some("/workspace".to_owned()),
        profile: None,
        model: None,
        approval_mode: None,
        status: willdeep_runtime_protocol::SessionStatus::Idle,
        active_turn_id: None,
        created_at: 1,
        updated_at: 2,
    };
    let result = willdeep_runtime_protocol::RewindSessionResult {
        session,
        message_count: 4,
        dropped_turn_ids: vec![uuid::Uuid::new_v4(), uuid::Uuid::new_v4()],
        workspace: Some(willdeep_runtime_protocol::WorkspaceRewindResult {
            checkpoint: "abc".to_owned(),
            before_checkpoint: "def".to_owned(),
            restored: vec!["src/lib.rs".to_owned()],
            removed: vec!["new.rs".to_owned(), "tmp.rs".to_owned()],
            skipped: Vec::new(),
            recovery_path: Some("/home/.willdeep/runtime/recovery/rewind-x".to_owned()),
        }),
    };
    let line = rewind_summary(&result, 2, Language::ZhCn);
    assert_eq!(
        line,
        "System: 已回到第 2 步，丢弃 2 步 · 文件恢复 1 · 移除 2 · 原件在 /home/.willdeep/runtime/recovery/rewind-x"
    );
    let chat_only = willdeep_runtime_protocol::RewindSessionResult {
        workspace: None,
        ..result
    };
    assert_eq!(
        rewind_summary(&chat_only, 0, Language::En),
        "System: Rewound to the beginning, dropped 2 step(s) · files untouched"
    );
}
