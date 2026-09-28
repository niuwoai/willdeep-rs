//! Ctrl+G：把输入框交给外部编辑器，行为照 `crontab -e` / `git commit`。
//!
//! 把当前输入写进临时文件，暂时交出终端，阻塞等编辑器退出，再读回来填进
//! 输入框——只填不发，用户看一眼再按 Enter。GUI 编辑器必须用会阻塞的写法
//! （`mate -w`、`code --wait`、`qe -w`），否则一打开就返回，读回的还是原文。

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// 查找顺序：本工具专用变量优先，然后与 crontab / git 一致。
const EDITOR_ENV_VARS: [&str; 3] = ["WILLDEEP_EDITOR", "VISUAL", "EDITOR"];
#[cfg(windows)]
const FALLBACK_EDITOR: &str = "notepad";
#[cfg(not(windows))]
const FALLBACK_EDITOR: &str = "vi";
/// shell 找不到命令时的退出码。
const COMMAND_NOT_FOUND_EXIT: i32 = 127;
const TEMP_FILE_PREFIX: &str = "willdeep-prompt-";
/// `.md` 让编辑器按 Markdown 高亮。
const TEMP_FILE_SUFFIX: &str = ".md";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EditorOutcome {
    /// 读回了新内容，替换输入框。
    Replaced(String),
    /// 内容没变。
    Unchanged,
    /// 清空了原本非空的输入：按 git 的习惯当作放弃，保留原文。
    Emptied,
    /// 编辑器非 0 退出（vim 的 `:cq`）或在等待时按了 Ctrl+C：放弃。
    Cancelled(Option<i32>),
    /// 编辑器命令不存在。
    NotFound,
}

/// 从环境里挑编辑器命令；空值跳过，全没有就用系统兜底。
pub(super) fn resolve_editor(lookup: impl Fn(&str) -> Option<String>) -> String {
    EDITOR_ENV_VARS
        .iter()
        .filter_map(|name| lookup(name))
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| FALLBACK_EDITOR.to_owned())
}

/// 编辑器写回的内容怎么落到输入框。编辑器几乎都会在末尾补一个换行，去掉
/// 末尾空行；开头与中间的空白原样保留。
pub(super) fn interpret_edit(original: &str, edited: &str) -> EditorOutcome {
    let edited = edited.trim_end_matches(['\n', '\r']);
    if edited == original.trim_end_matches(['\n', '\r']) {
        EditorOutcome::Unchanged
    } else if edited.trim().is_empty() && !original.trim().is_empty() {
        EditorOutcome::Emptied
    } else {
        EditorOutcome::Replaced(edited.to_owned())
    }
}

/// 构造编辑器进程。编辑器命令可以带参数（`mate -w`、`code --wait`），交给
/// shell 解析，文件路径作为位置参数传进去，路径里的空格不用另行转义——
/// 与 git 调用 `$GIT_EDITOR` 的方式一致。
fn editor_process(editor: &str, file: &Path) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut parts = editor.split_whitespace();
        let mut command = tokio::process::Command::new(parts.next().unwrap_or(FALLBACK_EDITOR));
        command.args(parts).arg(file);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(editor)
            .arg(file);
        command
    }
}

fn temp_file_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "{TEMP_FILE_PREFIX}{}{TEMP_FILE_SUFFIX}",
        uuid::Uuid::new_v4()
    ))
}

/// 退出 TUI 的终端接管，把屏幕还给编辑器。
fn suspend(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        DisableMouseCapture,
        DisableBracketedPaste,
        terminal::LeaveAlternateScreen
    )?;
    term.show_cursor()?;
    Ok(())
}

fn resume(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        term.backend_mut(),
        terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // 备用屏是新的，ratatui 的上一帧缓冲不再可信，整屏重画。
    term.clear()?;
    Ok(())
}

pub(super) struct EditorSession {
    pub(super) outcome: EditorOutcome,
    /// 临时文件没删掉：路径和原因，交给界面提示，不静默留一份草稿在磁盘上。
    pub(super) leftover: Option<String>,
}

