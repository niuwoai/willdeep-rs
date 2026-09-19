//! 中途文字与工具调用直接落进聊天区。
//!
//! 此前模型在两次工具调用之间说的话只作「思考中」临时行，下一次调工具就擦掉；
//! 工具调用只进活动区，折叠态只露最近两条。几十次工具调用的任务跑十分钟，聊天区
//! 一片空白，用户分不清是在干活还是卡死了。现在：
//!
//! - 每段定稿的中途文字都作 `WillDeep:` 行落进记录，流式增量仍走临时行预览；
//! - 每次工具调用先落一行 `· … 名字 · 摘要`，完成后原地改成 `✓` / `✗`；
//! - 收尾文字只追加还没显示过的部分（比如轮次上限提示），不重复最后一段。
//!
//! 进程内轮次与 Runtime 轮次共用这一套，行为一致。
use super::*;

/// 工具行状态标记：进行中 / 成功 / 失败。
const TOOL_PENDING: &str = "…";
const TOOL_DONE: &str = "✓";
const TOOL_FAILED: &str = "✗";

/// 一行工具记录。`· ` 前缀让渲染层按账目行的灰色画，不与正文抢注意力。
fn tool_line(marker: &str, name: &str, detail: Option<&str>) -> String {
    match detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("· {marker} {name} · {detail}"),
        None => format!("· {marker} {name}"),
    }
}

/// 收尾文字里还没在聊天区出现过的部分。
///
/// 进程内 `final_text` 就是最后一段中途文字；撞轮次上限时守护进程会在前面
/// 拼一段提示（`{提示}\n\n{最后一段}`）。两种情况都只追加新增的那部分，
/// 否则最后一段会在聊天区出现两遍。
fn unshown_reply(final_text: &str, narrated: Option<&str>) -> Option<String> {
    let final_text = final_text.trim();
    if final_text.is_empty() {
        return None;
    }
    let Some(narrated) = narrated.map(str::trim).filter(|value| !value.is_empty()) else {
        return Some(final_text.to_owned());
    };
    if final_text == narrated {
        return None;
    }
    match final_text.strip_suffix(narrated).map(str::trim_end) {
        Some("") => None,
        Some(head) => Some(head.to_owned()),
        None => Some(final_text.to_owned()),
    }
}

impl App {
    /// 模型这一轮定稿的一段话：落进记录，并记住它，收尾时据此去重。
    pub(super) fn note_narration(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.transient_thought = None;
        self.activity_line = self
            .language
            .text("正在整理思路", "Working through it", "考えを整理中")
            .to_owned();
        self.append_transcript(format!("WillDeep: {text}"));
        self.turn_narration = Some(text.to_owned());
    }

    /// 工具刚发起：先占一行，完成时原地改标记。
    pub(super) fn note_tool_requested(&mut self, name: &str, detail: Option<&str>) {
        self.transient_thought = None;
        self.append_transcript(tool_line(TOOL_PENDING, name, detail));
    }

    /// 工具结束：把最近那条同名的进行中行改成成功/失败。只按名字对行，完成事件
    /// 不带摘要也能对上；实在找不到（比如重开会话后才收到完成事件）就补一行，
    /// 宁可多一行也不丢结果。
    pub(super) fn note_tool_completed(&mut self, name: &str, detail: Option<&str>, is_error: bool) {
        let marker = if is_error { TOOL_FAILED } else { TOOL_DONE };
        let pending = tool_line(TOOL_PENDING, name, None);
        let with_detail = format!("{pending} · ");
        let settled = self
            .transcript
            .iter_mut()
            .rev()
            .find(|line| **line == pending || line.starts_with(&with_detail))
            .map(|line| *line = line.replacen(TOOL_PENDING, marker, 1))
            .is_some();
        if settled {
            self.transcript_height =
                rendered_transcript_height(&self.transcript, self.transcript_width);
        } else {
            self.append_transcript(tool_line(marker, name, detail));
        }
    }

    /// 收尾文字：只追加还没显示过的部分，返回追加了什么，供会话镜像持久化。
    pub(super) fn note_reply(&mut self, final_text: &str) -> Option<String> {
        self.transient_thought = None;
        let remainder = unshown_reply(final_text, self.turn_narration.as_deref());
        if let Some(text) = &remainder {
            self.append_transcript(format!("WillDeep: {text}"));
        }
        remainder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_line_keeps_detail_only_when_present() {
        assert_eq!(
            tool_line(TOOL_PENDING, "run_command", Some(" cargo test ")),
            "· … run_command · cargo test"
        );
        assert_eq!(
            tool_line(TOOL_DONE, "read_file", Some("  ")),
            "· ✓ read_file"
        );
        assert_eq!(tool_line(TOOL_FAILED, "edit_file", None), "· ✗ edit_file");
    }

    #[test]
    fn unshown_reply_skips_what_narration_already_showed() {
        // 正常收尾：收尾文字就是最后一段，不再重复。
        assert_eq!(unshown_reply("All done.", Some("All done.")), None);
        assert_eq!(unshown_reply("  All done.\n", Some("All done.")), None);
        // 轮次上限：守护进程把提示拼在最后一段前面，只补提示。
        assert_eq!(
            unshown_reply("⚠ limit hit\n\nAll done.", Some("All done.")),
            Some("⚠ limit hit".to_owned())
        );
        // 没有中途文字，或收尾文字另起一段：整段照常显示。
        assert_eq!(
            unshown_reply("Fresh answer", None),
            Some("Fresh answer".to_owned())
        );
        assert_eq!(
            unshown_reply("Different closing", Some("All done.")),
            Some("Different closing".to_owned())
        );
        assert_eq!(unshown_reply("   ", Some("All done.")), None);
    }

    #[test]
    fn tool_rows_settle_in_place_and_fall_back_to_a_new_row() {
        let mut app = App::new(Vec::new(), Language::En);
        app.note_tool_requested("run_command", Some("cargo test -p"));
        app.note_tool_requested("read_file", None);
        assert_eq!(
            app.transcript,
            vec!["· … run_command · cargo test -p", "· … read_file"]
        );

        // 完成事件不带摘要也要对上发起行；同名多行时改最近那条。
        app.note_tool_completed("run_command", None, true);
        app.note_tool_completed("read_file", None, false);
        assert_eq!(
            app.transcript,
            vec!["· ✗ run_command · cargo test -p", "· ✓ read_file"]
        );

        // 找不到发起行（重开会话后才收到完成事件）就补一行。
        app.note_tool_completed("edit_file", Some("main.rs"), false);
        assert_eq!(
            app.transcript.last().map(String::as_str),
            Some("· ✓ edit_file · main.rs")
        );
    }

    #[test]
    fn narration_then_reply_does_not_duplicate_the_last_paragraph() {
        let mut app = App::new(Vec::new(), Language::En);
        app.transient_thought = Some("streaming…".to_owned());
        app.note_narration("  Now update the log line  ");
        assert!(app.transient_thought.is_none());
        assert_eq!(app.transcript, vec!["WillDeep: Now update the log line"]);

        assert_eq!(app.note_reply("Now update the log line"), None);
        assert_eq!(app.transcript.len(), 1);

        assert_eq!(
            app.note_reply("⚠ limit\n\nNow update the log line"),
            Some("⚠ limit".to_owned())
        );
        assert_eq!(
            app.transcript,
            vec!["WillDeep: Now update the log line", "WillDeep: ⚠ limit"]
        );
    }
}
