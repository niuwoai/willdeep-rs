use super::*;

pub(super) fn render_agent_overlays(frame: &mut ratatui::Frame<'_>, app: &App) {
    if let Some(agent) = &app.agent_detail {
        render_agent_detail(frame, app, agent);
    }
    if let Some(review) = &app.worktree_review {
        render_worktree_review(frame, app, review);
    }
}

fn render_agent_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    agent: &crate::daemon::tui_bridge::RemoteAgent,
) {
    let content = agent_detail_content(app, agent);
    let popup = centered_rect(
        frame.area().width.min(92),
        (visual_lines(&content, frame.area().width.min(90) as usize) as u16 + 2)
            .min(frame.area().height)
            .max(1),
        frame.area(),
    );
    frame.render_widget(Clear, popup);
    let title = if agent.dedicated_worktree {
        app.language.text(
            "Agent 详情 · W 审查 Worktree · Esc 关闭",
            "Agent details · W review Worktree · Esc close",
            "Agent 詳細 · W Worktree レビュー · Esc 閉じる",
        )
    } else {
        app.language.text(
            "Agent 详情 · Esc 关闭",
            "Agent details · Esc close",
            "Agent 詳細 · Esc で閉じる",
        )
    };
    frame.render_widget(
        Paragraph::new(content)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::LightCyan)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
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
