use super::*;

pub(super) fn render_agent_overlays(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    app.agent_detail_action_rects.clear();
    if let Some(agent) = app.agent_detail.clone() {
        render_agent_detail(frame, app, &agent);
    }
    if let Some(review) = app.worktree_review.clone() {
        app.agent_detail_action_rects.clear();
        render_worktree_review(frame, app, &review);
    }
}

fn render_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &mut App,
    agent: &crate::daemon::tui_bridge::RemoteAgent,
) {
    let content = agent_detail_content(app, agent);
    let popup = centered_rect(
        frame.area().width.min(92),
        (visual_lines(&content, frame.area().width.min(90) as usize) as u16 + 3)
            .min(frame.area().height)
            .max(1),
        frame.area(),
    );
    let inner_width = popup.width.saturating_sub(2).max(1) as usize;
    let inner_height = popup.height.saturating_sub(3) as usize;
    let scroll =
        agent_detail_scroll_offset(&content, inner_width, inner_height, app.agent_detail_scroll);
    frame.render_widget(Clear, popup);
    let title = app.language.text(
        "Agent 详情 · ↑↓/PgUp/PgDn 滚动 · Esc 关闭",
        "Agent details · ↑↓/PgUp/PgDn scroll · Esc close",
        "Agent 詳細 · ↑↓/PgUp/PgDn スクロール · Esc 閉じる",
    );
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .padding(Padding::new(0, 0, 0, 1))
                    .border_style(Style::default().fg(Color::LightCyan)),
            )
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        popup,
    );
    render_agent_actions(frame, app, agent, popup);
}

fn render_agent_actions(
    frame: &mut ratatui::Frame<'_>,
    app: &mut App,
    agent: &crate::daemon::tui_bridge::RemoteAgent,
    popup: Rect,
) {
    let mut actions = Vec::new();
    if agent.background && agent.status == willdeep_core::RuntimeStatus::Working {
        actions.push((
            AgentDetailAction::Instruct,
            app.language.text("[I 补充]", "[I Instruct]", "[I 指示]"),
            Color::LightCyan,
        ));
        actions.push((
            AgentDetailAction::Stop,
            app.language.text("[K 停止]", "[K Stop]", "[K 停止]"),
            Color::Red,
        ));
    }
    if agent.background
        && matches!(
            agent.status,
            willdeep_core::RuntimeStatus::Blocked
                | willdeep_core::RuntimeStatus::Failed
                | willdeep_core::RuntimeStatus::Done
                | willdeep_core::RuntimeStatus::Cancelled
        )
    {
        actions.push((
            AgentDetailAction::Retry,
            app.language.text("[R 重试]", "[R Retry]", "[R 再試行]"),
            Color::Green,
        ));
        actions.push((
            AgentDetailAction::RetryWithModel,
            app.language
                .text("[M 换模型]", "[M Change model]", "[M モデル変更]"),
            Color::LightMagenta,
        ));
    }
    if agent.dedicated_worktree {
        actions.push((
            AgentDetailAction::ReviewWorktree,
            app.language
                .text("[W 查看 Diff]", "[W View Diff]", "[W Diff 表示]"),
            Color::Yellow,
        ));
    }
    let mut x = popup.x.saturating_add(1);
    let right = popup.right().saturating_sub(1);
    let y = popup.bottom().saturating_sub(2);
    for (action, label, color) in actions {
        let width = UnicodeWidthStr::width(label).saturating_add(1) as u16;
        if x >= right {
            break;
        }
        let rect = Rect::new(x, y, width.min(right.saturating_sub(x)), 1);
        if rect.width == 0 {
            break;
        }
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(color)),
            rect,
        );
        app.agent_detail_action_rects.push((rect, action));
        x = x.saturating_add(width);
    }
}

impl App {
    pub(super) fn agent_detail_action_at(
        &self,
        column: u16,
        row: u16,
    ) -> Option<AgentDetailAction> {
        let point = (column, row).into();
        self.agent_detail_action_rects
            .iter()
            .find_map(|(rect, action)| rect.contains(point).then_some(*action))
    }
}

pub(super) fn agent_detail_scroll_offset(
    content: &str,
    width: usize,
    height: usize,
    requested: usize,
) -> u16 {
    let max_scroll = visual_lines(content, width.max(1)).saturating_sub(height);
    requested.min(max_scroll).min(u16::MAX as usize) as u16
}

