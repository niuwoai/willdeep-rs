use super::*;

impl App {
    pub(super) fn new(transcript: Vec<String>, language: Language) -> Self {
        Self {
            input: PromptEditor::default(),
            transcript,
            running: false,
            approval: None,
            approval_selected: 0,
            approval_queue: VecDeque::new(),
            question: None,
            question_queue: VecDeque::new(),
            scroll_from_bottom: 0,
            follow_bottom: true,
            transcript_width: 78,
            transcript_height: 0,
            viewport_height: 10,
            tools: ToolActivity::default(),
            tools_expanded: false,
            activity_rect: Rect::default(),
            attachments: Vec::new(),
            selected_attachment: 0,
            prompt_rect: Rect::default(),
            prompt_scroll: 0,
            composer_expanded: false,
            notice: None,
            input_suggestion: None,
            input_suggestion_epoch: 0,
            runtime_turn_settled: false,
            goal: None,
            mobile_gateway: None,
            mobile_qr: None,
            queued_prompts: VecDeque::new(),
            pending_model: None,
            local_turn: None,
            latest_usage: Usage::default(),
            turn_input_tokens: 0,
            turn_output_tokens: 0,
            turn_first_reply: None,
            turn_started: None,
            last_progress_at: None,
            runtime_turn: false,
            stale_runtime_turn_snapshots: 0,
            last_elapsed: None,
            context_window: 128_000,
            context_tokens: 0,
            activity_line: String::new(),
            background_tasks: Vec::new(),
            workspace_attention: Vec::new(),
            runtime_attention: Vec::new(),
            kernel_attention: Vec::new(),
            runtime_gates: Vec::new(),
            runtime_version: None,
            runtime_version_warned: false,
            runtime_auto_upgrade_pending: false,
            runtime_auto_upgrade_tried: false,
            surfaced_gates: BTreeSet::new(),
            runtime_agents: Vec::new(),
            runtime_tools: Vec::new(),
            runtime_artifacts: Vec::new(),
            runtime_agent_selected: 0,
            agent_detail: None,
            agent_detail_scroll: 0,
            agent_detail_action_rects: Vec::new(),
            worktree_review: None,
            diff_review: None,
            runtime_event_cursor: 0,
            workspace_status: String::new(),
            progress_log: VecDeque::new(),
            language,
            transient_thought: None,
            turn_narration: None,
            tool_rows_expanded: false,
            transient_kind: StreamKind::Reply,
            bell_pending: false,
            selection_mode: false,
            native_selection_mode: false,
            chat_selection: None,
            transcript_rows: Vec::new(),
            transcript_render_offset: 0,
            skill_selected: 0,
            skill_menu_dismissed: false,
            command_selected: 0,
            command_menu_dismissed: false,
            quit_requested: false,
            focus: FocusPane::Prompt,
            // Hidden by default: the transcript is what the user came for,
            // and the status column is a lookup surface, not a permanent
            // one. `/sidebar` or Ctrl+B brings it back.
            sidebar_visible: false,
            sidebar_selected: 0,
            sidebar_expanded: [true, true, true, true],
            sidebar_scroll: 0,
            sidebar_rect: Rect::default(),
            sidebar_wide: false,
            help_visible: false,
            media: MediaState::default(),
            sidebar_hits: Vec::new(),
            sidebar_manual_scroll: false,
            attention_selected: 0,
            attention_read: BTreeSet::new(),
            task_detail: None,
            task_detail_scroll: 0,
            attention_detail: None,
            attention_diagnostics: None,
            attention_diff_rect: Rect::default(),
            attention_allow_rect: Rect::default(),
            attention_deny_rect: Rect::default(),
            search: None,
            workspace: None,
            palette: None,
            palette_rect: Rect::default(),
            palette_hits: Vec::new(),
            session_picker: None,
            rewind_picker: None,
            session_picker_rect: Rect::default(),
            session_picker_hits: Vec::new(),
            model_picker: None,
            model_picker_rect: Rect::default(),
            model_picker_hits: Vec::new(),
            workspace_picker: None,
            workspace_picker_rect: Rect::default(),
            workspace_picker_hits: Vec::new(),
            permission_picker: None,
            approval_mode: willdeep_core::ApprovalMode::Smart,
            approval_synced_session: None,
            routing_settings: None,
            routing_settings_rect: Rect::default(),
            pending_session_switch: None,
            pending_workspace_switch: None,
            transcript_rect: Rect::default(),
            command_rect: Rect::default(),
            command_hits: Vec::new(),
            skill_rect: Rect::default(),
            skill_hits: Vec::new(),
            approval_rect: Rect::default(),
            approval_action_hits: Vec::new(),
            question_rect: Rect::default(),
            question_hits: Vec::new(),
            search_rect: Rect::default(),
            mobile_qr_rect: Rect::default(),
            help_rect: Rect::default(),
            task_detail_rect: Rect::default(),
            attention_detail_rect: Rect::default(),
            agent_detail_rect: Rect::default(),
            worktree_review_rect: Rect::default(),
        }
    }
    pub(super) fn toggle_composer_expanded(&mut self) {
        self.composer_expanded = !self.composer_expanded;
        self.focus = FocusPane::Prompt;
    }
    pub(super) fn load_session(&mut self, session: &Session) {
        self.transcript = session_transcript(session, self.language);
        if self.transcript.is_empty() {
            self.transcript
                .push(welcome_message(&session.workspace, self.language));
        }
        self.input = PromptEditor::default();
        self.running = false;
        self.turn_started = None;
        self.last_progress_at = None;
        self.runtime_turn = false;
        self.discard_pending_approvals();
        self.discard_pending_questions();
        self.scroll_from_bottom = 0;
        self.follow_bottom = true;
        self.tools = ToolActivity::default();
        self.tools_expanded = false;
        self.attachments.clear();
        self.selected_attachment = 0;
        self.composer_expanded = false;
        self.notice = None;
        self.goal = session.goal.clone();
        self.transient_thought = None;
        self.selection_mode = false;
        self.native_selection_mode = false;
        self.chat_selection = None;
        self.transcript_rows.clear();
        self.transcript_render_offset = 0;
        self.progress_log.clear();
        self.search = None;
        self.palette = None;
        self.session_picker = None;
        self.rewind_picker = None;
        self.model_picker = None;
        self.workspace_picker = None;
        self.pending_session_switch = None;
        self.attention_read = session.attention_read.clone();
        self.workspace = Some(session.workspace.clone());
        self.workspace_status = workspace_status(&session.workspace, self.language);
        self.workspace_attention = workspace_attention(&session.workspace);
        self.focus = FocusPane::Prompt;
    }
    pub(super) fn sidebar_move(&mut self, delta: isize) {
        self.focus = FocusPane::Sidebar;
        self.sidebar_manual_scroll = false;
        self.sidebar_selected = if delta < 0 {
            self.sidebar_selected.checked_sub(1).unwrap_or(3)
        } else {
            (self.sidebar_selected + 1) % 4
        };
    }
    pub(super) fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Prompt => FocusPane::Chat,
            FocusPane::Chat => FocusPane::Activity,
            FocusPane::Activity => FocusPane::Sidebar,
            FocusPane::Sidebar => FocusPane::Prompt,
        };
    }
    pub(super) fn selected_attention(&self) -> Option<AttentionItem> {
        self.attention_items().get(self.attention_selected).cloned()
    }
    pub(super) fn selected_remote_gate(&self) -> Option<crate::daemon::RemoteGate> {
        let item = self.selected_attention()?;
        if let Some(id) = item
            .id
            .strip_prefix("runtime-interaction:")
            .and_then(|value| value.parse::<uuid::Uuid>().ok())
        {
            return self
                .runtime_gates
                .iter()
                .find(|gate| gate.id() == id)
                .cloned();
        }
        let task_id = item
            .id
            .strip_prefix("runtime-task:")?
            .parse::<uuid::Uuid>()
            .ok()?;
        self.runtime_gates
            .iter()
            .find(|gate| gate.task_id() == task_id)
            .cloned()
    }
    pub(super) fn selected_runtime_task_id(&self) -> Option<uuid::Uuid> {
        self.selected_attention()?
            .id
            .strip_prefix("runtime-task:")?
            .parse()
            .ok()
    }
    pub(super) fn attention_activate(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(item) = self.selected_attention() else {
            self.sidebar_toggle();
            return;
        };
        if let Some(index) = self
            .background_tasks
            .iter()
            .position(|task| task.id == item.id)
        {
            self.open_task_detail(index, registry);
        } else {
            self.task_detail = None;
            self.agent_detail = None;
            self.worktree_review = None;
            self.diff_review = None;
            self.attention_diagnostics = None;
            self.attention_detail = Some(item);
        }
    }
    pub(super) fn attention_stop(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(item) = self.selected_attention() else {
            return;
        };
        if item.status == RuntimeStatus::Working && registry.kill(&item.id) {
            self.notice = Some(format!(
                "{}: {}",
                self.language.text(
                    "已请求停止任务",
                    "Task stop requested",
                    "タスク停止を要求しました"
                ),
                item.id
            ));
        }
    }
    pub(super) fn attention_retry(&mut self, registry: &BackgroundTaskRegistry) -> bool {
        let Some(item) = self.selected_attention() else {
            return false;
        };
        if !matches!(
            item.status,
            RuntimeStatus::Blocked
                | RuntimeStatus::Failed
                | RuntimeStatus::Cancelled
                | RuntimeStatus::Partial
        ) {
            return false;
        }
        let Some(retried_id) = registry.retry(&item.id) else {
            self.notice = Some(
                self.language
                    .text(
                        "这个任务没有可重放的启动信息",
                        "This task has no replayable launcher",
                        "このタスクには再実行情報がありません",
                    )
                    .to_owned(),
            );
            return false;
        };
        self.attention_read.insert(item.id);
        self.notice = Some(format!(
            "{}: {retried_id}",
            self.language
                .text("已重新启动任务", "Task restarted", "タスクを再実行しました")
        ));
        true
    }
    /// Dismiss one Inbox item by id and close the detail popup. Running
    /// items are refused: they are still doing something, and hiding them
    /// would lose the only handle the user has on them.
    pub(super) fn attention_dismiss(&mut self, id: &str) -> bool {
        let running = self
            .attention_items()
            .into_iter()
            .find(|item| item.id == id)
            .is_some_and(|item| item.status == RuntimeStatus::Working);
        if running {
            return false;
        }
        self.attention_read.insert(id.to_owned());
        self.attention_detail = None;
        let remaining = self.attention_items().len();
        self.attention_selected = self.attention_selected.min(remaining.saturating_sub(1));
        true
    }

    pub(super) fn attention_mark_read(&mut self) -> bool {
        let Some(item) = self.selected_attention() else {
            return false;
        };
        if item.status != RuntimeStatus::Working {
            self.attention_read.insert(item.id);
            let remaining = self.attention_items().len();
            self.attention_selected = self.attention_selected.min(remaining.saturating_sub(1));
            return true;
        }
        false
    }
    pub(super) fn sidebar_toggle(&mut self) {
        self.sidebar_expanded[self.sidebar_selected] =
            !self.sidebar_expanded[self.sidebar_selected];
        self.sidebar_manual_scroll = false;
    }
    pub(super) fn sidebar_scroll_by(&mut self, delta: isize) {
        self.focus = FocusPane::Sidebar;
        self.sidebar_manual_scroll = true;
        self.sidebar_scroll = if delta < 0 {
            self.sidebar_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.sidebar_scroll.saturating_add(delta as usize)
        };
    }
    pub(super) fn sidebar_activate(&mut self, registry: &BackgroundTaskRegistry) {
        if self.sidebar_selected == 1 && self.sidebar_expanded[1] {
            self.attention_activate(registry);
        } else {
            self.sidebar_toggle();
        }
    }
    pub(super) fn prefill_new_agent(&mut self) {
        if !self.input.is_empty() || !self.attachments.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "输入区已有草稿或附件，请先发送或清空后再新建 Agent",
                        "The composer has a draft or attachments; send or clear it before creating an Agent",
                        "入力欄に下書きまたは添付があります。送信または消去してから Agent を作成してください",
                    )
                    .to_owned(),
            );
            return;
        }
        self.input.insert("/agent spawn reader ");
        self.focus = FocusPane::Prompt;
    }
    pub(super) fn open_task_detail(&mut self, index: usize, registry: &BackgroundTaskRegistry) {
        let Some(snapshot) = self.background_tasks.get(index).cloned() else {
            return;
        };
        let output = registry.output(&snapshot.id, 200).unwrap_or_else(|| {
            self.language
                .text("暂无输出", "No output", "出力なし")
                .to_owned()
        });
        self.attention_detail = None;
        self.agent_detail = None;
        self.worktree_review = None;
        self.diff_review = None;
        self.task_detail = Some(TaskDetail { snapshot, output });
        self.task_detail_scroll = 0;
    }
    pub(super) fn handle_task_detail_key(
        &mut self,
        key: KeyEvent,
        registry: &BackgroundTaskRegistry,
    ) {
        let Some(detail) = &self.task_detail else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.task_detail = None,
            KeyCode::Up => self.task_detail_scroll = self.task_detail_scroll.saturating_sub(1),
            KeyCode::Down => self.task_detail_scroll = self.task_detail_scroll.saturating_add(1),
            KeyCode::PageUp => self.task_detail_scroll = self.task_detail_scroll.saturating_sub(10),
            KeyCode::PageDown => {
                self.task_detail_scroll = self.task_detail_scroll.saturating_add(10)
            }
            KeyCode::Home => self.task_detail_scroll = 0,
            KeyCode::End => self.task_detail_scroll = detail.output.lines().count(),
            KeyCode::Char('k') | KeyCode::Char('K')
                if detail.snapshot.status == BackgroundTaskStatus::Running =>
            {
                let id = detail.snapshot.id.clone();
                if registry.kill(&id) {
                    self.notice = Some(format!(
                        "{}: {id}",
                        self.language.text(
                            "已请求停止任务",
                            "Task stop requested",
                            "タスク停止を要求しました"
                        )
                    ));
                    self.task_detail = None;
                }
            }
            _ => {}
        }
    }
    pub(super) fn handle_help_key(&mut self, key: KeyEvent) -> bool {
        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?')) {
                self.help_visible = false;
            }
            return true;
        }
        if key.code == KeyCode::F(1) || (key.code == KeyCode::Char('?') && self.input.is_empty()) {
            self.help_visible = true;
            return true;
        }
        false
    }
    pub(super) fn handle_search_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f'))
        {
            self.search = None;
            return;
        }
        let mut query_changed = false;
        if let Some(search) = &mut self.search {
            match key.code {
                KeyCode::Enter if !search.matches.is_empty() => {
                    search.selected = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        search
                            .selected
                            .checked_sub(1)
                            .unwrap_or(search.matches.len() - 1)
                    } else {
                        (search.selected + 1) % search.matches.len()
                    };
                }
                KeyCode::Left => search.editor.left(),
                KeyCode::Right => search.editor.right(),
                KeyCode::Home => search.editor.home(),
                KeyCode::End => search.editor.end(),
                KeyCode::Backspace => {
                    search.editor.backspace();
                    query_changed = true;
                }
                KeyCode::Delete => {
                    search.editor.delete();
                    query_changed = true;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
                {
                    search.editor.insert(&character.to_string());
                    query_changed = true;
                }
                _ => return,
            }
        }
        if query_changed {
            self.refresh_search_matches();
        }
        self.jump_to_search_match();
    }
    pub(super) fn open_palette(
        &mut self,
        skills: &SkillCatalog,
        store: &SessionStore,
        session: &Session,
    ) {
        let mut items = command_candidates(self.language)
            .into_iter()
            .map(|(command, description)| PaletteItem {
                label: command.to_owned(),
                description: description.to_owned(),
                action: PaletteAction::Command(command.to_owned()),
            })
            .collect::<Vec<_>>();
        items.extend(skills.list().iter().map(|skill| PaletteItem {
            label: format!("${}", skill.identifier),
            description: format!("{} · {}", skill.name, skill.description),
            action: PaletteAction::Skill(skill.identifier.clone()),
        }));
        items.push(PaletteItem {
            label: session.title.clone(),
            description: format!(
                "{} · {}",
                self.language
                    .text("当前会话", "Current session", "現在のセッション"),
                session.id
            ),
            action: PaletteAction::Session(session.id.to_string()),
        });
        {
            items.extend(
                store
                    .digests()
                    .into_iter()
                    .filter(|candidate| {
                        candidate.id != session.id && candidate.workspace == session.workspace
                    })
                    .take(30)
                    .map(|candidate| PaletteItem {
                        label: candidate.title,
                        description: format!(
                            "{} · {} · {}",
                            self.language.text("会话", "Session", "セッション"),
                            candidate.id,
                            candidate.workspace.display()
                        ),
                        action: PaletteAction::Session(candidate.id.to_string()),
                    }),
            );
        }
        items.extend(
            self.background_tasks
                .iter()
                .enumerate()
                .map(|(index, task)| {
                    let kind = if task.kind == willdeep_core::BackgroundTaskKind::Subagent {
                        self.language
                            .text("子 Agent", "Subagent", "サブエージェント")
                    } else {
                        self.language
                            .text("后台任务", "Background task", "バックグラウンドタスク")
                    };
                    PaletteItem {
                        label: task.id.clone(),
                        description: format!("{kind} · {:?} · {}", task.status, task.label),
                        action: PaletteAction::Task(index),
                    }
                }),
        );
        if let Some(workspace) = &self.workspace {
            items.extend(workspace_files(workspace, 300).into_iter().map(|path| {
                PaletteItem {
                    label: path.clone(),
                    description: self
                        .language
                        .text("工作区文件", "Workspace file", "ワークスペースファイル")
                        .to_owned(),
                    action: PaletteAction::File(path),
                }
            }));
        }
        let filtered = (0..items.len()).collect();
        self.palette = Some(PaletteState {
            editor: PromptEditor::default(),
            items,
            filtered,
            selected: 0,
        });
    }
    /// `Ctrl+R`、`/history` 和 `/session search` 共用这一个面板；三者只有初始
    /// 关键词与过滤器不同，行为（改词重查、方向键、Enter 进入）完全一致。
    pub(super) fn handle_palette_key(&mut self, key: KeyEvent, registry: &BackgroundTaskRegistry) {
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'))
        {
            self.palette = None;
            return;
        }
        let mut query_changed = false;
        let mut activate = false;
        if let Some(palette) = &mut self.palette {
            match key.code {
                KeyCode::Up | KeyCode::BackTab => {
                    if !palette.filtered.is_empty() {
                        palette.selected = palette
                            .selected
                            .checked_sub(1)
                            .unwrap_or(palette.filtered.len() - 1);
                    }
                }
                KeyCode::Down | KeyCode::Tab => {
                    if !palette.filtered.is_empty() {
                        palette.selected = (palette.selected + 1) % palette.filtered.len();
                    }
                }
                KeyCode::Enter if !palette.filtered.is_empty() => activate = true,
                KeyCode::Left => palette.editor.left(),
                KeyCode::Right => palette.editor.right(),
                KeyCode::Home => palette.editor.home(),
                KeyCode::End => palette.editor.end(),
                KeyCode::Backspace => {
                    palette.editor.backspace();
                    query_changed = true;
                }
                KeyCode::Delete => {
                    palette.editor.delete();
                    query_changed = true;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
                {
                    palette.editor.insert(&character.to_string());
                    query_changed = true;
                }
                _ => {}
            }
        }
        if query_changed {
            self.refresh_palette_matches();
        }
        if activate {
            self.activate_palette_selection(registry);
        }
    }
    pub(super) fn refresh_palette_matches(&mut self) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        let query = palette.editor.text().trim().to_lowercase();
        let mut ranked = palette
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let value = format!("{} {}", item.label, item.description).to_lowercase();
                fuzzy_score(&query, &value).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, index)| (*score, *index));
        palette.filtered = ranked.into_iter().map(|(_, index)| index).collect();
        palette.selected = 0;
    }
    pub(super) fn activate_palette_selection(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(palette) = self.palette.take() else {
            return;
        };
        let Some(item_index) = palette.filtered.get(palette.selected).copied() else {
            return;
        };
        match &palette.items[item_index].action {
            PaletteAction::Command(command) => {
                let suffix = if matches!(
                    command.as_str(),
                    "/goal"
                        | "/model"
                        | "/mobile"
                        | "/runtime"
                        | "/local"
                        | "/session"
                        | "/workspace"
                        | "/agent"
                ) {
                    " "
                } else {
                    ""
                };
                self.input.insert(&format!("{command}{suffix}"));
            }
            PaletteAction::Skill(identifier) => self.input.insert(&format!("${identifier} ")),
            PaletteAction::Session(id) => {
                self.pending_session_switch = Some(PendingSessionSwitch {
                    id: id.clone(),
                    archived: false,
                });
            }
            PaletteAction::Task(index) => self.open_task_detail(*index, registry),
            PaletteAction::File(path) => self.input.insert(&format!("{path} ")),
        }
    }
    pub(super) fn refresh_search_matches(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        let query = search.editor.text().trim().to_lowercase();
        search.matches = if query.is_empty() {
            Vec::new()
        } else {
            self.transcript
                .iter()
                .enumerate()
                .filter_map(|(index, value)| value.to_lowercase().contains(&query).then_some(index))
                .collect()
        };
        search.selected = 0;
    }
    pub(super) fn jump_to_search_match(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let Some(entry) = search.matches.get(search.selected).copied() else {
            return;
        };
        // 高度按折叠视图算；命中被收起的工具行时，落在替它说话的汇总行上。
        let folded = self.display_transcript();
        let total = rendered_transcript_height(&folded.rows, self.transcript_width);
        let through_match = folded
            .index_of
            .get(entry)
            .map(|&row| rendered_transcript_height(&folded.rows[..=row], self.transcript_width))
            .unwrap_or(total);
        self.follow_bottom = false;
        self.scroll_from_bottom = total
            .saturating_sub(through_match)
            .min(total.saturating_sub(self.viewport_height));
    }
    pub(super) fn edit_input(&mut self, edit: impl FnOnce(&mut PromptEditor)) {
        edit(&mut self.input);
        // 开始打字就放弃预测：灰字只在空输入框里有意义，删光了也不回来。
        if !self.input.is_empty() && self.input_suggestion.is_some() {
            self.clear_input_suggestion();
        }
        self.skill_selected = 0;
        self.skill_menu_dismissed = false;
        self.command_selected = 0;
        self.command_menu_dismissed = false;
    }
    pub(super) fn command_matches(&self) -> Vec<(&'static str, &'static str)> {
        let Some((start, query)) = self.input.marker_query('/') else {
            return Vec::new();
        };
        if start != 0 {
            return Vec::new();
        }
        let query = query.to_ascii_lowercase();
        command_candidates(self.language)
            .into_iter()
            .filter(|(command, description)| {
                command[1..].starts_with(&query)
                    || description.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }
    pub(super) fn handle_command_key(&mut self, key: KeyEvent) -> bool {
        if self.command_menu_dismissed || self.input.marker_query('/').is_none() {
            return false;
        }
        let matches = self.command_matches();
        let exact_command = command_candidates(self.language)
            .into_iter()
            .any(|(command, _)| self.input.text().trim() == command);
        match key.code {
            KeyCode::Esc => {
                self.command_menu_dismissed = true;
                true
            }
            KeyCode::Up if !matches.is_empty() => {
                self.command_selected = self
                    .command_selected
                    .checked_sub(1)
                    .unwrap_or(matches.len() - 1);
                true
            }
            KeyCode::Down if !matches.is_empty() => {
                self.command_selected = (self.command_selected + 1) % matches.len();
                true
            }
            KeyCode::Enter if exact_command => false,
            KeyCode::Tab | KeyCode::Enter
                if !matches.is_empty()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                let command = matches[self.command_selected.min(matches.len() - 1)].0;
                let suffix = if matches!(
                    command,
                    "/goal"
                        | "/mobile"
                        | "/runtime"
                        | "/local"
                        | "/session"
                        | "/workspace"
                        | "/agent"
                ) {
                    " "
                } else {
                    ""
                };
                self.input
                    .replace_before_cursor(0, &format!("{command}{suffix}"));
                self.command_selected = 0;
                self.command_menu_dismissed = true;
                true
            }
            _ => false,
        }
    }
    pub(super) fn skill_matches(&self, skills: &SkillCatalog) -> Vec<usize> {
        let Some((_, query)) = self.input.marker_query('$') else {
            return Vec::new();
        };
        let query = query.to_lowercase();
        skills
            .list()
            .iter()
            .enumerate()
            .filter(|(_, skill)| {
                format!("{} {} {}", skill.identifier, skill.name, skill.description)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
            .take(8)
            .collect()
    }
    pub(super) fn handle_skill_key(&mut self, key: KeyEvent, skills: &SkillCatalog) -> bool {
        if self.skill_menu_dismissed || self.input.marker_query('$').is_none() {
            return false;
        }
        let matches = self.skill_matches(skills);
        match key.code {
            KeyCode::Esc => {
                self.skill_menu_dismissed = true;
                true
            }
            KeyCode::Up if !matches.is_empty() => {
                self.skill_selected = self
                    .skill_selected
                    .checked_sub(1)
                    .unwrap_or(matches.len() - 1);
                true
            }
            KeyCode::Down if !matches.is_empty() => {
                self.skill_selected = (self.skill_selected + 1) % matches.len();
                true
            }
            KeyCode::Tab | KeyCode::Enter
                if !matches.is_empty()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                let selected = matches[self.skill_selected.min(matches.len() - 1)];
                let skill = &skills.list()[selected];
                let (start, _) = self.input.marker_query('$').expect("skill query exists");
                self.input
                    .replace_before_cursor(start, &format!("${} ", skill.identifier));
                self.skill_selected = 0;
                self.skill_menu_dismissed = true;
                true
            }
            _ => false,
        }
    }
    /// 收下一次请求的用量：累进本轮账目，并记住第一次响应的时刻。
    ///
    /// 这里接收完整响应的累计用量事件，以其到达时间作为首答的兜底值。
    /// 本地执行结束后优先使用 Core 记录的首个流式文本时间。
    pub(super) fn record_turn_usage(&mut self, usage: &Usage) {
        self.turn_input_tokens = self
            .turn_input_tokens
            .saturating_add(usage.input_tokens.unwrap_or(0));
        self.turn_output_tokens = self
            .turn_output_tokens
            .saturating_add(usage.output_tokens.unwrap_or(0));
        if self.turn_first_reply.is_none()
            && let Some(started) = self.turn_started
        {
            self.turn_first_reply = Some(started.elapsed());
        }
    }

    /// 一行浅色的本轮账目，跟在回答后面。
    ///
    /// 四个数各回答一件事：**首答**是等了多久才有第一个可用结果，**总耗时**
    /// 是这一轮从头到尾，两者的差包含后续生成、工具与后续轮次；输入含系统
    /// 提示词与整段历史，所以它比「你打的那句话」大得多是正常的。
    ///
    /// 本地流式文本以首个非空增量计时，非流式或纯工具响应以完整返回计时。
    /// 传 `Some(outcome)` 用本地轮次自己报的数（更权威，Provider 不报用量时
    /// 也仍有首答耗时）；Runtime 轮次没有 `AgentOutcome`，传 `None` 走本轮
    /// 累计。
    pub(super) fn append_turn_stats(&mut self, outcome: Option<&willdeep_core::AgentOutcome>) {
        let total = self
            .turn_started
            .map(|value| value.elapsed())
            .or(self.last_elapsed);
        let input = outcome.map_or(self.turn_input_tokens, |value| value.input_tokens);
        let output = outcome.map_or(self.turn_output_tokens, |value| value.output_tokens);
        let first_reply = outcome
            .and_then(|value| value.first_response_millis)
            .map(Duration::from_millis)
            .or(self.turn_first_reply);
        let turns = outcome.map(|value| value.turns);
        // 这行现在是「本轮结束」的分隔线：一个数都没有也要占一行，用户得知道
        // 轮到自己了。
        let mut parts = vec![
            self.language
                .text("本轮结束", "turn finished", "ターン終了")
                .to_owned(),
        ];
        if let Some(first) = first_reply {
            parts.push(format!(
                "{} {}",
                self.language.text("首答", "first reply", "初回応答"),
                format_elapsed_span(first.as_secs_f32(), 2)
            ));
        }
        if let Some(total) = total {
            parts.push(format!(
                "{} {}",
                self.language.text("总耗时", "total", "合計"),
                format_elapsed_span(total.as_secs_f32(), 2)
            ));
        }
        // Provider 不报用量时整段隐藏，而不是印一个像「真的用了 0 个 token」
        // 的 0——与状态栏对缓存率的处理同一条规矩。
        if input > 0 || output > 0 {
            parts.push(format!(
                "{} {}",
                self.language.text("输入", "in", "入力"),
                format_token_count(input)
            ));
            parts.push(format!(
                "{} {}",
                self.language.text("输出", "out", "出力"),
                format_token_count(output)
            ));
        }
        if let Some(turns) = turns.filter(|turns| *turns > 1) {
            parts.push(format!(
                "{} {}",
                self.language.text("轮次", "turns", "ターン"),
                turns
            ));
        }
        if self.tools.requested > 0 {
            parts.push(format!(
                "{} {}",
                self.language.text("工具", "tools", "ツール"),
                self.tools.requested
            ));
        }
        parts.push(
            self.language
                .text("轮到你", "your turn", "あなたの番")
                .to_owned(),
        );
        self.append_transcript(format!("{TURN_DIVIDER_PREFIX}{} ──", parts.join(" · ")));
    }

    pub(super) fn finish_turn(&mut self) {
        if let Some(started) = self.turn_started.take() {
            self.last_elapsed = Some(started.elapsed());
        }
        self.running = false;
        self.last_progress_at = None;
        self.runtime_turn = false;
        self.stale_runtime_turn_snapshots = 0;
        // 轮次结束，句柄就作废了；留着会让下一次 Esc 对着一个已完成的 Task 空掐。
        self.local_turn = None;
        self.transient_thought = None;
        self.turn_narration = None;
        self.activity_line = self.language.text("就绪", "Ready", "準備完了").to_owned();
        // 铃声只给审批、提问和后台任务的话，盯着别处的用户不知道轮到自己了。
        self.bell_pending = true;
    }

    /// 把预测清掉并翻世代：在途的结果回来时对不上号，自然丢弃。
    pub(super) fn clear_input_suggestion(&mut self) {
        self.input_suggestion = None;
        self.input_suggestion_epoch = self.input_suggestion_epoch.wrapping_add(1);
    }

    /// 预测结果落地前复核：世代号没变、还空闲、输入框还是空的、没有附件。
    /// 任一不满足就静默丢弃——晚到的建议比没有建议更碍事。
    pub(super) fn adopt_input_suggestion(
        &mut self,
        suggestion: Option<String>,
        epoch: u64,
    ) -> bool {
        if epoch != self.input_suggestion_epoch
            || self.running
            || !self.input.is_empty()
            || !self.attachments.is_empty()
        {
            return false;
        }
        let Some(suggestion) = suggestion else {
            return false;
        };
        self.input_suggestion = Some(suggestion);
        true
    }

    /// 灰字只在空闲且输入框为空时存在；其余时候当它不存在。
    pub(super) fn visible_input_suggestion(&self) -> Option<&str> {
        if self.running || !self.input.is_empty() {
            return None;
        }
        self.input_suggestion.as_deref()
    }

    /// Tab：把灰字填进输入框，**不发送**。输入框非空时 Tab 不归这里管。
    pub(super) fn accept_input_suggestion(&mut self) -> bool {
        let Some(suggestion) = self.visible_input_suggestion().map(str::to_owned) else {
            return false;
        };
        self.edit_input(|input| input.insert(&suggestion));
        true
    }

    /// Esc：放弃这条预测。没有可放弃的就返回 `false`，让 Esc 走它原来的路。
    pub(super) fn dismiss_input_suggestion(&mut self) -> bool {
        if self.visible_input_suggestion().is_none() {
            return false;
        }
        self.clear_input_suggestion();
        true
    }

    pub(super) fn take_runtime_turn_settled(&mut self) -> bool {
        std::mem::take(&mut self.runtime_turn_settled)
    }

    pub(super) fn begin_turn(&mut self, runtime_turn: bool, initial_progress: String) {
        // 新一轮开始，上一轮的预测作废；在途的结果回来也对不上世代号。
        self.clear_input_suggestion();
        let now = Instant::now();
        self.running = true;
        self.runtime_turn = runtime_turn;
        self.stale_runtime_turn_snapshots = 0;
        self.turn_started = Some(now);
        self.last_progress_at = Some(now);
        self.last_elapsed = None;
        self.turn_input_tokens = 0;
        self.turn_output_tokens = 0;
        self.turn_first_reply = None;
        self.turn_narration = None;
        self.progress_log.clear();
        self.record_progress(initial_progress);
    }

    pub(super) fn ensure_runtime_turn(&mut self) {
        if self.running {
            self.runtime_turn = true;
            return;
        }
        self.tools.reset();
        self.begin_turn(
            true,
            self.language
                .text(
                    "已重新连接 Runtime · 正在恢复进度",
                    "Runtime reconnected · restoring progress",
                    "Runtime に再接続 · 進捗を復元中",
                )
                .to_owned(),
        );
    }

    /// 用一份 Runtime 快照校准「工作中」状态。返回 `true` 表示界面上的 Runtime
    /// 轮次很可能是残留的，调用方应当去问 Runtime 确认并复位。
    ///
    /// 快照和事件流各走各的通道，谁先到没有保证。一份在任务还在跑时拍下的快照，
    /// 完全可能在 `turn.completed` 已经把界面复位之后才送到；此前这里见到「有活动
    /// 任务、界面没在跑」就无条件开一个轮次，而那轮的完成事件早被消费掉，再也
    /// 没有人来结束它——排队的提示词也就跟着永远发不出去。所以：
    ///
    /// - `snapshot_sequence` 落后于本地事件游标的快照，不能凭它开启轮次。
    /// - 新鲜快照连续几次都没有本会话的活动任务，而界面还在跑 Runtime 轮次，
    ///   就该怀疑是残留状态。真正在跑的轮次，快照里一定看得见它的任务。
    pub(super) fn observe_runtime_tasks(
        &mut self,
        tasks: &[crate::daemon::tui_bridge::RemoteTask],
        session_id: uuid::Uuid,
        snapshot_sequence: Option<u64>,
    ) -> bool {
        let has_active_task = tasks.iter().any(|task| {
            task.session_id == Some(session_id)
                && matches!(
                    task.status,
                    willdeep_runtime_protocol::TaskStatus::Queued
                        | willdeep_runtime_protocol::TaskStatus::Running
                        | willdeep_runtime_protocol::TaskStatus::Cancelling
                        | willdeep_runtime_protocol::TaskStatus::WaitingApproval
                        | willdeep_runtime_protocol::TaskStatus::WaitingAnswer
                )
        });
        // Runtime 不可达时序号为 None：那份快照什么都不能证明。
        let fresh = snapshot_sequence.is_some_and(|sequence| sequence >= self.runtime_event_cursor);
        if has_active_task {
            self.stale_runtime_turn_snapshots = 0;
            if !self.running && fresh {
                self.ensure_runtime_turn();
            }
            return false;
        }
        if !(self.running && self.runtime_turn && fresh) {
            return false;
        }
        self.stale_runtime_turn_snapshots = self.stale_runtime_turn_snapshots.saturating_add(1);
        self.stale_runtime_turn_snapshots >= STALE_RUNTIME_TURN_SNAPSHOTS
    }
    /// The Runtime's version when it differs from this binary's. The TUI is
    /// only a front end — a daemon started days ago keeps executing tools
    /// with its own (old) approval policy, so `willdeep --version` saying
    /// 0.22 proves nothing about what actually runs commands.
    pub(crate) fn stale_runtime_version(&self) -> Option<&str> {
        self.runtime_version
            .as_deref()
            .filter(|version| *version != willdeep_core::VERSION)
    }

    /// Record the Runtime version from a snapshot, announcing a mismatch in
    /// the transcript the first time it is seen.
    pub(super) fn observe_runtime_version(&mut self, version: Option<String>) {
        if self.runtime_version.as_deref() != version.as_deref() {
            // A handoff to a different Runtime deserves a fresh warning.
            self.runtime_version_warned = false;
        }
        self.runtime_version = version;
        let Some(stale) = self.stale_runtime_version().map(str::to_owned) else {
            return;
        };
        if self.runtime_version_warned {
            return;
        }
        self.runtime_version_warned = true;
        // 只升不降：Runtime 比客户端旧才考虑自动升级，每个客户端进程只试一次。
        if !self.runtime_auto_upgrade_tried && version_is_older(&stale, willdeep_core::VERSION) {
            self.runtime_auto_upgrade_pending = true;
        }
        let message = self
            .language
            .text(
                "⚠ Runtime {runtime} 与客户端 {client} 版本不一致。命令实际由 Runtime 执行，当前仍按旧版审批策略运行。请运行 `willdeep daemon upgrade` 后重试。",
                "⚠ Runtime {runtime} does not match client {client}. Commands execute inside the Runtime, which still applies its older approval policy. Run `willdeep daemon upgrade`, then retry.",
                "⚠ Runtime {runtime} とクライアント {client} のバージョンが不一致です。コマンドは Runtime 側で実行され、古い承認ポリシーが適用されます。`willdeep daemon upgrade` を実行してください。",
            )
            .replace("{runtime}", &stale)
            .replace("{client}", willdeep_core::VERSION);
        self.append_transcript(format!("System: {message}"));
    }

    /// Show an approval immediately, or queue it behind the one on screen.
    /// Returns true when the request became visible right now — the caller
    /// rings the terminal bell for that case, so a user looking elsewhere
    /// learns the turn is parked instead of watching it appear to hang.
    pub(super) fn enqueue_approval(&mut self, request: ApprovalRequest) -> bool {
        let waiting = self
            .language
            .text("等待你确认", "Waiting for you", "確認待ち");
        self.record_progress(format!("{waiting} · {}", first_line(&request.0)));
        if self.approval.is_some() {
            self.approval_queue.push_back(request);
            return false;
        }
        self.approval = Some(request);
        self.approval_selected = 0;
        true
    }

    /// Approval dialogs must remain usable while an IME owns alphabetic
    /// keystrokes. Unknown keys are consumed without deciding anything;
    /// Enter confirms the selected row and Esc explicitly denies.
    pub(super) fn handle_approval_key(&mut self, key: KeyEvent) {
        let Some((_, always, _)) = self.approval.as_ref() else {
            return;
        };
        let always = *always;
        let decisions = approval_decisions(always);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT);
        let decision = match key.code {
            KeyCode::Up | KeyCode::Left if plain => {
                self.approval_selected = self
                    .approval_selected
                    .checked_sub(1)
                    .unwrap_or(decisions.len() - 1);
                None
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Tab if plain => {
                self.approval_selected = (self.approval_selected + 1) % decisions.len();
                None
            }
            KeyCode::BackTab if plain => {
                self.approval_selected = self
                    .approval_selected
                    .checked_sub(1)
                    .unwrap_or(decisions.len() - 1);
                None
            }
            KeyCode::Enter if plain => decisions.get(self.approval_selected).copied(),
            KeyCode::Esc => Some(ApprovalDecision::Deny),
            KeyCode::Char('y' | 'Y' | '是') if plain => Some(ApprovalDecision::AllowOnce),
            KeyCode::Char('a' | 'A') if plain && always => Some(ApprovalDecision::AlwaysAllow),
            KeyCode::Char('n' | 'N' | '否') if plain => Some(ApprovalDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            self.resolve_approval(|_| decision);
        }
    }

    /// Some terminals surface committed IME text as a paste event. Accept
    /// only an exact approval word and never leak it into the prompt behind
    /// the modal.
    pub(super) fn handle_approval_text(&mut self, value: &str) {
        let Some((_, always, _)) = self.approval.as_ref() else {
            return;
        };
        let normalized = value.trim().to_lowercase();
        let decision = match normalized.as_str() {
            "y" | "yes" | "是" | "允许" | "同意" => Some(ApprovalDecision::AllowOnce),
            "a" | "always" | "始终允许" if *always => Some(ApprovalDecision::AlwaysAllow),
            "n" | "no" | "否" | "拒绝" => Some(ApprovalDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            self.resolve_approval(|_| decision);
        }
    }

    /// Answer the visible approval and immediately promote the next queued
    /// one, so a turn that needs three confirmations asks three times in a
    /// row instead of stalling after the first.
    pub(super) fn resolve_approval(&mut self, decide: impl FnOnce(bool) -> ApprovalDecision) {
        let Some((_, always, sender)) = self.approval.take() else {
            return;
        };
        let _ = sender.send(decide(always));
        self.approval = self.approval_queue.pop_front();
        self.approval_selected = 0;
    }

    /// Show a question immediately, or queue it behind the visible one.
    /// Returns true when it became visible right now.
    ///
    /// A question pops even while the user is typing: the draft prompt in
    /// `self.input` is untouched (the dialog carries its own editor), so
    /// nothing already typed is lost — keystrokes are only redirected from
    /// the moment it appears.
    pub(super) fn enqueue_question(&mut self, dialog: AskDialog) -> bool {
        let waiting = self
            .language
            .text("等待你回答", "Waiting for you", "回答待ち");
        self.record_progress(format!(
            "{waiting} · {}",
            first_line(&dialog.request.question)
        ));
        if self.question.is_some() {
            self.question_queue.push_back(dialog);
            return false;
        }
        self.question = Some(dialog);
        true
    }

    /// Promote the next queued question after the visible one is answered.
    pub(super) fn promote_next_question(&mut self) {
        self.question = self.question_queue.pop_front();
    }

    /// Answer every parked question with "no answer", visibly.
    pub(super) fn discard_pending_questions(&mut self) {
        let mut pending = Vec::new();
        if let Some(dialog) = self.question.take() {
            pending.push(dialog);
        }
        pending.extend(self.question_queue.drain(..));
        if pending.is_empty() {
            return;
        }
        for dialog in pending {
            let _ = dialog.sender.send(None);
        }
        self.notice = Some(
            self.language
                .text(
                    "切换会话已放弃待回答的提问",
                    "Pending questions dropped by session switch",
                    "セッション切り替えにより保留中の質問を破棄しました",
                )
                .to_owned(),
        );
    }

    /// Deny everything still parked, with a visible reason. Used when the
    /// user switches away from the session that raised them: dropping the
    /// senders would also deny, but silently.
    pub(super) fn discard_pending_approvals(&mut self) {
        let mut pending = Vec::new();
        if let Some(request) = self.approval.take() {
            pending.push(request);
        }
        pending.extend(self.approval_queue.drain(..));
        if pending.is_empty() {
            return;
        }
        for (_, _, sender) in pending {
            let _ = sender.send(ApprovalDecision::Deny);
        }
        self.notice = Some(
            self.language
                .text(
                    "切换会话已拒绝待处理的审批",
                    "Pending approvals denied by session switch",
                    "セッション切り替えにより保留中の承認を拒否しました",
                )
                .to_owned(),
        );
    }

    pub(super) fn record_progress(&mut self, value: String) {
        self.last_progress_at = Some(Instant::now());
        self.activity_line = value.clone();
        let elapsed = self
            .turn_started
            .map(|started| started.elapsed().as_secs_f32())
            .unwrap_or_default();
        self.progress_log
            .push_back(format!("{:>6} · {value}", format_elapsed_span(elapsed, 1)));
        while self.progress_log.len() > 12 {
            self.progress_log.pop_front();
        }
    }

    pub(super) fn working_summary(&self) -> Option<String> {
        let started = self.turn_started?;
        let elapsed = started.elapsed();
        let idle = self
            .last_progress_at
            .map(|progress| progress.elapsed())
            .unwrap_or(elapsed);
        Some(format_working_summary(
            self.language,
            self.runtime_turn,
            &self.activity_line,
            elapsed,
            idle,
        ))
    }
    pub(super) fn handle_question_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.question.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                if let Some(dialog) = self.question.take() {
                    let _ = dialog.sender.send(None);
                    self.promote_next_question();
                }
            }
            KeyCode::Up | KeyCode::BackTab => dialog.selected = dialog.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                if !dialog.request.options.is_empty() {
                    dialog.selected = (dialog.selected + 1) % dialog.request.options.len();
                }
            }
            KeyCode::Char(' ')
                if dialog.request.multi_select
                    && dialog.answer.is_empty()
                    && !dialog.checked.is_empty() =>
            {
                dialog.checked[dialog.selected] = !dialog.checked[dialog.selected];
            }
            KeyCode::Enter => {
                let dialog = self.question.take().expect("question dialog");
                let typed = dialog.answer.text().trim();
                let answer = if !typed.is_empty() {
                    typed.to_owned()
                } else if dialog.request.multi_select {
                    dialog
                        .request
                        .options
                        .into_iter()
                        .zip(dialog.checked)
                        .filter_map(|(option, checked)| checked.then_some(option))
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    dialog
                        .request
                        .options
                        .get(dialog.selected)
                        .cloned()
                        .unwrap_or_default()
                };
                let _ = dialog.sender.send(Some(answer));
                self.promote_next_question();
            }
            KeyCode::Left => dialog.answer.left(),
            KeyCode::Right => dialog.answer.right(),
            KeyCode::Home => dialog.answer.home(),
            KeyCode::End => dialog.answer.end(),
            KeyCode::Backspace => dialog.answer.backspace(),
            KeyCode::Delete => dialog.answer.delete(),
            KeyCode::Char(value)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                dialog.answer.insert(&value.to_string())
            }
            _ => {}
        }
    }
    pub(super) fn max_scroll(&self) -> usize {
        self.transcript_height.saturating_sub(self.viewport_height)
    }
    pub(super) fn scroll_up(&mut self, n: usize) {
        let max = self.max_scroll();
        if max == 0 {
            return self.scroll_to_bottom();
        }
        self.follow_bottom = false;
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(n).min(max);
    }
    pub(super) fn scroll_down(&mut self, n: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n);
        if self.scroll_from_bottom == 0 {
            self.follow_bottom = true;
        }
    }
    pub(super) fn scroll_to_top(&mut self) {
        let max = self.max_scroll();
        self.follow_bottom = max == 0;
        self.scroll_from_bottom = max;
    }
    pub(super) fn scroll_to_bottom(&mut self) {
        self.follow_bottom = true;
        self.scroll_from_bottom = 0;
    }
    pub(super) fn append_transcript(&mut self, v: String) {
        // 所有进记录的文字先过终端安全处理，制表符和控制字符不许碰终端光标。
        let v = terminal_safe_text(&v);
        let previous_height =
            rendered_transcript_height(&self.display_transcript().rows, self.transcript_width);
        if !v
            .strip_prefix("WillDeep: ")
            .is_some_and(|reply| append_plan_reply(&mut self.transcript, reply))
        {
            self.transcript.push(v);
        }
        self.refresh_transcript_height();
        if !self.follow_bottom {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(self.transcript_height.saturating_sub(previous_height));
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
    }
    pub(super) fn handle_paste(&mut self, value: String) {
        if value.contains('\n') || value.chars().count() > 200 {
            let n = self.attachments.len() + 1;
            self.attachments.push(DraftAttachment {
                message: MessageAttachment::Text {
                    name: format!("paste-{n}.txt"),
                    content: value,
                },
            });
            self.selected_attachment = self.attachments.len() - 1;
        } else {
            self.input.insert(&value);
        }
    }
    pub(super) fn delete_selected_attachment(&mut self) {
        if self.attachments.is_empty() {
            return;
        }
        let index = self.selected_attachment.min(self.attachments.len() - 1);
        self.attachments.remove(index);
        self.selected_attachment = index.saturating_sub(1);
        self.notice = Some("Attachment removed".to_owned());
    }
    pub(super) fn paste_clipboard_image(&mut self) {
        match clipboard_image() {
            Ok(value) => {
                self.attachments.push(value);
                self.selected_attachment = self.attachments.len() - 1;
                self.notice = Some("Clipboard image attached".to_owned());
            }
            Err(e) => self.notice = Some(format!("Clipboard image unavailable: {e}")),
        }
    }

    pub(super) fn transcript_selection_point(
        &self,
        x: u16,
        y: u16,
        clamp_to_viewport: bool,
    ) -> Option<ChatSelectionPoint> {
        if self.transcript_rows.is_empty()
            || self.transcript_rect.width < 3
            || self.transcript_rect.height < 3
        {
            return None;
        }
        let left = self.transcript_rect.x.saturating_add(1);
        let right = self.transcript_rect.right().saturating_sub(2);
        let top = self.transcript_rect.y.saturating_add(1);
        let bottom = self.transcript_rect.bottom().saturating_sub(2);
        if !clamp_to_viewport && (x < left || x > right || y < top || y > bottom) {
            return None;
        }
        let visible_row = y.clamp(top, bottom).saturating_sub(top) as usize;
        let row = self
            .transcript_render_offset
            .saturating_add(visible_row)
            .min(self.transcript_rows.len().saturating_sub(1));
        let row_width = UnicodeWidthStr::width(self.transcript_rows[row].as_str());
        let column = x
            .clamp(left, right)
            .saturating_sub(left)
            .min(row_width.saturating_sub(1).min(u16::MAX as usize) as u16)
            as usize;
        Some(ChatSelectionPoint { row, column })
    }

    pub(super) fn handle_chat_selection_mouse(&mut self, mouse: MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, false)
                else {
                    // 点在聊天区外（通常是输入框）：退出选区，让这一下照常去切焦点。
                    // 以前这里原样返回 `selection_mode`，选过字之后点输入框会被整个
                    // 吃掉，焦点切不过去、键盘也还锁在选区模式里。
                    if self.selection_mode {
                        self.exit_selection_mode();
                    }
                    return false;
                };
                self.chat_selection = Some(ChatSelection {
                    anchor: point,
                    head: point,
                });
                self.focus = FocusPane::Chat;
                self.selection_mode
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(mut selection) = self.chat_selection else {
                    return false;
                };
                let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, true)
                else {
                    return false;
                };
                selection.head = point;
                self.chat_selection = Some(selection);
                self.selection_mode = true;
                self.native_selection_mode = false;
                self.focus = FocusPane::Chat;
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.chat_selection.is_some() => {
                if !self.selection_mode {
                    self.chat_selection = None;
                    return false;
                }
                if let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, true)
                    && let Some(selection) = self.chat_selection.as_mut()
                {
                    selection.head = point;
                }
                // 松手不再自动写剪贴板。拖一下看看就把刚复制好的网址顶掉，
                // 之后贴进去的是上一条回复，谁也想不到——复制要人明确按键。
                let chars = self.selected_chat_text().chars().count();
                self.notice = Some(
                    self.language
                        .text(
                            "已选中 {n} 字 · Ctrl+C 或 y 复制 · q 引用 · Esc 取消",
                            "{n} chars selected · Ctrl+C or y copies · q quotes · Esc cancels",
                            "{n} 文字を選択 · Ctrl+C か y でコピー · q で引用 · Esc で解除",
                        )
                        .replace("{n}", &chars.to_string()),
                );
                true
            }
            MouseEventKind::ScrollUp if self.selection_mode => {
                self.scroll_up(3);
                true
            }
            MouseEventKind::ScrollDown if self.selection_mode => {
                self.scroll_down(3);
                true
            }
            _ => false,
        }
    }

    pub(super) fn enter_native_selection_mode(&mut self) {
        self.selection_mode = true;
        self.native_selection_mode = true;
        self.chat_selection = None;
        self.focus = FocusPane::Chat;
    }

    /// 选区模式里按了一个不属于选区的键：退出选区，把这个键交还给正常处理。
    /// 能打进输入框的键顺带把焦点切到输入框——用户是想打字，不是想被锁住。
    pub(super) fn release_selection_for_key(&mut self, key: KeyEvent) {
        self.exit_selection_mode();
        let typing = match key.code {
            KeyCode::Char(_) => !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
            KeyCode::Backspace | KeyCode::Enter | KeyCode::Delete => true,
            _ => false,
        };
        if typing {
            self.focus = FocusPane::Prompt;
        }
    }

    pub(super) fn exit_selection_mode(&mut self) {
        self.selection_mode = false;
        self.native_selection_mode = false;
        self.chat_selection = None;
    }

    pub(super) fn selected_chat_text(&self) -> String {
        let Some(selection) = self.chat_selection else {
            return String::new();
        };
        let (start, end) = selection.ordered_range();
        selected_text(&self.transcript_rows, start, end)
    }

    pub(super) fn copy_chat_selection(&mut self) {
        let value = self.selected_chat_text();
        if value.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "请先在聊天区拖动选择文字",
                        "Drag to select chat text first",
                        "先にチャット内の文字をドラッグ選択してください",
                    )
                    .to_owned(),
            );
            return;
        }
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(value)) {
            Ok(()) => {
                self.notice = Some(
                    self.language
                        .text(
                            "已复制所选文字",
                            "Selected text copied",
                            "選択した文字をコピーしました",
                        )
                        .to_owned(),
                );
            }
            Err(error) => {
                self.notice = Some(format!(
                    "{}: {error}",
                    self.language.text(
                        "复制到剪贴板失败",
                        "Copy to clipboard failed",
                        "クリップボードへのコピーに失敗"
                    )
                ));
            }
        }
    }

    pub(super) fn quote_chat_selection(&mut self) {
        let value = self.selected_chat_text();
        if value.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "请先在聊天区拖动选择文字",
                        "Drag to select chat text first",
                        "先にチャット内の文字をドラッグ選択してください",
                    )
                    .to_owned(),
            );
            return;
        }
        if !self.input.is_empty() {
            self.input.insert("\n\n");
        }
        self.input.insert(&quote_selected_text(&value));
        self.selection_mode = false;
        self.native_selection_mode = false;
        self.chat_selection = None;
        self.focus = FocusPane::Prompt;
        self.notice = Some(
            self.language
                .text(
                    "已引用到输入框",
                    "Selection quoted into the prompt",
                    "選択範囲を入力欄に引用しました",
                )
                .to_owned(),
        );
    }

    pub(super) fn handle_mouse(
        &mut self,
        x: u16,
        y: u16,
        registry: &BackgroundTaskRegistry,
        skills: &SkillCatalog,
    ) {
        if self.search.is_some() && self.search_rect.contains((x, y).into()) {
            if let Some(search) = &mut self.search {
                search.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.search_rect.x + 1) as usize,
                    self.search_rect.width.saturating_sub(2).max(1) as usize,
                );
            }
        } else if self.approval.is_some() && self.approval_rect.contains((x, y).into()) {
            let point = (x, y).into();
            if let Some((_, decision)) = self
                .approval_action_hits
                .iter()
                .find(|(rect, _)| rect.contains(point))
                .copied()
            {
                self.resolve_approval(|_| decision);
            }
        } else if self.question.is_some() && self.question_rect.contains((x, y).into()) {
            if let Some((_, selected)) = self
                .question_hits
                .iter()
                .find(|(row, _)| *row == y)
                .copied()
            {
                let multi = self
                    .question
                    .as_ref()
                    .is_some_and(|dialog| dialog.request.multi_select);
                if multi {
                    if let Some(dialog) = &mut self.question {
                        dialog.selected = selected;
                        dialog.checked[selected] = !dialog.checked[selected];
                    }
                } else if let Some(dialog) = self.question.take() {
                    let answer = dialog.request.options.get(selected).cloned();
                    let _ = dialog.sender.send(answer);
                    self.promote_next_question();
                }
            } else if y >= self.question_rect.bottom().saturating_sub(2) {
                let code = if x < self.question_rect.x + self.question_rect.width / 2 {
                    KeyCode::Esc
                } else {
                    KeyCode::Enter
                };
                self.handle_question_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
        } else if self.palette.is_some() && self.palette_rect.contains((x, y).into()) {
            if let Some((_, position)) =
                self.palette_hits.iter().find(|(row, _)| *row == y).copied()
            {
                if let Some(palette) = &mut self.palette {
                    palette.selected = position;
                }
                self.activate_palette_selection(registry);
            }
        } else if self.command_rect.contains((x, y).into()) {
            if let Some((_, selected)) =
                self.command_hits.iter().find(|(row, _)| *row == y).copied()
            {
                self.command_selected = selected;
                self.handle_command_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            }
        } else if self.skill_rect.contains((x, y).into()) {
            if let Some((_, selected)) = self.skill_hits.iter().find(|(row, _)| *row == y).copied()
            {
                self.skill_selected = selected;
                self.handle_skill_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), skills);
            }
        } else if self.sidebar_rect.contains((x, y).into()) {
            self.focus = FocusPane::Sidebar;
            if let Some((_, hit)) = self.sidebar_hits.iter().find(|(row, _)| *row == y).copied() {
                match hit {
                    SidebarHit::Section(section) => {
                        self.sidebar_selected = section;
                        self.sidebar_toggle();
                    }
                    SidebarHit::Attention(index) => {
                        self.sidebar_selected = 1;
                        self.attention_selected = index;
                        self.attention_activate(registry);
                    }
                    SidebarHit::NewAgent => self.prefill_new_agent(),
                }
            }
        } else if self.transcript_rect.contains((x, y).into()) {
            self.focus = FocusPane::Chat;
        } else if self.activity_rect.contains((x, y).into()) {
            self.focus = FocusPane::Activity;
        } else if self.prompt_rect.contains((x, y).into()) {
            self.focus = FocusPane::Prompt;
            let row = y.saturating_sub(self.prompt_rect.y + 1) as usize + self.prompt_scroll;
            let col = x.saturating_sub(self.prompt_rect.x + 1) as usize;
            self.input.set_cursor_visual(
                row,
                col,
                self.prompt_rect.width.saturating_sub(2) as usize,
            );
        }
    }
    pub(super) fn handle_slash_command(&mut self, prompt: &str, skills: &SkillCatalog) -> bool {
        let value = prompt.trim();
        if !value.starts_with('/') {
            return false;
        }
        let (command, args) = value.split_once(' ').unwrap_or((value, ""));
        if matches!(
            command,
            "/agent"
                | "/compress"
                | "/daemon"
                | "/diff"
                | "/rewind"
                | "/history"
                | "/local"
                | "/mobile"
                | "/model"
                | "/new"
                | "/routing"
                | "/runtime"
                | "/session"
                | "/webapp"
                | "/workspace"
        ) {
            return false;
        }
        match command {
            "/help" => self.append_transcript(help_text(self.language)),
            "/version" => {
                let report = self.version_report();
                self.append_transcript(report);
            }
            "/plan" => {
                toggle_plan_details(&mut self.transcript);
                self.refresh_transcript_height();
                self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
            }
            "/tools" => {
                let message = self.toggle_tool_rows();
                self.append_transcript(message);
            }
            "/exit" => {
                self.quit_requested = true;
                self.append_transcript(format!(
                    "System: {}",
                    self.language
                        .text("正在退出…", "Exiting…", "終了しています…")
                ));
            }
            "/goal" if args.trim().eq_ignore_ascii_case("off") => {
                self.goal = None;
                self.append_transcript("System: Goal mode disabled".to_owned());
            }
            "/goal" if !args.trim().is_empty() => {
                self.goal = Some(args.trim().to_owned());
                self.append_transcript(format!("System: Goal mode · {}", args.trim()));
            }
            "/goal" => self.append_transcript(format!(
                "System: Goal · {}",
                self.goal.as_deref().unwrap_or("not set")
            )),
            "/sidebar" => {
                let argument = args.trim().to_ascii_lowercase();
                let visible = match argument.as_str() {
                    "" | "toggle" => !self.sidebar_visible,
                    "on" | "show" | "open" => true,
                    "off" | "hide" | "close" => false,
                    other => {
                        self.append_transcript(format!(
                            "Error: usage: {command} [on|off] (got `{other}`)"
                        ));
                        return true;
                    }
                };
                self.sidebar_visible = visible;
                if !visible {
                    self.focus = FocusPane::Prompt;
                }
                self.append_transcript(format!(
                    "System: {}",
                    if visible {
                        self.language.text(
                            "状态栏已显示（Ctrl+B 隐藏）",
                            "Status sidebar shown (Ctrl+B hides it)",
                            "状態サイドバーを表示しました（Ctrl+B で非表示）",
                        )
                    } else {
                        self.language.text(
                            "状态栏已隐藏（/sidebar 或 Ctrl+B 显示）",
                            "Status sidebar hidden (/sidebar or Ctrl+B shows it)",
                            "状態サイドバーを非表示にしました（/sidebar または Ctrl+B で表示）",
                        )
                    }
                ));
            }
            "/skills" => {
                self.append_transcript(format!("System: Available skills\n{}", skills.summary()))
            }
            "/clear" => {
                self.transcript.clear();
                self.scroll_to_bottom();
            }
            _ => self.append_transcript(format!("Error: unknown command {command}; use /help")),
        }
        true
    }
    pub(super) fn handle_mobile_command(
        &mut self,
        prompt: &str,
        home: &std::path::Path,
        bridge: &RelayBridge,
        mobile_tx: &mpsc::UnboundedSender<MobilePrompt>,
        session: &Session,
    ) -> bool {
        let value = prompt.trim();
        if !matches!(
            value,
            "/mobile" | "/mobile show" | "/mobile hide" | "/mobile off"
        ) {
            return false;
        }
        match value {
            "/mobile off" => {
                self.mobile_gateway = None;
                self.mobile_qr = None;
                self.append_transcript("System: Mobile relay disconnected".to_owned());
            }
            "/mobile hide" => self.mobile_qr = None,
            _ => {
                if self.mobile_gateway.is_none() {
                    match RelayGateway::start(
                        home,
                        bridge.clone(),
                        mobile_tx.clone(),
                        mobile_snapshot(session),
                    ) {
                        Ok(gateway) => {
                            self.append_transcript(format!(
                                "System: Mobile relay connected · room {}",
                                gateway.room
                            ));
                            self.mobile_gateway = Some(gateway);
                        }
                        Err(error) => {
                            self.append_transcript(format!("Error: start mobile relay: {error:#}"))
                        }
                    }
                }
                self.mobile_qr = self
                    .mobile_gateway
                    .as_ref()
                    .map(|gateway| gateway.qr.clone());
            }
        }
        true
    }
    pub(super) fn enrich_prompt(&self, prompt: &str, skills: &SkillCatalog) -> String {
        let mut blocks = Vec::new();
        if let Some(goal) = &self.goal {
            blocks.push(format!(
                "<goal>\n{goal}\n</goal>\nContinue until this goal is genuinely complete."
            ));
        }
        for token in prompt
            .split_whitespace()
            .filter(|value| value.starts_with('$'))
        {
            let name = token
                .trim_start_matches('$')
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
            if !name.is_empty()
                && let Ok(body) = skills.read(name, None)
            {
                blocks.push(format!(
                    "<explicit_skill name=\"{name}\">\n{body}\n</explicit_skill>"
                ));
            }
        }
        if blocks.is_empty() {
            prompt.to_owned()
        } else {
            format!("{}\n\n{prompt}", blocks.join("\n\n"))
        }
    }
}

/// `a` 是否严格早于 `b`。认 `MAJOR.MINOR.PATCH` 与可选的 `-rcN`，正式版晚于同号 rc。
/// 解析不了就返回 false——拿不准就不自动升级。
pub(super) fn version_is_older(a: &str, b: &str) -> bool {
    fn parse(value: &str) -> Option<(u64, u64, u64, Option<u64>)> {
        let (core, pre) = match value.trim().split_once('-') {
            Some((core, pre)) => (core, Some(pre.strip_prefix("rc")?.parse().ok()?)),
            None => (value.trim(), None),
        };
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let parsed = (parts.next()??, parts.next()??, parts.next()??, pre);
        parts.next().is_none().then_some(parsed)
    }
    let (Some(a), Some(b)) = (parse(a), parse(b)) else {
        return false;
    };
    let rank = |pre: Option<u64>| pre.map_or((1, 0), |rc| (0, rc));
    (a.0, a.1, a.2, rank(a.3)) < (b.0, b.1, b.2, rank(b.3))
}
