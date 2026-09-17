//! `/permissions`：在终端里切换审批档位。
//!
//! 档位是这个 TUI 进程的选择：进程内 Agent（`/local`）立即换档，Runtime 会话
//! 经 `session.update_approval_mode` 同步——包括这一轮已经在跑的任务。退出
//! 后不写回配置，下次启动回到 `agent.approval`；`/permissions default <档位>`
//! 才会改配置。
//!
//! 两道防手滑：Shift+Tab 只在「严格 → 智能 → 工作区可写」之间循环，永远不会
//! 落到完全访问；选完全访问必须在确认页再按一次 `y`。

use super::*;
use willdeep_core::ApprovalMode;
use willdeep_core::tools::{ApprovalSource, ApprovalTrace};

const COMMAND: &str = "/permissions";
const COMMAND_ALIAS: &str = "/permission-mode";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PermissionCommand {
    Open,
    Switch(ApprovalMode),
    SaveDefault(ApprovalMode),
    Usage,
}

pub(super) struct PermissionPickerState {
    pub(super) selected: usize,
    /// 停在完全访问的确认页上。
    pub(super) confirming: bool,
}

pub(super) enum PermissionPickerAction {
    None,
    Close,
    Apply(ApprovalMode),
}

pub(super) fn parse(prompt: &str) -> Option<PermissionCommand> {
    let value = prompt.trim();
    let arguments = [COMMAND, COMMAND_ALIAS].iter().find_map(|head| {
        let rest = value.strip_prefix(head)?;
        (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest.trim())
    })?;
    let mut words = arguments.split_whitespace();
    let command = match (words.next(), words.next(), words.next()) {
        (None, _, _) => PermissionCommand::Open,
        (Some("default"), Some(mode), None) => selectable(mode)
            .map(PermissionCommand::SaveDefault)
            .unwrap_or(PermissionCommand::Usage),
        (Some(mode), None, _) => selectable(mode)
            .map(PermissionCommand::Switch)
            .unwrap_or(PermissionCommand::Usage),
        _ => PermissionCommand::Usage,
    };
    Some(command)
}

/// `read-only` 是工作区策略，不在会话里选。
fn selectable(value: &str) -> Option<ApprovalMode> {
    ApprovalMode::parse(value).filter(|mode| ApprovalMode::SELECTABLE.contains(mode))
}

pub(super) fn usage(language: Language) -> String {
    format!(
        "System: {}",
        language.text(
            "用法：/permissions [strict|smart|workspace-write|full-access]，或 /permissions default <档位> 写入配置；Shift+Tab 在前三档间切换",
            "Usage: /permissions [strict|smart|workspace-write|full-access], or /permissions default <mode> to save it; Shift+Tab cycles the first three",
            "使用法: /permissions [strict|smart|workspace-write|full-access]、設定に保存するには /permissions default <モード>。Shift+Tab で前の 3 つを切替",
        )
    )
}

pub(super) fn label(mode: ApprovalMode, language: Language) -> &'static str {
    match mode {
        ApprovalMode::ReadOnly => language.text("只读", "Read-only", "読み取り専用"),
        ApprovalMode::Strict => language.text("严格模式", "Strict", "厳格"),
        ApprovalMode::Smart => language.text("智能审核", "Smart review", "スマート審査"),
        ApprovalMode::WorkspaceAccess => {
            language.text("工作区可写", "Workspace write", "ワークスペース書き込み")
        }
        ApprovalMode::FullAccess => language.text("完全访问", "Full access", "フルアクセス"),
    }
}

fn summary(mode: ApprovalMode, language: Language) -> &'static str {
    match mode {
        ApprovalMode::ReadOnly => language.text(
            "只能读，写入与命令一律拒绝",
            "Reads only; writes and commands are refused",
            "読み取りのみ。書き込みとコマンドは拒否",
        ),
        ApprovalMode::Strict => language.text(
            "写文件、跑命令、联网都先问你",
            "Ask before every write, command, or network action",
            "書き込み・コマンド・通信のたびに確認",
        ),
        ApprovalMode::Smart => language.text(
            "工作区内写入免审；命令先过规则和 AI 审核，拿不准才问",
            "Workspace writes run; commands go through rules and AI review, ask only when unsure",
            "ワークスペース内の書き込みは自動。コマンドはルールと AI 審査、判断できない時だけ確認",
        ),
        ApprovalMode::WorkspaceAccess => language.text(
            "工作区内写入和围栏内命令免审，不过 AI；联网、远程操作仍问",
            "Workspace writes and fenced commands run without AI review; network and remote actions ask",
            "ワークスペース内の書き込みと囲い内のコマンドは AI 審査なしで実行。通信・リモート操作は確認",
        ),
        ApprovalMode::FullAccess => language.text(
            "联网、任意文件、MCP 都不再问；只有 rm -rf 这类破坏性命令仍问",
            "Network, any file, and MCP run without asking; only destructive commands like rm -rf still ask",
            "通信・任意ファイル・MCP も確認なし。rm -rf などの破壊的コマンドのみ確認",
        ),
    }
}

