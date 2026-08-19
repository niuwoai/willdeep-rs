use super::*;

const CONTEXT_WINDOWS: &[u64] = &[32_768, 49_152, 65_536, 131_072, 262_144, 1_000_000];

pub(super) struct RoutingSettingsState {
    pub(super) settings: crate::model_routing::ModelRoutingSettings,
    selected: usize,
    editor: Option<PromptEditor>,
    error: Option<String>,
}

pub(super) enum RoutingSettingsAction {
    None,
    Close,
    Save(crate::model_routing::ModelRoutingUpdate),
}

impl App {
    pub(super) fn open_routing_settings(
        &mut self,
        settings: crate::model_routing::ModelRoutingSettings,
    ) {
        self.palette = None;
        self.search = None;
        self.session_picker = None;
        self.model_picker = None;
        self.routing_settings = Some(RoutingSettingsState {
            settings,
            selected: 0,
            editor: None,
            error: None,
        });
    }

    pub(super) fn routing_settings_paste(&mut self, value: &str) -> bool {
        let Some(state) = self.routing_settings.as_mut() else {
            return false;
        };
        if let Some(editor) = state.editor.as_mut() {
            editor.insert(value);
        }
        true
    }

    pub(super) fn set_routing_settings_saved(
        &mut self,
        settings: crate::model_routing::ModelRoutingSettings,
    ) {
        let Some(state) = self.routing_settings.as_mut() else {
            return;
        };
        state.settings = settings;
        state.error = None;
    }

    pub(super) fn set_routing_settings_error(&mut self, error: String) {
        if let Some(state) = self.routing_settings.as_mut() {
            state.error = Some(error);
        }
    }

    pub(super) fn handle_routing_settings_key(&mut self, key: KeyEvent) -> RoutingSettingsAction {
        let Some(state) = self.routing_settings.as_mut() else {
            return RoutingSettingsAction::None;
        };
        if let Some(editor) = state.editor.as_mut() {
            match key.code {
                KeyCode::Esc => state.editor = None,
                KeyCode::Enter => {
                    let model = editor.text().trim().to_owned();
                    if model.is_empty() {
                        state.error = Some(
                            self.language
                                .text(
                                    "模型名称不能为空",
                                    "Model name cannot be empty",
                                    "モデル名は空にできません",
                                )
                                .to_owned(),
                        );
                    } else {
                        if state.selected == 0 {
                            state.settings.root_model = model;
                        } else if let Some(profile) =
                            state.settings.profiles.get_mut(state.selected - 1)
                        {
                            profile.model = Some(model);
                            profile.automatic = false;
                        }
                        state.editor = None;
                        refresh_effective(&mut state.settings);
                    }
                }
                KeyCode::Left => editor.left(),
                KeyCode::Right => editor.right(),
                KeyCode::Home => editor.home(),
                KeyCode::End => editor.end(),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Delete => editor.delete(),
                KeyCode::Char(character)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                    ) =>
                {
                    editor.insert(&character.to_string());
                }
                _ => {}
            }
            return RoutingSettingsAction::None;
        }
        if key.code == KeyCode::Esc {
            return RoutingSettingsAction::Close;
        }
        if key.code == KeyCode::Char('s')
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        {
            return RoutingSettingsAction::Save(state.settings.to_update());
        }
        let row_count = state.settings.profiles.len() + 1;
        match key.code {
            KeyCode::Up => state.selected = state.selected.checked_sub(1).unwrap_or(row_count - 1),
            KeyCode::Down => state.selected = (state.selected + 1) % row_count,
            KeyCode::Left => cycle_provider(&mut state.settings, state.selected, -1),
            KeyCode::Right | KeyCode::Tab => cycle_provider(&mut state.settings, state.selected, 1),
            KeyCode::Enter => {
                let model = if state.selected == 0 {
                    state.settings.root_model.clone()
                } else {
                    state
                        .settings
                        .profiles
                        .get(state.selected - 1)
                        .and_then(|profile| profile.model.clone())
                        .unwrap_or_else(|| {
                            state.settings.profiles[state.selected - 1]
                                .effective_model
                                .clone()
                        })
                };
                let mut editor = PromptEditor::default();
                editor.insert(&model);
                state.editor = Some(editor);
                state.error = None;
            }
            KeyCode::Char(' ') if state.selected > 0 => {
                if let Some(profile) = state.settings.profiles.get_mut(state.selected - 1) {
                    profile.provider_profile = None;
                    profile.model = None;
                    profile.automatic = true;
                }
                refresh_effective(&mut state.settings);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                state.settings.small_model_routing = !state.settings.small_model_routing
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                state.settings.auto_dispatch_read_only = !state.settings.auto_dispatch_read_only
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                state.settings.max_deep_calls_per_harness =
                    (state.settings.max_deep_calls_per_harness + 1).min(16)
            }
            KeyCode::Char('-') => {
                state.settings.max_deep_calls_per_harness =
                    state.settings.max_deep_calls_per_harness.saturating_sub(1)
            }
            KeyCode::Char('[') if state.selected > 0 => {
                cycle_context_window(&mut state.settings, state.selected - 1, -1)
            }
            KeyCode::Char(']') if state.selected > 0 => {
                cycle_context_window(&mut state.settings, state.selected - 1, 1)
            }
            _ => {}
        }
        RoutingSettingsAction::None
    }
}

