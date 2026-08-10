use super::*;

impl App {
    pub(super) fn attention_items(&self) -> Vec<AttentionItem> {
        let mut items = Vec::new();
        if let Some((description, _, _)) = &self.approval {
            items.push(AttentionItem::approval(description.clone()));
        }
        if let Some(dialog) = &self.question {
            items.push(AttentionItem::question(dialog.request.question.clone()));
        }
        items.extend(
            self.background_tasks
                .iter()
                .map(AttentionItem::from_background),
        );
        if !self.running {
            items.extend(self.workspace_attention.iter().cloned());
        }
        items.extend(self.runtime_attention.iter().cloned());
        items.retain(|item| !self.attention_read.contains(&item.id));
        sort_attention_items(&mut items);
        items
    }

    pub(super) fn attention_move(&mut self, delta: isize) {
        let count = self.attention_items().len();
        if count == 0 {
            self.attention_selected = 0;
            return;
        }
        self.attention_selected = if delta < 0 {
            self.attention_selected.checked_sub(1).unwrap_or(count - 1)
        } else {
            (self.attention_selected + 1) % count
        };
        self.sidebar_manual_scroll = false;
    }

    pub(super) fn runtime_agent_move(&mut self, delta: isize) {
        let count = self.runtime_agents.len().min(5);
        if count == 0 {
            self.runtime_agent_selected = 0;
            return;
        }
        self.runtime_agent_selected = if delta < 0 {
            self.runtime_agent_selected
                .checked_sub(1)
                .unwrap_or(count - 1)
        } else {
            (self.runtime_agent_selected + 1) % count
        };
        self.sidebar_manual_scroll = false;
    }

    pub(super) fn selected_runtime_agent(&self) -> Option<crate::daemon::tui_bridge::RemoteAgent> {
        self.runtime_agents
            .get(self.runtime_agent_selected)
            .cloned()
    }
}

