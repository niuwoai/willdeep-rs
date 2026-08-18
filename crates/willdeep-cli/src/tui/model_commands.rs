use super::*;

const MODEL_LIST_TIMEOUT_SECONDS: u64 = 20;
const MAX_MODEL_NAME_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ModelCommand {
    List,
    Switch(String),
}

#[derive(Default)]
pub(super) struct ModelPickerState {
    pub(super) editor: PromptEditor,
    pub(super) models: Vec<String>,
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    pub(super) current_model: String,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
}

pub(super) enum ModelPickerAction {
    None,
    Close,
    Select(String),
}

impl App {
    pub(super) fn open_model_picker(&mut self, current_model: String) {
        self.palette = None;
        self.search = None;
        self.session_picker = None;
        self.model_picker = Some(ModelPickerState {
            current_model,
            loading: true,
            ..Default::default()
        });
    }

    pub(super) fn set_model_picker_result(
        &mut self,
        result: std::result::Result<Vec<String>, String>,
    ) {
        let Some(picker) = self.model_picker.as_mut() else {
            return;
        };
        picker.loading = false;
        match result {
            Ok(models) => {
                picker.models = models;
                picker.error = None;
            }
            Err(error) => {
                picker.models.clear();
                picker.error = Some(error);
            }
        }
        self.refresh_model_picker_matches();
    }

    pub(super) fn refresh_model_picker_matches(&mut self) {
        let Some(picker) = self.model_picker.as_mut() else {
            return;
        };
        let query = picker.editor.text().trim().to_lowercase();
        let mut ranked = picker
            .models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| {
                fuzzy_score(&query, &model.to_lowercase()).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, index)| (*score, *index));
        picker.filtered = ranked.into_iter().map(|(_, index)| index).collect();
        picker.selected = 0;
    }

    pub(super) fn handle_model_picker_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        let Some(picker) = self.model_picker.as_mut() else {
            return ModelPickerAction::None;
        };
        if key.code == KeyCode::Esc {
            return ModelPickerAction::Close;
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
                let model_index = picker.filtered[picker.selected.min(picker.filtered.len() - 1)];
                return ModelPickerAction::Select(picker.models[model_index].clone());
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
            self.refresh_model_picker_matches();
        }
        ModelPickerAction::None
    }

    pub(super) fn model_picker_scroll(&mut self, delta: isize) {
        let Some(picker) = self.model_picker.as_mut() else {
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

    pub(super) fn activate_model_picker_at(&mut self, x: u16, y: u16) -> Option<String> {
        if !self.model_picker_rect.contains((x, y).into()) {
            return None;
        }
        if y == self.model_picker_rect.y.saturating_add(1) {
            if let Some(picker) = self.model_picker.as_mut() {
                picker.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.model_picker_rect.x + 3) as usize,
                    self.model_picker_rect.width.saturating_sub(4).max(1) as usize,
                );
            }
            return None;
        }
        let position = self
            .model_picker_hits
            .iter()
            .find_map(|(row, position)| (*row == y).then_some(*position))?;
        let picker = self.model_picker.as_mut()?;
        picker.selected = position.min(picker.filtered.len().saturating_sub(1));
        let model_index = *picker.filtered.get(picker.selected)?;
        picker.models.get(model_index).cloned()
    }
}

