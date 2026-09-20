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
    super::permission_commands::sync_session(app, session, runtime).await?;
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
            let cursor = app.runtime_event_cursor;
            // 这一轮由守护进程执行，**AI 输出每到一块它就写一次这个文件**。
            // 此前这里是 load → 改 → save：基线在 load 那一刻定格，save 时磁盘
            // 早已又变了，而游标算在执行指纹里（session/execution_state.rs），
            // 于是每次输出到达都可能判成执行冲突，`?` 一抛整个 TUI 退出——用户
            // 看到的就是「AI 刚要输出就闪退」，而那一轮其实已经跑完。
            //
            // `update` 在会话锁内从磁盘最新快照起改，没有这个窗口：守护进程
            // 刚写进去的消息全部保留，这里只叠加自己的游标与已读标记。
            let apply = |latest: &mut Session| {
                latest.attention_read.extend(attention_read);
                latest.runtime_managed = true;
                latest.runtime_event_cursor = cursor;
            };
            match store.load(session.id) {
                // 空会话不为游标落盘，与 `tui::run` 的启动写入同一条规则：
                // 别的工作区的事件照样会推进游标，光坐着不说话不该因此多出
                // 一条空会话。只在内存里对齐，游标随第一条消息一起写下去。
                Ok(mut latest) if latest.messages.is_empty() => {
                    apply(&mut latest);
                    *session = latest;
                }
                Ok(_) => *session = store.update(session.id, apply)?,
                // 托管会话理应已经落过盘；真没有就让它保持内存态，
                // 下一次事件或提交时再写。
                Err(willdeep_core::session::SessionError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if let Some(plan) = &session.current_plan {
                sync_persisted_plan(&mut app.transcript, plan);
                app.refresh_transcript_height();
                app.scroll_from_bottom = app.scroll_from_bottom.min(app.max_scroll());
            }
        } else {
            session.runtime_event_cursor = app.runtime_event_cursor;
            // 同上：空会话不为游标落盘。托管分支不走这里——那边已经在锁内
            // 写过了，再存一次又会开一个新的竞态窗口。
            if !session.messages.is_empty() {
                store.save(session)?;
            }
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
        // 撞轮次上限、预算耗尽这类「没收敛」的收尾走 partial，不是 completed。
        // 漏了它界面就一直挂着「工作中」，直到快照对账强行复位。
        "task.completed" | "turn.completed" | "task.partial" | "turn.partial" => app.finish_turn(),
        "task.failed" => {
            // 事件里带着 `exit_code=…` 这类活下来的线索，此前被整条丢掉，
            // 只打印一句固定文案。完整命令与错误在侧栏详情里（`task.diagnostics`）。
            app.append_transcript(format!(
                "Error: {}{}{}",
                app.language.text(
                    "Runtime 任务失败",
                    "Runtime task failed",
                    "Runtime タスクが失敗しました"
                ),
                failure_reason_text(&event.message, app.language)
                    .map(|reason| format!(" · {reason}"))
                    .unwrap_or_default(),
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
/// 公共事件流只放行一个闭集合里的失败分类（见 `daemon::event_stream::failure_reason`）。
/// 这里把那个标签翻成一句人话——一串 `failure_domain=provider` 谁也看不出是自己
/// 的上下文炸了还是机房掉线。
fn failure_reason_text(message: &str, language: Language) -> Option<&'static str> {
    let reason = message
        .split_whitespace()
        .find_map(|part| part.strip_prefix("reason="))?;
    Some(match reason {
        "context_overflow" => language.text(
            "上下文超出模型窗口，先 /compress 或开新会话",
            "Context exceeds the model window — run /compress or start a new session",
            "コンテキストがモデルの上限を超過 — /compress か新しいセッションを",
        ),
        "rate_limited" => language.text(
            "provider 限流，稍后重试",
            "Rate limited by the provider — retry later",
            "provider のレート制限 — 後で再試行",
        ),
        "auth" => language.text(
            "provider 认证失败，检查 API Key",
            "Provider authentication failed — check the API key",
            "provider 認証失敗 — API キーを確認",
        ),
        "quota" => language.text(
            "provider 额度或余额不足",
            "Provider quota or balance exhausted",
            "provider のクォータ・残高不足",
        ),
        "timeout" => language.text(
            "请求超时",
            "The request timed out",
            "リクエストがタイムアウト",
        ),
        "network" => language.text(
            "网络或 TLS 中断",
            "Network or TLS failure",
            "ネットワーク／TLS 障害",
        ),
        "provider_unavailable" => language.text(
            "provider 暂时不可用",
            "The provider is temporarily unavailable",
            "provider が一時的に利用不可",
        ),
        _ => language.text(
            "原因未分类，侧栏 Inbox 里有完整错误",
            "Unclassified — the sidebar Inbox carries the full error",
            "未分類 — 詳細はサイドバー Inbox に",
        ),
    })
}

fn event_details(message: &str) -> String {
    let details = message
        .split_whitespace()
        .filter(|part| !part.starts_with("task_id="))
        .filter(|part| !part.starts_with("reason="))
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
    if !matches!(output_type, Some("completed" | "partial")) {
        app.ensure_runtime_turn();
    }
    match output_type {
        // 与进程内轮次一致：定稿的一段话直接落进聊天区，流式增量仍走下面的
        // 临时预览行。非托管会话的镜像也同步记一条。
        Some("assistant_text") => {
            if let Some(text) = value
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                app.note_narration(text);
                return Some(Message::assistant(text, Vec::new()));
            }
        }
        Some("assistant_text_delta") => {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                app.stream_transient(StreamKind::Reply, text);
            }
        }
        // 思考型模型的思维链：正文往往为空，这行是用户唯一能看到的「它在干嘛」。
        Some("reasoning_delta") => {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                app.stream_transient(StreamKind::Reasoning, text);
            }
        }
        Some("turn_started") => {
            app.transient_thought = None;
            if let Some(turn) = value.get("turn").and_then(|value| value.as_u64()) {
                app.record_progress(format!(
                    "Runtime · {} {turn}",
                    app.language.text("轮次", "turn", "ターン")
                ));
            }
        }
        Some("tool_requested") => {
            app.transient_thought = None;
            if let Some(name) = value.get("name").and_then(|value| value.as_str()) {
                app.tools.requested(name);
                app.record_progress(format!(
                    "Runtime · {} {name}",
                    app.language.text("正在使用", "using", "使用中")
                ));
                app.note_tool_requested(name, value.get("detail").and_then(|value| value.as_str()));
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
                app.note_tool_completed(
                    name,
                    value.get("detail").and_then(|value| value.as_str()),
                    is_error,
                );
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
        // 压缩的两条反馈此前只有进程内轮次认，Runtime 托管会话整条丢掉：状态栏
        // 的占用只跟着 `usage` 走，而 usage 是请求成功才回来的——压缩前的真实
        // 体量（一次真实故障里是 94 万 token）从来没上过屏，用户盯着一个压缩后
        // 的 36% 一头撞进模型上限。两条路径的反馈必须一致。
        Some("compression_started") => {
            if let Some(tokens) = value.get("estimated_tokens").and_then(|value| value.as_u64()) {
                app.context_tokens = tokens;
            }
            app.record_progress(
                app.language
                    .text(
                        "正在压缩上下文",
                        "Compressing context",
                        "コンテキストを圧縮中",
                    )
                    .to_owned(),
            );
        }
        Some("compression_completed") => {
            if let Some(tokens) = value.get("estimated_tokens").and_then(|value| value.as_u64()) {
                app.context_tokens = tokens;
            }
            let compressed =
                app.language
                    .text("上下文已压缩", "Context compressed", "コンテキストを圧縮しました");
            let dropped = value
                .get("dropped_messages")
                .and_then(|value| value.as_u64())
                .unwrap_or_default();
            app.record_progress(if dropped > 0 {
                app.language.pick(
                    format!("{compressed} · 本轮请求丢弃 {dropped} 条最旧消息（存档不受影响）"),
                    format!(
                        "{compressed} · dropped {dropped} oldest message(s) from this request (the archive is untouched)"
                    ),
                    format!("{compressed} · 今回のリクエストから最も古い {dropped} 件を破棄（アーカイブは無変更）"),
                )
            } else {
                compressed.to_owned()
            });
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
        Some("provider_retry_started" | "subagent_retry_started") => {
            app.record_progress(format!(
                "Runtime · {} · {}",
                short_event_agent(&value),
                app.language.text("正在重试", "Retrying", "再試行中")
            ));
        }
        Some("subagent_retry_wait" | "provider_retry_wait") => {
            let id = short_event_agent(&value);
            let attempt = value
                .get("attempt")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            let delay = value
                .get("delay_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            app.record_progress(format!(
                "Runtime · {} {id} · {} {attempt} · {}s",
                app.language
                    .text("子 Agent", "subagent", "サブエージェント"),
                app.language
                    .text("等待重试", "Waiting to retry", "再試行を待機中"),
                delay.div_ceil(1000)
            ));
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
        // partial 的 text 里带着「轮次上限已用尽」这类提示，和 completed 一样要落进记录。
        // 用户插话送进了任务却没赶上下一次调模型：Runtime 交回来，排队等本轮结束。
        Some("steer_undelivered") => {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                app.requeue_steering(text.to_owned());
            }
        }
        Some("completed" | "partial") => {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                // 最后一段多半已经作中途文字显示过，只补没见过的部分（比如轮次
                // 上限提示）；镜像持久化也只补这一部分。
                let remainder = app.note_reply(text);
                // Runtime 轮次没有 `AgentOutcome`，账目走本轮累计。
                app.append_turn_stats(None);
                app.finish_turn();
                return remainder.map(|text| Message::assistant(&text, Vec::new()));
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

    /// 守护进程正在执行这一轮时，前台跟随事件写游标不能把自己写死。
    ///
    /// 现场：AI 输出每到一块守护进程就写一次会话文件。此前这里是
    /// load → 改 → save，基线在 load 那刻定格，save 时磁盘已经又变了，而游标
    /// 算在执行指纹里，于是判成执行冲突、`?` 抛出、TUI 在「结果刚要显示」时退出。
    ///
    /// 这条测试模拟那个窗口：拿到快照之后、写回之前，另一个写者（守护进程）
    /// 追加了一条消息。旧写法必然失败；现在这条路必须既写进游标、又保住
    /// 守护进程刚写的消息。
    #[test]
    fn following_runtime_events_survives_a_daemon_write_mid_turn() {
        let (root, store) = temporary_store("tui-runtime-follow-race");
        let mut session = Session::new(root.clone(), None, "managed session");
        session.runtime_managed = true;
        session.messages.push(Message::user("请开始"));
        store.save(&mut session).unwrap();

        // 前台此刻手上的快照（基线已定格）。
        let stale = store.load(session.id).unwrap();

        // 守护进程在这中间写入了这一轮的输出。
        store
            .update(session.id, |latest| {
                latest
                    .messages
                    .push(Message::assistant("模型输出", Vec::new()));
            })
            .unwrap();

        // 游标已经不算执行状态（见 session/execution_state.rs），所以单写书签
        // 本身不会再被判冲突。这里仍走 `update`：在会话锁内一次读改写，不给
        // 守护进程的写入留下「读到的和写回去的不是同一版」的窗口。
        let visible = store
            .update(session.id, |latest| {
                latest.runtime_managed = true;
                latest.runtime_event_cursor = 6385;
            })
            .unwrap();

        assert_eq!(visible.runtime_event_cursor, 6385);
        assert_eq!(
            visible.messages.last().unwrap().content,
            "模型输出",
            "守护进程刚写的输出必须保住"
        );
        assert!(
            stale.messages.len() < visible.messages.len(),
            "前台那份旧快照没有把守护进程的写入盖掉"
        );
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
