use super::*;

/// 落一轮已经跑完的结果；撞上并发执行冲突时，重载最新快照再写一次。
///
/// 为什么这里可以「重载后照写」，而 `SessionStore::save` 的冲突判定又必须留着：
/// 本轮的执行所有权由本进程持有（调用方在保存之后才 drop `ownership`），手里这
/// 份消息就是这条会话的权威续写。磁盘上那点变化不是另一轮对话——实测是会话被
/// 运行时接管时守护进程把 `runtime_managed` 置了位，而这个字段恰好算在执行指纹
/// 里（见 `session/execution_state.rs`），于是一次纯标志位的写入被判成了执行状态
/// 冲突。
///
/// 此前这里直接 `?`：用户敲完提示词、模型跑完一整轮，**结果刚要显示的那一刻**
/// TUI 退出，而那一轮的输出连同 token 一起丢掉。为一个标志位赔上一整轮，怎么算
/// 都不划算。
///
/// 取舍说明：如果磁盘上真有另一轮对话（两个执行者同时写一条会话），这里会用本轮
/// 结果覆盖它。那种情况本身已经是坏的，而原先的行为是两边都保不住——至少这样能
/// 保住用户刚刚等来的这一轮。
fn persist_turn_result(session: &mut Session, store: &SessionStore) -> Result<()> {
    match store.save(session) {
        Ok(()) => Ok(()),
        Err(willdeep_core::session::SessionError::ConcurrentUpdate(_)) => {
            let messages = std::mem::take(&mut session.messages);
            // `update` 在锁内从磁盘最新快照起改：别人刚写进去的字段全部保留，
            // 只把本轮的消息放上去。
            *session = store.update(session.id, |latest| latest.messages = messages)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn event_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent: Arc<Agent>,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    language: Language,
) -> Result<()> {
    // 先跟运行时对齐，再渲染。`runtime_event_head` 会在 CLI 版本变化时停掉旧
    // 守护进程、起一个新的，而新守护在恢复阶段会重写会话文件；把这一步放在
    // 组装 transcript 之后，屏幕上就是一份守护进程刚刚改过的过期记录。
    if session.runtime_event_cursor == 0 {
        let head = crate::daemon::runtime_event_head(&runtime.home)
            .await
            .unwrap_or_default();
        session.runtime_event_cursor = head;
        // 一条还没说过话的会话不该为了记一个事件游标就落盘。此前每开一次 TUI
        // 不敲字就关，磁盘上就多一条 0 消息会话，历史列表被它们挤满——而它们
        // 什么都没记录。游标会在第一条提示词落盘时一起写下去；在那之前丢掉它
        // 的唯一后果是下次从事件流头部重读，而空会话没有任何东西要重放。
        if !session.messages.is_empty() {
            // 用 `update` 而不是 `save`：这里写的只是事件游标这一个书签，而上面
            // 那次 `runtime_event_head` 可能刚好发生过守护进程换版重启——它在恢复
            // 阶段写过这个会话，于是 `save` 手上的基线在这一刻已经过期，冲突会以
            // `concurrent session update conflicts on execution` 把整个 TUI 打死
            // （升级后第一次启动必现）。`update` 从磁盘最新快照起改，书签重放上去
            // 无损，顺带把守护进程刚恢复出来的内容接回内存。
            match store.update(session.id, |latest| latest.runtime_event_cursor = head) {
                Ok(latest) => *session = latest,
                // 还没落过盘的会话没有「最新快照」可改，按原路首次写入。
                Err(willdeep_core::session::SessionError::Io(error))
                    if error.kind() == io::ErrorKind::NotFound =>
                {
                    store.save(session)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    let mut initial_transcript = session_transcript(session, language);
    if initial_transcript.is_empty() {
        initial_transcript.push(welcome_message(&session.workspace, language));
    }
    let mut app = App::new(initial_transcript, language);
    app.approval_mode = agent.approval_mode_handle().get();
    let (media_resize_tx, mut media_resize_rx) =
        mpsc::unbounded_channel::<ratatui_image::thread::ResizeRequest>();
    app.media = MediaState::detect(media_resize_tx);
    app.goal = session.goal.clone();
    app.runtime_event_cursor = session.runtime_event_cursor;
    app.workspace = Some(session.workspace.clone());
    app.workspace_status = workspace_status(&session.workspace, language);
    app.workspace_attention = workspace_attention(&session.workspace);
    app.attention_read = session.attention_read.clone();
    app.context_window = runtime.context_window.max(1);
    app.background_tasks = runtime.background_tasks.snapshots();
    let mut background_rx = runtime.background_tasks.subscribe();
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    let (runtime_snapshot_tx, mut runtime_snapshot_rx) =
        mpsc::unbounded_channel::<crate::daemon::RuntimeSnapshot>();
    let snapshot_in_flight = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (runtime_event_tx, mut runtime_event_rx) =
        mpsc::unbounded_channel::<Vec<crate::daemon::RemoteRuntimeEvent>>();
    let mut _runtime_event_follower = crate::daemon::start_runtime_event_follower(
        runtime.home.clone(),
        app.runtime_event_cursor,
        runtime.runtime_submit.workspace.clone(),
        runtime_event_tx.clone(),
    );
    let (mobile_tx, mut mobile_rx) = mpsc::unbounded_channel::<MobilePrompt>();
    loop {
        if let Some(target) = app.pending_session_switch.take() {
            if app.running {
                app.notice = Some(
                    language
                        .text(
                            "当前会话正在运行，结束后才能切换历史会话",
                            "Wait for the current turn to finish before switching Sessions",
                            "現在のターンが完了してから履歴セッションを切り替えてください",
                        )
                        .to_owned(),
                );
                continue;
            }
            let target_id = match uuid::Uuid::parse_str(&target.id) {
                Ok(id) => id,
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text(
                            "历史会话 ID 无效",
                            "Historical Session ID is invalid",
                            "履歴セッション ID が無効です"
                        )
                    ));
                    continue;
                }
            };
            let archived = if target.archived {
                true
            } else {
                crate::daemon::remote_session_states(&runtime.home)
                    .await
                    .ok()
                    .and_then(|states| {
                        states
                            .into_iter()
                            .find(|state| state.id == target_id)
                            .map(|state| state.archived)
                    })
                    .unwrap_or(false)
            };
            let unarchive = if archived {
                crate::daemon::set_remote_session_archived(&runtime.home, target_id, false).await
            } else {
                Ok(())
            };
            let switched = match unarchive {
                Ok(()) => {
                    session_commands::switch(&mut app, session, store, runtime, &target.id).await
                }
                Err(error) => Err(error),
            };
            match switched {
                Ok(message) => app.notice = Some(message),
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text(
                            "切换历史会话失败",
                            "Switch historical Session failed",
                            "履歴セッションの切り替えに失敗"
                        )
                    ));
                }
            }
        }
        // 排队的提示词在这里续上：本轮正常结束、被 Esc 中断、或 Runtime 报失败，
        // 都会走到这，不必在每个终态各写一遍。**待投递的运行时事件优先**——
        // 用户排在后面的那句话，很可能正是基于还没看到的后台结果说的。
        if !app.running
            && runtime.kernel.pending_wake_authority(session.id).is_none()
            && let Some(queued) = app.queued_prompts.pop_front()
        {
            app.attachments = queued.attachments;
            app.selected_attachment = 0;
            if queued.from_phone {
                app.append_transcript(format!("Phone: {}", queued.text));
                dispatch_prompt(
                    &mut app,
                    session,
                    store,
                    &runtime.skills,
                    &agent,
                    &runtime.tx,
                    queued.text,
                )?;
            } else {
                match prompt_execution(&queued.text) {
                    PromptExecution::Local(prompt) if !prompt.is_empty() => {
                        dispatch_prompt(
                            &mut app,
                            session,
                            store,
                            &runtime.skills,
                            &agent,
                            &runtime.tx,
                            prompt,
                        )?;
                    }
                    PromptExecution::Local(_) => {}
                    PromptExecution::Runtime(prompt) => {
                        match runtime_ui::submit_turn(&mut app, session, store, runtime, prompt)
                            .await
                        {
                            Ok(()) => {
                                app.notice = Some(
                                    language
                                        .text(
                                            "队列中的提示词已发出",
                                            "Queued prompt submitted",
                                            "キューのプロンプトを送信しました",
                                        )
                                        .to_owned(),
                                )
                            }
                            Err(error) => app.append_transcript(format!(
                                "Error: {}: {error}",
                                language.text(
                                    "提交排队的提示词失败",
                                    "Submitting the queued prompt failed",
                                    "キューのプロンプト送信に失敗"
                                )
                            )),
                        }
                    }
                }
            }
            continue;
        }
        draw(term, &mut app, &runtime.skills)?;
        tokio::select! {
            _=refresh.tick()=>{
                // Webhook delivery is detached; drain its failures here so a
                // dead endpoint shows up as a notice instead of silence.
                if let Some(error)=runtime.notifier.take_error(){app.notice=Some(format!("{}: {error}",language.text("通知 Webhook","Notification webhook","通知 Webhook")));}
                app.background_tasks=runtime.background_tasks.snapshots();
                // 事件内核的用户侧投影跟着秒级刷新走：它是观察面，不需要自己
                // 的通知通道。落盘也在这里收口——状态每变一次就写一次盘，
                // 打字的时候会卡在磁盘上。
                app.kernel_attention=runtime.kernel.pending_for_user().iter().map(AttentionItem::from_kernel_event).collect();
                // 脱离作业没有完成回调，只能在这里按秒看记录。发布之后要像进程内
                // 任务那样唤醒空闲会话，否则结论会一直躺在队列里等用户开口。
                let finished_jobs=crate::detached_delivery::publish_finished_jobs(&runtime.kernel,&runtime.detached_jobs,session.id);
                willdeep_core::kernel_store::flush(&runtime.kernel,&runtime.kernel_store);
                if finished_jobs>0 {
                    app.notice=Some(format!("{finished_jobs} background job(s) finished · queued as runtime events"));
                    execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;
                    wake_for_kernel_events(&mut app,session,store,&agent,runtime)?;
                }
                // 监视器事件已经直接进了内核。唤醒是否放行由内核判：被节流的事件只
                // 入队（Enqueue），不会把空闲会话拉起来。忙的时候回合收尾会再看一次。
                if runtime.background_tasks.take_monitor_signals()>0 {
                    willdeep_core::kernel_store::flush(&runtime.kernel,&runtime.kernel_store);
                    wake_for_kernel_events(&mut app,session,store,&agent,runtime)?;
                }
                // 上一份快照还没回来就不再叠加一份：几份并行在途的快照回来的
                // 顺序没有保证，越多越容易把一份旧的排到新的后面。
                if !snapshot_in_flight.swap(true,std::sync::atomic::Ordering::AcqRel) {
                    let home=runtime.home.clone();
                    let tx=runtime_snapshot_tx.clone();
                    let workspace=runtime.runtime_submit.workspace.clone();
                    let session_id=session.id;
                    let in_flight=snapshot_in_flight.clone();
                    tokio::spawn(async move {
                        let result=crate::daemon::runtime_snapshot(&home,&workspace,Some(session_id),crate::Surface::Tui).await;
                        in_flight.store(false,std::sync::atomic::Ordering::Release);
                        if let Ok(snapshot)=result{let _=tx.send(snapshot);}
                    });
                }
            },
            Some(snapshot)=runtime_snapshot_rx.recv()=>{
                runtime.notifier.attention_snapshot(&snapshot.attention);
                if app.observe_runtime_tasks(&snapshot.tasks,session.id,snapshot.event_sequence) {
                    runtime_ui::reconcile_stale_runtime_turn(&mut app,session,runtime).await;
                }
                app.runtime_attention=snapshot.attention;
                app.runtime_gates=snapshot.gates;
                app.runtime_agents=snapshot.agents;
                app.runtime_tools=snapshot.tools;
                app.runtime_artifacts=snapshot.artifacts;
                app.observe_runtime_version(snapshot.runtime_version);
                if runtime_ui::surface_pending_gates(&mut app,&runtime.home,&runtime.tx){
                    execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;
                }
            },
            Some(events)=runtime_event_rx.recv()=>runtime_ui::apply_runtime_events(&mut app,events,session,store)?,
            Some(request)=media_resize_rx.recv()=>{
                let tx=runtime.tx.clone();
                tokio::spawn(async move {
                    let result=tokio::task::spawn_blocking(move || request.resize_encode().map_err(|error|error.to_string()))
                        .await
                        .unwrap_or_else(|error|Err(format!("image resize worker failed: {error}")));
                    let _=tx.send(UiMessage::MediaResized(result));
                });
            },
            event=events.next()=>if let Some(Ok(event))=event { match event {
                Event::Paste(value)=>{
                    if app.approval.is_some() {
                        app.handle_approval_text(&value);
                    } else if app.routing_settings_paste(&value) {
                    } else if let Some(picker)=app.model_picker.as_mut() {
                        picker.editor.insert(&value);
                        app.refresh_model_picker_matches();
                    } else if let Some(picker)=app.session_picker.as_mut() {
                        picker.editor.insert(&value);
                        refresh_session_picker(&mut app,runtime,session).await;
                    } else {
                        app.handle_paste(value);
                    }
                },
                Event::Mouse(mouse)=>{
                    if app.question.is_some()||app.approval.is_some() {
                        if mouse.kind==MouseEventKind::Down(MouseButton::Left) {
                            app.handle_mouse(mouse.column,mouse.row,&runtime.background_tasks,&runtime.skills);
                        }
                        continue;
                    }
                    // 点在弹层边界外 == 按 Esc。放在所有弹层各自的鼠标处理之前，
                    // 否则「点外面」会先被下面那些 `continue` 吃掉。
                    if mouse.kind==MouseEventKind::Down(MouseButton::Left)
                        && app.dismiss_overlay_on_outside_click(mouse.column,mouse.row)
                    {
                        continue;
                    }
                    if app.media.is_open() {
                        let action=app.media.handle_mouse(mouse);
                        dispatch_media_action(action,&mut app,runtime);
                        continue;
                    }
                    if app.routing_settings.is_some() {
                        continue;
                    }
                    if app.model_picker.is_some() {
                        match mouse.kind {
                            MouseEventKind::ScrollUp=>app.model_picker_scroll(-1),
                            MouseEventKind::ScrollDown=>app.model_picker_scroll(1),
                            MouseEventKind::Down(MouseButton::Left)=>{
                                if let Some(model)=app.activate_model_picker_at(mouse.column,mouse.row) {
                                    match switch_model(&model,&mut app,session,store,runtime,&agent).await {
                                        Ok(message)=>{app.model_picker=None;app.append_transcript(format!("System: {message}"));},
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("切换模型失败","Model switch failed","モデル切替に失敗"))),
                                    }
                                }
                            },
                            _=>{},
                        }
                        continue;
                    }
                    if app.session_picker.is_some() {
                        if mouse.kind==MouseEventKind::Down(MouseButton::Left) {
                            app.activate_session_picker_at(mouse.column,mouse.row);
                        }
                        continue;
                    }
                    if let Some(action)=diff_review_mouse_action(app.diff_review.is_some(),mouse.kind) {
                        if let Some(review)=app.diff_review.as_mut()
                            && review.preview_draft.is_none()
                        {
                            match action {
                                DiffReviewMouseAction::ScrollUp=>review.scroll=review.scroll.saturating_sub(3),
                                DiffReviewMouseAction::ScrollDown=>review.scroll=review.scroll.saturating_add(3),
                                DiffReviewMouseAction::Consume=>{},
                            }
                        }
                        continue;
                    }
                    if mouse.kind==MouseEventKind::Down(MouseButton::Left)
                        && let Some(action)=app.diff_attention_action_at(mouse.column,mouse.row)
                    {
                        if let Err(error)=handle_diff_attention_action(action,&mut app,session,store,runtime,language).await {
                            app.notice=Some(format!("{}: {error}",language.text("Diff 操作失败","Diff action failed","Diff 操作に失敗")));
                        }
                        continue;
                    }
                    if mouse.kind==MouseEventKind::Down(MouseButton::Left)
                        && let Some(action)=app.agent_detail_action_at(mouse.column,mouse.row)
                    {
                        handle_agent_detail_action(action,&mut app,runtime,language).await;
                        continue;
                    }
                    if app.diff_review.is_none()
                        && app.agent_detail.is_none()
                        && app.task_detail.is_none()
                        && app.attention_detail.is_none()
                        && app.worktree_review.is_none()
                        && app.handle_chat_selection_mouse(mouse)
                    {
                        continue;
                    }
                    match mouse.kind {
                    MouseEventKind::Down(_)=>{
                        app.handle_mouse(mouse.column,mouse.row,&runtime.background_tasks,&runtime.skills);
                        if app.attention_detail.is_some()
                            && let Some(gate)=app.selected_remote_gate()
                        {
                            app.attention_detail=None;
                            open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                        }
                    },
                    MouseEventKind::ScrollUp if app.agent_detail.is_some()=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown if app.agent_detail.is_some()=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(3),
                    MouseEventKind::ScrollUp if app.task_detail.is_some()=>app.task_detail_scroll=app.task_detail_scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown if app.task_detail.is_some()=>app.task_detail_scroll=app.task_detail_scroll.saturating_add(3),
                    MouseEventKind::ScrollUp if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(-3),
                    MouseEventKind::ScrollDown if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(3),
                    MouseEventKind::ScrollUp if app.transcript_rect.contains((mouse.column,mouse.row).into())=>{app.focus=FocusPane::Chat;app.scroll_up(3);},
                    MouseEventKind::ScrollDown if app.transcript_rect.contains((mouse.column,mouse.row).into())=>{app.focus=FocusPane::Chat;app.scroll_down(3);},
                    MouseEventKind::ScrollUp=>app.scroll_up(3),
                    MouseEventKind::ScrollDown=>app.scroll_down(3),
                    _=>{}
                }},
                Event::Key(key) if key.kind==KeyEventKind::Press=>{
                    if app.native_selection_mode {
                        if selection_mode_exit_key(key) {
                            execute!(term.backend_mut(), EnableMouseCapture)?;
                            app.exit_selection_mode();
                            app.notice=Some(language.text("已恢复 WillDeep 鼠标操作","WillDeep mouse controls restored","WillDeep のマウス操作を復元しました").to_owned());
                        }
                        continue;
                    }
                    if app.selection_mode {
                        if is_selection_copy_key(key) {
                            app.copy_chat_selection();
                        } else if key.code==KeyCode::Char('q')&&!key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER|KeyModifiers::ALT) {
                            app.quote_chat_selection();
                        } else if selection_mode_exit_key(key) {
                            app.exit_selection_mode();
                            app.notice=Some(language.text("已恢复鼠标滚动和点击","Mouse scrolling and clicks restored","マウス操作を復元しました").to_owned());
                        }
                        continue;
                    }
                    if app.routing_settings.is_none()&&key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('s'){
                        execute!(term.backend_mut(), DisableMouseCapture)?;
                        app.enter_native_selection_mode();
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('c'){break;}
                    if key.code==KeyCode::Esc&&app.mobile_qr.take().is_some(){continue;}
                    if app.question.is_some(){app.handle_question_key(key);continue;}
                    if app.approval.is_some(){
                        app.handle_approval_key(key);
                        continue;
                    }
                    if app.media.is_open(){
                        let action=app.media.handle_key(key);
                        dispatch_media_action(action,&mut app,runtime);
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('l'){
                        if !app.media.open(&app.transcript){
                            app.notice=Some(language.text("当前会话里没有链接或图片","No links or images in this Session","現在のセッションにリンクや画像はありません").to_owned());
                        }
                        continue;
                    }
                    if let Some(detail)=app.attention_detail.clone(){
                        if detail.source==AttentionSource::DiffReview {
                            let action=diff_attention_action_for_key(key.code);
                            if let Some(action)=action {
                                if let Err(error)=handle_diff_attention_action(action,&mut app,session,store,runtime,language).await {
                                    app.notice=Some(format!("{}: {error}",language.text("Diff 操作失败","Diff action failed","Diff 操作に失敗")));
                                }
                            } else if key.code==KeyCode::Esc {
                                app.attention_detail=None;
                            }
                        } else if key.code==KeyCode::Esc{app.attention_detail=None;}
                        else if matches!(key.code,KeyCode::Char('m')|KeyCode::Char('M')) {
                            // Dismiss from the detail popup itself. Until now
                            // `M` only worked with the sidebar focused on the
                            // Inbox section, so an item whose only sane action
                            // was "stop showing me this" had no exit here.
                            if app.attention_dismiss(&detail.id) {
                                session.attention_read=app.attention_read.clone();
                                store.save(session)?;
                                app.notice=Some(language.text("已从 Inbox 移除该条目","Item dismissed from the Inbox","この項目を Inbox から削除しました").to_owned());
                            } else {
                                app.notice=Some(language.text("运行中的条目不能忽略","A running item cannot be dismissed","実行中の項目は削除できません").to_owned());
                            }
                        }
                        else if key.code==KeyCode::Enter
                            && let Some(gate)=app.selected_remote_gate()
                        {
                            app.attention_detail=None;
                            open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                        }
                        continue;
                    }
                    if app.task_detail.is_some(){app.handle_task_detail_key(key,&runtime.background_tasks);continue;}
                    if let Some(review)=app.worktree_review.clone(){
                        match key.code {
                            KeyCode::Esc=>app.worktree_review=None,
                            KeyCode::Char('m')|KeyCode::Char('M') if review.can_merge=>{
                                match crate::daemon::remote_merge(&runtime.home,review.agent_id,review.id).await {
                                    Ok(result)=>{app.notice=Some(format!("{} · {}",language.text("Worktree 已合并","Worktree merged","Worktree をマージしました"),result.root_snapshot_id));app.worktree_review=None;app.agent_detail=None;},
                                    Err(error)=>app.notice=Some(format!("{}: {error}",language.text("合并失败","Merge failed","マージ失敗"))),
                                }
                            }
                            _=>{}
                        }
                        continue;
                    }
                    if let Some(agent)=app.agent_detail.clone(){
                        match key.code {
                            KeyCode::Esc=>{app.agent_detail=None;app.agent_detail_scroll=0;},
                            KeyCode::Up=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(1),
                            KeyCode::Down=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(1),
                            KeyCode::PageUp=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(8),
                            KeyCode::PageDown=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(8),
                            KeyCode::Home=>app.agent_detail_scroll=0,
                            KeyCode::End=>app.agent_detail_scroll=usize::MAX,
                            KeyCode::Char('i')|KeyCode::Char('I') if agent.background&&agent.status==willdeep_core::RuntimeStatus::Working=>handle_agent_detail_action(AgentDetailAction::Instruct,&mut app,runtime,language).await,
                            KeyCode::Char('k')|KeyCode::Char('K') if agent.background&&agent.status==willdeep_core::RuntimeStatus::Working=>handle_agent_detail_action(AgentDetailAction::Stop,&mut app,runtime,language).await,
                            KeyCode::Char('r')|KeyCode::Char('R') if agent.background&&matches!(agent.status,willdeep_core::RuntimeStatus::Blocked|willdeep_core::RuntimeStatus::Failed|willdeep_core::RuntimeStatus::Done|willdeep_core::RuntimeStatus::Partial|willdeep_core::RuntimeStatus::Cancelled)=>handle_agent_detail_action(AgentDetailAction::Retry,&mut app,runtime,language).await,
                            KeyCode::Char('m')|KeyCode::Char('M') if agent.background&&matches!(agent.status,willdeep_core::RuntimeStatus::Blocked|willdeep_core::RuntimeStatus::Failed|willdeep_core::RuntimeStatus::Done|willdeep_core::RuntimeStatus::Partial|willdeep_core::RuntimeStatus::Cancelled)=>handle_agent_detail_action(AgentDetailAction::RetryWithModel,&mut app,runtime,language).await,
                            KeyCode::Char('w')|KeyCode::Char('W') if agent.dedicated_worktree=>handle_agent_detail_action(AgentDetailAction::ReviewWorktree,&mut app,runtime,language).await,
                            _=>{}
                        }
                        continue;
                    }
                    if app.diff_review.is_some(){
                        let mut close=false;
                        let mut force_full_redraw=false;
                        let mut open_file=None;
                        let mut review_action=None;
                        let mut revert_action=None;
                        let mut commit_preview_action=None;
                        let mut preview_draft_handled=false;
                        if let Some(review)=app.diff_review.as_mut(){
                            if review.commit_preview.is_some() {
                                if key.code==KeyCode::Esc{review.commit_preview=None;}
                                continue;
                            } else if let Some(draft)=review.preview_draft.as_mut() {
                                preview_draft_handled=true;
                                match key.code {
                                    KeyCode::Esc=>review.preview_draft=None,
                                    KeyCode::Tab=>draft.field=(draft.field+1)%3,
                                    KeyCode::BackTab=>draft.field=draft.field.checked_sub(1).unwrap_or(2),
                                    KeyCode::Enter if !draft.message.text().trim().is_empty()=>{
                                        commit_preview_action=Some((review.snapshot.id.clone(),draft.message.text().to_owned(),draft.remote.text().to_owned(),draft.tag.text().to_owned()));
                                        review.preview_draft=None;
                                    },
                                    KeyCode::Left=>draft.editor_mut().left(),
                                    KeyCode::Right=>draft.editor_mut().right(),
                                    KeyCode::Home=>draft.editor_mut().home(),
                                    KeyCode::End=>draft.editor_mut().end(),
                                    KeyCode::Backspace=>draft.editor_mut().backspace(),
                                    KeyCode::Delete=>draft.editor_mut().delete(),
                                    KeyCode::Char(value) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>draft.editor_mut().insert(&value.to_string()),
                                    _=>{},
                                }
                            } else if review.confirm_revert {
                                if matches!(key.code,KeyCode::Char('y')|KeyCode::Char('Y')) {
                                    revert_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),review.area));
                                }
                                review.confirm_revert=false;
                                if revert_action.is_none(){app.notice=Some(language.text("已取消撤销","Revert cancelled","取り消しをキャンセルしました").to_owned());}
                            } else if review.search.is_some() {
                                match key.code {
                                    KeyCode::Esc => review.search = None,
                                    KeyCode::Enter if !review.search_matches.is_empty() => {
                                        review.search_selected = if key.modifiers.contains(KeyModifiers::SHIFT) {
                                            review.search_selected.checked_sub(1).unwrap_or(review.search_matches.len() - 1)
                                        } else {
                                            (review.search_selected + 1) % review.search_matches.len()
                                        };
                                        review.scroll = review.search_matches[review.search_selected];
                                    }
                                    KeyCode::Left => review.search.as_mut().unwrap().left(),
                                    KeyCode::Right => review.search.as_mut().unwrap().right(),
                                    KeyCode::Home => review.search.as_mut().unwrap().home(),
                                    KeyCode::End => review.search.as_mut().unwrap().end(),
                                    KeyCode::Backspace => { review.search.as_mut().unwrap().backspace(); refresh_diff_search(review); }
                                    KeyCode::Delete => { review.search.as_mut().unwrap().delete(); refresh_diff_search(review); }
                                    KeyCode::Char(value) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER) => {
                                        review.search.as_mut().unwrap().insert(&value.to_string());
                                        refresh_diff_search(review);
                                    }
                                    _ => {}
                                }
                                continue;
                            } else {match key.code {
                                KeyCode::Esc if review.content.is_some()=>{review.content=None;review.scroll=0;force_full_redraw=true;},
                                KeyCode::Esc=>{close=true;force_full_redraw=true;},
                                KeyCode::Up if review.content.is_none()=>review.selected=review.selected.checked_sub(1).unwrap_or(review.snapshot.files.len().saturating_sub(1)),
                                KeyCode::Down if review.content.is_none()&&!review.snapshot.files.is_empty()=>review.selected=(review.selected+1)%review.snapshot.files.len(),
                                KeyCode::Enter if review.content.is_none()=>open_file=review.snapshot.files.get(review.selected).map(|file|(review.snapshot.id.clone(),file.path.clone(),review.area)),
                                KeyCode::Char('v')|KeyCode::Char('V') if review.content.is_some()=>{
                                    review.view=match review.view {DiffViewMode::Unified=>DiffViewMode::SideBySide,DiffViewMode::SideBySide=>DiffViewMode::Unified};
                                    refresh_diff_search(review);
                                },
                                KeyCode::Char('s')|KeyCode::Char('S') if review.content.is_some()=>{
                                    review.area=next_diff_area(review.area);
                                    open_file=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),review.area));
                                },
                                KeyCode::Char('/') if review.content.is_some()=>review.search=Some(PromptEditor::default()),
                                KeyCode::Char('n') if !review.search_matches.is_empty()=>{
                                    review.search_selected=(review.search_selected+1)%review.search_matches.len();
                                    review.scroll=review.search_matches[review.search_selected];
                                },
                                KeyCode::Char('N') if !review.search_matches.is_empty()=>{
                                    review.search_selected=review.search_selected.checked_sub(1).unwrap_or(review.search_matches.len()-1);
                                    review.scroll=review.search_matches[review.search_selected];
                                },
                                KeyCode::Char('a')|KeyCode::Char('A') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Accepted)),
                                KeyCode::Char('d')|KeyCode::Char('D') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Rejected)),
                                KeyCode::Char('c')|KeyCode::Char('C') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::ChangesRequested)),
                                KeyCode::Char('m')|KeyCode::Char('M') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Reviewed)),
                                KeyCode::Char('r')|KeyCode::Char('R') if review.content.is_some()=>review.confirm_revert=true,
                                KeyCode::Char('p')|KeyCode::Char('P')=>review.preview_draft=Some(CommitPreviewDraft::default()),
                                KeyCode::Up=>review.scroll=review.scroll.saturating_sub(1),
                                KeyCode::Down=>review.scroll=review.scroll.saturating_add(1),
                                KeyCode::PageUp=>review.scroll=review.scroll.saturating_sub(10),
                                KeyCode::PageDown=>review.scroll=review.scroll.saturating_add(10),
                                KeyCode::Home=>review.scroll=0,
                                _=>{}
                            }}
                        }
                        if preview_draft_handled&&commit_preview_action.is_none(){continue;}
                        if force_full_redraw{term.clear()?;}
                        if close{app.diff_review=None;continue;}
                        if let Some((snapshot_id,path,area))=open_file{
                            match crate::daemon::diff_review::remote_content(&runtime.home,&session.workspace,&snapshot_id,&path,area).await{
                                Ok(content)=>if let Some(review)=app.diff_review.as_mut(){review.content=Some((path,content));review.scroll=0;review.search_matches.clear();review.search_selected=0;},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("打开 Diff 失败","Open Diff failed","Diff を開けませんでした"))),
                            }
                        }
                        if let Some((snapshot_id,path,decision))=review_action{
                            let request=crate::daemon::diff_review::ReviewRequest{workspace:session.workspace.clone(),path:path.clone(),decision,note:None};
                            match crate::daemon::diff_review::remote_review(&runtime.home,&snapshot_id,&request).await{
                                Ok(record)=>{if let Some(review)=app.diff_review.as_mut(){review.reviews.insert(path,record.decision);}app.notice=Some(language.text("审查决定已保存","Review decision saved","レビュー結果を保存しました").to_owned());},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("保存审查决定失败","Save review decision failed","レビュー結果を保存できませんでした"))),
                            }
                        }
                        if let Some((snapshot_id,path,area))=revert_action{
                            let request=crate::daemon::diff_review::RevertRequest{workspace:session.workspace.clone(),path,area};
                            match crate::daemon::diff_review::remote_revert(&runtime.home,&snapshot_id,&request).await{
                                Ok(result)=>match crate::daemon::diff_review::remote_snapshot(&runtime.home,&session.workspace).await{
                                    Ok(snapshot)=>{if let Some(review)=app.diff_review.as_mut(){review.snapshot=snapshot;review.content=None;review.scroll=0;review.search_matches.clear();review.reviews.clear();review.verifications.clear();review.attributions.clear();}app.notice=Some(if let Some(path)=result.recovery_path{format!("{}: {}",language.text("已安全撤销，可从回收区恢复","Safely reverted; recovery copy","安全に戻しました。復元先"),path.display())}else{language.text("已安全撤销文件变更","File changes safely reverted","ファイル変更を安全に戻しました").to_owned()});},
                                    Err(error)=>app.notice=Some(format!("{}: {error}",language.text("撤销成功，但刷新 Diff 失败","Reverted, but refresh failed","取り消しましたが更新に失敗しました"))),
                                },
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("安全撤销失败","Safe revert failed","安全な取り消しに失敗しました"))),
                            }
                        }
                        if let Some((snapshot_id,message,remote,tag))=commit_preview_action{
                            let tag=(!tag.trim().is_empty()).then_some(tag);
                            match crate::daemon::diff_review::remote_commit_preview(&runtime.home,&session.workspace,&snapshot_id,&message,&remote,tag.as_deref()).await{
                                Ok(preview)=>if let Some(review)=app.diff_review.as_mut(){review.commit_preview=Some(preview);},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("生成 Commit Preview 失败","Commit Preview failed","Commit Preview に失敗しました"))),
                            }
                        }
                        continue;
                    }
                    if app.routing_settings.is_some(){
                        match app.handle_routing_settings_key(key) {
                            RoutingSettingsAction::None=>{},
                            RoutingSettingsAction::Close=>app.routing_settings=None,
                            RoutingSettingsAction::Save(update)=>{
                                let config_path=runtime.runtime_submit.config.clone().map(Ok).unwrap_or_else(crate::config::default_config_path);
                                match config_path.and_then(|path|crate::model_routing::save(&path,runtime.runtime_submit.profile.as_deref(),&update)) {
                                    Ok(settings)=>{
                                        app.set_routing_settings_saved(settings);
                                        app.notice=Some(language.text("模型与路由设置已保存","Models and routing saved","モデルとルーティングを保存しました").to_owned());
                                    },
                                    Err(error)=>app.set_routing_settings_error(error.to_string()),
                                }
                            },
                        }
                        continue;
                    }
                    if app.permission_picker.is_some(){
                        match app.handle_permission_picker_key(key) {
                            PermissionPickerAction::None=>{},
                            PermissionPickerAction::Close=>app.permission_picker=None,
                            PermissionPickerAction::Apply(mode)=>{
                                app.permission_picker=None;
                                match permission_commands::apply(mode,&mut app,session,runtime,&agent).await {
                                    Ok(message)=>app.append_transcript(message),
                                    Err(error)=>app.append_transcript(format!("Error: {}: {error:#}",language.text("切换审批模式失败","Approval mode switch failed","承認モードの切替に失敗"))),
                                }
                            },
                        }
                        continue;
                    }
                    if app.model_picker.is_some(){
                        match app.handle_model_picker_key(key) {
                            ModelPickerAction::None=>{},
                            ModelPickerAction::Close=>app.model_picker=None,
                            ModelPickerAction::Select(model)=>{
                                match switch_model(&model,&mut app,session,store,runtime,&agent).await {
                                    Ok(message)=>{app.model_picker=None;app.append_transcript(format!("System: {message}"));},
                                    Err(error)=>app.notice=Some(format!("{}: {error}",language.text("切换模型失败","Model switch failed","モデル切替に失敗"))),
                                }
                            },
                        }
                        continue;
                    }
                    if app.session_picker.is_some(){
                        match app.handle_session_picker_key(key) {
                            SessionPickerAction::None=>{},
                            SessionPickerAction::Close=>app.session_picker=None,
                            SessionPickerAction::Switch(target)=>{
                                app.pending_session_switch=Some(target);
                                app.session_picker=None;
                            },
                            SessionPickerAction::Refresh=>refresh_session_picker(&mut app,runtime,session).await,
                        }
                        continue;
                    }
                    if app.palette.is_some(){app.handle_palette_key(key,&runtime.background_tasks);continue;}
                    if app.search.is_some(){app.handle_search_key(key);continue;}
                    if app.handle_help_key(key) {continue;}
                    if key.code == KeyCode::F(2) {
                        app.toggle_composer_expanded();
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('p'){
                        app.open_palette(&runtime.skills,store,session);
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('r'){
                        if app.running {
                            app.notice=Some(language.text(
                                "当前会话正在运行，结束后才能切换历史会话",
                                "Wait for the current turn to finish before switching Sessions",
                                "現在のターンが完了してから履歴セッションを切り替えてください"
                            ).to_owned());
                        } else {
                            app.open_session_picker(session.id,SessionPickerRequest::default());
                            refresh_session_picker(&mut app,runtime,session).await;
                        }
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('f'){
                        app.search=Some(SearchState::default());
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('b'){
                        if app.sidebar_wide {
                            app.sidebar_visible = !app.sidebar_visible;
                            if !app.sidebar_visible {app.focus=FocusPane::Prompt;}
                        } else if app.focus==FocusPane::Sidebar {
                            app.focus=FocusPane::Prompt;
                        } else {
                            app.sidebar_visible=true;
                            app.focus=FocusPane::Sidebar;
                        }
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('w'){
                        app.sidebar_visible=true;
                        app.cycle_focus();
                        continue;
                    }
                    if app.focus==FocusPane::Chat {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Up=>app.scroll_up(1),
                            KeyCode::Down=>app.scroll_down(1),
                            KeyCode::Home=>app.scroll_to_top(),
                            KeyCode::End=>app.scroll_to_bottom(),
                            _=>{}
                        }
                        continue;
                    }
                    if app.focus==FocusPane::Activity {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Enter|KeyCode::Char(' ')=>app.tools_expanded = !app.tools_expanded,
                            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.tools_expanded = !app.tools_expanded,
                            _=>{}
                        }
                        continue;
                    }
                    if app.focus==FocusPane::Sidebar {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Up if app.sidebar_selected==1&&app.sidebar_expanded[1]=>app.attention_move(-1),
                            KeyCode::Down if app.sidebar_selected==1&&app.sidebar_expanded[1]=>app.attention_move(1),
                            KeyCode::Up if app.sidebar_selected==2&&app.sidebar_expanded[2]=>app.runtime_agent_move(-1),
                            KeyCode::Down if app.sidebar_selected==2&&app.sidebar_expanded[2]=>app.runtime_agent_move(1),
                            KeyCode::Up=>app.sidebar_move(-1),
                            KeyCode::Down=>app.sidebar_move(1),
                            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT)=>app.sidebar_move(-1),
                            KeyCode::Tab=>app.sidebar_move(1),
                            KeyCode::Enter=>{
                                if let Some(gate)=app.selected_remote_gate(){
                                    open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                                }else if app.sidebar_selected==2 {
                                    if let Some(agent)=app.selected_runtime_agent(){
                                        match crate::daemon::remote_agent_detail(&runtime.home,agent.id).await {
                                            Ok(detail)=>app.agent_detail=Some(detail),
                                            Err(error)=>app.notice=Some(format!("{}: {error}",language.text("加载 Agent 详情失败","Failed to load Agent details","Agent 詳細の読み込みに失敗"))),
                                        }
                                        app.agent_detail_scroll=0;
                                    }
                                }else{
                                    app.sidebar_activate(&runtime.background_tasks);
                                    // 打开的是 Runtime 任务详情时，把「哪条命令、为什么挂」一并取回来。
                                    load_attention_diagnostics(&mut app,runtime).await;
                                }
                            },
                            KeyCode::Char(' ')=>app.sidebar_toggle(),
                            KeyCode::Char('n')|KeyCode::Char('N') if app.sidebar_selected==2=>app.prefill_new_agent(),
                            KeyCode::Char('k')|KeyCode::Char('K') if app.sidebar_selected==1=>{
                                if let Some(id)=app.selected_runtime_task_id(){
                                    match crate::daemon::cancel_remote_task(&runtime.home,id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求停止 Runtime 任务","Runtime task stop requested","Runtime タスクの停止を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("停止失败","Stop failed","停止に失敗"))),
                                    }
                                }else{app.attention_stop(&runtime.background_tasks);}
                            },
                            KeyCode::Char('r')|KeyCode::Char('R') if app.sidebar_selected==1=>{
                                if app.attention_retry(&runtime.background_tasks){
                                    session.attention_read=app.attention_read.clone();
                                    store.save(session)?;
                                }
                            },
                            KeyCode::Char('k')|KeyCode::Char('K') if app.sidebar_selected==2=>{
                                if let Some(agent)=app.selected_runtime_agent(){
                                    match crate::daemon::stop_remote_agent(&runtime.home,agent.id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求停止子 Agent","Child Agent stop requested","子 Agent の停止を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("停止失败","Stop failed","停止に失敗"))),
                                    }
                                }
                            },
                            KeyCode::Char('r')|KeyCode::Char('R') if app.sidebar_selected==2=>{
                                if let Some(agent)=app.selected_runtime_agent(){
                                    match crate::daemon::retry_remote_agent(&runtime.home,agent.id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求重试子 Agent","Child Agent retry requested","子 Agent の再試行を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("重试失败","Retry failed","再試行に失敗"))),
                                    }
                                }
                            },
                            KeyCode::Char('m')|KeyCode::Char('M') if app.sidebar_selected==1=>{
                                if app.attention_mark_read(){
                                    session.attention_read=app.attention_read.clone();
                                    store.save(session)?;
                                }
                            },
                            _=>{}
                        }
                        continue;
                    }
                    // Shift+Tab 只在「严格 → 智能 → 工作区可写」间循环；完全访问必须走
                    // /permissions 的确认页。
                    if key.code==KeyCode::BackTab||(key.code==KeyCode::Tab&&key.modifiers.contains(KeyModifiers::SHIFT)) {
                        let next=app.approval_mode.next_in_cycle();
                        match permission_commands::apply(next,&mut app,session,runtime,&agent).await {
                            Ok(_)=>app.notice=Some(format!("{}：{} · Shift+Tab",language.text("审批模式","Approval mode","承認モード"),permission_commands::label(next,language))),
                            Err(error)=>app.notice=Some(format!("{}: {error:#}",language.text("切换审批模式失败","Approval mode switch failed","承認モードの切替に失敗"))),
                        }
                        continue;
                    }
                    if let Some(action) = prompt_line_navigation_for_key(key) {
                        match action {
                            PromptLineNavigation::Start => app.edit_input(|input| input.home()),
                            PromptLineNavigation::End => app.edit_input(|input| input.end()),
                        }
                        continue;
                    }
                    if app.handle_command_key(key) || app.handle_skill_key(key, &runtime.skills) { continue; }
                    if is_clipboard_image_paste_key(key) {
                        app.paste_clipboard_image();
                        continue;
                    }
                    match key.code {
                        KeyCode::PageUp=>app.scroll_up(app.viewport_height.saturating_sub(1).max(1)),KeyCode::PageDown=>app.scroll_down(app.viewport_height.saturating_sub(1).max(1)),
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT)=>app.scroll_up(1),KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT)=>app.scroll_down(1),
                        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL)=>app.scroll_to_top(),KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL)=>app.scroll_to_bottom(),
                        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.tools_expanded = !app.tools_expanded,
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.delete_selected_attachment(),
                        KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT|KeyModifiers::ALT)=>app.input.insert("\n"),
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.input.insert("\n"),
                        KeyCode::Enter if !app.input.is_empty()||!app.attachments.is_empty()=>{
                            // 本轮在跑时不再把 Enter 整条封死：本地命令照常执行，
                            // 提示词排队，其余命令说清楚为什么现在不行。
                            if app.running {
                                match busy_input(app.input.text()) {
                                    BusyInput::RunNow=>{},
                                    BusyInput::Queue=>{
                                        let text=app.input.take();
                                        app.append_transcript(format!("You: {text}"));
                                        app.queued_prompts.push_back(QueuedPrompt{
                                            text,
                                            attachments:std::mem::take(&mut app.attachments),
                                            from_phone:false,
                                        });
                                        app.selected_attachment=0;
                                        app.notice=Some(format!(
                                            "{} · {} {}",
                                            language.text("已排队，本轮结束后发送","Queued · sends when this turn finishes","キューに追加 · 現在のターン終了後に送信"),
                                            app.queued_prompts.len(),
                                            language.text("条等待 · Esc 立即中断","waiting · Esc interrupts now","件待機 · Esc で即中断"),
                                        ));
                                        continue;
                                    },
                                    BusyInput::Refuse=>{
                                        app.notice=Some(language.text(
                                            "该命令会改动会话或 Runtime，本轮结束后才能执行；Esc 可立即中断",
                                            "That command changes Session or Runtime state; it runs after this turn. Esc interrupts now",
                                            "このコマンドはセッション/Runtime を変更するため、ターン終了後に実行されます。Esc で即中断",
                                        ).to_owned());
                                        continue;
                                    },
                                }
                            }
                            let prompt=app.input.take();app.append_transcript(format!("You: {prompt}"));
                            if let Some(command)=permission_commands::parse(&prompt) {
                                match command {
                                    PermissionCommand::Open=>app.open_permission_picker(false),
                                    PermissionCommand::Switch(willdeep_core::ApprovalMode::FullAccess)=>app.open_permission_picker(true),
                                    PermissionCommand::Switch(mode)=>match permission_commands::apply(mode,&mut app,session,runtime,&agent).await {
                                        Ok(message)=>app.append_transcript(message),
                                        Err(error)=>app.append_transcript(format!("Error: {}: {error:#}",language.text("切换审批模式失败","Approval mode switch failed","承認モードの切替に失敗"))),
                                    },
                                    PermissionCommand::SaveDefault(mode)=>match permission_commands::save_default(mode,runtime) {
                                        Ok(path)=>app.append_transcript(format!("System: {}：{} · {path}",language.text("默认审批模式已写入配置，下次启动生效","Default approval mode saved; applies on next start","既定の承認モードを保存しました（次回起動から有効）"),permission_commands::label(mode,language))),
                                        Err(error)=>app.append_transcript(format!("Error: {}: {error:#}",language.text("写入默认审批模式失败","Save default approval mode failed","既定の承認モードの保存に失敗"))),
                                    },
                                    PermissionCommand::Usage=>app.append_transcript(permission_commands::usage(language)),
                                }
                                continue;
                            }
                            if app.handle_mobile_command(&prompt,&runtime.home,&runtime.relay_bridge,&mobile_tx,session){continue;}
                            match handle_agent_command(&prompt,&mut app,runtime,session.id).await {
                                Ok(true)=>continue,
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("Agent 操作失败","Agent action failed","Agent 操作に失敗しました")));continue;},
                            }
                            // `/history` 与 `/session search` 都开面板，必须排在 handle_session_command 前面。
                            match parse_session_picker_command(&prompt) {
                                // 这条分支本身就要求 !app.running；真正的运行中保护在切换那一步。
                                Ok(Some(request))=>{
                                    app.open_session_picker(session.id,request);
                                    refresh_session_picker(&mut app,runtime,session).await;
                                    continue;
                                },
                                Ok(None)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("打开历史会话面板失败","Open Session history panel failed","履歴セッションパネルを開けませんでした")));continue;},
                            }
                            // `/session retitle` 要 agent 与 UI 通道，落在这里而不是
                            // handle_session_command 里——摘要是网络往返，必须扔进后台任务。
                            if prompt.trim()=="/session retitle" {
                                if session.title_source==willdeep_core::TitleSource::User {
                                    app.append_transcript(format!("System: {}",language.text("这条会话的标题是你自己起的；先 /session rename 交回控制权再重算","This Session's title was set by you; hand it back with /session rename before recomputing","このセッション名はあなたが付けたものです。再計算する前に /session rename で戻してください")));
                                } else {
                                    dispatch_retitle(session,&agent,&runtime.tx,true);
                                    app.append_transcript(format!("System: {}",language.text("正在整理会话标题…","Retitling the Session…","セッション名を整理しています…")));
                                }
                                continue;
                            }
                            match handle_session_command(&prompt,&mut app,session,store,runtime).await {
                                Ok(true)=>continue,
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("会话操作失败","Session action failed","セッション操作に失敗しました")));continue;},
                            }
                            let previous_workspace=runtime.runtime_submit.workspace.clone();
                            match handle_workspace_command(&prompt,&mut app,session,store,runtime).await {
                                Ok(true)=>{
                                    if runtime.runtime_submit.workspace!=previous_workspace {
                                        while runtime_event_rx.try_recv().is_ok() {}
                                        _runtime_event_follower=crate::daemon::start_runtime_event_follower(
                                            runtime.home.clone(),
                                            app.runtime_event_cursor,
                                            runtime.runtime_submit.workspace.clone(),
                                            runtime_event_tx.clone(),
                                        );
                                    }
                                    continue;
                                },
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("工作区操作失败","Workspace action failed","ワークスペース操作に失敗しました")));continue;},
                            }
                            if let Some(model_command)=model_commands::parse(&prompt) {
                                match model_command {
                                    ModelCommand::List=>{
                                        let current=session.model.clone().unwrap_or_else(||runtime.provider_config.model.clone());
                                        request_model_list(&mut app,runtime,current);
                                    },
                                    ModelCommand::Switch(model)=>match switch_model(&model,&mut app,session,store,runtime,&agent).await {
                                        Ok(message)=>app.append_transcript(format!("System: {message}")),
                                        Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("切换模型失败","Model switch failed","モデル切替に失敗"))),
                                    },
                                }
                                continue;
                            }
                            if prompt.trim()=="/routing" {
                                let config_path=runtime.runtime_submit.config.clone().map(Ok).unwrap_or_else(crate::config::default_config_path);
                                match config_path.and_then(|path|crate::model_routing::load(&path,runtime.runtime_submit.profile.as_deref())) {
                                    Ok(settings)=>app.open_routing_settings(settings),
                                    Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("读取模型路由设置失败","Load model routing settings failed","モデルルーティング設定の読込に失敗"))),
                                }
                                continue;
                            }
                            if prompt.trim()=="/compress" {dispatch_compress(&mut app,session,store,&agent,&runtime.tx)?;continue;}
                            if let Some(parsed)=daemon_commands::parse(&prompt) {
                                match parsed {
                                    Ok(command)=>{
                                        app.append_transcript(format!("System: {} · {}",language.text("Runtime 操作已提交","Runtime action submitted","Runtime 操作を送信しました"),prompt.trim()));
                                        daemon_commands::dispatch(command,runtime.home.clone(),language,runtime.tx.clone());
                                    },
                                    Err(usage)=>app.append_transcript(format!("Error: {usage}")),
                                }
                                continue;
                            }
                            match webapp_commands::handle_webapp_command(&prompt,&runtime.home,&runtime.runtime_submit.workspace,runtime.runtime_submit.config.as_deref(),runtime.runtime_submit.profile.as_deref(),language).await {
                                Ok(Some(message))=>{app.append_transcript(message);continue;},
                                Ok(None)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("启动 Web App 失败","Start Web App failed","Web App の起動に失敗しました")));continue;},
                            }
                            if prompt.trim()=="/diff" {
                                match load_diff_review_state(&runtime.home,&session.workspace).await {
                                    Ok(review)=>app.diff_review=Some(review),
                                    Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("打开 Diff Review 失败","Open Diff Review failed","Diff Review を開けませんでした"))),
                                }
                                continue;
                            }
                            if let PromptExecution::Local(local_prompt)=prompt_execution(&prompt) {
                                if local_prompt.is_empty(){app.append_transcript(format!("System: {}",language.text("用法：/local <任务>","Usage: /local <task>","使用法: /local <タスク>")));continue;}
                                if session.workspace.canonicalize()?!=runtime.local_workspace.canonicalize()?{app.append_transcript(format!("System: {}",language.text("切换工作区后 /local 已禁用；请使用 Runtime，或从目标目录重新启动 TUI","/local is disabled after switching Workspace; use Runtime or restart the TUI from the target directory","ワークスペース切替後は /local を使用できません。Runtime を使うか対象ディレクトリから TUI を再起動してください")));continue;}
                                dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,local_prompt)?;
                                continue;
                            }
                            let runtime_alias=prompt.trim()=="/runtime"||prompt.trim().starts_with("/runtime ");
                            if !runtime_alias {
                                let previous_goal=app.goal.clone();
                                if app.handle_slash_command(&prompt,&runtime.skills){
                                    if app.goal!=previous_goal{session.goal=app.goal.clone();store.save(session)?;}
                                    if app.quit_requested{break;}
                                    continue;
                                }
                            }
                            let PromptExecution::Runtime(remote_prompt)=prompt_execution(&prompt) else {unreachable!("local prompts were handled above")};
                            match runtime_ui::submit_turn(&mut app,session,store,runtime,remote_prompt).await {
                                Ok(())=>app.notice=Some(language.text("AI 正在处理…","AI is working…","AI が処理しています…").to_owned()),
                                Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("提交 Runtime 轮次失败","Submit Runtime turn failed","Runtime ターンの送信に失敗"))),
                            }
                        }
                        // 走到这里说明没有任何弹层、焦点在输入框：Esc 就是「停下当前这轮」。
                        // 此前 TUI 根本没有中断入口，唯一的停止藏在侧栏 Inbox 的 K 键后面。
                        KeyCode::Esc if app.running=>{
                            match interrupt_turn(&mut app,session,runtime).await {
                                Ok(message)=>app.notice=Some(message),
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("中断失败","Interrupt failed","中断に失敗"))),
                            }
                        },
                        KeyCode::Left=>app.edit_input(|input| input.left()),KeyCode::Right=>app.edit_input(|input| input.right()),
                        KeyCode::Up=>{let width=app.prompt_rect.width.saturating_sub(2).max(1) as usize;app.edit_input(|input| input.up_visual(width));},
                        KeyCode::Down=>{let width=app.prompt_rect.width.saturating_sub(2).max(1) as usize;app.edit_input(|input| input.down_visual(width));},
                        KeyCode::Home=>app.edit_input(|input| input.home()),KeyCode::End=>app.edit_input(|input| input.end()),KeyCode::Backspace=>app.edit_input(|input| input.backspace()),KeyCode::Delete=>app.edit_input(|input| input.delete()),
                        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>app.edit_input(|input| input.insert(&c.to_string())),_=>{}
                    }
                }
                _=>{}
            }},
            Some(message)=runtime.rx.recv()=>match message {
                UiMessage::Agent(AgentEvent::ProviderProgress(event))=>match event {
                    willdeep_core::provider::ProviderEvent::RetryStarted{attempt}=>app.record_progress(format!("{} · {attempt}",language.text("正在重试","Retrying","再試行中"))),
                    willdeep_core::provider::ProviderEvent::TextDelta(text)=>{
                        app.activity_line=language.text("正在接收回复","Receiving reply","応答を受信中").to_owned();
                        let mut preview=app.transient_thought.take().unwrap_or_default();
                        preview.push_str(&text);
                        let skip=preview.chars().count().saturating_sub(THOUGHT_PREVIEW_CHARS);
                        app.transient_thought=Some(preview.chars().skip(skip).collect());
                    },
                    willdeep_core::provider::ProviderEvent::Usage(usage)=>{app.context_tokens=usage.input_tokens.unwrap_or(app.context_tokens);app.latest_usage=usage;},
                    willdeep_core::provider::ProviderEvent::RetryWait{attempt,delay}=>app.record_progress(format!("{} · {attempt} · {}s",language.text("服务端要求等待后重试","Waiting before provider retry","再試行を待機中"),delay.as_secs_f64())),
                },
                UiMessage::Agent(AgentEvent::AssistantText(v))=>app.note_narration(&v),
                UiMessage::Agent(AgentEvent::RouteDecided{tier,profile,confidence,auto_dispatched,..})=>app.record_progress(format!("{} {} · {} · {confidence}%{}",language.text("模型路由","Model route","モデルルート"),tier.as_str(),profile.as_deref().unwrap_or("root"),if auto_dispatched{language.text(" · 已自动下发"," · auto-dispatched"," · 自動ディスパッチ済み")}else{""})),
                UiMessage::Agent(AgentEvent::TurnStarted{turn})=>{app.transient_thought=None;app.record_progress(format!("{} {turn}",language.text("正在思考 · 准备轮次","Thinking · preparing turn","思考中 · ターンを準備")));},
                UiMessage::Agent(AgentEvent::TurnPreempted{turn})=>app.record_progress(format!("{} {turn}",language.text("已被运行时事件打断 · 轮次","Preempted by a runtime event · turn","ランタイムイベントで中断 · ターン"))),
                UiMessage::Agent(AgentEvent::ToolRequested(v))=>{app.record_progress(format!("{} {}",language.text("正在使用","Using","使用中"),v.name));app.tools.requested(&v.name);app.note_tool_requested(&v.name,crate::tool_detail(&v.name,&v.arguments).as_deref());},
                UiMessage::Agent(AgentEvent::ToolCompleted{call,is_error,..})=>{app.record_progress(format!("{} {}",if is_error{language.text("失败","Failed","失敗")}else{language.text("已完成","Finished","完了")},call.name));app.tools.completed(&call.name,is_error);app.note_tool_completed(&call.name,crate::tool_detail(&call.name,&call.arguments).as_deref(),is_error);if matches!(call.name.as_str(),"create_file"|"edit_file"|"run_command"|"create_worktree"){app.workspace_status=workspace_status(&session.workspace,language);app.workspace_attention=workspace_attention(&session.workspace);}},
                UiMessage::Agent(AgentEvent::Usage(v))=>{app.context_tokens=v.input_tokens.unwrap_or(app.context_tokens);app.record_turn_usage(&v);app.latest_usage=v;},
                UiMessage::Agent(AgentEvent::CompressionStarted{estimated_tokens})=>{app.context_tokens=estimated_tokens;app.record_progress(language.text("正在压缩上下文","Compressing context","コンテキストを圧縮中").to_owned());},
                UiMessage::Agent(AgentEvent::CompressionCompleted{estimated_tokens,dropped_messages})=>{app.context_tokens=estimated_tokens;let compressed=language.text("上下文已压缩","Context compressed","コンテキストを圧縮しました");app.record_progress(if dropped_messages>0{language.pick(format!("{compressed} · 本轮请求丢弃 {dropped_messages} 条最旧消息（存档不受影响）"),format!("{compressed} · dropped {dropped_messages} oldest message(s) from this request (the archive is untouched)"),format!("{compressed} · 今回のリクエストから最も古い {dropped_messages} 件を破棄（アーカイブは無変更）"))}else{compressed.to_owned()});},
                UiMessage::Agent(AgentEvent::BackgroundShellStarted{id})=>app.record_progress(format!("{} {id}",language.text("后台命令已启动","Background command started","バックグラウンドコマンド開始"))),
                UiMessage::Agent(AgentEvent::BackgroundShellCompleted{id,status,..})=>app.record_progress(format!("{} {id} · {status:?}",language.text("后台命令已结束","Background command finished","バックグラウンドコマンド完了"))),
                UiMessage::Agent(AgentEvent::SubagentStarted{id,profile,background,..})=>app.record_progress(format!("{} {} · {profile} · {}",language.text("子 Agent 已启动","Subagent started","サブエージェント開始"),id.to_string().get(..8).unwrap_or("agent"),if background{language.text("后台","background","バックグラウンド")}else{language.text("前台","foreground","フォアグラウンド")})),
                UiMessage::Agent(AgentEvent::SubagentCompleted{id,status,..})=>app.record_progress(format!("{} {} · {status:?}",language.text("子 Agent 已结束","Subagent finished","サブエージェント完了"),id.to_string().get(..8).unwrap_or("agent"))),
                UiMessage::Agent(AgentEvent::SubagentTurnStarted{id,turn})=>app.record_progress(format!("{} {} · {} {turn}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),language.text("轮次","turn","ターン"))),
                UiMessage::Agent(AgentEvent::SubagentToolRequested{id,name})=>app.record_progress(format!("{} {} · {} {name}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),language.text("正在使用","using","使用中"))),
                UiMessage::Agent(AgentEvent::SubagentToolCompleted{id,name,is_error})=>app.record_progress(format!("{} {} · {} {name}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),if is_error{language.text("失败","failed","失敗")}else{language.text("已完成","finished","完了")})),
                UiMessage::Agent(AgentEvent::SubagentUsage{..})=>{},
                UiMessage::Agent(AgentEvent::SubagentRetryStarted{id,attempt})=>app.record_progress(format!("{} {} · {} {attempt}",language.text("子 Agent","Subagent","サブエージェント"),&id.to_string()[..8],language.text("正在重试","Retrying","再試行中"))),
                UiMessage::Agent(AgentEvent::SubagentRetryWait{id,attempt,delay})=>app.record_progress(format!("{} {} · {} {attempt} · {}s",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),language.text("等待重试","Waiting to retry","再試行を待機中"),delay.as_secs_f64())),
                UiMessage::Agent(AgentEvent::SubagentVerdict{id,verifier_passed,attempts,..})=>{if let Some(passed)=verifier_passed{app.record_progress(format!("{} {} · {} · {attempts} {}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),if passed{language.text("验证通过","verified","検証通過")}else{language.text("验证未通过","not verified","検証失敗")},language.text("次尝试","attempt(s)","回試行")));}},
                UiMessage::Agent(AgentEvent::GoalContinuationInjected{rung})=>app.record_progress(format!("{} · {rung:?}",language.text("目标未达成 · 继续推进","Goal not met · continuing","目標未達成 · 継続します"))),
                UiMessage::Agent(AgentEvent::GoalBudgetLimited{reason})=>app.record_progress(format!("{} · {reason:?}",language.text("目标预算耗尽 · 转入收尾","Goal budget exhausted · wrapping up","目標の予算を使い切りました · まとめに移ります"))),
                UiMessage::Approval(v,a,s)=>{let detail=v.clone();if app.enqueue_approval((v,a,s)){runtime.notifier.attention_required(RuntimeStatus::WaitingApproval,"tool_approval",detail);execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;}},
                UiMessage::Question(request,sender)=>{let checked=vec![false;request.options.len()];let detail=request.question.clone();if app.enqueue_question(AskDialog{request,selected:0,checked,answer:PromptEditor::default(),sender}){runtime.notifier.attention_required(RuntimeStatus::WaitingAnswer,"ask_user",detail);execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;}},
                UiMessage::Finished(Ok(mut outcome), ownership)=>{crate::harness::present_partial_outcome(&mut outcome, language);runtime.notifier.task_stopped(&outcome);app.note_reply(&outcome.final_text);app.append_turn_stats(Some(&outcome));store.refresh_execution(session)?;session.messages=outcome.messages;persist_turn_result(session,store)?;drop(ownership);dispatch_retitle(session,&agent,&runtime.tx,false);app.finish_turn();wake_for_kernel_events(&mut app,session,store,&agent,runtime)?;},
                UiMessage::Finished(Err(e), ownership)=>{store.refresh_execution(session)?;drop(ownership);app.append_transcript(format!("Error: {e}"));app.finish_turn();},
                UiMessage::Compressed(Ok(messages), ownership)=>{store.refresh_execution(session)?;let changed=session.replace_with_compressed_messages(messages);persist_turn_result(session,store)?;drop(ownership);app.append_transcript(if changed{"System: Context compressed".to_owned()}else{"System: Context is too short to compress".to_owned()});app.finish_turn();},
                UiMessage::Compressed(Err(e), ownership)=>{store.refresh_execution(session)?;drop(ownership);app.append_transcript(format!("Error: context compression failed: {e}"));app.finish_turn();},
                UiMessage::RuntimeNotice(notice)=>app.notice=Some(notice),
                UiMessage::ModelsLoaded(result)=>app.set_model_picker_result(result),
                UiMessage::MediaLoaded{target,result}=>app.media.finish_load(target,result),
                UiMessage::MediaResized(result)=>{
                    if let Some(error)=app.media.finish_resize(result){
                        app.notice=Some(format!("{}: {error}",language.text("图片协议已降级","Image protocol downgraded","画像プロトコルをフォールバックしました")));
                    }
                },
                // 摘要失败是静默的：列表里还留着 L1 派生的标题，为一行装饰
                // 文字往聊天区塞报错不划算。改成功了才说一句。
                UiMessage::Retitled{title,requested}=>{let had_title=title.is_some();if crate::titling::adopt_summarized_title(session,title){store.save(session)?;runtime.notifier.set_session(&session.id.to_string(),Some(session.title.as_str()));app.notice=Some(format!("{}: {}",language.text("会话标题已整理","Session retitled","セッション名を整理しました"),session.title));}else if requested{app.append_transcript(format!("System: {}",if had_title{language.text("标题没有变化","The title is unchanged","タイトルに変更はありません")}else{language.text("标题整理失败：标题模型没有给出可用结果，沿用当前标题","Retitle failed: the title model returned nothing usable; keeping the current title","タイトル整理に失敗しました：タイトルモデルから有効な結果が得られなかったため、現在の名前を維持します")}));}},
            },
            Some(prompt)=mobile_rx.recv()=>{
                if app.running {app.queued_prompts.push_back(QueuedPrompt{text:prompt.text,attachments:Vec::new(),from_phone:true});app.notice=Some(format!("Phone request queued · {} waiting",app.queued_prompts.len()));}
                else {app.append_transcript(format!("Phone: {}",prompt.text));dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,prompt.text)?;}
            },
            Ok(event)=background_rx.recv()=>{
                let _=runtime.background_tasks.drain_pending();
                app.background_tasks=runtime.background_tasks.snapshots();
                // 后台结果交给内核，不再自己排一条通知：两条路同时向模型投递
                // 会让同一个结果讲两遍。正文由内核按来源净化后在 turn 边界注入。
                runtime.kernel.publish(
                    willdeep_core::kernel::background_task_event(session.id,&event.snapshot,event.notice),
                    willdeep_core::DedupPolicy::Once,
                );
                willdeep_core::kernel_store::flush(&runtime.kernel,&runtime.kernel_store);
                app.notice=Some(format!("{} finished · queued as a runtime event",event.snapshot.id));
                execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;
                // 忙的时候什么都不做：内核会在当前轮次的边界把它交出去。
                if !app.running {wake_for_kernel_events(&mut app,session,store,&agent,runtime)?;}
            },
        }
    }
    Ok(())
}
