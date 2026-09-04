use unicode_width::UnicodeWidthChar;
use willdeep_core::MessageAttachment;

#[derive(Default)]
pub struct PromptEditor {
    text: String,
    cursor: usize,
}

impl PromptEditor {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
    pub fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }
    pub fn backspace(&mut self) {
        if let Some((index, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(index..self.cursor, "");
            self.cursor = index;
        }
    }
    pub fn delete(&mut self) {
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + character.len_utf8(), "");
        }
    }
    pub fn left(&mut self) {
        if let Some((index, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }
    pub fn right(&mut self) {
        if let Some(character) = self.text[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }
    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }
    pub fn end(&mut self) {
        self.cursor += self.text[self.cursor..]
            .find('\n')
            .unwrap_or(self.text.len() - self.cursor);
    }
    pub fn up_visual(&mut self, width: usize) {
        let (row, column) = self.cursor_visual(width);
        if row > 0 {
            self.set_cursor_visual(row - 1, column, width);
        }
    }
    pub fn down_visual(&mut self, width: usize) {
        let (row, column) = self.cursor_visual(width);
        self.set_cursor_visual(row + 1, column, width);
    }
    pub fn cursor_visual(&self, width: usize) -> (usize, usize) {
        visual_position(&self.text, self.cursor, width)
    }
    pub fn wrapped_text(&self, width: usize) -> String {
        visual_rows(&self.text, width).join("\n")
    }
    pub fn visual_line_count(&self, width: usize) -> usize {
        visual_rows(&self.text, width).len()
    }
    pub fn marker_query(&self, marker: char) -> Option<(usize, &str)> {
        let prefix = &self.text[..self.cursor];
        let start = prefix
            .char_indices()
            .rev()
            .find_map(|(index, character)| {
                character
                    .is_whitespace()
                    .then_some(index + character.len_utf8())
            })
            .unwrap_or(0);
        let token = &prefix[start..];
        let query = token.strip_prefix(marker)?;
        query
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .then_some((start, query))
    }
    pub fn replace_before_cursor(&mut self, start: usize, value: &str) {
        if start <= self.cursor && self.text.is_char_boundary(start) {
            self.text.replace_range(start..self.cursor, value);
            self.cursor = start + value.len();
        }
    }
    pub fn set_cursor_visual(&mut self, target_row: usize, target_column: usize, width: usize) {
        self.cursor = byte_at_visual(&self.text, target_row, target_column, width);
    }
}

fn visual_position(text: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0;
    let mut column = 0;
    for character in text[..cursor].chars() {
        if character == '\n' {
            row += 1;
            column = 0;
            continue;
        }
        let size = UnicodeWidthChar::width(character).unwrap_or(0);
        if column + size > width {
            row += 1;
            column = 0;
        }
        column += size;
        if column >= width {
            row += 1;
            column = 0;
        }
    }
    (row, column)
}

fn byte_at_visual(text: &str, target_row: usize, target_column: usize, width: usize) -> usize {
    let width = width.max(1);
    let mut row = 0;
    let mut column = 0;
    for (index, character) in text.char_indices() {
        if row > target_row || (row == target_row && column >= target_column) {
            return index;
        }
        if character == '\n' {
            if row == target_row {
                return index;
            }
            row += 1;
            column = 0;
            continue;
        }
        let size = UnicodeWidthChar::width(character).unwrap_or(0);
        if column + size > width {
            row += 1;
            column = 0;
            if row > target_row {
                return index;
            }
        }
        column += size;
        if column >= width {
            row += 1;
            column = 0;
        }
    }
    text.len()
}

fn visual_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut column = 0;
    for character in text.chars() {
        if character == '\n' {
            rows.push(std::mem::take(&mut row));
            column = 0;
            continue;
        }
        let size = UnicodeWidthChar::width(character).unwrap_or(0);
        if column + size > width {
            rows.push(std::mem::take(&mut row));
            column = 0;
        }
        row.push(character);
        column += size;
        if column >= width {
            rows.push(std::mem::take(&mut row));
            column = 0;
        }
    }
    rows.push(row);
    rows
}

