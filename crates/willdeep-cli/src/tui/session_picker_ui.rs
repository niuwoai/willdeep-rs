//! 历史会话面板：`Ctrl+R`、`/history` 和 `/session search` 共用的那一个。
//!
//! 三个入口只有初始关键词与过滤器不同，之后的行为完全一致——改词即重查，
//! 方向键或鼠标选中，`Enter` 原地进入那条会话继续聊。

use super::*;

/// 面板最多留 20 条。Runtime 侧最多返回 100 条并按更新时间倒序，
/// 一屏放不下的长列表对「继续上一条会话」这件事没有帮助。
const SESSION_PICKER_LIMIT: usize = 20;

#[derive(Default)]
pub(super) struct SessionPickerState {
    pub(super) editor: PromptEditor,
    /// `/session search` 带进来的非文本过滤器（`--status` / `--profile` 等），
    /// 每次改关键词重新查询时都要一起发，否则一按键过滤条件就没了。
    pub(super) filters: Vec<(String, String)>,
    pub(super) results: Vec<willdeep_runtime_protocol::SessionSearchResult>,
    /// 结果被 [`SESSION_PICKER_LIMIT`] 截断过：标题写成 `20+`，
    /// 不能让人以为总共就这些。
    pub(super) truncated: bool,
    pub(super) selected: usize,
    pub(super) current_session: uuid::Uuid,
}

pub(super) enum SessionPickerAction {
    None,
    Refresh,
    Switch(PendingSessionSwitch),
    Close,
}

pub(super) struct PendingSessionSwitch {
    pub(super) id: String,
    pub(super) archived: bool,
}

impl App {
    pub(super) fn open_session_picker(
        &mut self,
        current_session: uuid::Uuid,
        request: SessionPickerRequest,
    ) {
        self.palette = None;
        self.search = None;
        let mut editor = PromptEditor::default();
        editor.insert(&request.query);
        self.session_picker = Some(SessionPickerState {
            editor,
            filters: request.filters,
            current_session,
            ..Default::default()
        });
    }

    pub(super) fn set_session_picker_results(
        &mut self,
        mut results: Vec<willdeep_runtime_protocol::SessionSearchResult>,
    ) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        // 一条消息都没有的会话进不了这个列表：它没有可继续的内容，标题永远是
        // 占位符，进去也只是回到一张白纸。当前会话是唯一的例外——人得看得见
        // 自己在哪儿。过滤必须发生在截断之前，否则一串刚建的空会话会按更新
        // 时间排在最前，把 20 个名额吃光，真正的历史一条都露不出来。
        results.retain(|result| result.message_count > 0 || result.id == picker.current_session);
        picker.truncated = results.len() > SESSION_PICKER_LIMIT;
        results.truncate(SESSION_PICKER_LIMIT);
        picker.results = results;
        picker.selected = picker.selected.min(picker.results.len().saturating_sub(1));
    }

    pub(super) fn handle_session_picker_key(&mut self, key: KeyEvent) -> SessionPickerAction {
        let Some(picker) = self.session_picker.as_mut() else {
            return SessionPickerAction::None;
        };
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r'))
        {
            return SessionPickerAction::Close;
        }
        match key.code {
            KeyCode::Up | KeyCode::BackTab if !picker.results.is_empty() => {
                picker.selected = picker
                    .selected
                    .checked_sub(1)
                    .unwrap_or(picker.results.len() - 1);
                SessionPickerAction::None
            }
            KeyCode::Down | KeyCode::Tab if !picker.results.is_empty() => {
                picker.selected = (picker.selected + 1) % picker.results.len();
                SessionPickerAction::None
            }
            KeyCode::Enter if !picker.results.is_empty() => {
                let result = &picker.results[picker.selected.min(picker.results.len() - 1)];
                SessionPickerAction::Switch(PendingSessionSwitch {
                    id: result.id.to_string(),
                    archived: result.status == willdeep_runtime_protocol::SessionStatus::Archived,
                })
            }
            KeyCode::Left => {
                picker.editor.left();
                SessionPickerAction::None
            }
            KeyCode::Right => {
                picker.editor.right();
                SessionPickerAction::None
            }
            KeyCode::Home => {
                picker.editor.home();
                SessionPickerAction::None
            }
            KeyCode::End => {
                picker.editor.end();
                SessionPickerAction::None
            }
            KeyCode::Backspace => {
                picker.editor.backspace();
                SessionPickerAction::Refresh
            }
            KeyCode::Delete => {
                picker.editor.delete();
                SessionPickerAction::Refresh
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                ) =>
            {
                picker.editor.insert(&character.to_string());
                SessionPickerAction::Refresh
            }
            _ => SessionPickerAction::None,
        }
    }

    pub(super) fn activate_session_picker_at(&mut self, x: u16, y: u16) -> bool {
        if !self.session_picker_rect.contains((x, y).into()) {
            return false;
        }
        if y == self.session_picker_rect.y.saturating_add(1) {
            if let Some(picker) = self.session_picker.as_mut() {
                picker.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.session_picker_rect.x + 3) as usize,
                    self.session_picker_rect.width.saturating_sub(4).max(1) as usize,
                );
            }
            return true;
        }
        let Some(position) = self
            .session_picker_hits
            .iter()
            .find_map(|(row, position)| (*row == y).then_some(*position))
        else {
            return true;
        };
        let Some(picker) = self.session_picker.as_mut() else {
            return true;
        };
        picker.selected = position.min(picker.results.len().saturating_sub(1));
        if let Some(result) = picker.results.get(picker.selected) {
            self.pending_session_switch = Some(PendingSessionSwitch {
                id: result.id.to_string(),
                archived: result.status == willdeep_runtime_protocol::SessionStatus::Archived,
            });
            self.session_picker = None;
        }
        true
    }
}