impl App {
    pub(super) fn open_permission_picker(&mut self, confirm_full_access: bool) {
        self.palette = None;
        self.search = None;
        self.session_picker = None;
        self.model_picker = None;
        let selected = if confirm_full_access {
            ApprovalMode::FullAccess
        } else {
            self.approval_mode
        };
        self.permission_picker = Some(PermissionPickerState {
            selected: ApprovalMode::SELECTABLE
                .iter()
                .position(|mode| *mode == selected)
                .unwrap_or(1),
            confirming: confirm_full_access,
        });
    }

    pub(super) fn handle_permission_picker_key(&mut self, key: KeyEvent) -> PermissionPickerAction {
        let Some(picker) = self.permission_picker.as_mut() else {
            return PermissionPickerAction::None;
        };
        let count = ApprovalMode::SELECTABLE.len();
        if picker.confirming {
            return match key.code {
                KeyCode::Char('y' | 'Y') => PermissionPickerAction::Apply(ApprovalMode::FullAccess),
                KeyCode::Esc | KeyCode::Char('n' | 'N') | KeyCode::Backspace => {
                    picker.confirming = false;
                    PermissionPickerAction::None
                }
                _ => PermissionPickerAction::None,
            };
        }
        match key.code {
            KeyCode::Esc => PermissionPickerAction::Close,
            KeyCode::Up | KeyCode::BackTab => {
                picker.selected = picker.selected.checked_sub(1).unwrap_or(count - 1);
                PermissionPickerAction::None
            }
            KeyCode::Down | KeyCode::Tab => {
                picker.selected = (picker.selected + 1) % count;
                PermissionPickerAction::None
            }
            KeyCode::Char(digit @ '1'..='4') => {
                picker.selected = digit as usize - '1' as usize;
                choose(picker)
            }
            KeyCode::Enter => choose(picker),
            _ => PermissionPickerAction::None,
        }
    }
}

fn choose(picker: &mut PermissionPickerState) -> PermissionPickerAction {
    let mode = ApprovalMode::SELECTABLE[picker.selected];
    if mode == ApprovalMode::FullAccess {
        picker.confirming = true;
        return PermissionPickerAction::None;
    }
    PermissionPickerAction::Apply(mode)
}

pub(super) fn mode_color(mode: ApprovalMode) -> Color {
    match mode {
        ApprovalMode::ReadOnly | ApprovalMode::Strict => Color::LightBlue,
        ApprovalMode::Smart => Color::LightGreen,
        ApprovalMode::WorkspaceAccess => Color::Yellow,
        ApprovalMode::FullAccess => Color::LightRed,
    }
}

pub(super) fn render_permission_picker(f: &mut ratatui::Frame<'_>, app: &mut App) {
    let Some(picker) = app.permission_picker.as_ref() else {
        return;
    };
    let language = app.language;
    let mut lines = Vec::new();
    let title = if picker.confirming {
        lines.push(Line::styled(
            language.text(
                "确认切换到「完全访问」？",
                "Switch to Full access?",
                "「フルアクセス」に切り替えますか？",
            ),
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::raw(summary(ApprovalMode::FullAccess, language)));
        lines.push(Line::raw(language.text(
            "Agent 可以联网、读写工作区外的文件、调用任意 MCP，都不会再问你。",
            "The Agent can reach the network, read and write files outside the workspace, and call any MCP tool without asking.",
            "Agent は確認なしで通信し、ワークスペース外のファイルを読み書きし、任意の MCP を呼び出せます。",
        )));
        lines.push(Line::raw(language.text(
            "只对当前 TUI 生效，退出后回到配置里的默认档。",
            "Applies to this TUI only; the configured default returns on restart.",
            "この TUI のみ有効。再起動で設定の既定値に戻ります。",
        )));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            language.text(
                "y 确认 · n / Esc 返回",
                "y confirm · n / Esc back",
                "y で確定 · n / Esc で戻る",
            ),
            Style::default().fg(Color::Yellow),
        ));
        language.text("完全访问", "Full access", "フルアクセス")
    } else {
        for (index, mode) in ApprovalMode::SELECTABLE.iter().enumerate() {
            let selected = index == picker.selected;
            let current = *mode == app.approval_mode;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(mode_color(*mode))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(mode_color(*mode))
            };
            lines.push(Line::styled(
                format!(
                    "{} {}. {}{}",
                    if selected { "▶" } else { " " },
                    index + 1,
                    label(*mode, language),
                    if current {
                        language.text(" · 当前", " · current", " · 現在")
                    } else {
                        ""
                    }
                ),
                style,
            ));
            lines.push(Line::styled(
                format!("     {}", summary(*mode, language)),
                Style::default().fg(Color::DarkGray),
            ));
        }
        language.text(
            "审批模式 · ↑/↓ · 1-4 · Enter · Esc",
            "Approval mode · ↑/↓ · 1-4 · Enter · Esc",
            "承認モード · ↑/↓ · 1-4 · Enter · Esc",
        )
    };
    let width = f.area().width.min(96);
    let height = f.area().height.min(lines.len() as u16 + 2);
    let popup = centered_rect(width, height, f.area());
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if picker.confirming {
                    Color::LightRed
                } else {
                    Color::LightCyan
                })),
        ),
        popup,
    );
}

