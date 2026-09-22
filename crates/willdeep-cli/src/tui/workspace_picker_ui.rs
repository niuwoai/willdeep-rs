//! 工作区面板：`/workspace` 开，方向键或鼠标选中，`Enter` 当场切过去。
//!
//! 在这之前 `/workspace` 只是把工作区打印成一串文本，切换要人把 UUID 从
//! 聊天记录里抄回输入框——那不是选择，那是听写。面板与 `/history` 同一套
//! 交互：输入即过滤，↑/↓ 选，`Enter` 进入，`Esc` 退出。

use super::*;

#[derive(Default)]
pub(super) struct WorkspacePickerState {
    pub(super) editor: PromptEditor,
    pub(super) workspaces: Vec<crate::daemon::RuntimeWorkspace>,
    /// 命中当前关键词的下标，按匹配度排序。
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    /// 当前会话所在的工作区根目录，用来在列表里标出「当前」。
    pub(super) current_root: PathBuf,
}

pub(super) enum WorkspacePickerAction {
    None,
    Close,
    Select(uuid::Uuid),
}

impl App {
    pub(super) fn open_workspace_picker(
        &mut self,
        workspaces: Vec<crate::daemon::RuntimeWorkspace>,
        current_root: PathBuf,
    ) {
        self.palette = None;
        self.search = None;
        self.session_picker = None;
        self.model_picker = None;
        let mut picker = WorkspacePickerState {
            workspaces,
            current_root,
            ..Default::default()
        };
        picker.filtered = (0..picker.workspaces.len()).collect();
        // 光标落在当前工作区上：列表一打开，人先看见自己在哪儿。
        picker.selected = picker
            .filtered
            .iter()
            .position(|index| {
                picker.workspaces[*index].root == picker.current_root
                    || picker.workspaces[*index].active
            })
            .unwrap_or(0);
        self.workspace_picker = Some(picker);
    }

    pub(super) fn refresh_workspace_picker_matches(&mut self) {
        let Some(picker) = self.workspace_picker.as_mut() else {
            return;
        };
        let query = picker.editor.text().trim().to_lowercase();
        // 名字、路径、ID 都参与匹配：人记得住的是名字或路径，而排查问题时
        // 手上往往只有一个 ID。
        let mut ranked = picker
            .workspaces
            .iter()
            .enumerate()
            .filter_map(|(index, workspace)| {
                let haystack = format!(
                    "{} {} {}",
                    workspace.name,
                    workspace.root.display(),
                    workspace.id
                )
                .to_lowercase();
                fuzzy_score(&query, &haystack).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, index)| (*score, *index));
        picker.filtered = ranked.into_iter().map(|(_, index)| index).collect();
        picker.selected = 0;
    }

    pub(super) fn handle_workspace_picker_key(&mut self, key: KeyEvent) -> WorkspacePickerAction {
        let Some(picker) = self.workspace_picker.as_mut() else {
            return WorkspacePickerAction::None;
        };
        if key.code == KeyCode::Esc {
            return WorkspacePickerAction::Close;
        }
        let mut query_changed = false;
        match key.code {
            KeyCode::Up | KeyCode::BackTab if !picker.filtered.is_empty() => {
                picker.selected = picker
                    .selected
                    .checked_sub(1)
                    .unwrap_or(picker.filtered.len() - 1);
            }
            KeyCode::Down | KeyCode::Tab if !picker.filtered.is_empty() => {
                picker.selected = (picker.selected + 1) % picker.filtered.len();
            }
            KeyCode::PageUp if !picker.filtered.is_empty() => {
                picker.selected = picker.selected.saturating_sub(10);
            }
            KeyCode::PageDown if !picker.filtered.is_empty() => {
                picker.selected = (picker.selected + 10).min(picker.filtered.len() - 1);
            }
            KeyCode::Enter if !picker.filtered.is_empty() => {
                let index = picker.filtered[picker.selected.min(picker.filtered.len() - 1)];
                return WorkspacePickerAction::Select(picker.workspaces[index].id);
            }
            KeyCode::Left => picker.editor.left(),
            KeyCode::Right => picker.editor.right(),
            KeyCode::Home => picker.editor.home(),
            KeyCode::End => picker.editor.end(),
            KeyCode::Backspace => {
                picker.editor.backspace();
                query_changed = true;
            }
            KeyCode::Delete => {
                picker.editor.delete();
                query_changed = true;
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                ) =>
            {
                picker.editor.insert(&character.to_string());
                query_changed = true;
            }
            _ => {}
        }
        if query_changed {
            self.refresh_workspace_picker_matches();
        }
        WorkspacePickerAction::None
    }

