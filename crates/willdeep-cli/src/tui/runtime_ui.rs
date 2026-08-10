use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PromptExecution {
    Runtime(String),
    Local(String),
}

pub(super) fn prompt_execution(prompt: &str) -> PromptExecution {
    let value = prompt.trim();
    if value == "/local" || value.starts_with("/local ") {
        return PromptExecution::Local(
            value
                .strip_prefix("/local")
                .unwrap_or_default()
                .trim()
                .to_owned(),
        );
    }
    if value == "/runtime" || value.starts_with("/runtime ") {
        return PromptExecution::Runtime(
            value
                .strip_prefix("/runtime")
                .unwrap_or_default()
                .trim()
                .to_owned(),
        );
    }
    PromptExecution::Runtime(prompt.to_owned())
}

pub(super) async fn submit_turn(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &TuiRuntime,
    prompt: String,
) -> Result<()> {
    let attachments: Vec<MessageAttachment> = std::mem::take(&mut app.attachments)
        .into_iter()
        .map(|value| value.message)
        .collect();
    let prompt = app.enrich_prompt(&prompt, &runtime.skills);
    let event_head = crate::daemon::runtime_event_head(&runtime.home)
        .await
        .unwrap_or(app.runtime_event_cursor);
    let remote_session = crate::daemon::ensure_runtime_session(
        &runtime.home,
        session.id,
        &session.workspace,
        session.profile.clone(),
        runtime.runtime_submit.model.clone(),
        session.title.clone(),
    )
    .await?;
    session.runtime_managed = true;
    if app.runtime_event_cursor == 0 {
        app.runtime_event_cursor = event_head;
        session.runtime_event_cursor = event_head;
    }
    // Persist ownership before scheduling the Turn. A very fast Harness must
    // never be overwritten by this client's stale history.
    store.save(session)?;
    crate::daemon::submit_runtime_turn(&runtime.home, remote_session.id, prompt, attachments)
        .await?;
    Ok(())
}

pub(super) fn apply_runtime_events(
    app: &mut App,
    mut events: Vec<crate::daemon::RemoteRuntimeEvent>,
    session: &mut Session,
    store: &SessionStore,
) -> Result<()> {
    events.sort_by_key(|event| event.sequence);
    let mut advanced = false;
    for event in events {
        if event.sequence <= app.runtime_event_cursor {
            continue;
        }
        if event.visible
            && event.session_id == Some(session.id)
            && let Some(message) = apply_runtime_event(app, &event)
            && !session.runtime_managed
        {
            session.messages.push(message);
        }
        app.runtime_event_cursor = event.sequence;
        advanced = true;
    }
    if advanced {
        if session.runtime_managed {
            let attention_read = session.attention_read.clone();
            let mut latest = store.load(session.id)?;
            latest.attention_read.extend(attention_read);
            latest.runtime_managed = true;
            latest.runtime_event_cursor = app.runtime_event_cursor;
            *session = latest;
        } else {
            session.runtime_event_cursor = app.runtime_event_cursor;
        }
        store.save(session)?;
    }
    Ok(())
}

fn apply_runtime_event(
    app: &mut App,
    event: &crate::daemon::RemoteRuntimeEvent,
) -> Option<Message> {
    match event.kind.as_str() {
        "task.output" => return apply_runtime_output(app, &event.message),
        "task.waiting_approval" => {
            app.notice = Some(
                app.language
                    .text(
                        "Runtime 任务正在等待审批",
                        "Runtime task is waiting for approval",
                        "Runtime タスクが承認待ちです",
                    )
                    .to_owned(),
            );
        }
        "task.waiting_answer" => {
            app.notice = Some(
                app.language
                    .text(
                        "Runtime 任务正在等待回答",
                        "Runtime task is waiting for an answer",
                        "Runtime タスクが回答待ちです",
                    )
                    .to_owned(),
            );
        }
        "task.failed" => app.append_transcript(format!(
            "Error: {}",
            app.language.text(
                "Runtime 任务失败",
                "Runtime task failed",
                "Runtime タスクが失敗しました"
            )
        )),
        "task.cancelled" => app.append_transcript(format!(
            "System: {}",
            app.language.text(
                "Runtime 任务已取消",
                "Runtime task cancelled",
                "Runtime タスクをキャンセルしました"
            )
        )),
        _ => {}
    }
    None
}