fn cycle_provider(
    settings: &mut crate::model_routing::ModelRoutingSettings,
    selected: usize,
    delta: isize,
) {
    if settings.providers.is_empty() {
        return;
    }
    if selected == 0 {
        let current = settings
            .providers
            .iter()
            .position(|provider| provider.id == settings.default_provider)
            .unwrap_or(0);
        let next = wrapped_index(current, settings.providers.len(), delta);
        settings.default_provider = settings.providers[next].id.clone();
        if !settings.providers[next].model.is_empty() {
            settings.root_model = settings.providers[next].model.clone();
        }
    } else if let Some(profile) = settings.profiles.get_mut(selected - 1) {
        let current = profile
            .provider_profile
            .as_ref()
            .and_then(|id| {
                settings
                    .providers
                    .iter()
                    .position(|provider| &provider.id == id)
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        let next = wrapped_index(current, settings.providers.len() + 1, delta);
        profile.provider_profile = next
            .checked_sub(1)
            .map(|index| settings.providers[index].id.clone());
        profile.automatic = profile.provider_profile.is_none() && profile.model.is_none();
    }
    refresh_effective(settings);
}

fn wrapped_index(current: usize, count: usize, delta: isize) -> usize {
    if delta < 0 {
        current.checked_sub(1).unwrap_or(count - 1)
    } else {
        (current + 1) % count
    }
}

fn cycle_context_window(
    settings: &mut crate::model_routing::ModelRoutingSettings,
    profile_index: usize,
    delta: isize,
) {
    let Some(profile) = settings.profiles.get_mut(profile_index) else {
        return;
    };
    let current = CONTEXT_WINDOWS
        .iter()
        .position(|window| *window == profile.context_window)
        .unwrap_or_else(|| {
            CONTEXT_WINDOWS
                .iter()
                .position(|window| *window > profile.context_window)
                .unwrap_or(CONTEXT_WINDOWS.len() - 1)
        });
    profile.context_window = CONTEXT_WINDOWS[wrapped_index(current, CONTEXT_WINDOWS.len(), delta)];
}

fn refresh_effective(settings: &mut crate::model_routing::ModelRoutingSettings) {
    for profile in &mut settings.profiles {
        let provider_id = profile
            .provider_profile
            .as_deref()
            .unwrap_or(&settings.default_provider);
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id);
        let some_im = provider.is_some_and(|provider| provider.provider == "some-im");
        profile.effective_provider = provider_id.to_owned();
        profile.recommended_model = (profile.provider_profile.is_none() && some_im)
            .then(|| willdeep_core::subagent::hosted_worker_model(&profile.id))
            .flatten();
        profile.effective_model = profile.model.clone().unwrap_or_else(|| {
            profile.recommended_model.clone().unwrap_or_else(|| {
                provider
                    .map(|provider| provider.model.clone())
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| settings.root_model.clone())
            })
        });
        profile.automatic = profile.provider_profile.is_none() && profile.model.is_none();
    }
}