pub(super) fn render_model_picker(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.model_picker_rect = Rect::default();
    app.model_picker_hits.clear();
    let Some(picker) = &app.model_picker else {
        return;
    };
    let width = f.area().width.min(100);
    let desired_rows = if picker.loading || picker.error.is_some() || picker.filtered.is_empty() {
        7
    } else {
        picker.filtered.len().min(18) as u16 + 3
    };
    let height = f.area().height.min(desired_rows.max(7));
    let popup = centered_rect(width, height, f.area());
    app.model_picker_rect = popup;
    let visible = popup.height.saturating_sub(3).max(1) as usize;
    let start = picker.selected.saturating_sub(visible - 1);
    let mut lines = vec![Line::styled(
        format!("› {}", picker.editor.text()),
        Style::default().fg(Color::Yellow),
    )];
    if picker.loading {
        lines.push(Line::styled(
            app.language.text(
                "正在从 /v1/models 获取模型…",
                "Loading models from /v1/models…",
                "/v1/models からモデルを取得中…",
            ),
            Style::default().fg(Color::LightCyan),
        ));
    } else if let Some(error) = &picker.error {
        lines.push(Line::styled(
            format!(
                "{}: {error}",
                app.language.text(
                    "获取模型失败；仍可使用 /model <模型名>",
                    "Model lookup failed; /model <name> still works",
                    "モデル取得失敗。/model <名前> は使用可能です",
                )
            ),
            Style::default().fg(Color::LightRed),
        ));
    } else if picker.filtered.is_empty() {
        lines.push(Line::styled(
            app.language.text(
                "没有匹配的模型",
                "No models match",
                "一致するモデルがありません",
            ),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        for (position, model_index) in picker.filtered.iter().enumerate().skip(start).take(visible)
        {
            let model = &picker.models[*model_index];
            let selected = position == picker.selected;
            let current = model == &picker.current_model;
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
                format!(
                    "{} {}{}",
                    if selected { "▶" } else { " " },
                    model,
                    if current {
                        app.language.text(" · 当前", " · current", " · 現在")
                    } else {
                        ""
                    }
                ),
                style,
            ));
            app.model_picker_hits
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
                        .text("选择模型", "Select model", "モデルを選択"),
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

pub(super) fn parse(prompt: &str) -> Option<ModelCommand> {
    let value = prompt.trim();
    if value == "/model" {
        return Some(ModelCommand::List);
    }
    value
        .strip_prefix("/model ")
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(|model| ModelCommand::Switch(model.to_owned()))
}

pub(super) fn request_model_list(app: &mut App, runtime: &TuiRuntime, current_model: String) {
    app.open_model_picker(current_model);
    let mut config = runtime.provider_config.clone();
    config.request_timeout_secs = MODEL_LIST_TIMEOUT_SECONDS;
    let tx = runtime.tx.clone();
    tokio::spawn(async move {
        let result = willdeep_core::provider::list_models(&config)
            .await
            .map_err(|error| error.to_string());
        let _ = tx.send(UiMessage::ModelsLoaded(result));
    });
}

pub(super) async fn switch_model(
    model: &str,
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    agent: &Arc<Agent>,
) -> Result<String> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL_NAME_BYTES || model.chars().any(char::is_control)
    {
        bail!("model must contain 1 to {MAX_MODEL_NAME_BYTES} bytes");
    }
    let previous_model = runtime.provider_config.model.clone();
    agent
        .set_model(model)
        .with_context(|| format!("configure local Agent model `{model}`"))?;
    let remote_result = async {
        crate::daemon::ensure_runtime_session(
            &runtime.home,
            session.id,
            &session.workspace,
            session.profile.clone(),
            Some(model.to_owned()),
            session.title.clone(),
        )
        .await?;
        crate::daemon::update_remote_session_model(&runtime.home, session.id, model.to_owned())
            .await
    }
    .await;
    if let Err(error) = remote_result {
        let _ = agent.set_model(&previous_model);
        return Err(error).with_context(|| format!("update Runtime Session model to `{model}`"));
    }
    session.model = Some(model.to_owned());
    runtime.runtime_submit.model = Some(model.to_owned());
    runtime.provider_config.model = model.to_owned();
    store.save(session)?;
    Ok(format!(
        "{}: {model}",
        app.language
            .text("模型已切换", "Model switched", "モデルを切り替えました")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_and_direct_switch_without_stealing_similar_commands() {
        assert_eq!(parse(" /model "), Some(ModelCommand::List));
        assert_eq!(
            parse("/model  qwen3-coder "),
            Some(ModelCommand::Switch("qwen3-coder".to_owned()))
        );
        assert_eq!(parse("/models"), None);
        assert_eq!(parse("/model-name"), None);
    }
}