fn apply_runtime_output(app: &mut App, message: &str) -> Option<Message> {
    let (_task, payload) = message.split_once(' ')?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return None;
    };
    match value.get("type").and_then(|value| value.as_str()) {
        Some("turn_started") => {
            if let Some(turn) = value.get("turn").and_then(|value| value.as_u64()) {
                app.record_progress(format!(
                    "Runtime · {} {turn}",
                    app.language.text("轮次", "turn", "ターン")
                ));
            }
        }
        Some("tool_requested") => {
            if let Some(name) = value.get("name").and_then(|value| value.as_str()) {
                app.tools.requested(name);
                app.record_progress(format!(
                    "Runtime · {} {name}",
                    app.language.text("正在使用", "using", "使用中")
                ));
            }
        }
        Some("tool_completed") => {
            if let Some(name) = value.get("name").and_then(|value| value.as_str()) {
                let is_error = value
                    .get("is_error")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                app.tools.completed(name, is_error);
                app.record_progress(format!(
                    "Runtime · {} {name}",
                    if is_error {
                        app.language.text("失败", "failed", "失敗")
                    } else {
                        app.language.text("已完成", "finished", "完了")
                    }
                ));
            }
        }
        Some("usage") => {
            app.latest_usage = Usage {
                input_tokens: value.get("input_tokens").and_then(|value| value.as_u64()),
                output_tokens: value.get("output_tokens").and_then(|value| value.as_u64()),
                total_tokens: value.get("total_tokens").and_then(|value| value.as_u64()),
            };
            app.context_tokens = app.latest_usage.input_tokens.unwrap_or(app.context_tokens);
        }
        Some("subagent_started") => {
            let id = short_event_agent(&value);
            let profile = value
                .get("profile")
                .and_then(|value| value.as_str())
                .unwrap_or("agent");
            app.record_progress(format!(
                "Runtime · {} {id} · {profile}",
                app.language
                    .text("子 Agent 启动", "subagent started", "サブエージェント開始")
            ));
        }
        Some("subagent_turn_started") => {
            let id = short_event_agent(&value);
            let turn = value
                .get("turn")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            app.record_progress(format!(
                "Runtime · {} {id} · {} {turn}",
                app.language
                    .text("子 Agent", "subagent", "サブエージェント"),
                app.language.text("轮次", "turn", "ターン")
            ));
        }
        Some("subagent_tool_requested") => {
            let id = short_event_agent(&value);
            if let Some(name) = value.get("name").and_then(|value| value.as_str()) {
                app.tools.requested(name);
                app.record_progress(format!(
                    "Runtime · {id} · {} {name}",
                    app.language.text("正在使用", "using", "使用中")
                ));
            }
        }
        Some("subagent_tool_completed") => {
            let id = short_event_agent(&value);
            if let Some(name) = value.get("name").and_then(|value| value.as_str()) {
                let is_error = value
                    .get("is_error")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                app.tools.completed(name, is_error);
                app.record_progress(format!(
                    "Runtime · {id} · {} {name}",
                    if is_error {
                        app.language.text("失败", "failed", "失敗")
                    } else {
                        app.language.text("已完成", "finished", "完了")
                    }
                ));
            }
        }
        Some("subagent_completed") => {
            let id = short_event_agent(&value);
            let status = value
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            app.record_progress(format!(
                "Runtime · {} {id} · {status}",
                app.language
                    .text("子 Agent 结束", "subagent finished", "サブエージェント完了")
            ));
        }
        Some("completed") => {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                app.append_transcript(format!("WillDeep: {text}"));
                return Some(Message::assistant(text, Vec::new()));
            }
        }
        _ => {}
    }
    None
}

fn short_event_agent(value: &serde_json::Value) -> &str {
    value
        .get("id")
        .and_then(|value| value.as_str())
        .and_then(|id| id.get(..8))
        .unwrap_or("agent")
}

pub(super) fn open_remote_gate(
    app: &mut App,
    gate: crate::daemon::RemoteGate,
    home: PathBuf,
    ui: mpsc::UnboundedSender<UiMessage>,
) {
    match gate {
        crate::daemon::RemoteGate::Approval {
            id,
            task_id: _,
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
            task_id: _,
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
