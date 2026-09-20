//! 中途文字与工具调用直接落进聊天区。
//!
//! 此前模型在两次工具调用之间说的话只作「思考中」临时行，下一次调工具就擦掉；
//! 工具调用只进活动区，折叠态只露最近两条。几十次工具调用的任务跑十分钟，聊天区
//! 一片空白，用户分不清是在干活还是卡死了。现在：
//!
//! - 每段定稿的中途文字都作 `WillDeep:` 行落进记录，流式增量仍走临时行预览；
//! - 每次工具调用先落一行 `· … 名字 · 摘要`，完成后原地改成 `✓` / `✗`；
//! - 收尾文字只追加还没显示过的部分（比如轮次上限提示），不重复最后一段；
//! - 显示层把每段连续工具行收起：早先的段一行汇总，最新的段一行汇总加最新一条，
//!   `/tools` 展开。
//!
//! 进程内轮次与 Runtime 轮次共用这一套，行为一致。
use super::*;

/// 工具行状态标记：进行中 / 成功 / 失败。
const TOOL_PENDING: &str = "…";
const TOOL_DONE: &str = "✓";
const TOOL_FAILED: &str = "✗";

/// 最新那段连续工具行最多占这么多行：一行汇总加最新一条，正在跑的那条永远看得见。
/// 更早的段一律只留一行汇总（只有一条时原样保留）。
const LAST_TOOL_RUN_ROWS: usize = 2;
/// 汇总行的标记，与三种工具行标记都不同；渲染层照样按 `· ` 账目行的灰色画。
const TOOL_FOLD_MARKER: &str = "⋯";

/// 一行工具记录。`· ` 前缀让渲染层按账目行的灰色画，不与正文抢注意力。
fn tool_line(marker: &str, name: &str, detail: Option<&str>) -> String {
    match detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        Some(detail) => format!("· {marker} {name} · {detail}"),
        None => format!("· {marker} {name}"),
    }
}

/// 这行是不是工具行；是的话给出它的状态标记。
fn tool_row_marker(line: &str) -> Option<&'static str> {
    let rest = line.strip_prefix("· ")?;
    [TOOL_PENDING, TOOL_DONE, TOOL_FAILED]
        .into_iter()
        .find(|marker| {
            rest.strip_prefix(marker)
                .is_some_and(|tail| tail.starts_with(' '))
        })
}

fn tool_row_name(line: &str) -> Option<&str> {
    let marker = tool_row_marker(line)?;
    let rest = line.strip_prefix("· ")?.strip_prefix(marker)?.trim_start();
    Some(rest.split(" · ").next().unwrap_or(rest))
}

/// 临时行里此刻流的是什么：思维链还是正文。换源时清缓冲，两种文字不拼在一起。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamKind {
    Reasoning,
    Reply,
}

/// 思维链临时行最多占这么多行：像滚动字幕，只看得到它此刻在想什么。
const THINKING_ROWS: usize = 3;

/// 从尾部截取一段，使 `prefix + 结果` 在 `width` 列内不超过 `rows` 行。空白折叠成
/// 单个空格，截过的以 `…` 开头。
pub(super) fn tail_within_rows(prefix: &str, text: &str, width: usize, rows: usize) -> String {
    let width = width.max(1);
    let fits = |candidate: &str| visual_lines(&format!("{prefix}{candidate}"), width) <= rows;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if fits(&compact) {
        return compact;
    }
    let chars: Vec<char> = compact.chars().collect();
    // 每行最多 width 个字符，先按这个上界取尾巴，再逐步收缩到装得下为止。
    let mut keep = (rows * width).min(chars.len());
    loop {
        let candidate: String = std::iter::once('…')
            .chain(chars[chars.len() - keep..].iter().copied())
            .collect();
        if keep == 0 || fits(&candidate) {
            return candidate;
        }
        keep -= (keep / 10).max(1);
    }
}

