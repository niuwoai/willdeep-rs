use super::*;

pub(super) struct DiffReviewState {
    pub snapshot: crate::daemon::diff_review::DiffSnapshot,
    pub selected: usize,
    pub content: Option<(String, String)>,
    pub scroll: usize,
    pub area: crate::daemon::diff_review::DiffArea,
    pub view: DiffViewMode,
    pub search: Option<PromptEditor>,
    pub search_matches: Vec<usize>,
    pub search_selected: usize,
    pub reviews: BTreeMap<String, crate::daemon::diff_review::ReviewDecision>,
    pub confirm_revert: bool,
    pub verifications: Vec<crate::daemon::diff_review::DiffVerificationRecord>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DiffViewMode {
    #[default]
    Unified,
    SideBySide,
}

pub(super) fn next_diff_area(
    area: crate::daemon::diff_review::DiffArea,
) -> crate::daemon::diff_review::DiffArea {
    use crate::daemon::diff_review::DiffArea;
    match area {
        DiffArea::Combined => DiffArea::Staged,
        DiffArea::Staged => DiffArea::Unstaged,
        DiffArea::Unstaged => DiffArea::Combined,
    }
}

pub(super) fn refresh_diff_search(review: &mut DiffReviewState) {
    let query = review
        .search
        .as_ref()
        .map(|editor| editor.text().trim().to_lowercase())
        .unwrap_or_default();
    let Some((_, content)) = &review.content else {
        review.search_matches.clear();
        review.search_selected = 0;
        return;
    };
    let lines = match review.view {
        DiffViewMode::Unified => content.lines().map(str::to_owned).collect(),
        DiffViewMode::SideBySide => diff_side_by_side_rows(content, 120),
    };
    review.search_matches = if query.is_empty() {
        Vec::new()
    } else {
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.to_lowercase().contains(&query).then_some(index))
            .collect()
    };
    review.search_selected = 0;
    if let Some(first) = review.search_matches.first() {
        review.scroll = *first;
    }
}

pub(super) fn diff_review_lines(content: &str, search_query: Option<&str>) -> Vec<Line<'static>> {
    content
        .lines()
        .map(|line| {
            let color = if line.starts_with("+++") || line.starts_with("---") {
                Color::Cyan
            } else if line.starts_with('+') {
                Color::Green
            } else if line.starts_with('-') {
                Color::Red
            } else if line.starts_with("@@") {
                Color::Yellow
            } else {
                Color::Gray
            };
            let matched = search_query
                .is_some_and(|query| line.to_lowercase().contains(&query.to_lowercase()));
            let style = Style::default().fg(color);
            Line::styled(
                line.to_owned(),
                if matched {
                    style.bg(Color::DarkGray)
                } else {
                    style
                },
            )
        })
        .collect()
}

pub(super) fn diff_side_by_side_lines(
    content: &str,
    width: u16,
    search_query: Option<&str>,
) -> Vec<Line<'static>> {
    diff_side_by_side_rows(content, width)
        .into_iter()
        .map(|line| {
            let matched = search_query
                .is_some_and(|query| line.to_lowercase().contains(&query.to_lowercase()));
            Line::styled(
                line,
                if matched {
                    Style::default().fg(Color::Gray).bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Gray)
                },
            )
        })
        .collect()
}

pub(super) fn diff_side_by_side_rows(content: &str, width: u16) -> Vec<String> {
    let column_width = (width.saturating_sub(3) / 2).max(8) as usize;
    let source = content.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let line = source[index];
        if line.starts_with('-') && !line.starts_with("---") {
            let right = source
                .get(index + 1)
                .copied()
                .filter(|next| next.starts_with('+') && !next.starts_with("+++"));
            rows.push(format!(
                "{} │ {}",
                fit_diff_column(line, column_width),
                fit_diff_column(right.unwrap_or(""), column_width)
            ));
            index += usize::from(right.is_some()) + 1;
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            rows.push(format!(
                "{} │ {}",
                " ".repeat(column_width),
                fit_diff_column(line, column_width)
            ));
        } else {
            let value = fit_diff_column(line, column_width);
            rows.push(format!("{value} │ {value}"));
        }
        index += 1;
    }
    rows
}

pub(super) fn diff_snapshot_lines(review: &DiffReviewState) -> Vec<Line<'static>> {
    let mut lines = review
        .verifications
        .iter()
        .rev()
        .take(3)
        .map(|verification| {
            let color = match verification.outcome {
                crate::daemon::diff_review::VerificationOutcome::Passed => Color::Green,
                _ => Color::Red,
            };
            Line::styled(
                format!(
                    "check {:?} · exit={} · {}",
                    verification.outcome,
                    verification
                        .exit_code
                        .map_or_else(|| "—".to_owned(), |code| code.to_string()),
                    verification.command
                ),
                Style::default().fg(color),
            )
        })
        .collect::<Vec<_>>();
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.extend(
        review
            .snapshot
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let marker = if index == review.selected { "▶" } else { " " };
                let stage = match (file.staged, file.unstaged) {
                    (true, true) => "S+U",
                    (true, false) => "S",
                    (false, true) => "U",
                    (false, false) => "-",
                };
                let style = if index == review.selected {
                    Style::default().fg(Color::Black).bg(Color::LightCyan)
                } else if file.binary {
                    Style::default().fg(Color::Magenta)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let decision =
                    review
                        .reviews
                        .get(&file.path)
                        .map_or("", |decision| match decision {
                            crate::daemon::diff_review::ReviewDecision::Accepted => " [accepted]",
                            crate::daemon::diff_review::ReviewDecision::Rejected => " [rejected]",
                            crate::daemon::diff_review::ReviewDecision::ChangesRequested => {
                                " [changes]"
                            }
                            crate::daemon::diff_review::ReviewDecision::Reviewed => " [reviewed]",
                        });
                Line::styled(
                    format!(
                        "{marker} {:?} [{stage}] +{} -{} {}{}{}",
                        file.kind,
                        file.additions,
                        file.deletions,
                        file.path,
                        if file.binary { " [binary]" } else { "" },
                        decision
                    ),
                    style,
                )
            }),
    );
    lines
}

fn fit_diff_column(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    output.push_str(&" ".repeat(width.saturating_sub(used)));
    output
}
