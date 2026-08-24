use super::*;

pub(super) fn dispatch_prompt(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    skills: &SkillCatalog,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
    prompt: String,
) -> Result<()> {
    // `/goal` 是长程续推的开关：目标在场时，宿主会拒绝模型的隐式收口（long-horizon.v1 RA1）。
    if let Some(continuation) = agent.goal_continuation() {
        match app.goal.as_deref() {
            Some(goal) => {
                continuation.activate(goal, willdeep_core::GoalBudget::default());
            }
            None => continuation.clear(),
        }
    }
    app.tools.reset();
    app.begin_turn(
        false,
        app.language
            .text(
                "正在思考 · 理解你的请求",
                "Thinking · understanding your request",
                "思考中 · リクエストを理解しています",
            )
            .to_owned(),
    );
    let history = session.messages.clone();
    let attachments = std::mem::take(&mut app.attachments)
        .into_iter()
        .map(|value| value.message)
        .collect::<Vec<_>>();
    let enriched = app.enrich_prompt(&prompt, skills);
    // L1 用**原始**提示词，不用 enrich 之后的：enrich 会把技能清单之类的
    // 系统素材拼进去，那些东西当标题毫无意义。
    crate::titling::apply_derived_title(session, &prompt, !attachments.is_empty());
    let user = Message::user_with_attachments(enriched, attachments);
    session.messages.push(user.clone());
    store.save(session)?;
    let agent = agent.clone();
    let tx = tx.clone();
    // 句柄留着，Esc 中断本地轮次时要靠它把在途的 Harness 掐掉。
    app.local_turn = Some(tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, user).await,
        ));
    }));
    Ok(())
}

pub(super) fn dispatch_compress(
    app: &mut App,
    session: &Session,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
) {
    app.begin_turn(false, "Compressing context".to_owned());
    let history = session.messages.clone();
    let agent = agent.clone();
    let tx = tx.clone();
    app.local_turn = Some(tokio::spawn(async move {
        let _ = tx.send(UiMessage::Compressed(agent.compress_history(history).await));
    }));
}

pub(super) fn dispatch_notification(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
    notice: String,
) -> Result<()> {
    app.begin_turn(false, "Handling background result".to_owned());
    let history = session.messages.clone();
    let message = Message::user(notice);
    session.messages.push(message.clone());
    store.save(session)?;
    let agent = agent.clone();
    let tx = tx.clone();
    app.local_turn = Some(tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, message).await,
        ));
    }));
    Ok(())
}

/// L2 摘要跑在后台任务里：它是一次网络往返，在事件循环里 `await` 会让界面
/// 在每轮收尾时僵住。跑完只回一个可选标题，写库仍由主循环做。
///
/// `force` 是 `/session retitle` 走的路：绕过「一个进程只试一次」，但绕不过
/// 人自己起的名字。
pub(super) fn dispatch_retitle(
    session: &Session,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
    force: bool,
) {
    if force {
        if session.title_source == willdeep_core::TitleSource::User {
            return;
        }
    } else if !crate::titling::claim_summary_attempt(session) {
        return;
    }
    let messages = session.messages.clone();
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let title = crate::titling::summarized_title(&agent, &messages).await;
        let _ = tx.send(UiMessage::Retitled {
            title,
            requested: force,
        });
    });
}