impl App {
    /// 聊天区底部的临时行：思维链灰色、只留最后 [`THINKING_ROWS`] 行；正文预览
    /// 照旧整段黄色。正文一开始，思维链缓冲已被清掉，这里自然只剩正文。
    pub(super) fn transient_row(&self) -> Option<String> {
        let thought = self.transient_thought.as_deref()?;
        let label = self.transient_label();
        Some(match self.transient_kind {
            StreamKind::Reasoning => {
                let prefix = format!("· {label}: ");
                let tail = tail_within_rows(&prefix, thought, self.transcript_width, THINKING_ROWS);
                format!("{prefix}{tail}")
            }
            StreamKind::Reply => format!("WillDeep · {label}: {thought}"),
        })
    }

    /// 流式增量进临时行：只留最后 [`THOUGHT_PREVIEW_CHARS`] 字，进程内轮次与
    /// Runtime 轮次共用。思考型模型正文常为空，思维链那行是用户唯一能看到的
    /// 「它此刻在干嘛」。
    pub(super) fn stream_transient(&mut self, kind: StreamKind, text: &str) {
        let text = terminal_safe_text(text);
        if self.transient_kind != kind {
            self.transient_thought = None;
            self.transient_kind = kind;
        }
        self.activity_line = match kind {
            StreamKind::Reasoning => self.language.text("正在思考", "Thinking", "思考中"),
            StreamKind::Reply => {
                self.language
                    .text("正在接收回复", "Receiving reply", "応答を受信中")
            }
        }
        .to_owned();
        let mut preview = self.transient_thought.take().unwrap_or_default();
        preview.push_str(&text);
        let skip = preview
            .chars()
            .count()
            .saturating_sub(THOUGHT_PREVIEW_CHARS);
        self.transient_thought = Some(preview.chars().skip(skip).collect());
    }

    /// 临时行前面的标签，随来源变。
    pub(super) fn transient_label(&self) -> &'static str {
        match self.transient_kind {
            StreamKind::Reasoning => self.language.text("思考中", "thinking", "思考中"),
            StreamKind::Reply => self.language.text("回复中", "replying", "応答中"),
        }
    }

    /// `/version`：终端这边的 CLI 版本与 Runtime 守护进程的版本各是多少。Runtime
    /// 版本来自快照，没连上就说没连上；两边不一致时点明，因为命令实际在 Runtime 里跑。
    pub(super) fn version_report(&self) -> String {
        let runtime = self.runtime_version.clone().unwrap_or_else(|| {
            self.language
                .text("未连接", "not connected", "未接続")
                .to_owned()
        });
        let mut report = format!("System: CLI {} · Runtime {runtime}", willdeep_core::VERSION);
        if self.stale_runtime_version().is_some() {
            report.push_str(self.language.text(
                " · 版本不一致，命令在 Runtime 里跑，请执行 `willdeep daemon upgrade` 对齐",
                " · versions differ; commands run inside the Runtime, run `willdeep daemon upgrade`",
                " · バージョン不一致。コマンドは Runtime 側で実行されます。`willdeep daemon upgrade` で揃えてください",
            ));
        }
        report
    }

    /// 输入框标题上的状态词：跑的时候回车只是排队，空闲时才轮到用户。
    pub(super) fn composer_state(&self) -> (&'static str, Color) {
        if self.running {
            (
                self.language.text(
                    "本轮进行中 · 回车即送达 · Esc 中止",
                    "Turn running · Enter steers now · Esc stops",
                    "ターン実行中 · Enter で即時送達 · Esc で中断",
                ),
                Color::Yellow,
            )
        } else {
            (
                self.language.text("轮到你", "Your turn", "あなたの番"),
                Color::Green,
            )
        }
    }

    /// 本轮在跑时排队的提示词先带「待发」标记回显；此前它和真正发出去的长得
    /// 一模一样，用户分不清哪句还没发。
    pub(super) fn queued_prompt_row(&self, text: &str) -> String {
        format!(
            "You: {}{}",
            self.language
                .text("［待发］ ", "[queued] ", "［送信待ち］ "),
            terminal_safe_text(text)
        )
    }

    /// 送进正在跑的那一轮却没赶上（任务在下一次调模型前就结束了）的插话：
    /// 说一声，然后排队，本轮结束后照常发出，一句话都不丢。
    pub(super) fn requeue_steering(&mut self, text: String) {
        self.append_transcript(format!(
            "System: {}",
            self.language.text(
                "上一条插话没赶上本轮，已排队，本轮结束后发送",
                "The last message missed this turn; queued to send when it finishes",
                "直前のメッセージはこのターンに間に合わず、終了後に送信するためキューに入れました",
            )
        ));
        self.queued_prompts.push_back(QueuedPrompt {
            text,
            attachments: Vec::new(),
            from_phone: false,
        });
    }

    /// 排队的提示词真正发出时，把那行「待发」标记去掉。
    pub(super) fn mark_queued_prompt_sent(&mut self, text: &str) {
        let queued = self.queued_prompt_row(text);
        if let Some(line) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|line| **line == queued)
        {
            *line = format!("You: {}", terminal_safe_text(text));
            self.refresh_transcript_height();
        }
    }
}

