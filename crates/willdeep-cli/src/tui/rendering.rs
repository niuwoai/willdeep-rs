use super::*;

pub(super) fn colored_transcript(entries: &[String], search_query: Option<&str>) -> Text<'static> {
    let mut lines = Vec::new();
    for value in entries {
        if let Some(content) = value.strip_prefix("WillDeep: ") {
            lines.extend(render_assistant_markdown(content));
            continue;
        }
        let style = if value.starts_with("You:") {
            Style::default().fg(Color::Cyan)
        } else if value.starts_with("Error:") {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Yellow)
        };
        lines.extend(
            value
                .lines()
                .map(|line| Line::styled(line.to_owned(), style)),
        );
    }
    let mut text = Text::from(lines);
    if let Some(query) = search_query {
        highlight_matches(&mut text, query);
    }
    text
}

fn highlight_matches(text: &mut Text<'static>, query: &str) {
    let Ok(pattern) = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
    else {
        return;
    };
    let highlight = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    for line in &mut text.lines {
        let spans = std::mem::take(&mut line.spans);
        line.spans = spans
            .into_iter()
            .flat_map(|span| {
                let value = span.content.into_owned();
                let mut output = Vec::new();
                let mut offset = 0;
                for found in pattern.find_iter(&value) {
                    if found.start() > offset {
                        output.push(Span::styled(
                            value[offset..found.start()].to_owned(),
                            span.style,
                        ));
                    }
                    output.push(Span::styled(
                        value[found.start()..found.end()].to_owned(),
                        span.style.patch(highlight),
                    ));
                    offset = found.end();
                }
                if offset < value.len() {
                    output.push(Span::styled(value[offset..].to_owned(), span.style));
                }
                if output.is_empty() {
                    output.push(Span::styled(value, span.style));
                }
                output
            })
            .collect();
    }
}

pub(super) fn rendered_transcript_height(entries: &[String], width: usize) -> usize {
    Paragraph::new(colored_transcript(entries, None))
        .wrap(Wrap { trim: false })
        .line_count(width.max(1).min(u16::MAX as usize) as u16)
}

pub(super) fn render_assistant_markdown(content: &str) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut code_block = false;
    for (index, raw) in content.lines().enumerate() {
        if raw.trim_start().starts_with("```") {
            code_block = !code_block;
            continue;
        }
        let prefix = (index == 0).then(|| {
            Span::styled(
                "WillDeep: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        });
        let mut spans = Vec::new();
        if let Some(prefix) = prefix {
            spans.push(prefix);
        }
        if code_block {
            spans.push(Span::styled(
                raw.to_owned(),
                Style::default().fg(Color::White).bg(Color::DarkGray),
            ));
        } else {
            let trimmed = raw.trim_start();
            let (marker, body, base) = if let Some(body) = trimmed.strip_prefix("### ") {
                (
                    "▸ ",
                    body,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if let Some(body) = trimmed.strip_prefix("## ") {
                (
                    "◆ ",
                    body,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if let Some(body) = trimmed.strip_prefix("# ") {
                (
                    "■ ",
                    body,
                    Style::default()
                        .fg(Color::LightYellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else if let Some(body) = trimmed.strip_prefix("> ") {
                (
                    "│ ",
                    body,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )
            } else if let Some(body) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                ("• ", body, Style::default().fg(Color::Green))
            } else {
                ("", raw, Style::default().fg(Color::Green))
            };
            if !marker.is_empty() {
                spans.push(Span::styled(marker, base));
            }
            spans.extend(render_inline_markdown(body, base));
        }
        output.push(Line::from(spans));
    }
    if output.is_empty() {
        output.push(Line::styled("WillDeep:", Style::default().fg(Color::Green)));
    }
    output
}

fn render_inline_markdown(value: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let bold = rest.find("**").map(|index| (index, "bold"));
        let code = rest.find('`').map(|index| (index, "code"));
        let link = rest.find('[').map(|index| (index, "link"));
        let Some((index, kind)) = [bold, code, link]
            .into_iter()
            .flatten()
            .min_by_key(|item| item.0)
        else {
            spans.push(Span::styled(rest.to_owned(), base));
            break;
        };
        if index > 0 {
            spans.push(Span::styled(rest[..index].to_owned(), base));
            rest = &rest[index..];
        }
        match kind {
            "bold" if rest[2..].find("**").is_some() => {
                let end = rest[2..].find("**").unwrap() + 2;
                spans.push(Span::styled(
                    rest[2..end].to_owned(),
                    base.add_modifier(Modifier::BOLD),
                ));
                rest = &rest[end + 2..];
            }
            "code" if rest[1..].find('`').is_some() => {
                let end = rest[1..].find('`').unwrap() + 1;
                spans.push(Span::styled(
                    rest[1..end].to_owned(),
                    Style::default().fg(Color::LightCyan).bg(Color::DarkGray),
                ));
                rest = &rest[end + 1..];
            }
            "link" if rest.find("](").is_some() => {
                let label_end = rest.find("](").unwrap();
                if let Some(url_end) = rest[label_end + 2..].find(')') {
                    let url_end = label_end + 2 + url_end;
                    spans.push(Span::styled(
                        rest[1..label_end].to_owned(),
                        Style::default()
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    spans.push(Span::styled(
                        format!(" ({})", &rest[label_end + 2..url_end]),
                        base,
                    ));
                    rest = &rest[url_end + 1..];
                } else {
                    spans.push(Span::styled(rest[..1].to_owned(), base));
                    rest = &rest[1..];
                }
            }
            _ => {
                spans.push(Span::styled(rest[..1].to_owned(), base));
                rest = &rest[1..];
            }
        }
    }
    spans
}

pub(super) fn compact_thought(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut compact = normalized.chars().take(180).collect::<String>();
    if normalized.chars().count() > 180 {
        compact.push('…');
    }
    compact
}

pub(super) fn visual_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.split('\n')
        .map(|line| {
            line.chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum::<usize>()
                .max(1)
                .div_ceil(width)
        })
        .sum()
}

pub(super) fn question_option_row(popup_y: u16, question: &str, width: usize, index: usize) -> u16 {
    popup_y
        .saturating_add(2)
        .saturating_add(visual_lines(question, width).min(u16::MAX as usize) as u16)
        .saturating_add(index.min(u16::MAX as usize) as u16)
}

pub(super) fn transcript(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message.role {
            willdeep_core::Role::User => Some(format!(
                "You: {}{}",
                message.content,
                if message.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" [{} attachment(s)]", message.attachments.len())
                }
            )),
            willdeep_core::Role::Assistant if !message.content.trim().is_empty() => {
                Some(format!("WillDeep: {}", message.content))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn welcome_message(workspace: &std::path::Path, language: Language) -> String {
    let project = workspace
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(language.text("当前工作区", "current workspace", "現在のワークスペース"));
    match language {
        Language::ZhCn => format!(
            "WillDeep: 你好，我已经进入 {project}。你可以直接告诉我想实现、修复或调查什么；我会先了解项目，再开始动手。"
        ),
        Language::En => format!(
            "WillDeep: Hello, I’m in {project}. Tell me what you want to build, fix, or investigate; I’ll inspect the project before making changes."
        ),
        Language::Ja => format!(
            "WillDeep: こんにちは。{project} を開きました。実装、修正、調査したいことを教えてください。まずプロジェクトを確認してから作業します。"
        ),
    }
}
