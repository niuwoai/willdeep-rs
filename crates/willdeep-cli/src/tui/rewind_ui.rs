//! `/rewind`：回到第 N 步。
//!
//! 面板列出还能回到的步骤（Runtime 给的），选一步后先确认：这一步之后的几轮会被
//! 丢掉，文件要不要一起回。真正的截断和文件恢复都在 Runtime 里做，面板只负责
//! 把问题问清楚——回退不可撤销到「像没发生过」，但被盖掉的文件原件进回收区，
//! 回退前的整棵树也留了快照，所以确认只问一次，不叠三层。

use super::*;

pub(super) struct RewindPickerState {
    pub(super) points: Vec<crate::daemon::RewindPoint>,
    pub(super) selected: usize,
    /// 已选中一步，正在问「怎么回」。
    pub(super) confirming: bool,
}

pub(super) enum RewindPickerAction {
    None,
    Close,
    Rewind {
        step: usize,
        through_turn_id: Option<uuid::Uuid>,
        restore_workspace: bool,
    },
}

impl App {
    pub(super) fn open_rewind_picker(&mut self, points: Vec<crate::daemon::RewindPoint>) {
        self.palette = None;
        self.search = None;
        self.session_picker = None;
        // 默认停在最近的一步：最常见的回退是「上一步做错了」。
        let selected = points.len().saturating_sub(1);
        self.rewind_picker = Some(RewindPickerState {
            points,
            selected,
            confirming: false,
        });
    }

    pub(super) fn handle_rewind_picker_key(&mut self, key: KeyEvent) -> RewindPickerAction {
        let Some(picker) = self.rewind_picker.as_mut() else {
            return RewindPickerAction::None;
        };
        if picker.points.is_empty() {
            return RewindPickerAction::Close;
        }
        let point = &picker.points[picker.selected.min(picker.points.len() - 1)];
        if picker.confirming {
            return match key.code {
                KeyCode::Esc => {
                    picker.confirming = false;
                    RewindPickerAction::None
                }
                KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('C') => {
                    RewindPickerAction::Rewind {
                        step: point.step,
                        through_turn_id: point.turn_id,
                        restore_workspace: point.can_restore_workspace,
                    }
                }
                KeyCode::Char('v') | KeyCode::Char('V') => RewindPickerAction::Rewind {
                    step: point.step,
                    through_turn_id: point.turn_id,
                    restore_workspace: false,
                },
                _ => RewindPickerAction::None,
            };
        }
        match key.code {
            KeyCode::Esc => RewindPickerAction::Close,
            KeyCode::Up | KeyCode::BackTab => {
                picker.selected = picker
                    .selected
                    .checked_sub(1)
                    .unwrap_or(picker.points.len() - 1);
                RewindPickerAction::None
            }
            KeyCode::Down | KeyCode::Tab => {
                picker.selected = (picker.selected + 1) % picker.points.len();
                RewindPickerAction::None
            }
            KeyCode::Enter => {
                picker.confirming = true;
                RewindPickerAction::None
            }
            _ => RewindPickerAction::None,
        }
    }
}

pub(super) fn rewind_point_line(
    point: &crate::daemon::RewindPoint,
    language: Language,
    selected: bool,
) -> String {
    let marker = if selected { "▶" } else { " " };
    let step = if point.step == 0 {
        language.text("开头", "beginning", "最初").to_owned()
    } else {
        format!(
            "{} {} {}",
            language.text("第", "step", "ステップ"),
            point.step,
            language.text("步", "", "")
        )
        .trim_end()
        .to_owned()
    };
    let files = if point.can_restore_workspace {
        language.text("文件✓", "files ✓", "ファイル✓")
    } else {
        language.text("仅对话", "chat only", "会話のみ")
    };
    let snippet = point.snippet.replace(['\r', '\n'], " ");
    if snippet.is_empty() {
        format!("{marker} {step} · {files}")
    } else {
        format!("{marker} {step} · {files} · {snippet}")
    }
}