/// 折叠后的聊天区视图：只给显示层用，记录本身一行不动。
pub(super) struct FoldedTranscript {
    pub(super) rows: Vec<String>,
    /// 每条原始记录在折叠视图里的位置；被收起的工具行指向替它们说话的汇总行，
    /// 搜索跳转命中它们时就落在汇总上。
    pub(super) index_of: Vec<usize>,
}

/// 每段连续的工具行（模型说一句话、轮次收尾都会切段）在显示层收起：不是最新那段
/// 的一律只留一行汇总，只有一条时原样保留；最新那段最多 [`LAST_TOOL_RUN_ROWS`]
/// 行，一行汇总加最新一条。`expanded` 为真时原样返回（`/tools`）。
pub(super) fn fold_tool_rows(
    entries: &[String],
    expanded: bool,
    language: Language,
) -> FoldedTranscript {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0;
    while cursor < entries.len() {
        if tool_row_marker(&entries[cursor]).is_none() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < entries.len() && tool_row_marker(&entries[cursor]).is_some() {
            cursor += 1;
        }
        runs.push((start, cursor));
    }
    let last_run = runs.last().copied();
    let mut rows = Vec::with_capacity(entries.len());
    let mut index_of = Vec::with_capacity(entries.len());
    let mut next_run = 0;
    let mut cursor = 0;
    while cursor < entries.len() {
        let Some(&(start, end)) = runs.get(next_run).filter(|(start, _)| *start == cursor) else {
            index_of.push(rows.len());
            rows.push(entries[cursor].clone());
            cursor += 1;
            continue;
        };
        next_run += 1;
        cursor = end;
        let run = &entries[start..end];
        let is_last = last_run == Some((start, end));
        let hidden = if expanded || run.len() == 1 {
            0
        } else if !is_last {
            run.len()
        } else if run.len() <= LAST_TOOL_RUN_ROWS {
            0
        } else {
            run.len() - (LAST_TOOL_RUN_ROWS - 1)
        };
        if hidden > 0 {
            let summary_index = rows.len();
            rows.push(fold_summary(&run[..hidden], is_last, language));
            index_of.extend(std::iter::repeat_n(summary_index, hidden));
        }
        for line in &run[hidden..] {
            index_of.push(rows.len());
            rows.push(line.clone());
        }
    }
    FoldedTranscript { rows, index_of }
}

/// `· ⋯ 已收起 12 条工具调用 · read_file×3 · run_command×9 · 2 失败 · /tools 展开`
/// 「/tools 展开」只挂在最新那段的汇总上，早先的段不重复提示。
fn fold_summary(hidden: &[String], with_hint: bool, language: Language) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failed = 0;
    for line in hidden {
        if let Some(name) = tool_row_name(line) {
            *counts.entry(name).or_default() += 1;
        }
        if tool_row_marker(line) == Some(TOOL_FAILED) {
            failed += 1;
        }
    }
    let mut summary = language
        .text(
            "已收起 {n} 条工具调用",
            "{n} earlier tool calls folded",
            "ツール呼び出し {n} 件を折りたたみ",
        )
        .replace("{n}", &hidden.len().to_string());
    for (name, count) in counts {
        summary.push_str(&format!(" · {name}×{count}"));
    }
    if failed > 0 {
        summary.push_str(
            &language
                .text(" · {k} 失败", " · {k} failed", " · {k} 失敗")
                .replace("{k}", &failed.to_string()),
        );
    }
    if with_hint {
        summary.push_str(language.text(
            " · /tools 展开",
            " · /tools to expand",
            " · /tools で展開",
        ));
    }
    format!("· {TOOL_FOLD_MARKER} {summary}")
}