pub struct DraftAttachment {
    pub message: MessageAttachment,
}
impl DraftAttachment {
    pub fn summary(&self) -> String {
        match &self.message {
            // 带一截正文预览：剪贴板里装的到底是不是想贴的东西，发送前就该看得出来。
            // 出过一回事——拖选聊天区自动复制顶掉了刚复制的网址，条目只写
            // 「3 lines · 146 B」，谁也看不出贴进去的是上一条回复。
            MessageAttachment::Text { content, .. } => format!(
                "Pasted text · {} lines · {} · 「{}」",
                content.lines().count().max(1),
                human_bytes(content.len()),
                text_preview(content)
            ),
            MessageAttachment::Image {
                name,
                width,
                height,
                data,
                ..
            } => format!(
                "{name} · {width}×{height} · {}",
                human_bytes(data.len() * 3 / 4)
            ),
        }
    }
}
/// 正文里第一行有字的内容，截到 [`PREVIEW_CHARS`] 个字符；截断了就加省略号。
const PREVIEW_CHARS: usize = 36;
fn text_preview(content: &str) -> String {
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut preview = line.chars().take(PREVIEW_CHARS).collect::<String>();
    if line.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 附件条目要露出正文首行：贴错了东西发送前就能看见。
    #[test]
    fn pasted_text_summary_previews_the_first_non_empty_line() {
        let attachment = DraftAttachment {
            message: MessageAttachment::Text {
                name: "paste-1.txt".to_owned(),
                content: "\nscripts/tsing_hub_probe.rb 是现成脚手架，可以加一组 effort 探针在 beta 环境跑。\n\n· 首答 9.11s".to_owned(),
            },
        };
        let summary = attachment.summary();
        assert!(summary.starts_with("Pasted text · 4 lines · "), "{summary}");
        assert!(summary.ends_with("「scripts/tsing_hub_probe.rb 是现成脚手架，可以…」"), "{summary}");

        let short = text_preview("  https://hub-beta.example.com/v1  ");
        assert_eq!(short, "https://hub-beta.example.com/v1");
        assert_eq!(text_preview("\n\n"), "");
    }

    #[test]
    fn edits_multiline_text_and_moves_cursor() {
        let mut e = PromptEditor::default();
        e.insert("one\ntwo");
        e.up_visual(80);
        e.home();
        e.insert("X");
        assert_eq!(e.text(), "Xone\ntwo");
        e.down_visual(80);
        e.end();
        e.backspace();
        assert_eq!(e.text(), "Xone\ntw");
    }
    #[test]
    fn maps_cjk_visual_cursor() {
        let mut e = PromptEditor::default();
        e.insert("中文ab");
        e.set_cursor_visual(0, 2, 10);
        e.insert("X");
        assert_eq!(e.text(), "中X文ab");
    }
    #[test]
    fn wraps_cjk_with_the_same_rows_used_by_the_cursor() {
        let mut editor = PromptEditor::default();
        editor.insert("甲乙丙丁\n最后一行");

        assert_eq!(editor.wrapped_text(6), "甲乙丙\n丁\n最后一\n行");
        assert_eq!(editor.visual_line_count(6), 4);
        assert_eq!(editor.cursor_visual(6), (3, 2));
    }
    #[test]
    fn keeps_a_trailing_cursor_row_after_an_exact_width_wrap() {
        let mut editor = PromptEditor::default();
        editor.insert("中文");

        assert_eq!(editor.wrapped_text(4), "中文\n");
        assert_eq!(editor.cursor_visual(4), (1, 0));
    }
    #[test]
    fn finds_and_replaces_marker_query_at_cursor() {
        let mut editor = PromptEditor::default();
        editor.insert("please use $image-pro");
        let (start, query) = editor.marker_query('$').unwrap();
        assert_eq!(query, "image-pro");

        editor.replace_before_cursor(start, "$image-processing ");
        assert_eq!(editor.text(), "please use $image-processing ");
        assert!(editor.marker_query('$').is_none());
    }
}
