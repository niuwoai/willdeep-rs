use super::*;

pub(super) fn render_attention_detail(f: &mut ratatui::Frame<'_>, app: &mut App) {
    app.attention_diff_rect = Rect::default();
    app.attention_allow_rect = Rect::default();
    app.attention_deny_rect = Rect::default();
    app.attention_detail_rect = Rect::default();
    let Some(detail) = &app.attention_detail else {
        return;
    };
    let actionable = app.selected_remote_gate().is_some();
    let diff_review = detail.source == AttentionSource::DiffReview;
    // 失败详情挂在正文后面：一条失败的 Inbox 项如果只写「Status: Failed」，
    // 打开详情等于什么都没说。
    let diagnostics = app
        .attention_diagnostics
        .as_ref()
        .filter(|loaded| loaded.item_id == detail.id)
        .map(|loaded| format!("\n\n{loaded}", loaded = loaded.text))
        .unwrap_or_default();
    let content = format!(
        "{} · {}\n\n{}\n\n{}{diagnostics}{}",
        attention_source_label(detail.source, app.language),
        runtime_status_label(detail.status, app.language),
        detail.title,
        detail.detail,
        if diff_review {
            "\n\n "
        } else if actionable {
            app.language.text(
                "\n\nEnter 打开审批",
                "\n\nEnter opens approval",
                "\n\nEnter で承認を開く",
            )
        } else {
            ""
        }
    );
    let popup = centered_rect(
        f.area().width.min(82),
        (visual_lines(&content, f.area().width.min(80) as usize) as u16 + 2)
            .min(f.area().height)
            .max(1),
        f.area(),
    );
    app.attention_detail_rect = popup;
    f.render_widget(Clear, popup);
    let title = if diff_review {
        app.language.text(
            "Diff 审批 · D/Enter 查看 · Y 通过 · N 拒绝 · Esc 关闭",
            "Diff review · D/Enter view · Y accept · N reject · Esc closes",
            "Diff レビュー · D/Enter 表示 · Y 承認 · N 拒否 · Esc 閉じる",
        )
    } else if actionable {
        app.language.text(
            "Inbox 详情 · Enter 审批 · M 忽略 · Esc 关闭",
            "Inbox detail · Enter approve · M dismiss · Esc closes",
            "Inbox 詳細 · Enter 承認 · M 削除 · Esc 閉じる",
        )
    } else {
        // Many Inbox entries have no action left but "stop showing me this" —
        // a task that failed days ago, for instance. Advertise the key.
        app.language.text(
            "Inbox 详情 · M 忽略 · Esc 关闭",
            "Inbox detail · M dismiss · Esc closes",
            "Inbox 詳細 · M 削除 · Esc 閉じる",
        )
    };
    f.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(attention_style(detail.status)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
    if diff_review && popup.height >= 3 {
        let labels = [
            app.language
                .text("[D 查看 Diff]", "[D View Diff]", "[D Diff 表示]"),
            app.language.text("[Y 通过]", "[Y Accept]", "[Y 承認]"),
            app.language.text("[N 拒绝]", "[N Reject]", "[N 拒否]"),
        ];
        let widths = labels.map(|label| label.chars().count() as u16 + 1);
        let total = widths
            .iter()
            .sum::<u16>()
            .min(popup.width.saturating_sub(2));
        let mut x = popup.x + popup.width.saturating_sub(total + 1);
        let y = popup.y + popup.height.saturating_sub(2);
        for (index, (label, width)) in labels.into_iter().zip(widths).enumerate() {
            let rect = Rect::new(x, y, width.min(popup.right().saturating_sub(x)), 1);
            match index {
                0 => app.attention_diff_rect = rect,
                1 => app.attention_allow_rect = rect,
                _ => app.attention_deny_rect = rect,
            }
            let color = match index {
                0 => Color::LightCyan,
                1 => Color::Green,
                _ => Color::Red,
            };
            f.render_widget(
                Paragraph::new(label).style(Style::default().fg(color)),
                rect,
            );
            x = x.saturating_add(width);
        }
    }
}

impl App {
    pub(super) fn diff_attention_action_at(
        &self,
        column: u16,
        row: u16,
    ) -> Option<DiffAttentionAction> {
        let point = (column, row).into();
        if self.attention_diff_rect.contains(point) {
            Some(DiffAttentionAction::Open)
        } else if self.attention_allow_rect.contains(point) {
            Some(DiffAttentionAction::Accept)
        } else if self.attention_deny_rect.contains(point) {
            Some(DiffAttentionAction::Reject)
        } else {
            None
        }
    }

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
                .filter(|task| background_task_visible(task))
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

    /// 侧栏是活动面板，不是归档：只保留在跑的 Agent 和刚结束的，
    /// 且丢掉没有任何轮次的已结束根 Agent。与 Web 侧栏同一套规则。
    pub(super) fn sidebar_runtime_agents(&self) -> Vec<&crate::daemon::tui_bridge::RemoteAgent> {
        let now = now_seconds();
        self.runtime_agents
            .iter()
            .filter(|agent| sidebar_runtime_agent_visible(agent, now))
            .collect()
    }

    pub(super) fn runtime_agent_move(&mut self, delta: isize) {
        let count = self.sidebar_runtime_agents().len().min(5);
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
        self.sidebar_runtime_agents()
            .get(self.runtime_agent_selected)
            .map(|agent| (*agent).clone())
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
                        sidebar_group_heading_style(),
                    ));
                    for (item_index, item) in grouped {
                        if app.sidebar_selected == 1 && app.attention_selected == item_index {
                            selected_attention_row = Some(lines.len());
                        }
                        logical_hits.push((lines.len(), SidebarHit::Attention(item_index)));
                        let elapsed = item
                            .elapsed_millis
                            .map(|millis| {
                                format!(" · {}", format_elapsed_span(millis as f32 / 1000.0, 1))
                            })
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
                // A stale Runtime executes tools with old policy while this
                // binary reports its own version everywhere else. Keep the
                // mismatch on screen until it is fixed.
                if let Some(stale) = app.stale_runtime_version() {
                    lines.push(Line::styled(
                        format!(
                            "  ⚠ {} {} ≠ {} {}",
                            app.language.text("运行时", "Runtime", "ランタイム"),
                            stale,
                            app.language.text("客户端", "client", "クライアント"),
                            willdeep_core::VERSION
                        ),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                    lines.push(Line::styled(
                        format!(
                            "  {}",
                            app.language.text(
                                "工具按旧版策略执行 · 请运行 willdeep daemon upgrade",
                                "Tools run old policy · run willdeep daemon upgrade",
                                "ツールは旧ポリシーで実行 · willdeep daemon upgrade を実行",
                            )
                        ),
                        Style::default().fg(Color::Yellow),
                    ));
                }
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
                logical_hits.push((lines.len(), SidebarHit::NewAgent));
                lines.push(Line::styled(
                    format!(
                        "  {}",
                        app.language.text(
                            "[N 新建只读 Agent]",
                            "[N New read-only Agent]",
                            "[N 読み取り専用 Agent を作成]"
                        )
                    ),
                    Style::default().fg(Color::LightCyan),
                ));
                let sidebar_agents = app
                    .sidebar_runtime_agents()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                let hidden_agents = app.runtime_agents.len() - sidebar_agents.len();
                if !sidebar_agents.is_empty() {
                    let hidden_note = if hidden_agents > 0 {
                        format!(
                            " (+{hidden_agents} {})",
                            app.language.text("已结束", "finished", "終了済み")
                        )
                    } else {
                        String::new()
                    };
                    lines.push(Line::styled(
                        format!(
                            "  {} · {}{hidden_note}",
                            app.language.text(
                                "Runtime 智能体",
                                "Runtime agents",
                                "Runtime エージェント"
                            ),
                            sidebar_agents.len()
                        ),
                        sidebar_group_heading_style(),
                    ));
                    app.runtime_agent_selected = app
                        .runtime_agent_selected
                        .min(sidebar_agents.len().min(5).saturating_sub(1));
                    for (agent_index, agent) in sidebar_agents.iter().take(5).enumerate() {
                        let short = agent.id.to_string();
                        let short = short.get(..6).unwrap_or(&short);
                        let profile = agent.profile.as_deref().unwrap_or("root");
                        let label = agent.label.as_deref().unwrap_or(profile);
                        let mode = if agent.background { " bg" } else { "" };
                        let duration = agent_duration_label(agent, app.language);
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
                                "        {label} · {} · {duration}",
                                agent.model.as_deref().unwrap_or("-")
                            ),
                            Style::default().fg(Color::DarkGray),
                        ));
                        lines.push(Line::styled(
                            format!(
                                "        T{}/{} · {} · {}/{} · {}s",
                                agent.current_turn,
                                agent
                                    .max_turns
                                    .map_or_else(|| "-".to_owned(), |turns| turns.to_string()),
                                agent.current_tool.as_deref().unwrap_or("-"),
                                agent
                                    .total_tokens
                                    .map_or_else(|| "-".to_owned(), |tokens| format!("{tokens}t")),
                                agent
                                    .token_budget
                                    .map_or_else(|| "-".to_owned(), |tokens| format!("{tokens}t")),
                                agent
                                    .timeout_seconds
                                    .map_or_else(|| "-".to_owned(), |seconds| seconds.to_string())
                            ),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                if !app.runtime_tools.is_empty() || !app.runtime_artifacts.is_empty() {
                    let running = app
                        .runtime_tools
                        .iter()
                        .filter(|tool| {
                            tool.status == willdeep_runtime_protocol::ToolStatus::Running
                        })
                        .count();
                    lines.push(Line::styled(
                        format!(
                            "  {}: {} · {}: {} · {}: {}",
                            app.language.text("工具", "Tools", "ツール"),
                            app.runtime_tools.len(),
                            app.language.text("运行中", "Running", "実行中"),
                            running,
                            app.language.text("产物", "Artifacts", "成果物"),
                            app.runtime_artifacts.len()
                        ),
                        Style::default().fg(Color::Gray),
                    ));
                    if let Some(tool) = app.runtime_tools.first() {
                        lines.push(Line::styled(
                            format!("    {} · {:?}", tool.name, tool.status),
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
                // 键盘和手机现在共用一条队列，标签不能再只说「手机」。
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language
                        .text("待发队列", "Queued prompts", "送信待ちキュー"),
                    app.queued_prompts.len()
                )));
            }
            _ => {}
        }
        lines.push(Line::raw(""));
    }
    let viewport = area.height.saturating_sub(3).max(1) as usize;
    let selected_row = if app.sidebar_selected == 1 {
        selected_attention_row.unwrap_or(headers[1])
    } else {
        headers[app.sidebar_selected]
    };
    if !app.sidebar_manual_scroll {
        if selected_row < app.sidebar_scroll {
            app.sidebar_scroll = selected_row;
        } else if selected_row >= app.sidebar_scroll.saturating_add(viewport) {
            app.sidebar_scroll = selected_row.saturating_sub(viewport - 1);
        }
    }
    let max_scroll = lines.len().saturating_sub(viewport);
    app.sidebar_scroll = app.sidebar_scroll.min(max_scroll);
    app.sidebar_hits = logical_hits
        .into_iter()
        .filter_map(|(row, hit)| {
            let visible_end = app.sidebar_scroll.saturating_add(viewport);
            if row < app.sidebar_scroll || row >= visible_end {
                return None;
            }
            let offset = row
                .saturating_sub(app.sidebar_scroll)
                .min(u16::MAX as usize) as u16;
            Some((area.y.saturating_add(1).saturating_add(offset), hit))
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
                    .padding(Padding::new(0, 0, 0, 1))
                    .border_style(Style::default().fg(border)),
            )
            .scroll((app.sidebar_scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
    if area.width > 4 && area.height > 2 {
        f.render_widget(
            Paragraph::new(format!("WillDeep v{}", willdeep_core::VERSION))
                .alignment(Alignment::Right)
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(area.x + 1, area.y + area.height - 2, area.width - 2, 1),
        );
    }
}

/// 分组标题继承终端的默认前景色，避免 ANSI Gray 在浅色配色中与背景融为一体。
/// 加粗保留它与普通内容的视觉层级，同时兼容浅色、深色和自定义终端主题。
pub(super) fn sidebar_group_heading_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// 已结束超过这个时长的 Agent 退出侧栏，去详情/历史里找。
const RECENTLY_FINISHED_SECONDS: u64 = 300;

/// 顺利收尾的后台任务在 Inbox 里停留这么久就自动回收。
const SETTLED_TASK_RETENTION_MILLIS: u64 = 60_000;

/// 失败/超时/被杀的任务留得久得多——那些还需要人处理——但也不是永远。
/// 一天前失败的后台命令早已不是"待处理"，只是噪音：它会把真正需要
/// 关注的条目挤出视野。要复盘去任务详情和历史里找。
const FAILED_TASK_RETENTION_MILLIS: u64 = 24 * 60 * 60 * 1_000;

pub(super) fn background_task_visible(task: &willdeep_core::BackgroundTaskSnapshot) -> bool {
    let retention = match task.status {
        // 运行中的任务没有终态时间戳，永远可见。
        willdeep_core::BackgroundTaskStatus::Running => return true,
        willdeep_core::BackgroundTaskStatus::Completed => SETTLED_TASK_RETENTION_MILLIS,
        _ => FAILED_TASK_RETENTION_MILLIS,
    };
    task.settled_millis
        .is_none_or(|settled| settled < retention)
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn agent_is_live(agent: &crate::daemon::tui_bridge::RemoteAgent) -> bool {
    matches!(
        agent.status,
        willdeep_core::RuntimeStatus::Idle
            | willdeep_core::RuntimeStatus::Working
            | willdeep_core::RuntimeStatus::Blocked
            | willdeep_core::RuntimeStatus::WaitingApproval
            | willdeep_core::RuntimeStatus::WaitingAnswer
    )
}

fn sidebar_runtime_agent_visible(agent: &crate::daemon::tui_bridge::RemoteAgent, now: u64) -> bool {
    if agent_is_live(agent) {
        return true;
    }
    // Runtime 没记下结束时间的一律当活着，宁可多显示也不隐藏可能还在跑的 Agent。
    let Some(completed_at) = agent.completed_at else {
        return true;
    };
    if agent.parent_id.is_none() && agent.current_turn == 0 {
        return false;
    }
    now.saturating_sub(completed_at) < RECENTLY_FINISHED_SECONDS
}

fn agent_elapsed_seconds(agent: &crate::daemon::tui_bridge::RemoteAgent) -> u64 {
    let end = agent.completed_at.unwrap_or_else(now_seconds);
    end.saturating_sub(agent.created_at)
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// 运行中的是「已运行」，结束的是「耗时」——同一个数字混着显示会让人以为进程还活着。
fn agent_duration_label(
    agent: &crate::daemon::tui_bridge::RemoteAgent,
    language: Language,
) -> String {
    let label = if agent_is_live(agent) {
        language.text("已运行", "running for", "実行時間")
    } else {
        language.text("耗时", "took", "所要時間")
    };
    format!("{label} {}", format_duration(agent_elapsed_seconds(agent)))
}