/// 按面板当前的关键词与过滤器重新向 Runtime 要一次结果。
pub(super) async fn refresh_session_picker(app: &mut App, runtime: &TuiRuntime, session: &Session) {
    let Some(picker) = app.session_picker.as_ref() else {
        return;
    };
    let query = picker.editor.text().trim().to_owned();
    let mut parameters = picker.filters.clone();
    // 默认锁在当前工作区；`/session search --workspace <路径>` 显式指定时不覆盖用户的选择。
    if !parameters.iter().any(|(key, _)| key == "workspace") {
        parameters.push((
            "workspace".to_owned(),
            session
                .workspace
                .canonicalize()
                .unwrap_or_else(|_| session.workspace.clone())
                .display()
                .to_string(),
        ));
    }
    if !query.is_empty() {
        parameters.push(("q".to_owned(), query));
    }
    match crate::daemon::search_remote_session_results(&runtime.home, &parameters).await {
        Ok(results) => app.set_session_picker_results(results),
        Err(error) => {
            app.notice = Some(format!(
                "{}: {error}",
                app.language.text(
                    "搜索历史会话失败",
                    "Historical Session search failed",
                    "履歴セッションの検索に失敗"
                )
            ));
        }
    }
}

pub(super) fn render_session_picker(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.session_picker_rect = Rect::default();
    app.session_picker_hits.clear();
    let Some(picker) = &app.session_picker else {
        return;
    };
    let width = f.area().width.min(100);
    let height = f
        .area()
        .height
        .min((picker.results.len().min(16) as u16 + 3).max(7));
    let popup = centered_rect(width, height, f.area());
    app.session_picker_rect = popup;
    let visible = popup.height.saturating_sub(3).max(1) as usize;
    let start = picker.selected.saturating_sub(visible - 1);
    let mut lines = vec![Line::styled(
        format!("› {}", picker.editor.text()),
        Style::default().fg(Color::Yellow),
    )];
    if picker.results.is_empty() {
        lines.push(Line::styled(
            app.language.text(
                "没有匹配的历史会话",
                "No historical Sessions match",
                "一致する履歴セッションがありません",
            ),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for (position, result) in picker.results.iter().enumerate().skip(start).take(visible) {
            let selected = position == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else if result.id == picker.current_session {
                Style::default().fg(Color::LightGreen)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::styled(
                session_picker_result_line(
                    result,
                    result.id == picker.current_session,
                    app.language,
                    selected,
                ),
                style,
            ));
            app.session_picker_hits
                .push((popup.y + 2 + (position - start) as u16, position));
        }
    }
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(format!(
                    "{} · {}/{}{} · ↑/↓/Tab · Enter · Esc",
                    // `/session search --workspace <路径>` 会离开当前工作区，
                    // 标题不能继续声称列的是当前工作区。
                    if picker.filters.is_empty() {
                        app.language.text(
                            "历史会话（当前工作区）",
                            "Session history (current Workspace)",
                            "履歴セッション（現在のワークスペース）",
                        )
                    } else {
                        app.language.text(
                            "历史会话（已过滤）",
                            "Session history (filtered)",
                            "履歴セッション（絞り込み中）",
                        )
                    },
                    if picker.results.is_empty() {
                        0
                    } else {
                        picker.selected + 1
                    },
                    picker.results.len(),
                    if picker.truncated { "+" } else { "" }
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightCyan)),
        ),
        popup,
    );
    if popup.width > 3 {
        let cursor = UnicodeWidthStr::width(picker.editor.text())
            .min(popup.width.saturating_sub(4) as usize) as u16;
        f.set_cursor_position((popup.x + 3 + cursor, popup.y + 1));
    }
}

