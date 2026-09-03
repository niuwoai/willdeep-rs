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
    persist_missing_core_session(session, store)?;
    let event_head = crate::daemon::runtime_event_head(&runtime.home)
        .await
        .unwrap_or(app.runtime_event_cursor);
    let remote_session = crate::daemon::ensure_runtime_session(
        &runtime.home,
        session.id,
        &session.workspace,
        session.profile.clone(),
        session.model.clone(),
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
    crate::daemon::submit_runtime_turn(
        &runtime.home,
        remote_session.id,
        prompt,
        attachments,
        crate::Surface::Tui,
    )
    .await?;
    app.tools.reset();
    app.begin_turn(
        true,
        app.language
            .text(
                "已提交 Runtime · 等待开始处理",
                "Submitted to Runtime · waiting to start",
                "Runtime に送信済み · 開始待ち",
            )
            .to_owned(),
    );
    Ok(())
}

/// 快照连续几次都说本会话没有活动任务，而界面还显示 Runtime 轮次在跑：去问
/// Runtime 一句，没有在途轮次就把残留的「工作中」复位——和 Esc 走的是同一个判据，
/// 只是不用人去按。Runtime 说有，或者根本问不到，都按兵不动：宁可多等一秒，
/// 也不复位一条真在跑的轮次。
pub(super) async fn reconcile_stale_runtime_turn(
    app: &mut App,
    session: &Session,
    runtime: &TuiRuntime,
) {
    match crate::daemon::remote_active_turn(&runtime.home, session.id).await {
        Ok(None) => {
            app.finish_turn();
            app.append_transcript(format!(
                "System: {}",
                app.language.text(
                    "Runtime 已无在途轮次，界面上残留的「工作中」已复位；排队的提示词继续发送",
                    "Runtime has no active turn; the stale busy state was reset and queued prompts continue",
                    "Runtime に進行中のターンはありません。残っていた実行中表示を戻し、キューのプロンプトを続行します",
                )
            ));
        }
        Ok(Some(_)) => app.stale_runtime_turn_snapshots = 0,
        Err(_) => {}
    }
}

/// Runtime 的 `session.create { id: Some(..) }` 是领养，不是凭空创建：对应 Core
/// Session 必须先存在。空白 TUI 会话为了不污染历史列表而有意不在启动时落盘，所以
/// 第一次真正提交恰好是补写它的最晚安全时机。
///
/// 已存在的会话只做可读性校验，绝不把 TUI 手里的旧副本写回去；否则另一个 Runtime
/// 客户端刚落下的消息可能被覆盖。Xedit 桥接会话也会由 `load` 找到，不会生成本地影子。
fn persist_missing_core_session(session: &mut Session, store: &SessionStore) -> Result<()> {
    match store.load(session.id) {
        Ok(_) => Ok(()),
        Err(willdeep_core::session::SessionError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            store
                .save(session)
                .context("persist Core Session before Runtime adoption")
        }
        Err(error) => Err(error).context("validate Core Session before Runtime adoption"),
    }
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
        // 同 `tui::run` 的启动写入：空会话不为游标落盘。别的工作区的事件照样
        // 会推进游标，光坐着不说话也能把一条空会话写出来。
        if !session.messages.is_empty() {
            store.save(session)?;
        }
    }
    Ok(())
}