    pub(super) fn workspace_picker_scroll(&mut self, delta: isize) {
        let Some(picker) = self.workspace_picker.as_mut() else {
            return;
        };
        if picker.filtered.is_empty() {
            return;
        }
        if delta < 0 {
            picker.selected = picker.selected.saturating_sub(1);
        } else {
            picker.selected = (picker.selected + 1).min(picker.filtered.len() - 1);
        }
    }

    /// 点中某一行就是选中它；点在输入行上只挪光标。返回要切过去的工作区。
    pub(super) fn activate_workspace_picker_at(&mut self, x: u16, y: u16) -> Option<uuid::Uuid> {
        if !self.workspace_picker_rect.contains((x, y).into()) {
            return None;
        }
        if y == self.workspace_picker_rect.y.saturating_add(1) {
            if let Some(picker) = self.workspace_picker.as_mut() {
                picker.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.workspace_picker_rect.x + 3) as usize,
                    self.workspace_picker_rect.width.saturating_sub(4).max(1) as usize,
                );
            }
            return None;
        }
        let position = self
            .workspace_picker_hits
            .iter()
            .find_map(|(row, position)| (*row == y).then_some(*position))?;
        let picker = self.workspace_picker.as_mut()?;
        picker.selected = position.min(picker.filtered.len().saturating_sub(1));
        let index = *picker.filtered.get(picker.selected)?;
        picker.workspaces.get(index).map(|workspace| workspace.id)
    }
}

pub(super) fn render_workspace_picker(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.workspace_picker_rect = Rect::default();
    app.workspace_picker_hits.clear();
    let Some(picker) = &app.workspace_picker else {
        return;
    };
    let width = f.area().width.min(100);
    let desired_rows = if picker.filtered.is_empty() {
        7
    } else {
        picker.filtered.len().min(18) as u16 + 3
    };
    let height = f.area().height.min(desired_rows.max(7));
    let popup = centered_rect(width, height, f.area());
    app.workspace_picker_rect = popup;
    let visible = popup.height.saturating_sub(3).max(1) as usize;
    let start = picker.selected.saturating_sub(visible - 1);
    let mut lines = vec![Line::styled(
        format!("› {}", picker.editor.text()),
        Style::default().fg(Color::Yellow),
    )];
    if picker.filtered.is_empty() {
        lines.push(Line::styled(
            app.language.text(
                "没有匹配的工作区",
                "No Workspaces match",
                "一致するワークスペースがありません",
            ),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for (position, index) in picker.filtered.iter().enumerate().skip(start).take(visible) {
            let workspace = &picker.workspaces[*index];
            let selected = position == picker.selected;
            let current = workspace.root == picker.current_root;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else if current {
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                workspace_picker_line(workspace, current, app.language, selected),
                style,
            ));
            app.workspace_picker_hits
                .push((popup.y + 2 + (position - start) as u16, position));
        }
    }
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(format!(
                    "{} · {}/{} · ↑/↓/PgUp/PgDn · Enter · Esc",
                    app.language
                        .text("选择工作区", "Select Workspace", "ワークスペースを選択"),
                    if picker.filtered.is_empty() {
                        0
                    } else {
                        picker.selected + 1
                    },
                    picker.filtered.len()
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

/// 一行的排版：名字在最前，路径其次，审批档位收尾。ID 不进这一行——
/// 面板里没人再需要它，要 ID 的人还有 `/workspace list`。
pub(super) fn workspace_picker_line(
    workspace: &crate::daemon::RuntimeWorkspace,
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
    format!(
        "{marker} {}{current} · {} · {}",
        workspace.name,
        workspace.root.display(),
        workspace.access.wire_name()
    )
}