impl App {
    /// 聊天区实际画出来的记录：工具行按规则收起，其余原样。
    pub(super) fn display_transcript(&self) -> FoldedTranscript {
        fold_tool_rows(&self.transcript, self.tool_rows_expanded, self.language)
    }

    /// 记录变了之后按折叠视图重算总高度；滚动、跟随底部全靠它。
    pub(super) fn refresh_transcript_height(&mut self) {
        self.transcript_height =
            rendered_transcript_height(&self.display_transcript().rows, self.transcript_width);
    }

    /// `/tools`：临时展开或收起全部工具行。返回给用户看的一句反馈。
    pub(super) fn toggle_tool_rows(&mut self) -> String {
        self.tool_rows_expanded = !self.tool_rows_expanded;
        self.refresh_transcript_height();
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
        format!(
            "System: {}",
            if self.tool_rows_expanded {
                self.language.text(
                    "工具调用行已全部展开 · 再输入 /tools 收起",
                    "All tool-call rows expanded · /tools again to fold",
                    "ツール行をすべて展開 · もう一度 /tools で折りたたみ",
                )
            } else {
                self.language.text(
                    "工具调用行已收起，只留最新几条 · 再输入 /tools 展开",
                    "Tool-call rows folded to the latest few · /tools again to expand",
                    "ツール行を最新数件に折りたたみ · もう一度 /tools で展開",
                )
            }
        )
    }

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
            self.refresh_transcript_height();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 交替的 run_command / read_file，每第三条失败。
    fn tool_rows(count: usize) -> Vec<String> {
        (0..count)
            .map(|index| {
                tool_line(
                    if index % 3 == 2 {
                        TOOL_FAILED
                    } else {
                        TOOL_DONE
                    },
                    if index % 2 == 0 {
                        "run_command"
                    } else {
                        "read_file"
                    },
                    None,
                )
            })
            .collect()
    }

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
    fn tool_rows_are_recognised_by_marker_and_name() {
        assert_eq!(
            tool_row_marker("· … run_command · cargo test"),
            Some(TOOL_PENDING)
        );
        assert_eq!(
            tool_row_name("· ✗ run_command · cargo test -p"),
            Some("run_command")
        );
        assert_eq!(tool_row_name("· ✓ read_file"), Some("read_file"));
        // 账目行、汇总行、正文都不是工具行。
        assert_eq!(tool_row_marker("· total 3.2s"), None);
        assert_eq!(tool_row_marker("· ⋯ 4 earlier tool calls folded"), None);
        assert_eq!(tool_row_marker("WillDeep: ✓ done"), None);
    }

    #[test]
    fn one_or_two_row_runs_stay_as_they_are() {
        for count in 1..=LAST_TOOL_RUN_ROWS {
            let entries = tool_rows(count);
            let folded = fold_tool_rows(&entries, false, Language::En);
            assert_eq!(folded.rows, entries);
            assert_eq!(folded.index_of, (0..count).collect::<Vec<_>>());
        }
    }

    #[test]
    fn the_latest_run_keeps_one_summary_plus_its_newest_row() {
        let mut entries = vec!["You: go".to_owned()];
        entries.extend(tool_rows(8));
        entries.push("WillDeep: done".to_owned());
        let folded = fold_tool_rows(&entries, false, Language::En);
        assert_eq!(
            folded.rows.len(),
            1 + LAST_TOOL_RUN_ROWS + 1,
            "{:?}",
            folded.rows
        );
        assert_eq!(folded.rows[0], "You: go");
        let summary = &folded.rows[1];
        assert!(
            summary.starts_with("· ⋯ 7 earlier tool calls folded"),
            "{summary}"
        );
        assert!(
            summary.contains(" · read_file×3 · run_command×4"),
            "{summary}"
        );
        assert!(summary.contains(" · 2 failed"), "{summary}");
        assert!(summary.ends_with(" · /tools to expand"), "{summary}");
        assert_eq!(folded.rows[2], entries[8], "the newest row stays visible");
        assert_eq!(folded.rows[3], "WillDeep: done");
        assert_eq!(folded.index_of, vec![0, 1, 1, 1, 1, 1, 1, 1, 2, 3]);
    }