/// 确认阶段那一行：把「丢什么、文件回不回」说全，键位跟在后面。
pub(super) fn rewind_confirm_line(
    point: &crate::daemon::RewindPoint,
    dropped: usize,
    language: Language,
) -> String {
    let target = if point.step == 0 {
        language
            .text("回到开头", "Rewind to the beginning", "最初に戻す")
            .to_owned()
    } else {
        format!(
            "{} {} {}",
            language.text("回到第", "Rewind to step", "ステップ"),
            point.step,
            language.text("步", "", "に戻す")
        )
        .trim_end()
        .to_owned()
    };
    let drop = format!(
        "{} {dropped} {}",
        language.text("丢弃之后的", "drop the following", "以降の"),
        language.text("步", "step(s)", "ステップを破棄")
    );
    if point.can_restore_workspace {
        format!(
            "{target} · {drop} · [Enter] {} · [v] {} · [Esc] {}",
            language.text("对话与文件", "conversation and files", "会話とファイル"),
            language.text("仅对话", "conversation only", "会話のみ"),
            language.text("返回", "back", "戻る"),
        )
    } else {
        format!(
            "{target} · {drop} · [Enter] {} · [Esc] {}",
            language.text(
                "仅对话（这一步没有文件检查点）",
                "conversation only (no file checkpoint for this step)",
                "会話のみ（このステップにファイルのチェックポイントはありません）"
            ),
            language.text("返回", "back", "戻る"),
        )
    }
}

pub(super) fn render_rewind_picker(f: &mut ratatui::Frame<'_>, app: &mut App) {
    let Some(picker) = &app.rewind_picker else {
        return;
    };
    let width = f.area().width.min(100);
    let height = f
        .area()
        .height
        .min((picker.points.len().min(14) as u16 + 4).max(7));
    let popup = centered_rect(width, height, f.area());
    let visible = popup.height.saturating_sub(4).max(1) as usize;
    let start = picker.selected.saturating_sub(visible - 1);
    let mut lines = Vec::new();
    for (position, point) in picker.points.iter().enumerate().skip(start).take(visible) {
        let selected = position == picker.selected;
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::styled(
            rewind_point_line(point, app.language, selected),
            style,
        ));
    }
    lines.push(Line::default());
    let footer = if let Some(point) = picker
        .confirming
        .then(|| picker.points.get(picker.selected))
        .flatten()
    {
        let dropped = picker.points.len() - picker.selected;
        Line::styled(
            rewind_confirm_line(point, dropped, app.language),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Line::styled(
            app.language.text(
                "之后的步骤会被丢弃；被覆盖的文件原件进回收区，回退前的工作树另有快照。",
                "Later steps are dropped; overwritten files go to the recovery area and the pre-rewind tree is snapshotted.",
                "以降のステップは破棄されます。上書きされたファイルは回収領域へ、巻き戻し前のツリーはスナップショットに残ります。",
            ),
            Style::default().fg(Color::DarkGray),
        )
    };
    lines.push(footer);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(format!(
                    "{} · {}/{} · ↑/↓ · Enter · Esc",
                    app.language
                        .text("回到第 N 步", "Rewind to a step", "ステップに戻す"),
                    picker.selected + 1,
                    picker.points.len()
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightCyan)),
        ),
        popup,
    );
}

/// 回退成功后写进聊天区的那一行。
pub(super) fn rewind_summary(
    result: &willdeep_runtime_protocol::RewindSessionResult,
    step: usize,
    language: Language,
) -> String {
    let mut parts = vec![format!(
        "{}{}{}",
        if step == 0 {
            language
                .text("已回到开头", "Rewound to the beginning", "最初に戻しました")
                .to_owned()
        } else {
            format!(
                "{} {} {}",
                language.text("已回到第", "Rewound to step", "ステップ"),
                step,
                language.text("步", "", "に戻しました")
            )
            .trim_end()
            .to_owned()
        },
        language.text("，丢弃 ", ", dropped ", "、"),
        format!(
            "{} {}",
            result.dropped_turn_ids.len(),
            language.text("步", "step(s)", "ステップを破棄")
        )
    )];
    match &result.workspace {
        Some(workspace) => {
            parts.push(format!(
                "{} {} · {} {}",
                language.text("文件恢复", "files restored", "復元したファイル"),
                workspace.restored.len(),
                language.text("移除", "removed", "削除"),
                workspace.removed.len()
            ));
            if let Some(recovery) = &workspace.recovery_path {
                parts.push(format!(
                    "{} {recovery}",
                    language.text("原件在", "originals in", "元のファイルは")
                ));
            }
            if !workspace.skipped.is_empty() {
                parts.push(format!(
                    "{} {}",
                    language.text("未能恢复", "not restored", "復元できず"),
                    workspace.skipped.join(", ")
                ));
            }
        }
        None => parts.push(
            language
                .text("文件未动", "files untouched", "ファイルは変更なし")
                .to_owned(),
        ),
    }
    format!("System: {}", parts.join(" · "))
}
