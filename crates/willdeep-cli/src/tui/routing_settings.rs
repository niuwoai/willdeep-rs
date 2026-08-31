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

/// 面板里的一行。三段共用一个扁平索引：主模型、五个职责、三个档位。
///
/// 抽成枚举是因为原来每处都在写 `selected - 1`——中间再插一段，每一处都要
/// 同步改，漏一处就是「按 `[` 改的是隔壁那行的上下文」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Root,
    Profile(usize),
    Tier(usize),
}

fn row_at(settings: &crate::model_routing::ModelRoutingSettings, selected: usize) -> Row {
    let Some(index) = selected.checked_sub(1) else {
        return Row::Root;
    };
    match index.checked_sub(settings.profiles.len()) {
        Some(tier) => Row::Tier(tier),
        None => Row::Profile(index),
    }
}

impl RoutingSettingsState {
    fn row_count(&self) -> usize {
        1 + self.settings.profiles.len() + self.settings.tiers.len()
    }

    fn row(&self) -> Row {
        row_at(&self.settings, self.selected)
    }

    /// 当前行显示的模型，供编辑器预填。
    fn current_model(&self) -> String {
        match self.row() {
            Row::Root => self.settings.root_model.clone(),
            Row::Profile(index) => {
                self.settings
                    .profiles
                    .get(index)
                    .map_or_else(String::new, |profile| {
                        profile
                            .model
                            .clone()
                            .unwrap_or_else(|| profile.effective_model.clone())
                    })
            }
            Row::Tier(index) => self
                .settings
                .tiers
                .get(index)
                .map_or_else(String::new, |tier| {
                    tier.model
                        .clone()
                        .unwrap_or_else(|| tier.effective_model.clone())
                }),
        }
    }
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
                        match row_at(&state.settings, state.selected) {
                            Row::Root => state.settings.root_model = model,
                            Row::Profile(index) => {
                                if let Some(profile) = state.settings.profiles.get_mut(index) {
                                    profile.model = Some(model);
                                    profile.automatic = false;
                                }
                            }
                            Row::Tier(index) => {
                                if let Some(tier) = state.settings.tiers.get_mut(index) {
                                    tier.model = Some(model);
                                    tier.automatic = false;
                                }
                            }
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
        let row_count = state.row_count();
        match key.code {
            KeyCode::Up => state.selected = state.selected.checked_sub(1).unwrap_or(row_count - 1),
            KeyCode::Down => state.selected = (state.selected + 1) % row_count,
            KeyCode::Left => cycle_provider(&mut state.settings, state.selected, -1),
            KeyCode::Right | KeyCode::Tab => cycle_provider(&mut state.settings, state.selected, 1),
            KeyCode::Enter => {
                let model = state.current_model();
                let mut editor = PromptEditor::default();
                editor.insert(&model);
                state.editor = Some(editor);
                state.error = None;
            }
            KeyCode::Char(' ') => {
                match row_at(&state.settings, state.selected) {
                    Row::Root => {}
                    Row::Profile(index) => {
                        if let Some(profile) = state.settings.profiles.get_mut(index) {
                            profile.provider_profile = None;
                            profile.model = None;
                            profile.automatic = true;
                        }
                    }
                    Row::Tier(index) => {
                        if let Some(tier) = state.settings.tiers.get_mut(index) {
                            tier.provider_profile = None;
                            tier.model = None;
                            tier.automatic = true;
                        }
                    }
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
            KeyCode::Char('[') => cycle_context_window(&mut state.settings, state.selected, -1),
            KeyCode::Char(']') => cycle_context_window(&mut state.settings, state.selected, 1),
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
    } else {
        // `providers.len() + 1` 的那个 +1 是「跟随主模型」这一档，索引 0。
        let position = |current: &Option<String>| {
            current
                .as_ref()
                .and_then(|id| {
                    settings
                        .providers
                        .iter()
                        .position(|provider| &provider.id == id)
                })
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        let count = settings.providers.len() + 1;
        let ids = settings
            .providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>();
        match row_at(settings, selected) {
            Row::Root => {}
            Row::Profile(index) => {
                if let Some(profile) = settings.profiles.get_mut(index) {
                    let next = wrapped_index(position(&profile.provider_profile), count, delta);
                    profile.provider_profile = next.checked_sub(1).map(|index| ids[index].clone());
                    profile.automatic =
                        profile.provider_profile.is_none() && profile.model.is_none();
                }
            }
            Row::Tier(index) => {
                if let Some(tier) = settings.tiers.get_mut(index) {
                    let next = wrapped_index(position(&tier.provider_profile), count, delta);
                    tier.provider_profile = next.checked_sub(1).map(|index| ids[index].clone());
                    tier.automatic = tier.provider_profile.is_none() && tier.model.is_none();
                }
            }
        }
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
    selected: usize,
    delta: isize,
) {
    let step = |current: u64| {
        let index = CONTEXT_WINDOWS
            .iter()
            .position(|window| *window == current)
            .unwrap_or_else(|| {
                CONTEXT_WINDOWS
                    .iter()
                    .position(|window| *window > current)
                    .unwrap_or(CONTEXT_WINDOWS.len() - 1)
            });
        CONTEXT_WINDOWS[wrapped_index(index, CONTEXT_WINDOWS.len(), delta)]
    };
    match row_at(settings, selected) {
        Row::Root => {}
        Row::Profile(index) => {
            if let Some(profile) = settings.profiles.get_mut(index) {
                profile.context_window = step(profile.context_window);
            }
        }
        Row::Tier(index) => {
            if let Some(tier) = settings.tiers.get_mut(index) {
                tier.context_window = step(tier.context_window);
                // 改过窗口就不再是「跟随档位默认预算」了，得写进 config，
                // 否则保存之后这一格会弹回默认值。
                tier.automatic = false;
            }
        }
    }
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
    for tier in &mut settings.tiers {
        let provider_id = tier
            .provider_profile
            .as_deref()
            .unwrap_or(&settings.default_provider);
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id);
        let some_im = provider.is_some_and(|provider| provider.provider == "some-im");
        tier.effective_provider = provider_id.to_owned();
        // 基础档在网关上就是工种自己的模型，没有独立的「档位默认」可推荐。
        tier.recommended_model = (some_im && tier.id != "standard")
            .then(|| {
                willdeep_core::WorkerTier::parse(&tier.id)
                    .map(|parsed| parsed.default_hosted_model().to_owned())
            })
            .flatten();
        tier.effective_model = tier.model.clone().unwrap_or_else(|| {
            tier.recommended_model.clone().unwrap_or_else(|| {
                provider
                    .map(|provider| provider.model.clone())
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| settings.root_model.clone())
            })
        });
    }
}

pub(super) fn render_routing_settings(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.routing_settings_rect = Rect::default();
    let Some(state) = app.routing_settings.as_ref() else {
        return;
    };
    let width = f.area().width.min(112);
    // 3 行表头 + 主模型 + 五个职责 + 档位小标题 + 三个档位 + 提示行 + 上下边框。
    // 此前这里写死 22，加了档位那一段之后正好被裁掉——弹层高度必须跟着行数走，
    // 否则每次加一行都要有人记得回来改这个数字。
    let rows = 3 + 1 + state.settings.profiles.len() + 1 + state.settings.tiers.len() + 1 + 2;
    let height = f
        .area()
        .height
        .min(u16::try_from(rows).unwrap_or(u16::MAX).max(12));
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
    lines.push(Line::styled(
        routing_row(
            language.text("档位", "tier", "ティア"),
            "Provider",
            language.text("模型", "model", "モデル"),
            language.text("预算", "budget", "予算"),
            language.text("模式", "mode", "モード"),
        ),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ));
    for (index, tier) in state.settings.tiers.iter().enumerate() {
        let mode = if tier.requires_admission {
            // 专家档绑到多贵的模型都还有票据兜着，这件事必须在选它的地方说。
            language.text("需票据", "ticket", "チケット")
        } else if tier.automatic {
            language.text("推荐", "recommended", "推奨")
        } else {
            language.text("覆盖", "override", "上書き")
        };
        lines.push(Line::styled(
            routing_row(
                tier_label(&tier.id, language),
                &tier.effective_provider,
                &tier.effective_model,
                &format_context(tier.context_window),
                mode,
            ),
            selected_style(state.selected == state.settings.profiles.len() + index + 1),
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
    // 编辑行永远是 `lines` 的最后一行；光标要落在它**实际渲染到**的那一行上，
    // 所以这个行数必须在 `lines` 被 Paragraph 拿走之前先记下来。
    let editor_row = popup.y + u16::try_from(lines.len()).unwrap_or(u16::MAX);
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
        // 此前这里写的是 `popup.y + popup.height - 2`，等于假设内容正好把弹层
        // 填满。弹层高度是固定的 22 行，工种表填不满时它比内容高，光标就掉进
        // 内容下方的空白里——人看到的是「字在这儿、光标在那儿」，删了几个字符
        // 也不知道删在哪。
        let row = editor_row.min(popup.bottom().saturating_sub(2));
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

fn tier_label(id: &str, language: Language) -> &'static str {
    match id {
        "standard" => language.text("基础", "Standard", "標準"),
        "advanced" => language.text("进阶", "Advanced", "上位"),
        "expert" => language.text("专家", "Expert", "エキスパート"),
        _ => language.text("未知", "Unknown", "不明"),
    }
}

fn profile_label(id: &str, language: Language) -> &'static str {
    match id {
        "scout" => language.text("侦查", "Scout", "偵察"),
        "reader" => language.text("阅读", "Reader", "読解"),
        "editor" => language.text("单文件编辑", "Editor", "単一編集"),
        "implementer" => language.text("实现", "Implementer", "実装"),
        "tester" => language.text("测试与审核", "Tester", "テスト・レビュー"),
        "ops_runner" => language.text("运维执行", "Ops runner", "運用実行"),
        "judge" => language.text("独立裁判", "Judge", "独立判定"),
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
            tiers: tier_fixture(),
        };
        settings.profiles[0].provider_profile = None;
        settings.profiles[0].model = None;
        refresh_effective(&mut settings);
        assert!(settings.profiles[0].automatic);
        assert_eq!(settings.profiles[0].effective_model, "someim-32b");
    }

    fn tier_fixture() -> Vec<crate::model_routing::TierRoutingSettings> {
        willdeep_core::WorkerTier::ALL
            .iter()
            .map(|tier| crate::model_routing::TierRoutingSettings {
                id: tier.as_str().to_owned(),
                provider_profile: None,
                model: None,
                context_window: tier.context_budget(),
                automatic: true,
                effective_provider: "some-im".to_owned(),
                effective_model: String::new(),
                recommended_model: None,
                requires_admission: tier.requires_admission(),
            })
            .collect()
    }

    /// 档位在面板上必须解析成和运行时同一个模型。
    ///
    /// 这是 0.51 那个毛病的复发点：一张只写在文档里的表，让界面显示一个运行时
    /// 根本不会用的模型。基础档没有独立的档位默认——它就是工种自己的模型——
    /// 所以那一行不该凭空冒出一个推荐值。
    #[test]
    fn tier_rows_resolve_to_the_same_models_the_runtime_uses() {
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
            profiles: Vec::new(),
            tiers: tier_fixture(),
        };
        refresh_effective(&mut settings);
        assert_eq!(
            settings.tiers[0].recommended_model, None,
            "基础档没有独立的档位默认"
        );
        assert_eq!(settings.tiers[0].effective_model, "glm-5");
        assert_eq!(
            settings.tiers[1].effective_model,
            willdeep_core::WorkerTier::Advanced.default_hosted_model()
        );
        assert_eq!(
            settings.tiers[2].effective_model,
            willdeep_core::WorkerTier::Expert.default_hosted_model()
        );
        // 专家档要票据这件事必须一路带到界面上。
        assert!(settings.tiers[2].requires_admission);
        assert!(!settings.tiers[0].requires_admission);
    }

    /// 改了档位的窗口就不再是「跟随默认预算」，否则保存之后会弹回默认值。
    #[test]
    fn nudging_a_tier_budget_marks_it_as_an_override() {
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
            profiles: Vec::new(),
            tiers: tier_fixture(),
        };
        // 没有职责行时，索引 1 就是第一个档位。
        assert_eq!(row_at(&settings, 1), Row::Tier(0));
        let before = settings.tiers[0].context_window;
        cycle_context_window(&mut settings, 1, 1);
        assert_ne!(settings.tiers[0].context_window, before);
        assert!(!settings.tiers[0].automatic);
        // automatic 的档不写回 context_window，改过的要写。
        let update = settings.to_update();
        let tiers = update.tiers.expect("tiers");
        assert_eq!(
            tiers[0].context_window,
            Some(settings.tiers[0].context_window)
        );
        assert_eq!(tiers[1].context_window, None);
    }

    /// 光标必须和它正在编辑的那行字在同一行。
    ///
    /// 此前光标行写死成「弹层倒数第二行」，而弹层高度是固定的 22 行——工种表
    /// 填不满时，光标掉进内容下方的空白里：屏幕上字在这儿、光标在那儿，
    /// 删掉几个字符也不知道删在哪。断言不写死行号，直接比对「`模型:` 渲染在
    /// 哪一行」和「光标在哪一行」。
    #[test]
    fn routing_editor_cursor_sits_on_the_line_it_edits() {
        let settings = crate::model_routing::ModelRoutingSettings {
            revision: "r1".to_owned(),
            default_provider: "some-im".to_owned(),
            active_provider_override: None,
            root_model: "deepseek-v4-flash".to_owned(),
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
                provider_profile: None,
                model: None,
                context_window: 32_768,
                automatic: true,
                effective_provider: "some-im".to_owned(),
                effective_model: "someim-32b-scout".to_owned(),
                recommended_model: None,
            }],
            // 带上档位那一段：弹层高度此前写死 22 行，加了这三行之后正好被裁掉，
            // 光标又会掉回内容外面。这条测试顺带守住高度跟着行数走。
            tiers: tier_fixture(),
        };
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.open_routing_settings(settings);
        // Enter 进入主模型的编辑态。
        app.handle_routing_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // 屏幕远高于弹层内容，正是此前会错位的形状。
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_routing_settings(frame, &mut app))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let editor_row = (0..buffer.area.height)
            .find(|row| {
                // 双宽字符在缓冲区里占两格，第二格是空白；比对前先把空格抹掉。
                (0..buffer.area.width)
                    .map(|column| buffer[(column, *row)].symbol())
                    .collect::<String>()
                    .replace(' ', "")
                    .contains("模型:")
            })
            .expect("编辑行必须渲染出来");

        let cursor = terminal.get_cursor_position().expect("光标位置");
        assert_eq!(
            cursor.y, editor_row,
            "光标落在第 {} 行，而 `模型:` 渲染在第 {editor_row} 行",
            cursor.y
        );
        // 列也要紧跟在文字后面：`模型: ` 占 6 列，模型名 17 列。
        let popup_left = buffer.area.width.saturating_sub(112) / 2;
        assert_eq!(cursor.x, popup_left + 1 + 6 + 17);
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