pub(super) fn render_sidebar(f: &mut ratatui::Frame<'_>, app: &mut App, area: Rect) {
    let relay = if app.mobile_gateway.is_some() {
        app.language.text("已连接", "connected", "接続済み")
    } else {
        app.language.text("关闭", "off", "オフ")
    };
    let attention = app.attention_items();
    app.attention_selected = app
        .attention_selected
        .min(attention.len().saturating_sub(1));
    let mut agents = vec![StatusRollup::leaf(
        RuntimeScopeKind::Agent,
        "main",
        if app.running {
            RuntimeStatus::Working
        } else {
            RuntimeStatus::Idle
        },
    )];
    agents.extend(
        attention
            .iter()
            .map(|item| StatusRollup::leaf(RuntimeScopeKind::Agent, item.id.clone(), item.status)),
    );
    let session = StatusRollup::group(
        RuntimeScopeKind::Session,
        "current",
        RuntimeStatus::Idle,
        agents,
    );
    let workspace = StatusRollup::group(
        RuntimeScopeKind::Workspace,
        "current",
        RuntimeStatus::Idle,
        vec![session],
    );
    let titles = [
        app.language.text("工作区", "Workspace", "ワークスペース"),
        app.language.text("需要关注", "Attention Inbox", "要確認"),
        app.language.text("运行状态", "Runtime", "実行状態"),
        app.language
            .text("移动中继", "Mobile relay", "モバイルリレー"),
    ];
    let mut lines = Vec::new();
    let mut headers = [0usize; 4];
    let mut logical_hits = Vec::new();
    let mut selected_attention_row = None;
    for (section, title) in titles.into_iter().enumerate() {
        headers[section] = lines.len();
        logical_hits.push((lines.len(), SidebarHit::Section(section)));
        let selected = app.focus == FocusPane::Sidebar && app.sidebar_selected == section;
        let marker = if app.sidebar_expanded[section] {
            "▾"
        } else {
            "▸"
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        };
        lines.push(Line::styled(format!("{marker} {title}"), style));
        if !app.sidebar_expanded[section] {
            continue;
        }
        match section {
            0 => lines.extend(
                app.workspace_status
                    .lines()
                    .map(|line| Line::raw(format!("  {line}"))),
            ),
            1 if attention.is_empty() => lines.push(Line::raw(format!(
                "  {}",
                app.language
                    .text("目前无需处理", "Nothing needs attention", "対応事項なし")
            ))),
            1 => {
                for (group, heading) in [
                    (
                        AttentionSection::NeedsYou,
                        app.language.text("需要你处理", "Needs you", "対応が必要"),
                    ),
                    (
                        AttentionSection::Working,
                        app.language.text("正在工作", "Working", "作業中"),
                    ),
                    (
                        AttentionSection::Recent,
                        app.language.text("最近完成", "Recent", "最近完了"),
                    ),
                ] {
                    let grouped = attention
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| item.status.section() == Some(group))
                        .collect::<Vec<_>>();
                    if grouped.is_empty() {
                        continue;
                    }
                    lines.push(Line::styled(
                        format!("  {heading} · {}", grouped.len()),
                        Style::default().fg(Color::Gray),
                    ));
                    for (item_index, item) in grouped {
                        if app.sidebar_selected == 1 && app.attention_selected == item_index {
                            selected_attention_row = Some(lines.len());
                        }
                        logical_hits.push((lines.len(), SidebarHit::Attention(item_index)));
                        let elapsed = item
                            .elapsed_millis
                            .map(|millis| format!(" · {:.1}s", millis as f64 / 1000.0))
                            .unwrap_or_default();
                        lines.push(Line::styled(
                            format!(
                                "    {} · {}{elapsed}",
                                attention_source_label(item.source, app.language),
                                runtime_status_label(item.status, app.language),
                            ),
                            if app.focus == FocusPane::Sidebar
                                && app.sidebar_selected == 1
                                && app.attention_selected == item_index
                            {
                                attention_style(item.status)
                                    .bg(Color::DarkGray)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                attention_style(item.status)
                            },
                        ));
                        lines.push(Line::styled(
                            format!("      {}", item.title),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
            }
            2 => {
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language.text("智能体", "Agent", "エージェント"),
                    runtime_status_label(workspace.status, app.language)
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}/{}",
                    app.language
                        .text("工具完成", "Tools complete", "ツール完了"),
                    app.tools.completed,
                    app.tools.requested
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language.text("失败", "Failed", "失敗"),
                    app.tools.failed
                )));
                if !app.runtime_agents.is_empty() {
                    lines.push(Line::styled(
                        format!(
                            "  {} · {}",
                            app.language.text(
                                "Runtime 智能体",
                                "Runtime agents",
                                "Runtime エージェント"
                            ),
                            app.runtime_agents.len()
                        ),
                        Style::default().fg(Color::Gray),
                    ));
                    app.runtime_agent_selected = app
                        .runtime_agent_selected
                        .min(app.runtime_agents.len().min(5).saturating_sub(1));
                    for (agent_index, agent) in app.runtime_agents.iter().take(5).enumerate() {
                        let short = agent.id.to_string();
                        let short = short.get(..6).unwrap_or(&short);
                        let profile = agent.profile.as_deref().unwrap_or("root");
                        let label = agent.label.as_deref().unwrap_or(profile);
                        let mode = if agent.background { " bg" } else { "" };
                        let prefix = if agent.parent_id.is_some() {
                            "      ↳"
                        } else {
                            "    "
                        };
                        let agent_style = if app.focus == FocusPane::Sidebar
                            && app.sidebar_selected == 2
                            && app.runtime_agent_selected == agent_index
                        {
                            attention_style(agent.status).add_modifier(Modifier::REVERSED)
                        } else {
                            attention_style(agent.status)
                        };
                        lines.push(Line::styled(
                            format!(
                                "{prefix} {short} · {profile}{mode} · {}",
                                runtime_status_label(agent.status, app.language)
                            ),
                            agent_style,
                        ));
                        lines.push(Line::styled(
                            format!(
                                "        {label} · T{} · {} · {}",
                                agent.current_turn,
                                agent.current_tool.as_deref().unwrap_or("-"),
                                agent
                                    .total_tokens
                                    .map_or_else(|| "-".to_owned(), |tokens| format!("{tokens}t"))
                            ),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
            }
            3 => {
                lines.push(Line::raw(format!(
                    "  {}: {relay}",
                    app.language.text("中继", "Relay", "リレー")
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language
                        .text("手机队列", "Phone queue", "モバイルキュー"),
                    app.mobile_queue.len()
                )));
            }
            _ => {}
        }
        lines.push(Line::raw(""));
    }
    let viewport = area.height.saturating_sub(2).max(1) as usize;
    let selected_row = if app.sidebar_selected == 1 {
        selected_attention_row.unwrap_or(headers[1])
    } else {
        headers[app.sidebar_selected]
    };
    if !app.sidebar_manual_scroll {
        if selected_row < app.sidebar_scroll {
            app.sidebar_scroll = selected_row;
        } else if selected_row >= app.sidebar_scroll + viewport {
            app.sidebar_scroll = selected_row.saturating_sub(viewport - 1);
        }
    }
    let max_scroll = lines.len().saturating_sub(viewport);
    app.sidebar_scroll = app.sidebar_scroll.min(max_scroll);
    app.sidebar_hits = logical_hits
        .into_iter()
        .filter_map(|(row, hit)| {
            (row >= app.sidebar_scroll && row < app.sidebar_scroll + viewport)
                .then_some((area.y + 1 + (row - app.sidebar_scroll) as u16, hit))
        })
        .collect();
    let border = if app.focus == FocusPane::Sidebar {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(app.language.text(
                        "状态 · Ctrl+W 聚焦 · Ctrl+B 隐藏",
                        "Status · Ctrl+W focus · Ctrl+B hide",
                        "状態 · Ctrl+W フォーカス · Ctrl+B 非表示",
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            )
            .scroll((app.sidebar_scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}