/// 切档：先换进程内 Agent，再同步 Runtime 会话；同步失败就把本地换回去，
/// 不留下「界面说换了、Runtime 还在旧档」的状态。
pub(super) async fn apply(
    mode: ApprovalMode,
    app: &mut App,
    session: &Session,
    runtime: &TuiRuntime,
    agent: &Arc<Agent>,
) -> Result<String> {
    let handle = agent.approval_mode_handle();
    let previous = handle.set(mode);
    if session.runtime_managed
        && let Err(error) =
            crate::daemon::update_remote_session_approval_mode(&runtime.home, session.id, mode)
                .await
    {
        handle.set(previous);
        return Err(error.context("update Runtime Session approval mode"));
    }
    // 还没进过 Runtime 的会话，在第一次提交时再同步，免得为了切档凭空登记
    // 一条空会话。
    app.approval_synced_session = session.runtime_managed.then_some(session.id);
    app.approval_mode = mode;
    crate::harness::record_approval_trace(
        &runtime.home.join("approvals.jsonl"),
        &ApprovalTrace {
            command: mode.as_str().to_owned(),
            source: ApprovalSource::ModeChange,
            detail: format!(
                "operator switched approval mode from {} to {} in the TUI",
                previous.as_str(),
                mode.as_str()
            ),
        },
    );
    let language = app.language;
    let mut message = format!(
        "System: {}：{} · {}",
        language.text("审批模式已切换", "Approval mode", "承認モード"),
        label(mode, language),
        summary(mode, language)
    );
    if mode == ApprovalMode::WorkspaceAccess && !willdeep_core::sandbox::available() {
        message.push_str(language.text(
            "\n  这台机器没有写入围栏（sandbox-exec / bwrap），无法保证命令留在工作区内，未分类的命令仍会问你。",
            "\n  No write fence (sandbox-exec / bwrap) on this machine, so unclassified commands will still ask.",
            "\n  このマシンには書き込み囲い（sandbox-exec / bwrap）がないため、未分類のコマンドは引き続き確認します。",
        ));
    }
    Ok(message)
}

/// 提交 Runtime 轮次前调用：会话第一次进 Runtime、或切换过会话之后，把当前
/// 档位带过去。
pub(super) async fn sync_session(
    app: &mut App,
    session: &Session,
    runtime: &TuiRuntime,
) -> Result<()> {
    if app.approval_synced_session == Some(session.id) {
        return Ok(());
    }
    crate::daemon::update_remote_session_approval_mode(
        &runtime.home,
        session.id,
        app.approval_mode,
    )
    .await
    .context("sync approval mode to the Runtime Session")?;
    app.approval_synced_session = Some(session.id);
    Ok(())
}

pub(super) fn save_default(mode: ApprovalMode, runtime: &TuiRuntime) -> Result<String> {
    let path = runtime
        .runtime_submit
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::config::default_config_path)?;
    crate::model_routing::save_default_approval(&path, mode)?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_spellings_and_rejects_neighbours() {
        assert_eq!(parse("/permissions"), Some(PermissionCommand::Open));
        assert_eq!(
            parse(" /permission-mode  full "),
            Some(PermissionCommand::Switch(ApprovalMode::FullAccess))
        );
        assert_eq!(
            parse("/permissions default workspace-write"),
            Some(PermissionCommand::SaveDefault(
                ApprovalMode::WorkspaceAccess
            ))
        );
        assert_eq!(
            parse("/permissions read-only"),
            Some(PermissionCommand::Usage)
        );
        assert_eq!(
            parse("/permissions smart extra"),
            Some(PermissionCommand::Usage)
        );
        assert_eq!(parse("/permissionsx"), None);
        assert_eq!(parse("/model"), None);
    }

    #[test]
    fn picker_needs_a_second_keypress_for_full_access() {
        let mut app = App::new(Vec::new(), Language::En);
        app.open_permission_picker(false);
        let press = |app: &mut App, code| {
            app.handle_permission_picker_key(KeyEvent::new(code, KeyModifiers::NONE))
        };
        assert!(matches!(
            press(&mut app, KeyCode::Char('4')),
            PermissionPickerAction::None
        ));
        assert!(app.permission_picker.as_ref().unwrap().confirming);
        assert!(matches!(
            press(&mut app, KeyCode::Esc),
            PermissionPickerAction::None
        ));
        assert!(!app.permission_picker.as_ref().unwrap().confirming);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            press(&mut app, KeyCode::Char('y')),
            PermissionPickerAction::Apply(ApprovalMode::FullAccess)
        ));

        app.open_permission_picker(false);
        assert!(matches!(
            press(&mut app, KeyCode::Char('3')),
            PermissionPickerAction::Apply(ApprovalMode::WorkspaceAccess)
        ));
    }
}
