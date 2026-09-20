use super::*;
use willdeep_core::conversation::{Plan, StepStatus};

const PLAN_PREFIX: &str = "Plan: ";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct PlanTranscript {
    pub plan: Plan,
    pub details: Vec<String>,
    pub expanded: bool,
}

fn plan_entry(card: &PlanTranscript) -> String {
    format!(
        "{PLAN_PREFIX}{}",
        serde_json::to_string(card).expect("plan serializes")
    )
}

pub(super) fn decode_plan_entry(entry: &str) -> Option<PlanTranscript> {
    serde_json::from_str(entry.strip_prefix(PLAN_PREFIX)?).ok()
}

pub(super) fn render_plan(plan: &Plan, width: usize) -> Vec<Line<'static>> {
    let ended = plan
        .steps
        .iter()
        .filter(|step| matches!(step.status, StepStatus::Done | StepStatus::Skipped))
        .count();
    let mut lines = vec![Line::styled(
        format!("┌ ▣ {ended}/{} · /plan", plan.steps.len()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if !plan.summary.is_empty() {
        lines.extend(render_assistant_markdown(
            &plan.summary,
            width.saturating_sub(2),
        ));
    }
    for step in &plan.steps {
        let (symbol, color) = match step.status {
            StepStatus::Pending => ("○", Color::DarkGray),
            StepStatus::InProgress => ("›", Color::Cyan),
            StepStatus::Done => ("✓", Color::Green),
            StepStatus::Skipped => ("–", Color::DarkGray),
            StepStatus::Failed => ("!", Color::Red),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("│ {symbol} "), Style::default().fg(color)),
            Span::raw(step.text.clone()),
        ]));
        if let Some(detail) = &step.detail {
            lines.push(Line::styled(
                format!("│   {detail}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    lines.push(Line::styled("└", Style::default().fg(Color::Cyan)));
    lines
}

/// Replace a plan in place for live replies, using the same parser as history.
pub(super) fn append_plan_reply(entries: &mut Vec<String>, reply: &str) -> bool {
    let latest = entries
        .iter()
        .rposition(|entry| decode_plan_entry(entry).is_some());
    let previous = latest.and_then(|index| decode_plan_entry(&entries[index]));
    let Some((plan, is_new)) = willdeep_core::conversation::parse_plan_reply(
        reply,
        previous.as_ref().map(|card| &card.plan),
    ) else {
        return false;
    };
    let mut card = if !is_new && let Some(previous) = previous {
        previous
    } else {
        PlanTranscript {
            plan: plan.clone(),
            details: Vec::new(),
            expanded: false,
        }
    };
    card.plan = plan;
    if !card.details.iter().any(|detail| detail == reply) {
        card.details.push(reply.to_owned());
    }
    if let Some(index) = latest.filter(|_| !is_new) {
        entries[index] = plan_entry(&card);
    } else {
        entries.push(plan_entry(&card));
    }
    true
}

pub(super) fn session_transcript(session: &Session, language: Language) -> Vec<String> {
    projected_transcript(&session.messages, session.current_plan.as_ref(), language)
}

pub(super) fn projected_transcript(
    messages: &[Message],
    plan: Option<&Plan>,
    language: Language,
) -> Vec<String> {
    willdeep_core::conversation::project(messages, plan)
        .into_iter()
        .flat_map(|item| {
            if let Some(plan) = item.plan {
                return vec![plan_entry(&PlanTranscript {
                    plan,
                    details: item.details,
                    expanded: false,
                })];
            }
            match item.role {
                "user" => vec![format!(
                    "You: {}{}",
                    item.content,
                    if item.attachment_count == 0 {
                        String::new()
                    } else {
                        format!(" [{} attachment(s)]", item.attachment_count)
                    }
                )],
                "system" => vec![format!(
                    "System: {}",
                    language.text(
                        "系统自动推进",
                        "Automatic system activity",
                        "システムの自動進行"
                    )
                )],
                _ => {
                    // 与实时路径同一套行：先正文（有的话），再逐条工具行。
                    let mut rows = Vec::with_capacity(1 + item.tools.len());
                    if !item.content.trim().is_empty() {
                        rows.push(format!("WillDeep: {}", item.content));
                    }
                    rows.extend(item.tools.iter().map(narration::replayed_tool_row));
                    rows
                }
            }
        })
        .collect()
}

pub(super) fn toggle_plan_details(entries: &mut [String]) {
    if let Some(entry) = entries
        .iter_mut()
        .rev()
        .find(|entry| decode_plan_entry(entry).is_some())
    {
        let mut card = decode_plan_entry(entry).expect("selected plan");
        card.expanded = !card.expanded;
        *entry = plan_entry(&card);
    }
}

pub(super) fn sync_persisted_plan(entries: &mut [String], plan: &Plan) {
    for entry in entries.iter_mut().rev() {
        let Some(mut card) = decode_plan_entry(entry) else {
            continue;
        };
        if card.plan.steps.len() == plan.steps.len()
            && card
                .plan
                .steps
                .iter()
                .zip(&plan.steps)
                .all(|(a, b)| a.text == b.text)
        {
            card.plan = plan.clone();
            *entry = plan_entry(&card);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "```plan\n1. 验证接口\n2. 发布\n```";
    const PROGRESS: &str = "没有新增状态变化。\n```progress\n1: done\n2: skipped\n```";

    #[test]
    fn live_plan_updates_in_place_and_details_can_be_toggled() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.append_transcript(format!("WillDeep: {PLAN}"));
        app.append_transcript(format!("WillDeep: {PROGRESS}"));
        app.append_transcript(format!("WillDeep: {PROGRESS}"));
        assert_eq!(app.transcript.len(), 1);
        let card = decode_plan_entry(&app.transcript[0]).unwrap();
        assert_eq!(card.plan.steps[0].status, StepStatus::Done);
        assert_eq!(card.details.len(), 2);
        let collapsed = colored_transcript_at_width(&app.transcript, None, 24).to_string();
        assert!(collapsed.contains("2/2"));
        assert!(!collapsed.contains("没有新增状态变化"));
        assert!(app.handle_slash_command("/plan", &SkillCatalog::default()));
        let expanded = colored_transcript_at_width(&app.transcript, None, 24).to_string();
        assert!(expanded.contains("没有新增状态变化"));
        app.handle_slash_command("/plan", &SkillCatalog::default());
        assert!(!decode_plan_entry(&app.transcript[0]).unwrap().expanded);
    }

    #[test]
    fn reopened_history_matches_live_plan_state() {
        let history = [
            Message::assistant(PLAN, vec![]),
            Message::assistant(PROGRESS, vec![]),
        ];
        let mut live = Vec::new();
        append_plan_reply(&mut live, PLAN);
        append_plan_reply(&mut live, PROGRESS);
        assert_eq!(live, projected_transcript(&history, None, Language::ZhCn));
    }

    #[test]
    fn narrow_terminal_renders_statuses_without_raw_fences() {
        use ratatui::backend::TestBackend;
        let entries = projected_transcript(
            &[
                Message::assistant(PLAN, vec![]),
                Message::assistant(PROGRESS, vec![]),
            ],
            None,
            Language::ZhCn,
        );
        let mut terminal = Terminal::new(TestBackend::new(24, 12)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(colored_transcript_at_width(&entries, None, 24))
                        .wrap(Wrap { trim: false }),
                    frame.area(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..12)
            .map(|y| (0..24).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // TestBackend stores a padding cell after every wide CJK glyph.
        assert!(rendered.replace(' ', "").contains("验证接口"), "{rendered}");
        assert!(rendered.contains('✓'));
        assert!(rendered.contains('–'));
        assert!(!rendered.contains("```"));
    }

    /// 会话重开后工具行照样回放：有结果的 ✓，没结果的 …，与实时路径同一种行。
    #[test]
    fn reopened_history_replays_tool_rows_between_replies() {
        let call = |id: &str, name: &str, arguments: &str| willdeep_core::types::ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        };
        let history = [
            Message::user("go"),
            Message::assistant(
                "Looking around first.",
                vec![call("a", "run_command", r#"{"command":"cargo test -p x"}"#)],
            ),
            Message::tool(&call("a", "run_command", "{}"), "ok"),
            Message::assistant("", vec![call("b", "read_file", r#"{"path":"x"}"#)]),
            Message::assistant("done", Vec::new()),
        ];
        assert_eq!(
            projected_transcript(&history, None, Language::En),
            vec![
                "You: go",
                "WillDeep: Looking around first.",
                "· ✓ run_command · cargo test -p",
                "· … read_file",
                "WillDeep: done",
            ]
        );
    }

    #[test]
    fn host_instruction_never_uses_user_label() {
        let entries = projected_transcript(
            &[
                Message::host_instruction("internal prompt"),
                Message::user("internal prompt"),
            ],
            None,
            Language::ZhCn,
        );
        assert_eq!(entries[0], "System: 系统自动推进");
        assert_eq!(entries[1], "You: internal prompt");
    }
}
