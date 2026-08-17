use super::*;

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub(super) fn colored_transcript_at_width(
    entries: &[String],
    search_query: Option<&str>,
    width: usize,
) -> Text<'static> {
    let mut lines = Vec::new();
    for value in entries {
        if let Some(content) = value.strip_prefix("WillDeep: ") {
            lines.extend(render_assistant_markdown(content, width));
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
    wrap_styled_text(colored_transcript_at_width(entries, None, width), width)
        .lines
        .len()
}

pub(super) fn wrap_styled_text(text: Text<'static>, width: usize) -> Text<'static> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for source in text.lines {
        let cells = source
            .spans
            .into_iter()
            .flat_map(|span| {
                span.content
                    .chars()
                    .map(move |character| StyledCharacter {
                        character,
                        style: span.style,
                        width: UnicodeWidthChar::width(character).unwrap_or(0),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if cells.is_empty() {
            rows.push(Line::default());
            continue;
        }
        let mut line = Vec::new();
        let mut line_width: usize = 0;
        let mut whitespace = Vec::new();
        let mut index = 0;
        while index < cells.len() {
            if cells[index].character.is_whitespace() {
                whitespace.push(cells[index]);
                index += 1;
                continue;
            }
            let word_start = index;
            while index < cells.len() && !cells[index].character.is_whitespace() {
                index += 1;
            }
            let word = &cells[word_start..index];
            let whitespace_width = whitespace.iter().map(|cell| cell.width).sum::<usize>();
            let word_width = word.iter().map(|cell| cell.width).sum::<usize>();
            if word_width <= width {
                if line_width > 0
                    && line_width
                        .saturating_add(whitespace_width)
                        .saturating_add(word_width)
                        > width
                {
                    rows.push(Line::from(std::mem::take(&mut line)));
                    line_width = 0;
                    whitespace.clear();
                }
                append_styled_cells(&mut line, &whitespace);
                line_width = line_width.saturating_add(whitespace_width);
                whitespace.clear();
                append_styled_cells(&mut line, word);
                line_width = line_width.saturating_add(word_width);
                continue;
            }
            if line_width > 0 && line_width.saturating_add(whitespace_width) >= width {
                rows.push(Line::from(std::mem::take(&mut line)));
                line_width = 0;
                whitespace.clear();
            }
            for cell in whitespace.drain(..).chain(word.iter().copied()) {
                if line_width > 0 && line_width.saturating_add(cell.width) > width {
                    rows.push(Line::from(std::mem::take(&mut line)));
                    line_width = 0;
                }
                push_styled_character(&mut line, cell.character, cell.style);
                line_width = line_width.saturating_add(cell.width);
            }
        }
        for cell in whitespace {
            if line_width > 0 && line_width.saturating_add(cell.width) > width {
                rows.push(Line::from(std::mem::take(&mut line)));
                line_width = 0;
            }
            push_styled_character(&mut line, cell.character, cell.style);
            line_width = line_width.saturating_add(cell.width);
        }
        if !line.is_empty() {
            rows.push(Line::from(line));
        }
    }
    Text::from(rows)
}

#[derive(Clone, Copy)]
struct StyledCharacter {
    character: char,
    style: Style,
    width: usize,
}

fn append_styled_cells(spans: &mut Vec<Span<'static>>, cells: &[StyledCharacter]) {
    for cell in cells {
        push_styled_character(spans, cell.character, cell.style);
    }
}

fn push_styled_character(spans: &mut Vec<Span<'static>>, character: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(character);
        return;
    }
    spans.push(Span::styled(character.to_string(), style));
}

pub(super) fn text_rows(text: &Text<'static>) -> Vec<String> {
    text.lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

pub(super) fn selected_text(rows: &[String], start: (usize, usize), end: (usize, usize)) -> String {
    if rows.is_empty() || start >= end {
        return String::new();
    }
    let last_row = end.0.min(rows.len().saturating_sub(1));
    (start.0..=last_row)
        .map(|row| {
            let start_column = if row == start.0 { start.1 } else { 0 };
            let end_column = if row == end.0 { end.1 } else { usize::MAX };
            display_column_slice(&rows[row], start_column, end_column)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn display_column_slice(value: &str, start: usize, end: usize) -> String {
    let mut output = String::new();
    let mut column = 0;
    let mut previous_selected = false;
    for character in value.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        let selected = if width == 0 {
            previous_selected
        } else {
            column < end && column.saturating_add(width) > start
        };
        if selected {
            output.push(character);
        }
        previous_selected = selected;
        column = column.saturating_add(width);
    }
    output
}

pub(super) fn highlight_text_selection(
    text: &mut Text<'static>,
    start: (usize, usize),
    end: (usize, usize),
) {
    if start >= end {
        return;
    }
    let selection_style = Style::default()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD);
    for (row, line) in text.lines.iter_mut().enumerate() {
        if row < start.0 || row > end.0 {
            continue;
        }
        let start_column = if row == start.0 { start.1 } else { 0 };
        let end_column = if row == end.0 { end.1 } else { usize::MAX };
        let source = std::mem::take(&mut line.spans);
        let mut column = 0;
        let mut previous_selected = false;
        let mut highlighted = Vec::new();
        for span in source {
            for character in span.content.chars() {
                let width = UnicodeWidthChar::width(character).unwrap_or(0);
                let selected = if width == 0 {
                    previous_selected
                } else {
                    column < end_column && column.saturating_add(width) > start_column
                };
                let style = if selected {
                    span.style.patch(selection_style)
                } else {
                    span.style
                };
                push_styled_character(&mut highlighted, character, style);
                previous_selected = selected;
                column = column.saturating_add(width);
            }
        }
        line.spans = highlighted;
    }
}

pub(super) fn render_assistant_markdown(content: &str, width: usize) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut code_block = false;
    let lines = content.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let raw = lines[index];
        if raw.trim_start().starts_with("```") {
            code_block = !code_block;
            index += 1;
            continue;
        }
        if code_block {
            let mut spans = assistant_prefix(index == 0);
            spans.push(Span::styled(
                raw.to_owned(),
                Style::default().fg(Color::White).bg(Color::DarkGray),
            ));
            output.push(Line::from(spans));
            index += 1;
            continue;
        }

        if index + 1 < lines.len()
            && parse_table_row(raw).is_some()
            && is_table_separator(lines[index + 1])
        {
            let mut rows = vec![parse_table_row(raw).expect("table header was checked")];
            index += 2;
            while index < lines.len() {
                let Some(row) = parse_table_row(lines[index]) else {
                    break;
                };
                rows.push(row);
                index += 1;
            }
            if output.is_empty() {
                output.push(Line::from(assistant_prefix(true)));
            }
            output.extend(render_markdown_table(&rows, width));
            continue;
        }

        let normalized = normalize_html_breaks(raw);
        for (piece_index, piece) in normalized.split('\n').enumerate() {
            output.push(render_markdown_line(piece, index == 0 && piece_index == 0));
        }
        index += 1;
    }
    if output.is_empty() {
        output.push(Line::styled("WillDeep:", Style::default().fg(Color::Green)));
    }
    output
}

fn assistant_prefix(show: bool) -> Vec<Span<'static>> {
    show.then(|| {
        Span::styled(
            "WillDeep: ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    })
    .into_iter()
    .collect()
}

fn render_markdown_line(raw: &str, show_prefix: bool) -> Line<'static> {
    let mut spans = assistant_prefix(show_prefix);
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
    Line::from(spans)
}

fn normalize_html_breaks(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('<') {
        output.push_str(&rest[..index]);
        let candidate = &rest[index..];
        let tag_length = ["<br />", "<br/>", "<br>"]
            .into_iter()
            .find(|tag| {
                candidate
                    .get(..tag.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(tag))
            })
            .map(str::len);
        if let Some(tag_length) = tag_length {
            output.push('\n');
            rest = &candidate[tag_length..];
        } else {
            output.push('<');
            rest = &candidate[1..];
        }
    }
    output.push_str(rest);
    output
}

fn parse_table_row(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return None;
    }
    let cells = trimmed[1..trimmed.len() - 1]
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn is_table_separator(value: &str) -> bool {
    parse_table_row(value).is_some_and(|cells| {
        cells.iter().all(|cell| {
            let marker = cell.trim().trim_matches(':').trim();
            marker.len() >= 3 && marker.bytes().all(|byte| byte == b'-')
        })
    })
}

fn terminal_cell_text(value: &str) -> String {
    normalize_html_breaks(value)
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
}

fn table_column_widths(rows: &[Vec<String>], width: usize) -> Vec<usize> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut natural = vec![1; columns];
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            for line in terminal_cell_text(cell).split('\n') {
                natural[column] = natural[column].max(UnicodeWidthStr::width(line).max(1));
            }
        }
    }
    let separators = columns.saturating_sub(1) * 3;
    let available = width.max(columns + separators).saturating_sub(separators);
    if natural.iter().sum::<usize>() <= available {
        return natural;
    }
    let mut result = vec![1; columns];
    let mut remaining = available.saturating_sub(columns);
    while remaining > 0 {
        let mut changed = false;
        for column in 0..columns {
            if result[column] < natural[column] && remaining > 0 {
                result[column] += 1;
                remaining -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    result
}

fn wrap_table_cell(value: &str, width: usize) -> Vec<String> {
    let mut output = Vec::new();
    for explicit_line in terminal_cell_text(value).split('\n') {
        let mut line = String::new();
        let mut columns = 0;
        for character in explicit_line.chars() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if columns + character_width > width && !line.is_empty() {
                output.push(std::mem::take(&mut line));
                columns = 0;
            }
            line.push(character);
            columns += character_width;
            if columns >= width {
                output.push(std::mem::take(&mut line));
                columns = 0;
            }
        }
        if !line.is_empty() || explicit_line.is_empty() {
            output.push(line);
        }
    }
    if output.is_empty() {
        output.push(String::new());
    }
    output
}

fn render_markdown_table(rows: &[Vec<String>], width: usize) -> Vec<Line<'static>> {
    let widths = table_column_widths(rows, width.max(1));
    let mut output = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let cells = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                wrap_table_cell(row.get(column).map(String::as_str).unwrap_or(""), *width)
            })
            .collect::<Vec<_>>();
        let row_height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..row_height {
            let mut spans = Vec::new();
            for (column, column_width) in widths.iter().enumerate() {
                if column > 0 {
                    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                }
                let value = cells[column]
                    .get(line_index)
                    .map(String::as_str)
                    .unwrap_or("");
                let padding = column_width.saturating_sub(UnicodeWidthStr::width(value));
                let style = if row_index == 0 {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                };
                spans.push(Span::styled(value.to_owned(), style));
                spans.push(Span::styled(" ".repeat(padding), style));
            }
            output.push(Line::from(spans));
        }
        if row_index == 0 {
            let separator = widths
                .iter()
                .map(|width| "─".repeat(*width))
                .collect::<Vec<_>>()
                .join("─┼─");
            output.push(Line::styled(
                separator,
                Style::default().fg(Color::DarkGray),
            ));
        }
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
