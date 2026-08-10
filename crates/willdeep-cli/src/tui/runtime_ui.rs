use super::*;

pub(super) fn open_remote_gate(
    app: &mut App,
    gate: crate::daemon::RemoteGate,
    home: PathBuf,
    ui: mpsc::UnboundedSender<UiMessage>,
) {
    match gate {
        crate::daemon::RemoteGate::Approval {
            id,
            description,
            always_allow_available,
        } => {
            let language = app.language;
            let (sender, receiver) = oneshot::channel();
            app.approval = Some((description, always_allow_available, sender));
            tokio::spawn(async move {
                let decision = receiver.await.unwrap_or(ApprovalDecision::Deny);
                let result = crate::daemon::resolve_remote_approval(&home, id, decision).await;
                let notice = match result {
                    Ok(()) => language
                        .text(
                            "Runtime 审批已解决",
                            "Runtime approval resolved",
                            "Runtime 承認を解決しました",
                        )
                        .to_owned(),
                    Err(error) => format!(
                        "{}: {error}",
                        language.text(
                            "Runtime 审批失败",
                            "Runtime approval failed",
                            "Runtime 承認に失敗"
                        )
                    ),
                };
                let _ = ui.send(UiMessage::RuntimeNotice(notice));
            });
        }
        crate::daemon::RemoteGate::Question {
            id,
            question,
            options,
            multi_select,
        } => {
            let language = app.language;
            let request = UserQuestion {
                question,
                options,
                multi_select,
            };
            let checked = vec![false; request.options.len()];
            let (sender, receiver) = oneshot::channel();
            app.question = Some(AskDialog {
                request,
                selected: 0,
                checked,
                answer: PromptEditor::default(),
                sender,
            });
            tokio::spawn(async move {
                let answer = receiver.await.unwrap_or(None);
                let result = crate::daemon::answer_remote_question(&home, id, answer).await;
                let notice = match result {
                    Ok(()) => language
                        .text(
                            "Runtime 问题已回答",
                            "Runtime question answered",
                            "Runtime の質問に回答しました",
                        )
                        .to_owned(),
                    Err(error) => format!(
                        "{}: {error}",
                        language.text(
                            "Runtime 回答失败",
                            "Runtime answer failed",
                            "Runtime 回答に失敗"
                        )
                    ),
                };
                let _ = ui.send(UiMessage::RuntimeNotice(notice));
            });
        }
    }
}