pub(super) fn render_routing_settings(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.routing_settings_rect = Rect::default();
    let Some(state) = app.routing_settings.as_ref() else {
        return;
    };
    let width = f.area().width.min(112);
    let height = f.area().height.min(22);
    let popup = Rect {
        x: f.area().x + f.area().width.saturating_sub(width) / 2,
        y: f.area().y + f.area().height.saturating_sub(height) / 2,
        width,
        height,
    };
    app.routing_settings_rect = popup;
    let language = app.language;
    let enabled = |value: bool| {
        if value {
            language.text("开", "on", "オン")
        } else {
            language.text("关", "off", "オフ")
        }
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!(
                    "{}={}  {}={}  Deep={}  ",
                    language.text("路由", "routing", "ルーティング"),
                    enabled(state.settings.small_model_routing),
                    language.text("只读自动派工", "read-only auto dispatch", "読取自動委任"),
                    enabled(state.settings.auto_dispatch_read_only),
                    state.settings.max_deep_calls_per_harness,
                ),
                Style::default().fg(Color::LightCyan),
            ),
            Span::styled(
                language.text(
                    "R/A 切换 · +/- Deep · Ctrl+S 保存",
                    "R/A toggle · +/- Deep · Ctrl+S save",
                    "R/A 切替 · +/- Deep · Ctrl+S 保存",
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::styled(
            language.text(
                "↑↓ 选择 · ←→ Provider · Enter 编辑模型 · Space 推荐默认 · [] 上下文",
                "↑↓ select · ←→ provider · Enter edit model · Space recommended · [] context",
                "↑↓ 選択 · ←→ Provider · Enter モデル編集 · Space 推奨 · [] コンテキスト",
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Line::styled(
            routing_row(
                language.text("工种", "profile", "プロファイル"),
                "Provider",
                language.text("模型", "model", "モデル"),
                language.text("上下文", "context", "文脈"),
                language.text("模式", "mode", "モード"),
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    let root_style = selected_style(state.selected == 0);
    lines.push(Line::styled(
        routing_row(
            language.text("主模型", "Root", "ルート"),
            &state.settings.default_provider,
            &state.settings.root_model,
            "—",
            language.text("持久化", "persistent", "永続"),
        ),
        root_style,
    ));
    for (index, profile) in state.settings.profiles.iter().enumerate() {
        let mode = if profile.automatic {
            language.text("推荐", "recommended", "推奨")
        } else {
            language.text("覆盖", "override", "上書き")
        };
        lines.push(Line::styled(
            routing_row(
                profile_label(&profile.id, language),
                &profile.effective_provider,
                &profile.effective_model,
                &format_context(profile.context_window),
                mode,
            ),
            selected_style(state.selected == index + 1),
        ));
    }
    if let Some(editor) = state.editor.as_ref() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", language.text("模型", "Model", "モデル")),
                Style::default().fg(Color::LightCyan),
            ),
            Span::raw(editor.text()),
        ]));
    } else if let Some(error) = state.error.as_ref() {
        lines.push(Line::styled(
            truncate_cell(error, popup.width.saturating_sub(4) as usize),
            Style::default().fg(Color::LightRed),
        ));
    } else {
        lines.push(Line::styled(
            language.text(
                "保存后对新 Harness/子 Agent 生效；当前运行任务不变。",
                "Saved values apply to new harnesses/child Agents; running work is unchanged.",
                "保存内容は新しい Harness/子 Agent に適用され、実行中の処理は変わりません。",
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(language.text("模型与路由", "Models & routing", "モデルとルーティング"))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightCyan)),
        ),
        popup,
    );
    if let Some(editor) = state.editor.as_ref() {
        let row = popup.y + popup.height.saturating_sub(2);
        let prefix = language.text("模型: ", "Model: ", "モデル: ");
        let column = popup.x
            + 1
            + UnicodeWidthStr::width(prefix) as u16
            + UnicodeWidthStr::width(editor.text()) as u16;
        f.set_cursor_position((column.min(popup.right().saturating_sub(2)), row));
    }
}

fn selected_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightCyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn profile_label(id: &str, language: Language) -> &'static str {
    match id {
        "scout" => language.text("侦查", "Scout", "偵察"),
        "reader" => language.text("阅读", "Reader", "読解"),
        "editor" => language.text("单文件编辑", "Editor", "単一編集"),
        "implementer" => language.text("实现", "Implementer", "実装"),
        "test_fixer" => language.text("测试修复", "Test fixer", "テスト修正"),
        "build_fixer" => language.text("构建修复", "Build fixer", "ビルド修正"),
        "log_inspector" => language.text("日志分析", "Log inspector", "ログ解析"),
        "git_detective" => language.text("Git 追溯", "Git detective", "Git 追跡"),
        "deep" => "Deep",
        _ => language.text("未知", "Unknown", "不明"),
    }
}

