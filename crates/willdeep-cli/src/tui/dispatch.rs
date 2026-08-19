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
        .collect();
    let enriched = app.enrich_prompt(&prompt, skills);
    let user = Message::user_with_attachments(enriched, attachments);
    session.messages.push(user.clone());
    store.save(session)?;
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, user).await,
        ));
    });
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
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Compressed(agent.compress_history(history).await));
    });
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
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, message).await,
        ));
    });
    Ok(())
}