/// 用外部编辑器编辑 `original`。
///
/// `events` 会被换成一个新的 `EventStream`：旧的那个在后台线程里读着 stdin，
/// 不先停掉它，vim 收到的按键会被它抢走一部分。
pub(super) async fn edit(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: &mut EventStream,
    editor: &str,
    original: &str,
    workspace: Option<&Path>,
) -> Result<EditorSession> {
    let file = temp_file_path();
    std::fs::write(&file, original)
        .with_context(|| format!("write prompt draft to {}", file.display()))?;
    drop(std::mem::replace(events, EventStream::new()));
    suspend(term)?;
    let status = run_editor(editor, &file, workspace).await;
    // 不管编辑器怎么结束，终端都要先收回来，否则错误提示没地方显示。
    let resumed = resume(term);
    let edited = std::fs::read_to_string(&file);
    let leftover = std::fs::remove_file(&file)
        .err()
        .map(|error| format!("{}: {error}", file.display()));
    resumed?;
    let outcome = match status? {
        EditorExit::Interrupted => EditorOutcome::Cancelled(None),
        EditorExit::Code(Some(COMMAND_NOT_FOUND_EXIT)) => EditorOutcome::NotFound,
        EditorExit::Code(Some(0)) => {
            let edited = edited
                .with_context(|| format!("read prompt draft back from {}", file.display()))?;
            interpret_edit(original, &edited)
        }
        EditorExit::Code(code) => EditorOutcome::Cancelled(code),
    };
    Ok(EditorSession { outcome, leftover })
}

enum EditorExit {
    Code(Option<i32>),
    /// GUI 编辑器等待期间终端处在普通模式，Ctrl+C 会发 SIGINT 给整个前台
    /// 进程组。接住它当作放弃，而不是让 WillDeep 跟着被杀、终端留在半残状态。
    Interrupted,
}

async fn run_editor(editor: &str, file: &Path, workspace: Option<&Path>) -> Result<EditorExit> {
    let mut command = editor_process(editor, file);
    if let Some(workspace) = workspace.filter(|path| path.is_dir()) {
        command.current_dir(workspace);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(EditorExit::Code(Some(COMMAND_NOT_FOUND_EXIT)));
        }
        Err(error) => return Err(error).with_context(|| format!("launch editor `{editor}`")),
    };
    tokio::select! {
        status = child.wait() => {
            let status = status.with_context(|| format!("wait for editor `{editor}`"))?;
            Ok(EditorExit::Code(status.code()))
        }
        _ = tokio::signal::ctrl_c() => {
            // 编辑器可能已经先一步退出了，这时 kill 报错无关紧要；放弃的结论不变。
            let _ = child.kill().await;
            Ok(EditorExit::Interrupted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_editor_follows_willdeep_visual_editor_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            resolve_editor(env(&[
                ("EDITOR", "vim"),
                ("VISUAL", "mate -w"),
                ("WILLDEEP_EDITOR", "qe -w")
            ])),
            "qe -w"
        );
        assert_eq!(
            resolve_editor(env(&[("EDITOR", "vim"), ("VISUAL", "mate -w")])),
            "mate -w"
        );
        assert_eq!(
            resolve_editor(env(&[("WILLDEEP_EDITOR", "  "), ("EDITOR", "nvim")])),
            "nvim"
        );
        assert_eq!(resolve_editor(env(&[])), FALLBACK_EDITOR);
    }

    #[test]
    fn interpret_edit_strips_trailing_newlines_and_detects_no_change() {
        assert_eq!(interpret_edit("你好", "你好\n"), EditorOutcome::Unchanged);
        assert_eq!(
            interpret_edit("", "  第一行\n\n第二行\n\n"),
            EditorOutcome::Replaced("  第一行\n\n第二行".to_owned())
        );
        assert_eq!(interpret_edit("", "\n"), EditorOutcome::Unchanged);
    }

    #[test]
    fn interpret_edit_treats_clearing_a_draft_as_giving_up() {
        assert_eq!(interpret_edit("草稿", "\n  \n"), EditorOutcome::Emptied);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn editor_command_with_arguments_receives_the_file_path() {
        let dir = std::env::temp_dir().join(format!("willdeep editor {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("draft with space.md");
        std::fs::write(&file, "old").unwrap();
        // 带参数的编辑器命令 + 含空格的路径：shell 解析命令、路径作为整体传入。
        let exit = run_editor("printf '%s' edited >", &file, None)
            .await
            .unwrap();
        assert!(matches!(exit, EditorExit::Code(Some(0))));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited");
        let exit = run_editor("false", &file, None).await.unwrap();
        assert!(matches!(exit, EditorExit::Code(Some(1))));
        let exit = run_editor("willdeep-no-such-editor-xyz", &file, None)
            .await
            .unwrap();
        assert!(matches!(
            exit,
            EditorExit::Code(Some(COMMAND_NOT_FOUND_EXIT))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