    #[test]
    fn earlier_runs_collapse_to_one_line_and_expanded_view_shows_all() {
        let mut entries = tool_rows(5);
        entries.push("WillDeep: half way".to_owned());
        entries.extend(tool_rows(3));
        let folded = fold_tool_rows(&entries, false, Language::ZhCn);
        assert_eq!(
            folded.rows.len(),
            1 + 1 + LAST_TOOL_RUN_ROWS,
            "{:?}",
            folded.rows
        );
        assert!(
            folded.rows[0].starts_with("· ⋯ 已收起 5 条工具调用"),
            "{}",
            folded.rows[0]
        );
        assert!(
            !folded.rows[0].contains("/tools"),
            "the hint belongs to the latest run only: {}",
            folded.rows[0]
        );
        assert_eq!(folded.rows[1], "WillDeep: half way");
        assert!(
            folded.rows[2].starts_with("· ⋯ 已收起 2 条工具调用")
                && folded.rows[2].ends_with("/tools 展开"),
            "{}",
            folded.rows[2]
        );
        assert_eq!(folded.rows[3], entries[8]);
        assert_eq!(folded.index_of, vec![0, 0, 0, 0, 0, 1, 2, 2, 3]);

        // 早先的段只有一条时原样保留，不值得配一行汇总。
        let mut single = tool_rows(1);
        single.push("WillDeep: x".to_owned());
        single.extend(tool_rows(3));
        assert_eq!(
            fold_tool_rows(&single, false, Language::ZhCn).rows[0],
            single[0]
        );

        assert_eq!(fold_tool_rows(&entries, true, Language::ZhCn).rows, entries);
    }

    #[test]
    fn tools_command_toggles_folding_and_reports() {
        let mut app = App::new(Vec::new(), Language::En);
        for line in tool_rows(9) {
            app.append_transcript(line);
        }
        assert_eq!(app.display_transcript().rows.len(), LAST_TOOL_RUN_ROWS);

        assert!(app.handle_slash_command("/tools", &SkillCatalog::default()));
        assert!(app.tool_rows_expanded);
        assert!(app.transcript.last().unwrap().starts_with("System: "));
        assert_eq!(app.display_transcript().rows.len(), 9 + 1);

        assert!(app.handle_slash_command("/tools", &SkillCatalog::default()));
        assert!(!app.tool_rows_expanded);
        assert_eq!(app.display_transcript().rows.len(), LAST_TOOL_RUN_ROWS + 2);
    }

    #[test]
    fn transient_line_switches_source_and_keeps_only_the_tail() {
        let mut app = App::new(Vec::new(), Language::En);
        app.stream_transient(StreamKind::Reasoning, "先看日志");
        app.stream_transient(StreamKind::Reasoning, "，再改");
        assert_eq!(app.transient_thought.as_deref(), Some("先看日志，再改"));
        assert_eq!(app.transient_label(), "thinking");
        assert_eq!(app.activity_line, "Thinking");

        // 正文一来，思维链缓冲清掉，标签跟着换，两种文字不拼在一起。
        app.stream_transient(StreamKind::Reply, "好的");
        assert_eq!(app.transient_thought.as_deref(), Some("好的"));
        assert_eq!(app.transient_label(), "replying");
        assert_eq!(app.activity_line, "Receiving reply");

        app.stream_transient(StreamKind::Reply, &"x".repeat(THOUGHT_PREVIEW_CHARS * 2));
        assert_eq!(
            app.transient_thought
                .as_deref()
                .map(|value| value.chars().count()),
            Some(THOUGHT_PREVIEW_CHARS)
        );
    }

    #[test]
    fn terminal_safe_text_expands_tabs_and_keeps_newlines() {
        assert_eq!(terminal_safe_text("a\tb\nc\td"), "a   b\nc   d");
        assert_eq!(
            terminal_safe_text("\tif err != nil {"),
            "    if err != nil {"
        );
        assert_eq!(terminal_safe_text("x\r\u{1b}[0m"), "x\\u{1b}[0m");
    }

