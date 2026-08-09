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

pub struct DraftAttachment {
    pub message: MessageAttachment,
}
impl DraftAttachment {
    pub fn summary(&self) -> String {
        match &self.message {
            MessageAttachment::Text { content, .. } => format!(
                "Pasted text · {} lines · {}",
                content.lines().count().max(1),
                human_bytes(content.len())
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
}