fn truncate_cell(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    let mut result = String::new();
    for character in value.chars() {
        if UnicodeWidthStr::width(result.as_str())
            + unicode_width::UnicodeWidthChar::width(character).unwrap_or(0)
            + 1
            > width
        {
            break;
        }
        result.push(character);
    }
    result.push('…');
    result
}

fn routing_row(label: &str, provider: &str, model: &str, context: &str, mode: &str) -> String {
    format!(
        "{} {} {} {}  {}",
        pad_cell(label, 16),
        pad_cell(provider, 14),
        pad_cell(model, 32),
        pad_cell(context, 9),
        mode,
    )
}

fn pad_cell(value: &str, width: usize) -> String {
    let value = truncate_cell(value, width);
    let padding = width.saturating_sub(UnicodeWidthStr::width(value.as_str()));
    format!("{value}{}", " ".repeat(padding))
}

fn format_context(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else {
        format!("{}K", value / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_reset_restores_hosted_some_im_worker() {
        let mut settings = crate::model_routing::ModelRoutingSettings {
            revision: "r1".to_owned(),
            default_provider: "some-im".to_owned(),
            active_provider_override: None,
            root_model: "glm-5".to_owned(),
            small_model_routing: true,
            auto_dispatch_read_only: true,
            max_deep_calls_per_harness: 1,
            providers: vec![crate::model_routing::ModelProviderOption {
                id: "some-im".to_owned(),
                provider: "some-im".to_owned(),
                model: "glm-5".to_owned(),
            }],
            profiles: vec![crate::model_routing::ProfileRoutingSettings {
                id: "scout".to_owned(),
                provider_profile: Some("some-im".to_owned()),
                model: Some("custom".to_owned()),
                context_window: 32_768,
                automatic: false,
                effective_provider: "some-im".to_owned(),
                effective_model: "custom".to_owned(),
                recommended_model: None,
            }],
        };
        settings.profiles[0].provider_profile = None;
        settings.profiles[0].model = None;
        refresh_effective(&mut settings);
        assert!(settings.profiles[0].automatic);
        assert_eq!(settings.profiles[0].effective_model, "someim-32b-scout");
    }

    #[test]
    fn routing_rows_align_cjk_by_terminal_width() {
        let row = routing_row(
            "测试修复",
            "some-im",
            "someim-32b-test-fixer",
            "64K",
            "推荐",
        );
        assert_eq!(
            UnicodeWidthStr::width(row.as_str()),
            16 + 1 + 14 + 1 + 32 + 1 + 9 + 2 + 4
        );
    }
}