pub(super) fn agent_detail_content(
    app: &App,
    agent: &crate::daemon::tui_bridge::RemoteAgent,
) -> String {
    let mut lines = vec![format!(
        "{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {:?}\n{}: {}/{}\n{}: {}/{}\n{}: {}s\n{}: {}\n{}: {}\n{}: {}",
        app.language.text("Agent", "Agent", "エージェント"),
        agent.id,
        app.language.text("父级", "Parent", "親"),
        agent
            .parent_id
            .map_or_else(|| "—".to_owned(), |id| id.to_string()),
        app.language.text("Profile", "Profile", "プロファイル"),
        agent.profile.as_deref().unwrap_or("—"),
        app.language.text("模型", "Model", "モデル"),
        agent.model.as_deref().unwrap_or("—"),
        app.language.text("标签", "Label", "ラベル"),
        agent.label.as_deref().unwrap_or("—"),
        app.language.text("状态", "Status", "状態"),
        agent.status,
        app.language.text("轮次", "Turns", "ターン"),
        agent.current_turn,
        agent
            .max_turns
            .map_or_else(|| "—".to_owned(), |value| value.to_string()),
        app.language.text("Token", "Tokens", "トークン"),
        agent
            .total_tokens
            .map_or_else(|| "—".to_owned(), |value| value.to_string()),
        agent
            .token_budget
            .map_or_else(|| "—".to_owned(), |value| value.to_string()),
        app.language.text("时限", "Timeout", "タイムアウト"),
        agent
            .timeout_seconds
            .map_or_else(|| "—".to_owned(), |value| value.to_string()),
        app.language
            .text("当前工具", "Current tool", "現在のツール"),
        agent.current_tool.as_deref().unwrap_or("—"),
        app.language.text("Worktree", "Worktree", "Worktree"),
        agent.workspace.display(),
        app.language.text("分支", "Branch", "ブランチ"),
        agent.worktree_branch.as_deref().unwrap_or("—")
    )];
    let tools = app
        .runtime_tools
        .iter()
        .filter(|tool| tool.agent_id == agent.id)
        .take(8)
        .collect::<Vec<_>>();
    lines.push(format!(
        "\n{} ({})",
        app.language
            .text("工具时间线", "Tool timeline", "ツールタイムライン"),
        tools.len()
    ));
    if tools.is_empty() {
        lines.push(
            app.language
                .text("  尚无工具调用", "  No tool calls", "  ツール呼び出しなし")
                .to_owned(),
        );
    } else {
        lines.extend(tools.into_iter().map(|tool| {
            let elapsed = tool.completed_at_ms.map(|completed_at| {
                format!("{}ms", completed_at.saturating_sub(tool.started_at_ms))
            });
            format!(
                "  {:?} · {} · {}",
                tool.status,
                tool.name,
                elapsed.as_deref().unwrap_or_else(|| app.language.text(
                    "运行中",
                    "running",
                    "実行中"
                ))
            )
        }));
    }
    let artifacts = app
        .runtime_artifacts
        .iter()
        .filter(|artifact| artifact.agent_id == agent.id)
        .take(8)
        .collect::<Vec<_>>();
    lines.push(format!(
        "\n{} ({})",
        app.language
            .text("Diff 摘要", "Diff summary", "Diff サマリー"),
        artifacts.len()
    ));
    if artifacts.is_empty() {
        lines.push(
            app.language
                .text(
                    "  尚无工作区变更",
                    "  No workspace changes",
                    "  ワークスペース変更なし",
                )
                .to_owned(),
        );
    } else {
        lines.extend(
            artifacts
                .into_iter()
                .map(|artifact| format!("  {} · {}", artifact.item_count, artifact.title)),
        );
    }
    lines.push(format!(
        "\n{}\n{}",
        app.language
            .text("结果报告", "Result report", "結果レポート"),
        agent.report.as_deref().unwrap_or_else(|| app.language.text(
            "尚无结果报告",
            "No result report yet",
            "結果レポートはまだありません"
        ))
    ));
    lines.join("\n")
}

fn render_worktree_review(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    review: &crate::daemon::WorktreeReview,
) {
    let mut lines = vec![
        format!("Review: {}", review.id),
        format!("Branch: {}", review.branch),
        format!("Root: {}", review.root_workspace.display()),
        format!("Worktree: {}", review.worktree.display()),
        format!(
            "{}: {}  +{} -{}  {} bytes",
            app.language.text("变更", "Changes", "変更"),
            review.files.len(),
            review.additions,
            review.deletions,
            review.patch_bytes
        ),
    ];
    for blocker in &review.blockers {
        lines.push(format!(
            "{}: {blocker}",
            app.language.text("阻断", "Blocked", "ブロック")
        ));
    }
    for file in &review.files {
        lines.push(format!("{:?}  {}", file.kind, file.path));
    }
    let content = lines.join("\n");
    let popup = centered_rect(
        frame.area().width.min(100),
        (visual_lines(&content, frame.area().width.min(98) as usize) as u16 + 2)
            .min(frame.area().height)
            .max(1),
        frame.area(),
    );
    frame.render_widget(Clear, popup);
    let title = if review.can_merge {
        app.language.text(
            "Worktree 审查 · M 批准合并 · Esc 关闭",
            "Worktree review · M approve merge · Esc close",
            "Worktree レビュー · M マージ承認 · Esc 閉じる",
        )
    } else {
        app.language.text(
            "Worktree 审查 · 当前不可合并 · Esc 关闭",
            "Worktree review · merge blocked · Esc close",
            "Worktree レビュー · マージ不可 · Esc 閉じる",
        )
    };
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(if review.can_merge {
                        Color::Green
                    } else {
                        Color::Red
                    })),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}
