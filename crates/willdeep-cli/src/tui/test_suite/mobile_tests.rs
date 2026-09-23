use super::*;

fn status(
    enabled: bool,
    connected: bool,
    phone_active: bool,
) -> willdeep_runtime_protocol::MobileRelayStatus {
    willdeep_runtime_protocol::MobileRelayStatus {
        enabled,
        connected,
        phone_active,
        last_phone_command_at: None,
        relay_host: Some("j.niuwoai.com".to_owned()),
    }
}

/// 侧栏状态来自 Runtime：这个终端开没开过 `/mobile` 都一样。
#[test]
fn sidebar_relay_labels_follow_the_runtime_status() {
    let mut app = App::new(Vec::new(), Language::En);
    assert_eq!(
        sidebar::mobile_relay_labels(&app),
        ("Runtime unavailable", None)
    );
    app.mobile_status = Some(status(false, false, false));
    assert_eq!(sidebar::mobile_relay_labels(&app), ("off", None));
    app.mobile_status = Some(status(true, false, false));
    assert_eq!(
        sidebar::mobile_relay_labels(&app),
        ("reconnecting", Some("not connected"))
    );
    app.mobile_status = Some(status(true, true, true));
    assert_eq!(
        sidebar::mobile_relay_labels(&app),
        ("connected", Some("active"))
    );
}

#[tokio::test]
async fn mobile_commands_are_remote_controls_for_the_runtime_relay() {
    let mut app = App::new(Vec::new(), Language::En);
    let (tx, _rx) = mpsc::unbounded_channel();
    let home = std::path::Path::new("/nonexistent-willdeep-home");
    assert!(!app.handle_mobile_command("/mobile please", home, &tx));
    assert!(!app.handle_mobile_command("hello", home, &tx));

    // 只收起二维码，不碰 Runtime 里的中继。
    app.mobile_qr = Some("▀▄".to_owned());
    assert!(app.handle_mobile_command("/mobile hide", home, &tx));
    assert!(app.mobile_qr.is_none());
}

#[test]
fn relay_results_update_the_qr_and_the_sidebar() {
    let mut app = App::new(Vec::new(), Language::En);
    app.apply_mobile_relay(MobileRelayUpdate::Enabled {
        qr: "▀▄▀".to_owned(),
        status: status(true, true, false),
    });
    assert_eq!(app.mobile_qr.as_deref(), Some("▀▄▀"));
    assert!(
        app.mobile_status
            .as_ref()
            .is_some_and(|status| status.enabled)
    );
    assert!(
        app.transcript
            .iter()
            .any(|line| line.contains("stays up after this terminal closes")),
        "要讲清楚中继归 Runtime：关终端不会断"
    );

    app.apply_mobile_relay(MobileRelayUpdate::Disabled);
    assert!(app.mobile_qr.is_none());
    assert!(
        app.mobile_status
            .as_ref()
            .is_some_and(|status| !status.enabled && !status.connected)
    );

    app.apply_mobile_relay(MobileRelayUpdate::Failed(
        "the running Runtime predates the mobile relay".to_owned(),
    ));
    assert!(
        app.transcript
            .last()
            .is_some_and(|line| line.starts_with("Error:") && line.contains("predates"))
    );
}

/// 手机上先答掉的审批：TUI 里对应的对话框收起，排在后面的顶上来。
#[test]
fn dialogs_nobody_waits_for_anymore_are_withdrawn() {
    let mut app = App::new(Vec::new(), Language::En);
    let (answered_elsewhere, receiver) = oneshot::channel();
    app.enqueue_approval((
        "run command: cargo test".to_owned(),
        false,
        answered_elsewhere,
    ));
    let (still_waiting, mut live) = oneshot::channel();
    app.enqueue_approval(("write file: src/main.rs".to_owned(), false, still_waiting));
    let (question_sender, question_receiver) = oneshot::channel();
    app.enqueue_question(AskDialog {
        request: UserQuestion {
            question: "Which branch?".to_owned(),
            options: vec!["main".to_owned()],
            multi_select: false,
        },
        selected: 0,
        checked: vec![false],
        answer: PromptEditor::default(),
        sender: question_sender,
    });

    assert_eq!(app.withdraw_abandoned_dialogs(), 0, "都还有人等着");
    drop(receiver);
    drop(question_receiver);
    assert_eq!(app.withdraw_abandoned_dialogs(), 2);
    assert!(
        app.approval
            .as_ref()
            .is_some_and(|(description, _, _)| description.contains("src/main.rs")),
        "排在后面的审批顶上来"
    );
    assert!(app.question.is_none());
    app.resolve_approval(|_| ApprovalDecision::AllowOnce);
    assert_eq!(live.try_recv(), Ok(ApprovalDecision::AllowOnce));
}