    #[test]
    fn thinking_tail_fits_the_last_rows_and_collapses_whitespace() {
        let prefix = "· thinking: ";
        let text = "word ".repeat(200);
        let tail = tail_within_rows(prefix, &text, 40, THINKING_ROWS);
        assert!(visual_lines(&format!("{prefix}{tail}"), 40) <= THINKING_ROWS);
        assert!(tail.starts_with('…'), "{tail}");
        assert!(tail.ends_with("word"), "{tail}");
        assert_eq!(
            tail_within_rows(prefix, "short\n\n  thought", 40, THINKING_ROWS),
            "short thought"
        );
    }

    #[test]
    fn transient_row_is_capped_grey_thinking_until_the_reply_starts() {
        let mut app = App::new(Vec::new(), Language::En);
        app.transcript_width = 30;
        app.stream_transient(StreamKind::Reasoning, &"think\t".repeat(100));
        let row = app.transient_row().unwrap();
        assert!(row.starts_with("· thinking: "), "{row}");
        assert!(!row.contains('\t'));
        assert!(visual_lines(&row, 30) <= THINKING_ROWS);

        // 正文一开始，思维链整段消失，只剩回复预览。
        app.stream_transient(StreamKind::Reply, "Hello");
        assert_eq!(
            app.transient_row().as_deref(),
            Some("WillDeep · replying: Hello")
        );
    }

    #[test]
    fn version_command_reports_cli_and_runtime_and_flags_a_mismatch() {
        let mut app = App::new(Vec::new(), Language::En);
        assert!(app.handle_slash_command("/version", &SkillCatalog::default()));
        let line = app.transcript.last().cloned().unwrap();
        assert_eq!(
            line,
            format!(
                "System: CLI {} · Runtime not connected",
                willdeep_core::VERSION
            )
        );

        app.runtime_version = Some(willdeep_core::VERSION.to_owned());
        assert!(!app.version_report().contains("versions differ"));

        app.runtime_version = Some("0.1.0".to_owned());
        let report = app.version_report();
        assert!(report.contains("Runtime 0.1.0"), "{report}");
        assert!(report.contains("versions differ"), "{report}");
    }

    #[test]
    fn undelivered_steering_is_requeued_with_a_notice() {
        let mut app = App::new(Vec::new(), Language::En);
        app.requeue_steering("先别删".to_owned());
        assert_eq!(app.queued_prompts.len(), 1);
        assert_eq!(app.queued_prompts[0].text, "先别删");
        assert!(app.transcript[0].starts_with("System: The last message missed this turn"));
    }

    #[test]
    fn queued_prompts_are_marked_until_they_are_sent() {
        let mut app = App::new(Vec::new(), Language::En);
        let row = app.queued_prompt_row("fix it");
        app.append_transcript(row);
        assert_eq!(app.transcript, vec!["You: [queued] fix it"]);
        app.mark_queued_prompt_sent("fix it");
        assert_eq!(app.transcript, vec!["You: fix it"]);
        // 找不到对应的待发行时什么都不动。
        app.mark_queued_prompt_sent("other");
        assert_eq!(app.transcript, vec!["You: fix it"]);
    }

    #[test]
    fn turn_end_leaves_a_divider_rings_the_bell_and_flips_the_composer_state() {
        let mut app = App::new(Vec::new(), Language::En);
        assert_eq!(app.composer_state().0, "Your turn");
        app.begin_turn(false, "working".to_owned());
        assert!(app.composer_state().0.contains("Enter steers now"));
        assert!(!app.bell_pending);

        app.note_tool_requested("read_file", None);
        app.append_turn_stats(None);
        let divider = app.transcript.last().cloned().unwrap();
        assert!(divider.starts_with(TURN_DIVIDER_PREFIX), "{divider}");
        assert!(divider.contains("turn finished"), "{divider}");
        assert!(divider.ends_with("your turn ──"), "{divider}");

        app.finish_turn();
        assert!(std::mem::take(&mut app.bell_pending));
        assert_eq!(app.composer_state().0, "Your turn");
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