/// 一行结果的排版。标题在最前，ID 挪到状态之后。
///
/// 此前 ID 紧跟标题，而当标题全是 `New session` 时，一屏看下来只有八位十六进制
/// 在变——那不是列表，那是让人用 UUID 认对话。ID 仍然留着：`/session switch`
/// 要它，出问题时人也要靠它对上日志。
pub(super) fn session_picker_result_line(
    result: &willdeep_runtime_protocol::SessionSearchResult,
    current: bool,
    language: Language,
    selected: bool,
) -> String {
    let marker = if selected { "▶" } else { " " };
    let current = if current {
        format!(" [{}]", language.text("当前", "current", "現在"))
    } else {
        String::new()
    };
    let title = result.title.replace(['\r', '\n'], " ");
    let origin = session_origin_label(result.origin, language)
        .map(|label| format!(" [{label}]"))
        .unwrap_or_default();
    let snippet = result
        .snippet
        .as_deref()
        .map(|value| value.replace(['\r', '\n'], " "))
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" · {value}"))
        .unwrap_or_default();
    let short_id = result.id.simple().to_string();
    format!(
        "{marker} {title}{current}{origin} · {} · {} {} · {}{snippet}",
        session_status_label(result.status, language),
        result.message_count,
        language.text("条消息", "messages", "件のメッセージ"),
        &short_id[..8],
    )
}

/// 非 Runtime 来源要在行里说清楚：Xedit 的会话是只读桥接（续聊会在 rs 这边
/// 落一份副本），本地会话则是还没被 Runtime 领养过的。两者都没有 Provider
/// 和模型元数据，不标出来的话，人会以为是这些字段丢了。
fn session_origin_label(
    origin: willdeep_runtime_protocol::SessionOrigin,
    language: Language,
) -> Option<&'static str> {
    use willdeep_runtime_protocol::SessionOrigin;
    match origin {
        SessionOrigin::Runtime => None,
        SessionOrigin::Local => Some(language.text("本地", "local", "ローカル")),
        SessionOrigin::Xedit => Some("Xedit"),
    }
}

fn session_status_label(
    status: willdeep_runtime_protocol::SessionStatus,
    language: Language,
) -> &'static str {
    use willdeep_runtime_protocol::SessionStatus;
    match status {
        SessionStatus::Idle => language.text("空闲", "idle", "待機中"),
        SessionStatus::Queued => language.text("排队中", "queued", "待機列"),
        SessionStatus::Running => language.text("运行中", "running", "実行中"),
        SessionStatus::WaitingApproval => language.text("等待审批", "waiting approval", "承認待ち"),
        SessionStatus::WaitingAnswer => language.text("等待回答", "waiting answer", "回答待ち"),
        SessionStatus::Failed => language.text("失败", "failed", "失敗"),
        SessionStatus::Interrupted => language.text("已中断", "interrupted", "中断"),
        SessionStatus::Archived => language.text("已归档", "archived", "アーカイブ済み"),
    }
}