fn apply_runtime_event(
    app: &mut App,
    event: &crate::daemon::RemoteRuntimeEvent,
) -> Option<Message> {
    match event.kind.as_str() {
        "task.output" => return apply_runtime_output(app, &event.message),
        "task.queued" | "task.started" => {
            app.ensure_runtime_turn();
            app.record_progress(
                app.language
                    .text(
                        "Runtime 已接收 · 正在启动",
                        "Runtime accepted · starting",
                        "Runtime が受信 · 起動中",
                    )
                    .to_owned(),
            );
        }
        "task.waiting_approval" => {
            app.ensure_runtime_turn();
            app.record_progress(
                app.language
                    .text("等待你的审批", "Waiting for your approval", "承認待ち")
                    .to_owned(),
            );
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
            app.ensure_runtime_turn();
            app.record_progress(
                app.language
                    .text("等待你的回答", "Waiting for your answer", "回答待ち")
                    .to_owned(),
            );
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
        "task.completed" | "turn.completed" => app.finish_turn(),
        "task.failed" => {
            // 事件里带着 `exit_code=…` 这类活下来的线索，此前被整条丢掉，
            // 只打印一句固定文案。完整命令与错误在侧栏详情里（`task.diagnostics`）。
            app.append_transcript(format!(
                "Error: {}{}",
                app.language.text(
                    "Runtime 任务失败",
                    "Runtime task failed",
                    "Runtime タスクが失敗しました"
                ),
                event_details(&event.message)
            ));
            app.record_progress(
                app.language
                    .text(
                        "任务失败 · 侧栏 Inbox 里按 Enter 看失败命令",
                        "Task failed · Enter on the sidebar Inbox item shows the failing command",
                        "タスク失敗 · サイドバー Inbox で Enter を押すと失敗コマンドを表示",
                    )
                    .to_owned(),
            );
            app.finish_turn();
        }
        "task.interrupted" => {
            app.append_transcript(format!(
                "Error: {}",
                app.language.text(
                    "Runtime 任务已中断",
                    "Runtime task was interrupted",
                    "Runtime タスクが中断しました"
                )
            ));
            app.finish_turn();
        }
        "task.cancelled" => {
            app.append_transcript(format!(
                "System: {}",
                app.language.text(
                    "Runtime 任务已取消",
                    "Runtime task cancelled",
                    "Runtime タスクをキャンセルしました"
                )
            ));
            app.finish_turn();
        }
        _ => {}
    }
    None
}

/// 事件消息形如 `task_id=… exit_code=1`。task_id 对用户没意义，剩下的有。
fn event_details(message: &str) -> String {
    let details = message
        .split_whitespace()
        .filter(|part| !part.starts_with("task_id="))
        .collect::<Vec<_>>()
        .join(" ");
    if details.is_empty() {
        String::new()
    } else {
        format!(" · {details}")
    }
}

fn apply_runtime_output(app: &mut App, message: &str) -> Option<Message> {
    let (_task, payload) = message.split_once(' ')?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return None;
    };
    let output_type = value.get("type").and_then(|value| value.as_str());
    if output_type != Some("completed") {
        app.ensure_runtime_turn();
    }
    match output_type {
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
            let usage = Usage {
                input_tokens: value.get("input_tokens").and_then(|value| value.as_u64()),
                output_tokens: value.get("output_tokens").and_then(|value| value.as_u64()),
                total_tokens: value.get("total_tokens").and_then(|value| value.as_u64()),
                cache_read_tokens: value
                    .get("cache_read_tokens")
                    .and_then(|value| value.as_u64()),
            };
            // 本轮账目要累计，状态栏要最后一次，两者不能互相顶替。
            app.record_turn_usage(&usage);
            app.latest_usage = usage;
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
                // Runtime 轮次没有 `AgentOutcome`，账目走本轮累计。
                app.append_turn_stats(None);
                app.finish_turn();
                return Some(Message::assistant(text, Vec::new()));
            }
            app.finish_turn();
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

/// Auto-open Runtime approvals and questions as soon as a snapshot reveals
/// them, instead of leaving a parked task discoverable only as an inbox row.
/// Returns true when a dialog became visible right now, so the caller can
/// ring the bell.
pub(super) fn surface_pending_gates(
    app: &mut App,
    home: &std::path::Path,
    ui: &mpsc::UnboundedSender<UiMessage>,
) -> bool {
    let live = app
        .runtime_gates
        .iter()
        .map(crate::daemon::RemoteGate::id)
        .collect::<std::collections::BTreeSet<_>>();
    // Forget gates the Runtime resolved elsewhere, so a re-raised
    // interaction can surface again.
    app.surfaced_gates.retain(|id| live.contains(id));
    let fresh = app
        .runtime_gates
        .iter()
        .filter(|gate| !app.surfaced_gates.contains(&gate.id()))
        .cloned()
        .collect::<Vec<_>>();
    let mut shown = false;
    for gate in fresh {
        shown |= open_remote_gate(app, gate, home.to_path_buf(), ui.clone());
    }
    shown
}

/// Returns true when the gate became visible immediately (rather than being
/// queued behind another approval).
pub(super) fn open_remote_gate(
    app: &mut App,
    gate: crate::daemon::RemoteGate,
    home: PathBuf,
    ui: mpsc::UnboundedSender<UiMessage>,
) -> bool {
    app.surfaced_gates.insert(gate.id());
    match gate {
        crate::daemon::RemoteGate::Approval {
            id,
            task_id: _,
            description,
            always_allow_available,
        } => {
            let language = app.language;
            let (sender, receiver) = oneshot::channel();
            let visible = app.enqueue_approval((description, always_allow_available, sender));
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
            visible
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
            let visible = app.enqueue_question(AskDialog {
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
            visible
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_store(label: &str) -> (PathBuf, SessionStore) {
        let root = std::env::temp_dir().join(format!(
            "willdeep-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SessionStore::new(&root);
        (root, store)
    }

    #[test]
    fn first_runtime_submission_persists_the_core_session_before_adoption() {
        let (root, store) = temporary_store("tui-first-runtime-adoption");
        let mut session = Session::new(root.clone(), None, "");
        assert!(store.load(session.id).is_err());

        persist_missing_core_session(&mut session, &store).unwrap();

        let persisted = store.load(session.id).unwrap();
        assert_eq!(persisted.id, session.id);
        assert!(persisted.messages.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adoption_prerequisite_never_overwrites_existing_core_history() {
        let (root, store) = temporary_store("tui-adoption-preserves-history");
        let mut visible = Session::new(root.clone(), None, "stale TUI copy");
        let mut persisted = visible.clone();
        persisted
            .messages
            .push(Message::assistant("new Runtime answer", Vec::new()));
        store.save(&mut persisted).unwrap();

        persist_missing_core_session(&mut visible, &store).unwrap();

        let restored = store.load(visible.id).unwrap();
        assert_eq!(restored.messages.len(), 1);
        assert_eq!(restored.messages[0].content, "new Runtime answer");
        std::fs::remove_dir_all(root).unwrap();
    }
}
