use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Cursor};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use image::{DynamicImage, ImageFormat, RgbaImage};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use regex::RegexBuilder;
use tokio::sync::{mpsc, oneshot};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use willdeep_core::types::Usage;
use willdeep_core::{
    Agent, AgentEvent, ApprovalDecision, Approver, AttentionItem, AttentionSection,
    AttentionSource, BackgroundTaskRegistry, BackgroundTaskSnapshot, BackgroundTaskStatus,
    EventSink, Message, MessageAttachment, RuntimeScopeKind, RuntimeStatus, Session, SessionStore,
    SkillCatalog, StatusRollup, UserQuestion, sort_attention_items,
};

use crate::editor::{DraftAttachment, PromptEditor};
use crate::i18n::Language;
use crate::mobile::{MobilePrompt, RelayBridge, RelayGateway};

mod activity;
mod agent_commands;
mod agent_worktree_ui;
mod command_catalog;
mod daemon_commands;
mod diff_review_ui;
mod dispatch;
mod rendering;
mod runtime_ui;
mod session_commands;
mod sidebar;
mod webapp_commands;
mod workspace_attention;
mod workspace_commands;
use activity::ToolActivity;
use agent_commands::handle_agent_command;
use agent_worktree_ui::render_agent_overlays;
use command_catalog::command_candidates;
use diff_review_ui::*;
use dispatch::{dispatch_compress, dispatch_notification, dispatch_prompt};
use rendering::*;
use runtime_ui::open_remote_gate;
use runtime_ui::{PromptExecution, prompt_execution};
use session_commands::handle_session_command;
use sidebar::{render_attention_detail, render_sidebar};
use workspace_attention::workspace_attention;
use workspace_commands::handle_workspace_command;

pub enum UiMessage {
    Agent(AgentEvent),
    Approval(String, bool, oneshot::Sender<ApprovalDecision>),
    Question(UserQuestion, oneshot::Sender<Option<String>>),
    Finished(Result<willdeep_core::AgentOutcome, willdeep_core::AgentError>),
    Compressed(Result<Vec<Message>, willdeep_core::AgentError>),
    RuntimeNotice(String),
}
pub type TuiSender = mpsc::UnboundedSender<UiMessage>;
pub struct TuiSink {
    pub ui: mpsc::UnboundedSender<UiMessage>,
    pub relay: RelayBridge,
}
#[async_trait]
impl EventSink for TuiSink {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::AssistantText(value) = &event {
            self.relay.publish_assistant(value);
        }
        let _ = self.ui.send(UiMessage::Agent(event));
    }
}
pub struct TuiApprover(pub mpsc::UnboundedSender<UiMessage>);
#[async_trait]
impl Approver for TuiApprover {
    async fn approve(&self, description: &str, always_allow_available: bool) -> ApprovalDecision {
        let (tx, rx) = oneshot::channel();
        if self
            .0
            .send(UiMessage::Approval(
                description.to_owned(),
                always_allow_available,
                tx,
            ))
            .is_err()
        {
            return ApprovalDecision::Deny;
        }
        rx.await.unwrap_or(ApprovalDecision::Deny)
    }
    async fn ask_user(&self, question: UserQuestion) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        if self.0.send(UiMessage::Question(question, tx)).is_err() {
            return None;
        }
        rx.await.unwrap_or(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChatSelectionPoint {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatSelection {
    anchor: ChatSelectionPoint,
    head: ChatSelectionPoint,
}

impl ChatSelection {
    fn ordered_range(self) -> ((usize, usize), (usize, usize)) {
        let (start, inclusive_end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (
            (start.row, start.column),
            (inclusive_end.row, inclusive_end.column.saturating_add(1)),
        )
    }
}

struct App {
    input: PromptEditor,
    transcript: Vec<String>,
    running: bool,
    approval: Option<ApprovalRequest>,
    /// Approvals that arrived while another one was on screen. Without this
    /// queue the newer request overwrote the older one, dropping its oneshot
    /// sender — which the harness reads as a silent Deny the user never saw.
    approval_queue: VecDeque<ApprovalRequest>,
    question: Option<AskDialog>,
    /// Questions waiting behind the one on screen. Same reasoning as
    /// `approval_queue`: overwriting dropped the sender, which the harness
    /// reads as "no answer".
    question_queue: VecDeque<AskDialog>,
    scroll_from_bottom: usize,
    follow_bottom: bool,
    transcript_width: usize,
    transcript_height: usize,
    viewport_height: usize,
    tools: ToolActivity,
    tools_expanded: bool,
    activity_rect: Rect,
    attachments: Vec<DraftAttachment>,
    selected_attachment: usize,
    prompt_rect: Rect,
    prompt_scroll: usize,
    composer_expanded: bool,
    notice: Option<String>,
    goal: Option<String>,
    mobile_gateway: Option<RelayGateway>,
    mobile_qr: Option<String>,
    mobile_queue: VecDeque<String>,
    latest_usage: Usage,
    turn_started: Option<Instant>,
    last_progress_at: Option<Instant>,
    runtime_turn: bool,
    last_elapsed: Option<Duration>,
    context_window: u64,
    context_tokens: u64,
    activity_line: String,
    background_tasks: Vec<BackgroundTaskSnapshot>,
    workspace_attention: Vec<AttentionItem>,
    runtime_attention: Vec<AttentionItem>,
    runtime_gates: Vec<crate::daemon::RemoteGate>,
    /// Version of the Runtime that actually executes tools, when one is
    /// reachable. `None` means no Runtime (everything runs in-process).
    runtime_version: Option<String>,
    /// A version mismatch is announced once in the transcript; the sidebar
    /// warning then stays up on its own.
    runtime_version_warned: bool,
    /// Runtime interactions already turned into a dialog, so a snapshot that
    /// still lists them does not reopen the same card every second.
    surfaced_gates: BTreeSet<uuid::Uuid>,
    runtime_agents: Vec<crate::daemon::tui_bridge::RemoteAgent>,
    runtime_tools: Vec<willdeep_runtime_protocol::RuntimeTool>,
    runtime_artifacts: Vec<willdeep_runtime_protocol::RuntimeArtifact>,
    runtime_agent_selected: usize,
    agent_detail: Option<crate::daemon::tui_bridge::RemoteAgent>,
    agent_detail_scroll: usize,
    agent_detail_action_rects: Vec<(Rect, AgentDetailAction)>,
    worktree_review: Option<crate::daemon::WorktreeReview>,
    diff_review: Option<DiffReviewState>,
    runtime_event_cursor: u64,
    background_notices: VecDeque<String>,
    workspace_status: String,
    progress_log: VecDeque<String>,
    language: Language,
    transient_thought: Option<String>,
    selection_mode: bool,
    chat_selection: Option<ChatSelection>,
    transcript_rows: Vec<String>,
    transcript_render_offset: usize,
    skill_selected: usize,
    skill_menu_dismissed: bool,
    command_selected: usize,
    command_menu_dismissed: bool,
    focus: FocusPane,
    sidebar_visible: bool,
    sidebar_selected: usize,
    sidebar_expanded: [bool; 4],
    sidebar_scroll: usize,
    sidebar_rect: Rect,
    sidebar_wide: bool,
    help_visible: bool,
    sidebar_hits: Vec<(u16, SidebarHit)>,
    sidebar_manual_scroll: bool,
    attention_selected: usize,
    attention_read: BTreeSet<String>,
    task_detail: Option<TaskDetail>,
    task_detail_scroll: usize,
    attention_detail: Option<AttentionItem>,
    attention_diff_rect: Rect,
    attention_allow_rect: Rect,
    attention_deny_rect: Rect,
    search: Option<SearchState>,
    workspace: Option<PathBuf>,
    palette: Option<PaletteState>,
    palette_rect: Rect,
    palette_hits: Vec<(u16, usize)>,
    session_picker: Option<SessionPickerState>,
    session_picker_rect: Rect,
    session_picker_hits: Vec<(u16, usize)>,
    pending_session_switch: Option<PendingSessionSwitch>,
    transcript_rect: Rect,
    command_rect: Rect,
    command_hits: Vec<(u16, usize)>,
    skill_rect: Rect,
    skill_hits: Vec<(u16, usize)>,
    approval_rect: Rect,
    question_rect: Rect,
    question_hits: Vec<(u16, usize)>,
    search_rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusPane {
    Prompt,
    Chat,
    Activity,
    Sidebar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarHit {
    Section(usize),
    Attention(usize),
    NewAgent,
}

struct TaskDetail {
    snapshot: BackgroundTaskSnapshot,
    output: String,
}

#[derive(Default)]
struct SearchState {
    editor: PromptEditor,
    matches: Vec<usize>,
    selected: usize,
}

struct PaletteState {
    editor: PromptEditor,
    items: Vec<PaletteItem>,
    filtered: Vec<usize>,
    selected: usize,
}

struct PaletteItem {
    label: String,
    description: String,
    action: PaletteAction,
}

#[derive(Default)]
struct SessionPickerState {
    editor: PromptEditor,
    results: Vec<willdeep_runtime_protocol::SessionSearchResult>,
    selected: usize,
    current_session: uuid::Uuid,
}

enum SessionPickerAction {
    None,
    Refresh,
    Switch(PendingSessionSwitch),
    Close,
}

struct PendingSessionSwitch {
    id: String,
    archived: bool,
}

enum PaletteAction {
    Command(String),
    Skill(String),
    Session(String),
    Task(usize),
    File(String),
}

/// A pending approval: what is being asked, whether Always Allow applies,
/// and the channel the waiting harness is parked on.
type ApprovalRequest = (String, bool, oneshot::Sender<ApprovalDecision>);

struct AskDialog {
    request: UserQuestion,
    selected: usize,
    checked: Vec<bool>,
    answer: PromptEditor,
    sender: oneshot::Sender<Option<String>>,
}

pub async fn run(
    agent: Arc<Agent>,
    mut session: Session,
    store: SessionStore,
    home: PathBuf,
    skills: Arc<SkillCatalog>,
    relay_bridge: RelayBridge,
    ui: (
        mpsc::UnboundedSender<UiMessage>,
        mpsc::UnboundedReceiver<UiMessage>,
        u64,
        Arc<BackgroundTaskRegistry>,
        crate::daemon::RuntimeSubmitOptions,
        Language,
        crate::notify::Notifier,
    ),
) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout))?;
    ui.6.set_session(&session.id.to_string(), Some(session.title.as_str()));
    let mut runtime = TuiRuntime {
        home,
        notifier: ui.6,
        skills,
        relay_bridge,
        context_window: ui.2,
        background_tasks: ui.3,
        runtime_submit: ui.4,
        local_workspace: session.workspace.clone(),
        tx: ui.0,
        rx: ui.1,
    };
    let result = event_loop(&mut term, agent, &mut session, &store, &mut runtime, ui.5).await;
    terminal::disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        DisableMouseCapture,
        DisableBracketedPaste,
        terminal::LeaveAlternateScreen
    )?;
    term.show_cursor()?;
    result
}

struct TuiRuntime {
    home: PathBuf,
    notifier: crate::notify::Notifier,
    skills: Arc<SkillCatalog>,
    relay_bridge: RelayBridge,
    context_window: u64,
    background_tasks: Arc<BackgroundTaskRegistry>,
    runtime_submit: crate::daemon::RuntimeSubmitOptions,
    local_workspace: PathBuf,
    tx: mpsc::UnboundedSender<UiMessage>,
    rx: mpsc::UnboundedReceiver<UiMessage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffAttentionAction {
    Open,
    Accept,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentDetailAction {
    Instruct,
    Stop,
    Retry,
    RetryWithModel,
    ReviewWorktree,
}

fn diff_attention_action_for_key(code: KeyCode) -> Option<DiffAttentionAction> {
    match code {
        KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('D') => Some(DiffAttentionAction::Open),
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(DiffAttentionAction::Accept),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(DiffAttentionAction::Reject),
        _ => None,
    }
}

fn selection_mode_exit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn is_selection_copy_key(key: KeyEvent) -> bool {
    (key.code == KeyCode::Char('c')
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER))
        || (key.code == KeyCode::Char('y') && key.modifiers == KeyModifiers::NONE)
}

fn quote_selected_text(value: &str) -> String {
    value
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptLineNavigation {
    Start,
    End,
}

fn prompt_line_navigation_for_key(key: KeyEvent) -> Option<PromptLineNavigation> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('a') => Some(PromptLineNavigation::Start),
        KeyCode::Char('e') => Some(PromptLineNavigation::End),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffReviewMouseAction {
    ScrollUp,
    ScrollDown,
    Consume,
}

fn diff_review_mouse_action(
    diff_review_open: bool,
    kind: MouseEventKind,
) -> Option<DiffReviewMouseAction> {
    if !diff_review_open {
        return None;
    }
    Some(match kind {
        MouseEventKind::ScrollUp => DiffReviewMouseAction::ScrollUp,
        MouseEventKind::ScrollDown => DiffReviewMouseAction::ScrollDown,
        _ => DiffReviewMouseAction::Consume,
    })
}

fn prefill_agent_command(
    app: &mut App,
    agent_id: uuid::Uuid,
    action: AgentDetailAction,
    language: Language,
) {
    if !app.input.is_empty() || !app.attachments.is_empty() {
        app.notice = Some(
            language
                .text(
                    "输入区已有草稿或附件，请先发送或清空后再操作 Agent",
                    "The composer has a draft or attachments; send or clear it before controlling the Agent",
                    "入力欄に下書きまたは添付があります。送信または消去してから Agent を操作してください",
                )
                .to_owned(),
        );
        return;
    }
    let command = match action {
        AgentDetailAction::Instruct => format!("/agent instruct {agent_id} "),
        AgentDetailAction::RetryWithModel => format!("/agent retry {agent_id} --model "),
        _ => return,
    };
    app.input.insert(&command);
    app.focus = FocusPane::Prompt;
    app.agent_detail = None;
    app.agent_detail_scroll = 0;
}

async fn handle_agent_detail_action(
    action: AgentDetailAction,
    app: &mut App,
    runtime: &TuiRuntime,
    language: Language,
) {
    let Some(agent) = app.agent_detail.clone() else {
        return;
    };
    match action {
        AgentDetailAction::Instruct | AgentDetailAction::RetryWithModel => {
            prefill_agent_command(app, agent.id, action, language);
        }
        AgentDetailAction::Stop => {
            match crate::daemon::stop_remote_agent(&runtime.home, agent.id).await {
                Ok(()) => {
                    app.agent_detail = None;
                    app.notice = Some(
                        language
                            .text(
                                "已请求停止子 Agent",
                                "Child Agent stop requested",
                                "子 Agent の停止を要求しました",
                            )
                            .to_owned(),
                    );
                }
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text("停止失败", "Stop failed", "停止に失敗")
                    ))
                }
            }
        }
        AgentDetailAction::Retry => {
            match crate::daemon::retry_remote_agent(&runtime.home, agent.id).await {
                Ok(()) => {
                    app.agent_detail = None;
                    app.notice = Some(
                        language
                            .text(
                                "已请求重试子 Agent",
                                "Child Agent retry requested",
                                "子 Agent の再試行を要求しました",
                            )
                            .to_owned(),
                    );
                }
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text("重试失败", "Retry failed", "再試行に失敗")
                    ))
                }
            }
        }
        AgentDetailAction::ReviewWorktree => {
            match crate::daemon::remote_review(&runtime.home, agent.id).await {
                Ok(review) => app.worktree_review = Some(review),
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text(
                            "Worktree 审查失败",
                            "Worktree review failed",
                            "Worktree レビュー失敗"
                        )
                    ))
                }
            }
        }
    }
}

async fn load_diff_review_state(
    home: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<DiffReviewState> {
    let snapshot = crate::daemon::diff_review::remote_snapshot(home, workspace).await?;
    let reviews = crate::daemon::diff_review::remote_reviews(home, workspace, &snapshot.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|record| (record.path, record.decision))
        .collect();
    let verifications =
        crate::daemon::diff_review::remote_verifications(home, workspace, &snapshot.id)
            .await
            .unwrap_or_default();
    let attributions =
        crate::daemon::diff_review::remote_attributions(home, workspace, &snapshot.id)
            .await
            .unwrap_or_default();
    Ok(DiffReviewState {
        snapshot,
        selected: 0,
        content: None,
        scroll: 0,
        area: crate::daemon::diff_review::DiffArea::Combined,
        view: DiffViewMode::Unified,
        search: None,
        search_matches: Vec::new(),
        search_selected: 0,
        reviews,
        confirm_revert: false,
        verifications,
        attributions,
        commit_preview: None,
        preview_draft: None,
    })
}

async fn handle_diff_attention_action(
    action: DiffAttentionAction,
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    runtime: &TuiRuntime,
    language: Language,
) -> Result<()> {
    if matches!(action, DiffAttentionAction::Open) {
        match load_diff_review_state(&runtime.home, &session.workspace).await {
            Ok(review) => {
                app.diff_review = Some(review);
                app.attention_detail = None;
            }
            Err(error) => {
                app.notice = Some(format!(
                    "{}: {error}",
                    language.text(
                        "打开 Diff Review 失败",
                        "Open Diff Review failed",
                        "Diff Review を開けませんでした"
                    )
                ));
            }
        }
        return Ok(());
    }

    let decision = if matches!(action, DiffAttentionAction::Accept) {
        crate::daemon::diff_review::ReviewDecision::Accepted
    } else {
        crate::daemon::diff_review::ReviewDecision::Rejected
    };
    let snapshot =
        crate::daemon::diff_review::remote_snapshot(&runtime.home, &session.workspace).await?;
    if snapshot.has_conflicts && matches!(action, DiffAttentionAction::Accept) {
        app.notice = Some(
            language
                .text(
                    "存在未解决冲突，不能整批通过；请先查看 Diff",
                    "Unresolved conflicts prevent bulk acceptance; inspect the Diff first",
                    "未解決の競合があるため一括承認できません。Diff を確認してください",
                )
                .to_owned(),
        );
        return Ok(());
    }
    // Reviewing every file is one request per path against the Runtime. On a
    // fifteen-file change that is tens of seconds, and awaiting it here would
    // freeze the UI for the whole time — the popup would sit there looking
    // like the key press was ignored. Close the popup now, submit in the
    // background, and report the outcome through the notice channel.
    let paths = snapshot
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    if let Some(detail) = app.attention_detail.take() {
        app.attention_read.insert(detail.id);
        session.attention_read = app.attention_read.clone();
        store.save(session)?;
    }
    app.notice = Some(format!(
        "{} · {}",
        language.text(
            "正在提交 Diff 审批",
            "Submitting Diff review",
            "Diff レビューを送信中",
        ),
        paths.len()
    ));
    let home = runtime.home.clone();
    let workspace = session.workspace.clone();
    let ui = runtime.tx.clone();
    let snapshot_id = snapshot.id.clone();
    let accepted = matches!(action, DiffAttentionAction::Accept);
    tokio::spawn(async move {
        let result = crate::daemon::diff_review::remote_review_many(
            &home,
            &snapshot_id,
            &workspace,
            &paths,
            decision,
        )
        .await;
        let notice = match result {
            Ok(reviewed) => format!(
                "{} · {reviewed}",
                if accepted {
                    language.text(
                        "已通过当前 Diff",
                        "Current Diff accepted",
                        "現在の Diff を承認しました",
                    )
                } else {
                    language.text(
                        "已拒绝当前 Diff",
                        "Current Diff rejected",
                        "現在の Diff を拒否しました",
                    )
                }
            ),
            Err(error) => format!(
                "{}: {error}",
                language.text("Diff 操作失败", "Diff action failed", "Diff 操作に失敗")
            ),
        };
        let _ = ui.send(UiMessage::RuntimeNotice(notice));
    });
    Ok(())
}

async fn refresh_session_picker(app: &mut App, runtime: &TuiRuntime, session: &Session) {
    let Some(picker) = app.session_picker.as_ref() else {
        return;
    };
    let query = picker.editor.text().trim().to_owned();
    let workspace = session
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| session.workspace.clone())
        .display()
        .to_string();
    let mut parameters = vec![("workspace".to_owned(), workspace)];
    if !query.is_empty() {
        parameters.push(("q".to_owned(), query));
    }
    match crate::daemon::search_remote_session_results(&runtime.home, &parameters).await {
        Ok(results) => app.set_session_picker_results(results),
        Err(error) => {
            app.notice = Some(format!(
                "{}: {error}",
                app.language.text(
                    "搜索历史会话失败",
                    "Historical Session search failed",
                    "履歴セッションの検索に失敗"
                )
            ));
        }
    }
}

async fn event_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent: Arc<Agent>,
    session: &mut Session,
    store: &SessionStore,
    runtime: &mut TuiRuntime,
    language: Language,
) -> Result<()> {
    let mut initial_transcript = transcript(&session.messages);
    if initial_transcript.is_empty() {
        initial_transcript.push(welcome_message(&session.workspace, language));
    }
    let mut app = App::new(initial_transcript, language);
    app.goal = session.goal.clone();
    if session.runtime_event_cursor == 0 {
        session.runtime_event_cursor = crate::daemon::runtime_event_head(&runtime.home)
            .await
            .unwrap_or_default();
        store.save(session)?;
    }
    app.runtime_event_cursor = session.runtime_event_cursor;
    app.workspace = Some(session.workspace.clone());
    app.workspace_status = workspace_status(&session.workspace, language);
    app.workspace_attention = workspace_attention(&session.workspace);
    app.attention_read = session.attention_read.clone();
    app.context_window = runtime.context_window.max(1);
    app.background_tasks = runtime.background_tasks.snapshots();
    let mut background_rx = runtime.background_tasks.subscribe();
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    let (runtime_snapshot_tx, mut runtime_snapshot_rx) =
        mpsc::unbounded_channel::<crate::daemon::RuntimeSnapshot>();
    let (runtime_event_tx, mut runtime_event_rx) =
        mpsc::unbounded_channel::<Vec<crate::daemon::RemoteRuntimeEvent>>();
    let mut _runtime_event_follower = crate::daemon::start_runtime_event_follower(
        runtime.home.clone(),
        app.runtime_event_cursor,
        runtime.runtime_submit.workspace.clone(),
        runtime_event_tx.clone(),
    );
    let (mobile_tx, mut mobile_rx) = mpsc::unbounded_channel::<MobilePrompt>();
    loop {
        if let Some(target) = app.pending_session_switch.take() {
            if app.running {
                app.notice = Some(
                    language
                        .text(
                            "当前会话正在运行，结束后才能切换历史会话",
                            "Wait for the current turn to finish before switching Sessions",
                            "現在のターンが完了してから履歴セッションを切り替えてください",
                        )
                        .to_owned(),
                );
                continue;
            }
            let target_id = match uuid::Uuid::parse_str(&target.id) {
                Ok(id) => id,
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text(
                            "历史会话 ID 无效",
                            "Historical Session ID is invalid",
                            "履歴セッション ID が無効です"
                        )
                    ));
                    continue;
                }
            };
            let archived = if target.archived {
                true
            } else {
                crate::daemon::remote_session_states(&runtime.home)
                    .await
                    .ok()
                    .and_then(|states| {
                        states
                            .into_iter()
                            .find(|state| state.id == target_id)
                            .map(|state| state.archived)
                    })
                    .unwrap_or(false)
            };
            let unarchive = if archived {
                crate::daemon::set_remote_session_archived(&runtime.home, target_id, false).await
            } else {
                Ok(())
            };
            let switched = match unarchive {
                Ok(()) => {
                    session_commands::switch(&mut app, session, store, runtime, &target.id).await
                }
                Err(error) => Err(error),
            };
            match switched {
                Ok(message) => app.notice = Some(message),
                Err(error) => {
                    app.notice = Some(format!(
                        "{}: {error}",
                        language.text(
                            "切换历史会话失败",
                            "Switch historical Session failed",
                            "履歴セッションの切り替えに失敗"
                        )
                    ));
                }
            }
        }
        draw(term, &mut app, &runtime.skills)?;
        tokio::select! {
            _=refresh.tick()=>{
                // Webhook delivery is detached; drain its failures here so a
                // dead endpoint shows up as a notice instead of silence.
                if let Some(error)=runtime.notifier.take_error(){app.notice=Some(format!("{}: {error}",language.text("通知 Webhook","Notification webhook","通知 Webhook")));}
                app.background_tasks=runtime.background_tasks.snapshots();
                let home=runtime.home.clone();
                let tx=runtime_snapshot_tx.clone();
                let workspace=runtime.runtime_submit.workspace.clone();
                tokio::spawn(async move {if let Ok(snapshot)=crate::daemon::runtime_snapshot(&home,&workspace).await{let _=tx.send(snapshot);}});
            },
            Some(snapshot)=runtime_snapshot_rx.recv()=>{
                runtime.notifier.attention_snapshot(&snapshot.attention);
                app.observe_runtime_tasks(&snapshot.tasks,session.id);
                app.runtime_attention=snapshot.attention;
                app.runtime_gates=snapshot.gates;
                app.runtime_agents=snapshot.agents;
                app.runtime_tools=snapshot.tools;
                app.runtime_artifacts=snapshot.artifacts;
                app.observe_runtime_version(snapshot.runtime_version);
                if runtime_ui::surface_pending_gates(&mut app,&runtime.home,&runtime.tx){
                    execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;
                }
            },
            Some(events)=runtime_event_rx.recv()=>runtime_ui::apply_runtime_events(&mut app,events,session,store)?,
            event=events.next()=>if let Some(Ok(event))=event { match event {
                Event::Paste(value)=>{
                    if let Some(picker)=app.session_picker.as_mut() {
                        picker.editor.insert(&value);
                        refresh_session_picker(&mut app,runtime,session).await;
                    } else {
                        app.handle_paste(value);
                    }
                },
                Event::Mouse(mouse)=>{
                    if app.question.is_some()||app.approval.is_some() {
                        if matches!(mouse.kind,MouseEventKind::Down(_)) {
                            app.handle_mouse(mouse.column,mouse.row,&runtime.background_tasks,&runtime.skills);
                        }
                        continue;
                    }
                    if app.session_picker.is_some() {
                        if mouse.kind==MouseEventKind::Down(MouseButton::Left) {
                            app.activate_session_picker_at(mouse.column,mouse.row);
                        }
                        continue;
                    }
                    if let Some(action)=diff_review_mouse_action(app.diff_review.is_some(),mouse.kind) {
                        if let Some(review)=app.diff_review.as_mut()
                            && review.preview_draft.is_none()
                        {
                            match action {
                                DiffReviewMouseAction::ScrollUp=>review.scroll=review.scroll.saturating_sub(3),
                                DiffReviewMouseAction::ScrollDown=>review.scroll=review.scroll.saturating_add(3),
                                DiffReviewMouseAction::Consume=>{},
                            }
                        }
                        continue;
                    }
                    if mouse.kind==MouseEventKind::Down(MouseButton::Left)
                        && let Some(action)=app.diff_attention_action_at(mouse.column,mouse.row)
                    {
                        if let Err(error)=handle_diff_attention_action(action,&mut app,session,store,runtime,language).await {
                            app.notice=Some(format!("{}: {error}",language.text("Diff 操作失败","Diff action failed","Diff 操作に失敗")));
                        }
                        continue;
                    }
                    if mouse.kind==MouseEventKind::Down(MouseButton::Left)
                        && let Some(action)=app.agent_detail_action_at(mouse.column,mouse.row)
                    {
                        handle_agent_detail_action(action,&mut app,runtime,language).await;
                        continue;
                    }
                    if app.diff_review.is_none()
                        && app.agent_detail.is_none()
                        && app.task_detail.is_none()
                        && app.attention_detail.is_none()
                        && app.worktree_review.is_none()
                        && app.handle_chat_selection_mouse(mouse)
                    {
                        continue;
                    }
                    match mouse.kind {
                    MouseEventKind::Down(_)=>{
                        app.handle_mouse(mouse.column,mouse.row,&runtime.background_tasks,&runtime.skills);
                        if app.attention_detail.is_some()
                            && let Some(gate)=app.selected_remote_gate()
                        {
                            app.attention_detail=None;
                            open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                        }
                    },
                    MouseEventKind::ScrollUp if app.agent_detail.is_some()=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown if app.agent_detail.is_some()=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(3),
                    MouseEventKind::ScrollUp if app.task_detail.is_some()=>app.task_detail_scroll=app.task_detail_scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown if app.task_detail.is_some()=>app.task_detail_scroll=app.task_detail_scroll.saturating_add(3),
                    MouseEventKind::ScrollUp if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(-3),
                    MouseEventKind::ScrollDown if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(3),
                    MouseEventKind::ScrollUp if app.transcript_rect.contains((mouse.column,mouse.row).into())=>{app.focus=FocusPane::Chat;app.scroll_up(3);},
                    MouseEventKind::ScrollDown if app.transcript_rect.contains((mouse.column,mouse.row).into())=>{app.focus=FocusPane::Chat;app.scroll_down(3);},
                    MouseEventKind::ScrollUp=>app.scroll_up(3),
                    MouseEventKind::ScrollDown=>app.scroll_down(3),
                    _=>{}
                }},
                Event::Key(key) if key.kind==KeyEventKind::Press=>{
                    if app.selection_mode {
                        if is_selection_copy_key(key) {
                            app.copy_chat_selection();
                        } else if key.code==KeyCode::Char('q')&&!key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER|KeyModifiers::ALT) {
                            app.quote_chat_selection();
                        } else if selection_mode_exit_key(key) {
                            app.selection_mode=false;
                            app.chat_selection=None;
                            app.notice=Some(language.text("已恢复鼠标滚动和点击","Mouse scrolling and clicks restored","マウス操作を復元しました").to_owned());
                        }
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('s'){
                        app.selection_mode = true;
                        app.chat_selection = None;
                        app.focus = FocusPane::Chat;
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('c'){break;}
                    if key.code==KeyCode::Esc&&app.mobile_qr.take().is_some(){continue;}
                    if app.question.is_some(){app.handle_question_key(key);continue;}
                    if app.approval.is_some(){
                        app.resolve_approval(|always|match key.code {KeyCode::Char('y')|KeyCode::Char('Y')=>ApprovalDecision::AllowOnce,KeyCode::Char('a')|KeyCode::Char('A') if always=>ApprovalDecision::AlwaysAllow,_=>ApprovalDecision::Deny});
                        continue;
                    }
                    if let Some(detail)=app.attention_detail.clone(){
                        if detail.source==AttentionSource::DiffReview {
                            let action=diff_attention_action_for_key(key.code);
                            if let Some(action)=action {
                                if let Err(error)=handle_diff_attention_action(action,&mut app,session,store,runtime,language).await {
                                    app.notice=Some(format!("{}: {error}",language.text("Diff 操作失败","Diff action failed","Diff 操作に失敗")));
                                }
                            } else if key.code==KeyCode::Esc {
                                app.attention_detail=None;
                            }
                        } else if key.code==KeyCode::Esc{app.attention_detail=None;}
                        else if matches!(key.code,KeyCode::Char('m')|KeyCode::Char('M')) {
                            // Dismiss from the detail popup itself. Until now
                            // `M` only worked with the sidebar focused on the
                            // Inbox section, so an item whose only sane action
                            // was "stop showing me this" had no exit here.
                            if app.attention_dismiss(&detail.id) {
                                session.attention_read=app.attention_read.clone();
                                store.save(session)?;
                                app.notice=Some(language.text("已从 Inbox 移除该条目","Item dismissed from the Inbox","この項目を Inbox から削除しました").to_owned());
                            } else {
                                app.notice=Some(language.text("运行中的条目不能忽略","A running item cannot be dismissed","実行中の項目は削除できません").to_owned());
                            }
                        }
                        else if key.code==KeyCode::Enter
                            && let Some(gate)=app.selected_remote_gate()
                        {
                            app.attention_detail=None;
                            open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                        }
                        continue;
                    }
                    if app.task_detail.is_some(){app.handle_task_detail_key(key,&runtime.background_tasks);continue;}
                    if let Some(review)=app.worktree_review.clone(){
                        match key.code {
                            KeyCode::Esc=>app.worktree_review=None,
                            KeyCode::Char('m')|KeyCode::Char('M') if review.can_merge=>{
                                match crate::daemon::remote_merge(&runtime.home,review.agent_id,review.id).await {
                                    Ok(result)=>{app.notice=Some(format!("{} · {}",language.text("Worktree 已合并","Worktree merged","Worktree をマージしました"),result.root_snapshot_id));app.worktree_review=None;app.agent_detail=None;},
                                    Err(error)=>app.notice=Some(format!("{}: {error}",language.text("合并失败","Merge failed","マージ失敗"))),
                                }
                            }
                            _=>{}
                        }
                        continue;
                    }
                    if let Some(agent)=app.agent_detail.clone(){
                        match key.code {
                            KeyCode::Esc=>{app.agent_detail=None;app.agent_detail_scroll=0;},
                            KeyCode::Up=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(1),
                            KeyCode::Down=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(1),
                            KeyCode::PageUp=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_sub(8),
                            KeyCode::PageDown=>app.agent_detail_scroll=app.agent_detail_scroll.saturating_add(8),
                            KeyCode::Home=>app.agent_detail_scroll=0,
                            KeyCode::End=>app.agent_detail_scroll=usize::MAX,
                            KeyCode::Char('i')|KeyCode::Char('I') if agent.background&&agent.status==willdeep_core::RuntimeStatus::Working=>handle_agent_detail_action(AgentDetailAction::Instruct,&mut app,runtime,language).await,
                            KeyCode::Char('k')|KeyCode::Char('K') if agent.background&&agent.status==willdeep_core::RuntimeStatus::Working=>handle_agent_detail_action(AgentDetailAction::Stop,&mut app,runtime,language).await,
                            KeyCode::Char('r')|KeyCode::Char('R') if agent.background&&matches!(agent.status,willdeep_core::RuntimeStatus::Blocked|willdeep_core::RuntimeStatus::Failed|willdeep_core::RuntimeStatus::Done|willdeep_core::RuntimeStatus::Cancelled)=>handle_agent_detail_action(AgentDetailAction::Retry,&mut app,runtime,language).await,
                            KeyCode::Char('m')|KeyCode::Char('M') if agent.background&&matches!(agent.status,willdeep_core::RuntimeStatus::Blocked|willdeep_core::RuntimeStatus::Failed|willdeep_core::RuntimeStatus::Done|willdeep_core::RuntimeStatus::Cancelled)=>handle_agent_detail_action(AgentDetailAction::RetryWithModel,&mut app,runtime,language).await,
                            KeyCode::Char('w')|KeyCode::Char('W') if agent.dedicated_worktree=>handle_agent_detail_action(AgentDetailAction::ReviewWorktree,&mut app,runtime,language).await,
                            _=>{}
                        }
                        continue;
                    }
                    if app.diff_review.is_some(){
                        let mut close=false;
                        let mut force_full_redraw=false;
                        let mut open_file=None;
                        let mut review_action=None;
                        let mut revert_action=None;
                        let mut commit_preview_action=None;
                        let mut preview_draft_handled=false;
                        if let Some(review)=app.diff_review.as_mut(){
                            if review.commit_preview.is_some() {
                                if key.code==KeyCode::Esc{review.commit_preview=None;}
                                continue;
                            } else if let Some(draft)=review.preview_draft.as_mut() {
                                preview_draft_handled=true;
                                match key.code {
                                    KeyCode::Esc=>review.preview_draft=None,
                                    KeyCode::Tab=>draft.field=(draft.field+1)%3,
                                    KeyCode::BackTab=>draft.field=draft.field.checked_sub(1).unwrap_or(2),
                                    KeyCode::Enter if !draft.message.text().trim().is_empty()=>{
                                        commit_preview_action=Some((review.snapshot.id.clone(),draft.message.text().to_owned(),draft.remote.text().to_owned(),draft.tag.text().to_owned()));
                                        review.preview_draft=None;
                                    },
                                    KeyCode::Left=>draft.editor_mut().left(),
                                    KeyCode::Right=>draft.editor_mut().right(),
                                    KeyCode::Home=>draft.editor_mut().home(),
                                    KeyCode::End=>draft.editor_mut().end(),
                                    KeyCode::Backspace=>draft.editor_mut().backspace(),
                                    KeyCode::Delete=>draft.editor_mut().delete(),
                                    KeyCode::Char(value) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>draft.editor_mut().insert(&value.to_string()),
                                    _=>{},
                                }
                            } else if review.confirm_revert {
                                if matches!(key.code,KeyCode::Char('y')|KeyCode::Char('Y')) {
                                    revert_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),review.area));
                                }
                                review.confirm_revert=false;
                                if revert_action.is_none(){app.notice=Some(language.text("已取消撤销","Revert cancelled","取り消しをキャンセルしました").to_owned());}
                            } else if review.search.is_some() {
                                match key.code {
                                    KeyCode::Esc => review.search = None,
                                    KeyCode::Enter if !review.search_matches.is_empty() => {
                                        review.search_selected = if key.modifiers.contains(KeyModifiers::SHIFT) {
                                            review.search_selected.checked_sub(1).unwrap_or(review.search_matches.len() - 1)
                                        } else {
                                            (review.search_selected + 1) % review.search_matches.len()
                                        };
                                        review.scroll = review.search_matches[review.search_selected];
                                    }
                                    KeyCode::Left => review.search.as_mut().unwrap().left(),
                                    KeyCode::Right => review.search.as_mut().unwrap().right(),
                                    KeyCode::Home => review.search.as_mut().unwrap().home(),
                                    KeyCode::End => review.search.as_mut().unwrap().end(),
                                    KeyCode::Backspace => { review.search.as_mut().unwrap().backspace(); refresh_diff_search(review); }
                                    KeyCode::Delete => { review.search.as_mut().unwrap().delete(); refresh_diff_search(review); }
                                    KeyCode::Char(value) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER) => {
                                        review.search.as_mut().unwrap().insert(&value.to_string());
                                        refresh_diff_search(review);
                                    }
                                    _ => {}
                                }
                                continue;
                            } else {match key.code {
                                KeyCode::Esc if review.content.is_some()=>{review.content=None;review.scroll=0;force_full_redraw=true;},
                                KeyCode::Esc=>{close=true;force_full_redraw=true;},
                                KeyCode::Up if review.content.is_none()=>review.selected=review.selected.checked_sub(1).unwrap_or(review.snapshot.files.len().saturating_sub(1)),
                                KeyCode::Down if review.content.is_none()&&!review.snapshot.files.is_empty()=>review.selected=(review.selected+1)%review.snapshot.files.len(),
                                KeyCode::Enter if review.content.is_none()=>open_file=review.snapshot.files.get(review.selected).map(|file|(review.snapshot.id.clone(),file.path.clone(),review.area)),
                                KeyCode::Char('v')|KeyCode::Char('V') if review.content.is_some()=>{
                                    review.view=match review.view {DiffViewMode::Unified=>DiffViewMode::SideBySide,DiffViewMode::SideBySide=>DiffViewMode::Unified};
                                    refresh_diff_search(review);
                                },
                                KeyCode::Char('s')|KeyCode::Char('S') if review.content.is_some()=>{
                                    review.area=next_diff_area(review.area);
                                    open_file=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),review.area));
                                },
                                KeyCode::Char('/') if review.content.is_some()=>review.search=Some(PromptEditor::default()),
                                KeyCode::Char('n') if !review.search_matches.is_empty()=>{
                                    review.search_selected=(review.search_selected+1)%review.search_matches.len();
                                    review.scroll=review.search_matches[review.search_selected];
                                },
                                KeyCode::Char('N') if !review.search_matches.is_empty()=>{
                                    review.search_selected=review.search_selected.checked_sub(1).unwrap_or(review.search_matches.len()-1);
                                    review.scroll=review.search_matches[review.search_selected];
                                },
                                KeyCode::Char('a')|KeyCode::Char('A') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Accepted)),
                                KeyCode::Char('d')|KeyCode::Char('D') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Rejected)),
                                KeyCode::Char('c')|KeyCode::Char('C') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::ChangesRequested)),
                                KeyCode::Char('m')|KeyCode::Char('M') if review.content.is_some()=>review_action=review.content.as_ref().map(|(path,_)|(review.snapshot.id.clone(),path.clone(),crate::daemon::diff_review::ReviewDecision::Reviewed)),
                                KeyCode::Char('r')|KeyCode::Char('R') if review.content.is_some()=>review.confirm_revert=true,
                                KeyCode::Char('p')|KeyCode::Char('P')=>review.preview_draft=Some(CommitPreviewDraft::default()),
                                KeyCode::Up=>review.scroll=review.scroll.saturating_sub(1),
                                KeyCode::Down=>review.scroll=review.scroll.saturating_add(1),
                                KeyCode::PageUp=>review.scroll=review.scroll.saturating_sub(10),
                                KeyCode::PageDown=>review.scroll=review.scroll.saturating_add(10),
                                KeyCode::Home=>review.scroll=0,
                                _=>{}
                            }}
                        }
                        if preview_draft_handled&&commit_preview_action.is_none(){continue;}
                        if force_full_redraw{term.clear()?;}
                        if close{app.diff_review=None;continue;}
                        if let Some((snapshot_id,path,area))=open_file{
                            match crate::daemon::diff_review::remote_content(&runtime.home,&session.workspace,&snapshot_id,&path,area).await{
                                Ok(content)=>if let Some(review)=app.diff_review.as_mut(){review.content=Some((path,content));review.scroll=0;review.search_matches.clear();review.search_selected=0;},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("打开 Diff 失败","Open Diff failed","Diff を開けませんでした"))),
                            }
                        }
                        if let Some((snapshot_id,path,decision))=review_action{
                            let request=crate::daemon::diff_review::ReviewRequest{workspace:session.workspace.clone(),path:path.clone(),decision,note:None};
                            match crate::daemon::diff_review::remote_review(&runtime.home,&snapshot_id,&request).await{
                                Ok(record)=>{if let Some(review)=app.diff_review.as_mut(){review.reviews.insert(path,record.decision);}app.notice=Some(language.text("审查决定已保存","Review decision saved","レビュー結果を保存しました").to_owned());},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("保存审查决定失败","Save review decision failed","レビュー結果を保存できませんでした"))),
                            }
                        }
                        if let Some((snapshot_id,path,area))=revert_action{
                            let request=crate::daemon::diff_review::RevertRequest{workspace:session.workspace.clone(),path,area};
                            match crate::daemon::diff_review::remote_revert(&runtime.home,&snapshot_id,&request).await{
                                Ok(result)=>match crate::daemon::diff_review::remote_snapshot(&runtime.home,&session.workspace).await{
                                    Ok(snapshot)=>{if let Some(review)=app.diff_review.as_mut(){review.snapshot=snapshot;review.content=None;review.scroll=0;review.search_matches.clear();review.reviews.clear();review.verifications.clear();review.attributions.clear();}app.notice=Some(if let Some(path)=result.recovery_path{format!("{}: {}",language.text("已安全撤销，可从回收区恢复","Safely reverted; recovery copy","安全に戻しました。復元先"),path.display())}else{language.text("已安全撤销文件变更","File changes safely reverted","ファイル変更を安全に戻しました").to_owned()});},
                                    Err(error)=>app.notice=Some(format!("{}: {error}",language.text("撤销成功，但刷新 Diff 失败","Reverted, but refresh failed","取り消しましたが更新に失敗しました"))),
                                },
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("安全撤销失败","Safe revert failed","安全な取り消しに失敗しました"))),
                            }
                        }
                        if let Some((snapshot_id,message,remote,tag))=commit_preview_action{
                            let tag=(!tag.trim().is_empty()).then_some(tag);
                            match crate::daemon::diff_review::remote_commit_preview(&runtime.home,&session.workspace,&snapshot_id,&message,&remote,tag.as_deref()).await{
                                Ok(preview)=>if let Some(review)=app.diff_review.as_mut(){review.commit_preview=Some(preview);},
                                Err(error)=>app.notice=Some(format!("{}: {error}",language.text("生成 Commit Preview 失败","Commit Preview failed","Commit Preview に失敗しました"))),
                            }
                        }
                        continue;
                    }
                    if app.session_picker.is_some(){
                        match app.handle_session_picker_key(key) {
                            SessionPickerAction::None=>{},
                            SessionPickerAction::Close=>app.session_picker=None,
                            SessionPickerAction::Switch(target)=>{
                                app.pending_session_switch=Some(target);
                                app.session_picker=None;
                            },
                            SessionPickerAction::Refresh=>refresh_session_picker(&mut app,runtime,session).await,
                        }
                        continue;
                    }
                    if app.palette.is_some(){app.handle_palette_key(key,&runtime.background_tasks);continue;}
                    if app.search.is_some(){app.handle_search_key(key);continue;}
                    if app.handle_help_key(key) {continue;}
                    if key.code == KeyCode::F(2) {
                        app.toggle_composer_expanded();
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('p'){
                        app.open_palette(&runtime.skills,store,session);
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('r'){
                        if app.running {
                            app.notice=Some(language.text(
                                "当前会话正在运行，结束后才能切换历史会话",
                                "Wait for the current turn to finish before switching Sessions",
                                "現在のターンが完了してから履歴セッションを切り替えてください"
                            ).to_owned());
                        } else {
                            app.open_session_picker(session.id);
                            refresh_session_picker(&mut app,runtime,session).await;
                        }
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('f'){
                        app.search=Some(SearchState::default());
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('b'){
                        if app.sidebar_wide {
                            app.sidebar_visible = !app.sidebar_visible;
                            if !app.sidebar_visible {app.focus=FocusPane::Prompt;}
                        } else if app.focus==FocusPane::Sidebar {
                            app.focus=FocusPane::Prompt;
                        } else {
                            app.sidebar_visible=true;
                            app.focus=FocusPane::Sidebar;
                        }
                        continue;
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('w'){
                        app.sidebar_visible=true;
                        app.cycle_focus();
                        continue;
                    }
                    if app.focus==FocusPane::Chat {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Up=>app.scroll_up(1),
                            KeyCode::Down=>app.scroll_down(1),
                            KeyCode::Home=>app.scroll_to_top(),
                            KeyCode::End=>app.scroll_to_bottom(),
                            _=>{}
                        }
                        continue;
                    }
                    if app.focus==FocusPane::Activity {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Enter|KeyCode::Char(' ')=>app.tools_expanded = !app.tools_expanded,
                            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.tools_expanded = !app.tools_expanded,
                            _=>{}
                        }
                        continue;
                    }
                    if app.focus==FocusPane::Sidebar {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Up if app.sidebar_selected==1&&app.sidebar_expanded[1]=>app.attention_move(-1),
                            KeyCode::Down if app.sidebar_selected==1&&app.sidebar_expanded[1]=>app.attention_move(1),
                            KeyCode::Up if app.sidebar_selected==2&&app.sidebar_expanded[2]=>app.runtime_agent_move(-1),
                            KeyCode::Down if app.sidebar_selected==2&&app.sidebar_expanded[2]=>app.runtime_agent_move(1),
                            KeyCode::Up=>app.sidebar_move(-1),
                            KeyCode::Down=>app.sidebar_move(1),
                            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT)=>app.sidebar_move(-1),
                            KeyCode::Tab=>app.sidebar_move(1),
                            KeyCode::Enter=>{
                                if let Some(gate)=app.selected_remote_gate(){
                                    open_remote_gate(&mut app,gate,runtime.home.clone(),runtime.tx.clone());
                                }else if app.sidebar_selected==2 {
                                    if let Some(agent)=app.selected_runtime_agent(){
                                        match crate::daemon::remote_agent_detail(&runtime.home,agent.id).await {
                                            Ok(detail)=>app.agent_detail=Some(detail),
                                            Err(error)=>app.notice=Some(format!("{}: {error}",language.text("加载 Agent 详情失败","Failed to load Agent details","Agent 詳細の読み込みに失敗"))),
                                        }
                                        app.agent_detail_scroll=0;
                                    }
                                }else{app.sidebar_activate(&runtime.background_tasks);}
                            },
                            KeyCode::Char(' ')=>app.sidebar_toggle(),
                            KeyCode::Char('n')|KeyCode::Char('N') if app.sidebar_selected==2=>app.prefill_new_agent(),
                            KeyCode::Char('k')|KeyCode::Char('K') if app.sidebar_selected==1=>{
                                if let Some(id)=app.selected_runtime_task_id(){
                                    match crate::daemon::cancel_remote_task(&runtime.home,id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求停止 Runtime 任务","Runtime task stop requested","Runtime タスクの停止を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("停止失败","Stop failed","停止に失敗"))),
                                    }
                                }else{app.attention_stop(&runtime.background_tasks);}
                            },
                            KeyCode::Char('r')|KeyCode::Char('R') if app.sidebar_selected==1=>{
                                if app.attention_retry(&runtime.background_tasks){
                                    session.attention_read=app.attention_read.clone();
                                    store.save(session)?;
                                }
                            },
                            KeyCode::Char('k')|KeyCode::Char('K') if app.sidebar_selected==2=>{
                                if let Some(agent)=app.selected_runtime_agent(){
                                    match crate::daemon::stop_remote_agent(&runtime.home,agent.id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求停止子 Agent","Child Agent stop requested","子 Agent の停止を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("停止失败","Stop failed","停止に失敗"))),
                                    }
                                }
                            },
                            KeyCode::Char('r')|KeyCode::Char('R') if app.sidebar_selected==2=>{
                                if let Some(agent)=app.selected_runtime_agent(){
                                    match crate::daemon::retry_remote_agent(&runtime.home,agent.id).await {
                                        Ok(())=>app.notice=Some(language.text("已请求重试子 Agent","Child Agent retry requested","子 Agent の再試行を要求しました").to_owned()),
                                        Err(error)=>app.notice=Some(format!("{}: {error}",language.text("重试失败","Retry failed","再試行に失敗"))),
                                    }
                                }
                            },
                            KeyCode::Char('m')|KeyCode::Char('M') if app.sidebar_selected==1=>{
                                if app.attention_mark_read(){
                                    session.attention_read=app.attention_read.clone();
                                    store.save(session)?;
                                }
                            },
                            _=>{}
                        }
                        continue;
                    }
                    if let Some(action) = prompt_line_navigation_for_key(key) {
                        match action {
                            PromptLineNavigation::Start => app.edit_input(|input| input.home()),
                            PromptLineNavigation::End => app.edit_input(|input| input.end()),
                        }
                        continue;
                    }
                    if app.handle_command_key(key) || app.handle_skill_key(key, &runtime.skills) { continue; }
                    match key.code {
                        KeyCode::PageUp=>app.scroll_up(app.viewport_height.saturating_sub(1).max(1)),KeyCode::PageDown=>app.scroll_down(app.viewport_height.saturating_sub(1).max(1)),
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT)=>app.scroll_up(1),KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT)=>app.scroll_down(1),
                        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL)=>app.scroll_to_top(),KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL)=>app.scroll_to_bottom(),
                        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.tools_expanded = !app.tools_expanded,
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.delete_selected_attachment(),
                    KeyCode::Char('v') if (key.modifiers.contains(KeyModifiers::CONTROL)&&key.modifiers.contains(KeyModifiers::SHIFT))||key.modifiers.contains(KeyModifiers::SUPER)=>app.paste_clipboard_image(),
                        KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT|KeyModifiers::ALT)=>app.input.insert("\n"),
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL)=>app.input.insert("\n"),
                        KeyCode::Enter if !app.running&&(!app.input.is_empty()||!app.attachments.is_empty())=>{
                            let prompt=app.input.take();app.append_transcript(format!("You: {prompt}"));
                            if app.handle_mobile_command(&prompt,&runtime.home,&runtime.relay_bridge,&mobile_tx,session){continue;}
                            match handle_agent_command(&prompt,&mut app,runtime,session.id).await {
                                Ok(true)=>continue,
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("Agent 操作失败","Agent action failed","Agent 操作に失敗しました")));continue;},
                            }
                            match handle_session_command(&prompt,&mut app,session,store,runtime).await {
                                Ok(true)=>continue,
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("会话操作失败","Session action failed","セッション操作に失敗しました")));continue;},
                            }
                            let previous_workspace=runtime.runtime_submit.workspace.clone();
                            match handle_workspace_command(&prompt,&mut app,session,store,runtime).await {
                                Ok(true)=>{
                                    if runtime.runtime_submit.workspace!=previous_workspace {
                                        while runtime_event_rx.try_recv().is_ok() {}
                                        _runtime_event_follower=crate::daemon::start_runtime_event_follower(
                                            runtime.home.clone(),
                                            app.runtime_event_cursor,
                                            runtime.runtime_submit.workspace.clone(),
                                            runtime_event_tx.clone(),
                                        );
                                    }
                                    continue;
                                },
                                Ok(false)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("工作区操作失败","Workspace action failed","ワークスペース操作に失敗しました")));continue;},
                            }
                            if prompt.trim()=="/compress" {dispatch_compress(&mut app,session,&agent,&runtime.tx);continue;}
                            if let Some(parsed)=daemon_commands::parse(&prompt) {
                                match parsed {
                                    Ok(command)=>{
                                        app.append_transcript(format!("System: {} · {}",language.text("Runtime 操作已提交","Runtime action submitted","Runtime 操作を送信しました"),prompt.trim()));
                                        daemon_commands::dispatch(command,runtime.home.clone(),language,runtime.tx.clone());
                                    },
                                    Err(usage)=>app.append_transcript(format!("Error: {usage}")),
                                }
                                continue;
                            }
                            match webapp_commands::handle_webapp_command(&prompt,&runtime.home,&runtime.runtime_submit.workspace,runtime.runtime_submit.config.as_deref(),runtime.runtime_submit.profile.as_deref(),language).await {
                                Ok(Some(message))=>{app.append_transcript(message);continue;},
                                Ok(None)=>{},
                                Err(error)=>{app.append_transcript(format!("Error: {}: {error}",language.text("启动 Web App 失败","Start Web App failed","Web App の起動に失敗しました")));continue;},
                            }
                            if prompt.trim()=="/diff" {
                                match load_diff_review_state(&runtime.home,&session.workspace).await {
                                    Ok(review)=>app.diff_review=Some(review),
                                    Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("打开 Diff Review 失败","Open Diff Review failed","Diff Review を開けませんでした"))),
                                }
                                continue;
                            }
                            if let PromptExecution::Local(local_prompt)=prompt_execution(&prompt) {
                                if local_prompt.is_empty(){app.append_transcript(format!("System: {}",language.text("用法：/local <任务>","Usage: /local <task>","使用法: /local <タスク>")));continue;}
                                if session.workspace.canonicalize()?!=runtime.local_workspace.canonicalize()?{app.append_transcript(format!("System: {}",language.text("切换工作区后 /local 已禁用；请使用 Runtime，或从目标目录重新启动 TUI","/local is disabled after switching Workspace; use Runtime or restart the TUI from the target directory","ワークスペース切替後は /local を使用できません。Runtime を使うか対象ディレクトリから TUI を再起動してください")));continue;}
                                dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,local_prompt)?;
                                continue;
                            }
                            let runtime_alias=prompt.trim()=="/runtime"||prompt.trim().starts_with("/runtime ");
                            if !runtime_alias {
                                let previous_goal=app.goal.clone();
                                if app.handle_slash_command(&prompt,&runtime.skills){
                                    if app.goal!=previous_goal{session.goal=app.goal.clone();store.save(session)?;}
                                    continue;
                                }
                            }
                            let PromptExecution::Runtime(remote_prompt)=prompt_execution(&prompt) else {unreachable!("local prompts were handled above")};
                            match runtime_ui::submit_turn(&mut app,session,store,runtime,remote_prompt).await {
                                Ok(())=>app.notice=Some(language.text("AI 正在处理…","AI is working…","AI が処理しています…").to_owned()),
                                Err(error)=>app.append_transcript(format!("Error: {}: {error}",language.text("提交 Runtime 轮次失败","Submit Runtime turn failed","Runtime ターンの送信に失敗"))),
                            }
                        }
                        KeyCode::Left=>app.edit_input(|input| input.left()),KeyCode::Right=>app.edit_input(|input| input.right()),
                        KeyCode::Up=>{let width=app.prompt_rect.width.saturating_sub(2).max(1) as usize;app.edit_input(|input| input.up_visual(width));},
                        KeyCode::Down=>{let width=app.prompt_rect.width.saturating_sub(2).max(1) as usize;app.edit_input(|input| input.down_visual(width));},
                        KeyCode::Home=>app.edit_input(|input| input.home()),KeyCode::End=>app.edit_input(|input| input.end()),KeyCode::Backspace=>app.edit_input(|input| input.backspace()),KeyCode::Delete=>app.edit_input(|input| input.delete()),
                        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>app.edit_input(|input| input.insert(&c.to_string())),_=>{}
                    }
                }
                _=>{}
            }},
            Some(message)=runtime.rx.recv()=>match message {
                UiMessage::Agent(AgentEvent::AssistantText(v))=>{app.activity_line=language.text("正在整理思路","Working through it","考えを整理中").to_owned();app.transient_thought=Some(compact_thought(&v));},
                UiMessage::Agent(AgentEvent::TurnStarted{turn})=>app.record_progress(format!("{} {turn}",language.text("正在思考 · 准备轮次","Thinking · preparing turn","思考中 · ターンを準備"))),
                UiMessage::Agent(AgentEvent::ToolRequested(v))=>{app.transient_thought=None;app.record_progress(format!("{} {}",language.text("正在使用","Using","使用中"),v.name));app.tools.requested(&v.name);},
                UiMessage::Agent(AgentEvent::ToolCompleted{call,is_error,..})=>{app.record_progress(format!("{} {}",if is_error{language.text("失败","Failed","失敗")}else{language.text("已完成","Finished","完了")},call.name));app.tools.completed(&call.name,is_error);if matches!(call.name.as_str(),"create_file"|"edit_file"|"run_command"|"create_worktree"){app.workspace_status=workspace_status(&session.workspace,language);app.workspace_attention=workspace_attention(&session.workspace);}},
                UiMessage::Agent(AgentEvent::Usage(v))=>{app.context_tokens=v.input_tokens.unwrap_or(app.context_tokens);app.latest_usage=v;},
                UiMessage::Agent(AgentEvent::CompressionStarted{estimated_tokens})=>{app.context_tokens=estimated_tokens;app.record_progress(language.text("正在压缩上下文","Compressing context","コンテキストを圧縮中").to_owned());},
                UiMessage::Agent(AgentEvent::CompressionCompleted{estimated_tokens})=>{app.context_tokens=estimated_tokens;app.record_progress(language.text("上下文已压缩","Context compressed","コンテキストを圧縮しました").to_owned());},
                UiMessage::Agent(AgentEvent::BackgroundShellStarted{id})=>app.record_progress(format!("{} {id}",language.text("后台命令已启动","Background command started","バックグラウンドコマンド開始"))),
                UiMessage::Agent(AgentEvent::BackgroundShellCompleted{id,status,..})=>app.record_progress(format!("{} {id} · {status:?}",language.text("后台命令已结束","Background command finished","バックグラウンドコマンド完了"))),
                UiMessage::Agent(AgentEvent::SubagentStarted{id,profile,background,..})=>app.record_progress(format!("{} {} · {profile} · {}",language.text("子 Agent 已启动","Subagent started","サブエージェント開始"),id.to_string().get(..8).unwrap_or("agent"),if background{language.text("后台","background","バックグラウンド")}else{language.text("前台","foreground","フォアグラウンド")})),
                UiMessage::Agent(AgentEvent::SubagentCompleted{id,status,..})=>app.record_progress(format!("{} {} · {status:?}",language.text("子 Agent 已结束","Subagent finished","サブエージェント完了"),id.to_string().get(..8).unwrap_or("agent"))),
                UiMessage::Agent(AgentEvent::SubagentTurnStarted{id,turn})=>app.record_progress(format!("{} {} · {} {turn}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),language.text("轮次","turn","ターン"))),
                UiMessage::Agent(AgentEvent::SubagentToolRequested{id,name})=>app.record_progress(format!("{} {} · {} {name}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),language.text("正在使用","using","使用中"))),
                UiMessage::Agent(AgentEvent::SubagentToolCompleted{id,name,is_error})=>app.record_progress(format!("{} {} · {} {name}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),if is_error{language.text("失败","failed","失敗")}else{language.text("已完成","finished","完了")})),
                UiMessage::Agent(AgentEvent::SubagentUsage{..})=>{},
                UiMessage::Agent(AgentEvent::SubagentVerdict{id,verifier_passed,attempts,..})=>{if let Some(passed)=verifier_passed{app.record_progress(format!("{} {} · {} · {attempts} {}",language.text("子 Agent","Subagent","サブエージェント"),id.to_string().get(..8).unwrap_or("agent"),if passed{language.text("验证通过","verified","検証通過")}else{language.text("验证未通过","not verified","検証失敗")},language.text("次尝试","attempt(s)","回試行")));}},
                UiMessage::Agent(AgentEvent::GoalContinuationInjected{rung})=>app.record_progress(format!("{} · {rung:?}",language.text("目标未达成 · 继续推进","Goal not met · continuing","目標未達成 · 継続します"))),
                UiMessage::Agent(AgentEvent::GoalBudgetLimited{reason})=>app.record_progress(format!("{} · {reason:?}",language.text("目标预算耗尽 · 转入收尾","Goal budget exhausted · wrapping up","目標の予算を使い切りました · まとめに移ります"))),
                UiMessage::Approval(v,a,s)=>{let detail=v.clone();if app.enqueue_approval((v,a,s)){runtime.notifier.attention_required(RuntimeStatus::WaitingApproval,"tool_approval",detail);execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;}},
                UiMessage::Question(request,sender)=>{let checked=vec![false;request.options.len()];let detail=request.question.clone();if app.enqueue_question(AskDialog{request,selected:0,checked,answer:PromptEditor::default(),sender}){runtime.notifier.attention_required(RuntimeStatus::WaitingAnswer,"ask_user",detail);execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;}},
                UiMessage::Finished(Ok(outcome))=>{app.transient_thought=None;runtime.notifier.task_completed(outcome.final_text.as_str());app.append_transcript(format!("WillDeep: {}",outcome.final_text));session.messages=outcome.messages;store.save(session)?;app.finish_turn();if let Some(notice)=app.background_notices.pop_front(){app.append_transcript("System: Background result returned to main harness".to_owned());dispatch_notification(&mut app,session,store,&agent,&runtime.tx,notice)?;}else if let Some(prompt)=app.mobile_queue.pop_front(){app.append_transcript(format!("Phone: {prompt}"));dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,prompt)?;}},
                UiMessage::Finished(Err(e))=>{app.append_transcript(format!("Error: {e}"));app.finish_turn();},
                UiMessage::Compressed(Ok(messages))=>{let changed=session.replace_with_compressed_messages(messages);store.save(session)?;app.append_transcript(if changed{"System: Context compressed".to_owned()}else{"System: Context is too short to compress".to_owned()});app.finish_turn();},
                UiMessage::Compressed(Err(e))=>{app.append_transcript(format!("Error: context compression failed: {e}"));app.finish_turn();},
                UiMessage::RuntimeNotice(notice)=>app.notice=Some(notice),
            },
            Some(prompt)=mobile_rx.recv()=>{
                if app.running {app.mobile_queue.push_back(prompt.text);app.notice=Some(format!("Phone request queued · {} waiting",app.mobile_queue.len()));}
                else {app.append_transcript(format!("Phone: {}",prompt.text));dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,prompt.text)?;}
            },
            Ok(event)=background_rx.recv()=>{
                let _=runtime.background_tasks.drain_pending();
                app.background_tasks=runtime.background_tasks.snapshots();
                app.background_notices.push_back(event.notice);
                app.notice=Some(format!("{} finished · returning result to main harness",event.snapshot.id));
                execute!(term.backend_mut(),crossterm::style::Print("\x07"))?;
                if !app.running && let Some(notice)=app.background_notices.pop_front(){dispatch_notification(&mut app,session,store,&agent,&runtime.tx,notice)?;}
            },
        }
    }
    Ok(())
}

impl App {
    fn new(transcript: Vec<String>, language: Language) -> Self {
        Self {
            input: PromptEditor::default(),
            transcript,
            running: false,
            approval: None,
            approval_queue: VecDeque::new(),
            question: None,
            question_queue: VecDeque::new(),
            scroll_from_bottom: 0,
            follow_bottom: true,
            transcript_width: 78,
            transcript_height: 0,
            viewport_height: 10,
            tools: ToolActivity::default(),
            tools_expanded: false,
            activity_rect: Rect::default(),
            attachments: Vec::new(),
            selected_attachment: 0,
            prompt_rect: Rect::default(),
            prompt_scroll: 0,
            composer_expanded: false,
            notice: None,
            goal: None,
            mobile_gateway: None,
            mobile_qr: None,
            mobile_queue: VecDeque::new(),
            latest_usage: Usage::default(),
            turn_started: None,
            last_progress_at: None,
            runtime_turn: false,
            last_elapsed: None,
            context_window: 128_000,
            context_tokens: 0,
            activity_line: String::new(),
            background_tasks: Vec::new(),
            workspace_attention: Vec::new(),
            runtime_attention: Vec::new(),
            runtime_gates: Vec::new(),
            runtime_version: None,
            runtime_version_warned: false,
            surfaced_gates: BTreeSet::new(),
            runtime_agents: Vec::new(),
            runtime_tools: Vec::new(),
            runtime_artifacts: Vec::new(),
            runtime_agent_selected: 0,
            agent_detail: None,
            agent_detail_scroll: 0,
            agent_detail_action_rects: Vec::new(),
            worktree_review: None,
            diff_review: None,
            runtime_event_cursor: 0,
            background_notices: VecDeque::new(),
            workspace_status: String::new(),
            progress_log: VecDeque::new(),
            language,
            transient_thought: None,
            selection_mode: false,
            chat_selection: None,
            transcript_rows: Vec::new(),
            transcript_render_offset: 0,
            skill_selected: 0,
            skill_menu_dismissed: false,
            command_selected: 0,
            command_menu_dismissed: false,
            focus: FocusPane::Prompt,
            // Hidden by default: the transcript is what the user came for,
            // and the status column is a lookup surface, not a permanent
            // one. `/sidebar` or Ctrl+B brings it back.
            sidebar_visible: false,
            sidebar_selected: 0,
            sidebar_expanded: [true, true, true, true],
            sidebar_scroll: 0,
            sidebar_rect: Rect::default(),
            sidebar_wide: false,
            help_visible: false,
            sidebar_hits: Vec::new(),
            sidebar_manual_scroll: false,
            attention_selected: 0,
            attention_read: BTreeSet::new(),
            task_detail: None,
            task_detail_scroll: 0,
            attention_detail: None,
            attention_diff_rect: Rect::default(),
            attention_allow_rect: Rect::default(),
            attention_deny_rect: Rect::default(),
            search: None,
            workspace: None,
            palette: None,
            palette_rect: Rect::default(),
            palette_hits: Vec::new(),
            session_picker: None,
            session_picker_rect: Rect::default(),
            session_picker_hits: Vec::new(),
            pending_session_switch: None,
            transcript_rect: Rect::default(),
            command_rect: Rect::default(),
            command_hits: Vec::new(),
            skill_rect: Rect::default(),
            skill_hits: Vec::new(),
            approval_rect: Rect::default(),
            question_rect: Rect::default(),
            question_hits: Vec::new(),
            search_rect: Rect::default(),
        }
    }
    fn toggle_composer_expanded(&mut self) {
        self.composer_expanded = !self.composer_expanded;
        self.focus = FocusPane::Prompt;
    }
    fn load_session(&mut self, session: &Session) {
        self.transcript = transcript(&session.messages);
        if self.transcript.is_empty() {
            self.transcript
                .push(welcome_message(&session.workspace, self.language));
        }
        self.input = PromptEditor::default();
        self.running = false;
        self.turn_started = None;
        self.last_progress_at = None;
        self.runtime_turn = false;
        self.discard_pending_approvals();
        self.discard_pending_questions();
        self.scroll_from_bottom = 0;
        self.follow_bottom = true;
        self.tools = ToolActivity::default();
        self.tools_expanded = false;
        self.attachments.clear();
        self.selected_attachment = 0;
        self.composer_expanded = false;
        self.notice = None;
        self.goal = session.goal.clone();
        self.transient_thought = None;
        self.selection_mode = false;
        self.chat_selection = None;
        self.transcript_rows.clear();
        self.transcript_render_offset = 0;
        self.progress_log.clear();
        self.search = None;
        self.palette = None;
        self.session_picker = None;
        self.pending_session_switch = None;
        self.attention_read = session.attention_read.clone();
        self.workspace = Some(session.workspace.clone());
        self.workspace_status = workspace_status(&session.workspace, self.language);
        self.workspace_attention = workspace_attention(&session.workspace);
        self.focus = FocusPane::Prompt;
    }
    fn sidebar_move(&mut self, delta: isize) {
        self.focus = FocusPane::Sidebar;
        self.sidebar_manual_scroll = false;
        self.sidebar_selected = if delta < 0 {
            self.sidebar_selected.checked_sub(1).unwrap_or(3)
        } else {
            (self.sidebar_selected + 1) % 4
        };
    }
    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::Prompt => FocusPane::Chat,
            FocusPane::Chat => FocusPane::Activity,
            FocusPane::Activity => FocusPane::Sidebar,
            FocusPane::Sidebar => FocusPane::Prompt,
        };
    }
    fn selected_attention(&self) -> Option<AttentionItem> {
        self.attention_items().get(self.attention_selected).cloned()
    }
    fn selected_remote_gate(&self) -> Option<crate::daemon::RemoteGate> {
        let item = self.selected_attention()?;
        if let Some(id) = item
            .id
            .strip_prefix("runtime-interaction:")
            .and_then(|value| value.parse::<uuid::Uuid>().ok())
        {
            return self
                .runtime_gates
                .iter()
                .find(|gate| gate.id() == id)
                .cloned();
        }
        let task_id = item
            .id
            .strip_prefix("runtime-task:")?
            .parse::<uuid::Uuid>()
            .ok()?;
        self.runtime_gates
            .iter()
            .find(|gate| gate.task_id() == task_id)
            .cloned()
    }
    fn selected_runtime_task_id(&self) -> Option<uuid::Uuid> {
        self.selected_attention()?
            .id
            .strip_prefix("runtime-task:")?
            .parse()
            .ok()
    }
    fn attention_activate(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(item) = self.selected_attention() else {
            self.sidebar_toggle();
            return;
        };
        if let Some(index) = self
            .background_tasks
            .iter()
            .position(|task| task.id == item.id)
        {
            self.open_task_detail(index, registry);
        } else {
            self.task_detail = None;
            self.agent_detail = None;
            self.worktree_review = None;
            self.diff_review = None;
            self.attention_detail = Some(item);
        }
    }
    fn attention_stop(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(item) = self.selected_attention() else {
            return;
        };
        if item.status == RuntimeStatus::Working && registry.kill(&item.id) {
            self.notice = Some(format!(
                "{}: {}",
                self.language.text(
                    "已请求停止任务",
                    "Task stop requested",
                    "タスク停止を要求しました"
                ),
                item.id
            ));
        }
    }
    fn attention_retry(&mut self, registry: &BackgroundTaskRegistry) -> bool {
        let Some(item) = self.selected_attention() else {
            return false;
        };
        if !matches!(
            item.status,
            RuntimeStatus::Blocked | RuntimeStatus::Failed | RuntimeStatus::Cancelled
        ) {
            return false;
        }
        let Some(retried_id) = registry.retry(&item.id) else {
            self.notice = Some(
                self.language
                    .text(
                        "这个任务没有可重放的启动信息",
                        "This task has no replayable launcher",
                        "このタスクには再実行情報がありません",
                    )
                    .to_owned(),
            );
            return false;
        };
        self.attention_read.insert(item.id);
        self.notice = Some(format!(
            "{}: {retried_id}",
            self.language
                .text("已重新启动任务", "Task restarted", "タスクを再実行しました")
        ));
        true
    }
    /// Dismiss one Inbox item by id and close the detail popup. Running
    /// items are refused: they are still doing something, and hiding them
    /// would lose the only handle the user has on them.
    fn attention_dismiss(&mut self, id: &str) -> bool {
        let running = self
            .attention_items()
            .into_iter()
            .find(|item| item.id == id)
            .is_some_and(|item| item.status == RuntimeStatus::Working);
        if running {
            return false;
        }
        self.attention_read.insert(id.to_owned());
        self.attention_detail = None;
        let remaining = self.attention_items().len();
        self.attention_selected = self.attention_selected.min(remaining.saturating_sub(1));
        true
    }

    fn attention_mark_read(&mut self) -> bool {
        let Some(item) = self.selected_attention() else {
            return false;
        };
        if item.status != RuntimeStatus::Working {
            self.attention_read.insert(item.id);
            let remaining = self.attention_items().len();
            self.attention_selected = self.attention_selected.min(remaining.saturating_sub(1));
            return true;
        }
        false
    }
    fn sidebar_toggle(&mut self) {
        self.sidebar_expanded[self.sidebar_selected] =
            !self.sidebar_expanded[self.sidebar_selected];
        self.sidebar_manual_scroll = false;
    }
    fn sidebar_scroll_by(&mut self, delta: isize) {
        self.focus = FocusPane::Sidebar;
        self.sidebar_manual_scroll = true;
        self.sidebar_scroll = if delta < 0 {
            self.sidebar_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.sidebar_scroll.saturating_add(delta as usize)
        };
    }
    fn sidebar_activate(&mut self, registry: &BackgroundTaskRegistry) {
        if self.sidebar_selected == 1 && self.sidebar_expanded[1] {
            self.attention_activate(registry);
        } else {
            self.sidebar_toggle();
        }
    }
    fn prefill_new_agent(&mut self) {
        if !self.input.is_empty() || !self.attachments.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "输入区已有草稿或附件，请先发送或清空后再新建 Agent",
                        "The composer has a draft or attachments; send or clear it before creating an Agent",
                        "入力欄に下書きまたは添付があります。送信または消去してから Agent を作成してください",
                    )
                    .to_owned(),
            );
            return;
        }
        self.input.insert("/agent spawn scout ");
        self.focus = FocusPane::Prompt;
    }
    fn open_task_detail(&mut self, index: usize, registry: &BackgroundTaskRegistry) {
        let Some(snapshot) = self.background_tasks.get(index).cloned() else {
            return;
        };
        let output = registry.output(&snapshot.id, 200).unwrap_or_else(|| {
            self.language
                .text("暂无输出", "No output", "出力なし")
                .to_owned()
        });
        self.attention_detail = None;
        self.agent_detail = None;
        self.worktree_review = None;
        self.diff_review = None;
        self.task_detail = Some(TaskDetail { snapshot, output });
        self.task_detail_scroll = 0;
    }
    fn handle_task_detail_key(&mut self, key: KeyEvent, registry: &BackgroundTaskRegistry) {
        let Some(detail) = &self.task_detail else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.task_detail = None,
            KeyCode::Up => self.task_detail_scroll = self.task_detail_scroll.saturating_sub(1),
            KeyCode::Down => self.task_detail_scroll = self.task_detail_scroll.saturating_add(1),
            KeyCode::PageUp => self.task_detail_scroll = self.task_detail_scroll.saturating_sub(10),
            KeyCode::PageDown => {
                self.task_detail_scroll = self.task_detail_scroll.saturating_add(10)
            }
            KeyCode::Home => self.task_detail_scroll = 0,
            KeyCode::End => self.task_detail_scroll = detail.output.lines().count(),
            KeyCode::Char('k') | KeyCode::Char('K')
                if detail.snapshot.status == BackgroundTaskStatus::Running =>
            {
                let id = detail.snapshot.id.clone();
                if registry.kill(&id) {
                    self.notice = Some(format!(
                        "{}: {id}",
                        self.language.text(
                            "已请求停止任务",
                            "Task stop requested",
                            "タスク停止を要求しました"
                        )
                    ));
                    self.task_detail = None;
                }
            }
            _ => {}
        }
    }
    fn handle_help_key(&mut self, key: KeyEvent) -> bool {
        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?')) {
                self.help_visible = false;
            }
            return true;
        }
        if key.code == KeyCode::F(1) || (key.code == KeyCode::Char('?') && self.input.is_empty()) {
            self.help_visible = true;
            return true;
        }
        false
    }
    fn handle_search_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f'))
        {
            self.search = None;
            return;
        }
        let mut query_changed = false;
        if let Some(search) = &mut self.search {
            match key.code {
                KeyCode::Enter if !search.matches.is_empty() => {
                    search.selected = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        search
                            .selected
                            .checked_sub(1)
                            .unwrap_or(search.matches.len() - 1)
                    } else {
                        (search.selected + 1) % search.matches.len()
                    };
                }
                KeyCode::Left => search.editor.left(),
                KeyCode::Right => search.editor.right(),
                KeyCode::Home => search.editor.home(),
                KeyCode::End => search.editor.end(),
                KeyCode::Backspace => {
                    search.editor.backspace();
                    query_changed = true;
                }
                KeyCode::Delete => {
                    search.editor.delete();
                    query_changed = true;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
                {
                    search.editor.insert(&character.to_string());
                    query_changed = true;
                }
                _ => return,
            }
        }
        if query_changed {
            self.refresh_search_matches();
        }
        self.jump_to_search_match();
    }
    fn open_palette(&mut self, skills: &SkillCatalog, store: &SessionStore, session: &Session) {
        let mut items = command_candidates(self.language)
            .into_iter()
            .map(|(command, description)| PaletteItem {
                label: command.to_owned(),
                description: description.to_owned(),
                action: PaletteAction::Command(command.to_owned()),
            })
            .collect::<Vec<_>>();
        items.extend(skills.list().iter().map(|skill| PaletteItem {
            label: format!("${}", skill.identifier),
            description: format!("{} · {}", skill.name, skill.description),
            action: PaletteAction::Skill(skill.identifier.clone()),
        }));
        items.push(PaletteItem {
            label: session.title.clone(),
            description: format!(
                "{} · {}",
                self.language
                    .text("当前会话", "Current session", "現在のセッション"),
                session.id
            ),
            action: PaletteAction::Session(session.id.to_string()),
        });
        {
            items.extend(
                store
                    .digests()
                    .into_iter()
                    .filter(|candidate| {
                        candidate.id != session.id && candidate.workspace == session.workspace
                    })
                    .take(30)
                    .map(|candidate| PaletteItem {
                        label: candidate.title,
                        description: format!(
                            "{} · {} · {}",
                            self.language.text("会话", "Session", "セッション"),
                            candidate.id,
                            candidate.workspace.display()
                        ),
                        action: PaletteAction::Session(candidate.id.to_string()),
                    }),
            );
        }
        items.extend(
            self.background_tasks
                .iter()
                .enumerate()
                .map(|(index, task)| {
                    let kind = if task.kind == willdeep_core::BackgroundTaskKind::Subagent {
                        self.language
                            .text("子 Agent", "Subagent", "サブエージェント")
                    } else {
                        self.language
                            .text("后台任务", "Background task", "バックグラウンドタスク")
                    };
                    PaletteItem {
                        label: task.id.clone(),
                        description: format!("{kind} · {:?} · {}", task.status, task.label),
                        action: PaletteAction::Task(index),
                    }
                }),
        );
        if let Some(workspace) = &self.workspace {
            items.extend(workspace_files(workspace, 300).into_iter().map(|path| {
                PaletteItem {
                    label: path.clone(),
                    description: self
                        .language
                        .text("工作区文件", "Workspace file", "ワークスペースファイル")
                        .to_owned(),
                    action: PaletteAction::File(path),
                }
            }));
        }
        let filtered = (0..items.len()).collect();
        self.palette = Some(PaletteState {
            editor: PromptEditor::default(),
            items,
            filtered,
            selected: 0,
        });
    }
    fn open_session_picker(&mut self, current_session: uuid::Uuid) {
        self.palette = None;
        self.search = None;
        self.session_picker = Some(SessionPickerState {
            current_session,
            ..Default::default()
        });
    }
    fn set_session_picker_results(
        &mut self,
        results: Vec<willdeep_runtime_protocol::SessionSearchResult>,
    ) {
        let Some(picker) = self.session_picker.as_mut() else {
            return;
        };
        picker.results = results;
        picker.selected = picker.selected.min(picker.results.len().saturating_sub(1));
    }
    fn handle_session_picker_key(&mut self, key: KeyEvent) -> SessionPickerAction {
        let Some(picker) = self.session_picker.as_mut() else {
            return SessionPickerAction::None;
        };
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r'))
        {
            return SessionPickerAction::Close;
        }
        match key.code {
            KeyCode::Up | KeyCode::BackTab if !picker.results.is_empty() => {
                picker.selected = picker
                    .selected
                    .checked_sub(1)
                    .unwrap_or(picker.results.len() - 1);
                SessionPickerAction::None
            }
            KeyCode::Down | KeyCode::Tab if !picker.results.is_empty() => {
                picker.selected = (picker.selected + 1) % picker.results.len();
                SessionPickerAction::None
            }
            KeyCode::Enter if !picker.results.is_empty() => {
                let result = &picker.results[picker.selected.min(picker.results.len() - 1)];
                SessionPickerAction::Switch(PendingSessionSwitch {
                    id: result.id.to_string(),
                    archived: result.status == willdeep_runtime_protocol::SessionStatus::Archived,
                })
            }
            KeyCode::Left => {
                picker.editor.left();
                SessionPickerAction::None
            }
            KeyCode::Right => {
                picker.editor.right();
                SessionPickerAction::None
            }
            KeyCode::Home => {
                picker.editor.home();
                SessionPickerAction::None
            }
            KeyCode::End => {
                picker.editor.end();
                SessionPickerAction::None
            }
            KeyCode::Backspace => {
                picker.editor.backspace();
                SessionPickerAction::Refresh
            }
            KeyCode::Delete => {
                picker.editor.delete();
                SessionPickerAction::Refresh
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                ) =>
            {
                picker.editor.insert(&character.to_string());
                SessionPickerAction::Refresh
            }
            _ => SessionPickerAction::None,
        }
    }
    fn handle_palette_key(&mut self, key: KeyEvent, registry: &BackgroundTaskRegistry) {
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'))
        {
            self.palette = None;
            return;
        }
        let mut query_changed = false;
        let mut activate = false;
        if let Some(palette) = &mut self.palette {
            match key.code {
                KeyCode::Up | KeyCode::BackTab => {
                    if !palette.filtered.is_empty() {
                        palette.selected = palette
                            .selected
                            .checked_sub(1)
                            .unwrap_or(palette.filtered.len() - 1);
                    }
                }
                KeyCode::Down | KeyCode::Tab => {
                    if !palette.filtered.is_empty() {
                        palette.selected = (palette.selected + 1) % palette.filtered.len();
                    }
                }
                KeyCode::Enter if !palette.filtered.is_empty() => activate = true,
                KeyCode::Left => palette.editor.left(),
                KeyCode::Right => palette.editor.right(),
                KeyCode::Home => palette.editor.home(),
                KeyCode::End => palette.editor.end(),
                KeyCode::Backspace => {
                    palette.editor.backspace();
                    query_changed = true;
                }
                KeyCode::Delete => {
                    palette.editor.delete();
                    query_changed = true;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
                {
                    palette.editor.insert(&character.to_string());
                    query_changed = true;
                }
                _ => {}
            }
        }
        if query_changed {
            self.refresh_palette_matches();
        }
        if activate {
            self.activate_palette_selection(registry);
        }
    }
    fn refresh_palette_matches(&mut self) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        let query = palette.editor.text().trim().to_lowercase();
        let mut ranked = palette
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let value = format!("{} {}", item.label, item.description).to_lowercase();
                fuzzy_score(&query, &value).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, index)| (*score, *index));
        palette.filtered = ranked.into_iter().map(|(_, index)| index).collect();
        palette.selected = 0;
    }
    fn activate_palette_selection(&mut self, registry: &BackgroundTaskRegistry) {
        let Some(palette) = self.palette.take() else {
            return;
        };
        let Some(item_index) = palette.filtered.get(palette.selected).copied() else {
            return;
        };
        match &palette.items[item_index].action {
            PaletteAction::Command(command) => {
                let suffix = if matches!(
                    command.as_str(),
                    "/goal"
                        | "/mobile"
                        | "/runtime"
                        | "/local"
                        | "/session"
                        | "/workspace"
                        | "/agent"
                ) {
                    " "
                } else {
                    ""
                };
                self.input.insert(&format!("{command}{suffix}"));
            }
            PaletteAction::Skill(identifier) => self.input.insert(&format!("${identifier} ")),
            PaletteAction::Session(id) => {
                self.pending_session_switch = Some(PendingSessionSwitch {
                    id: id.clone(),
                    archived: false,
                });
            }
            PaletteAction::Task(index) => self.open_task_detail(*index, registry),
            PaletteAction::File(path) => self.input.insert(&format!("{path} ")),
        }
    }
    fn refresh_search_matches(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        let query = search.editor.text().trim().to_lowercase();
        search.matches = if query.is_empty() {
            Vec::new()
        } else {
            self.transcript
                .iter()
                .enumerate()
                .filter_map(|(index, value)| value.to_lowercase().contains(&query).then_some(index))
                .collect()
        };
        search.selected = 0;
    }
    fn jump_to_search_match(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        let Some(entry) = search.matches.get(search.selected).copied() else {
            return;
        };
        let total = rendered_transcript_height(&self.transcript, self.transcript_width);
        let through_match =
            rendered_transcript_height(&self.transcript[..=entry], self.transcript_width);
        self.follow_bottom = false;
        self.scroll_from_bottom = total
            .saturating_sub(through_match)
            .min(total.saturating_sub(self.viewport_height));
    }
    fn edit_input(&mut self, edit: impl FnOnce(&mut PromptEditor)) {
        edit(&mut self.input);
        self.skill_selected = 0;
        self.skill_menu_dismissed = false;
        self.command_selected = 0;
        self.command_menu_dismissed = false;
    }
    fn command_matches(&self) -> Vec<(&'static str, &'static str)> {
        let Some((start, query)) = self.input.marker_query('/') else {
            return Vec::new();
        };
        if start != 0 {
            return Vec::new();
        }
        let query = query.to_ascii_lowercase();
        command_candidates(self.language)
            .into_iter()
            .filter(|(command, description)| {
                command[1..].starts_with(&query)
                    || description.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }
    fn handle_command_key(&mut self, key: KeyEvent) -> bool {
        if self.command_menu_dismissed || self.input.marker_query('/').is_none() {
            return false;
        }
        let matches = self.command_matches();
        match key.code {
            KeyCode::Esc => {
                self.command_menu_dismissed = true;
                true
            }
            KeyCode::Up if !matches.is_empty() => {
                self.command_selected = self
                    .command_selected
                    .checked_sub(1)
                    .unwrap_or(matches.len() - 1);
                true
            }
            KeyCode::Down if !matches.is_empty() => {
                self.command_selected = (self.command_selected + 1) % matches.len();
                true
            }
            KeyCode::Tab | KeyCode::Enter
                if !matches.is_empty()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                let command = matches[self.command_selected.min(matches.len() - 1)].0;
                let suffix = if matches!(
                    command,
                    "/goal"
                        | "/mobile"
                        | "/runtime"
                        | "/local"
                        | "/session"
                        | "/workspace"
                        | "/agent"
                ) {
                    " "
                } else {
                    ""
                };
                self.input
                    .replace_before_cursor(0, &format!("{command}{suffix}"));
                self.command_selected = 0;
                self.command_menu_dismissed = true;
                true
            }
            _ => false,
        }
    }
    fn skill_matches(&self, skills: &SkillCatalog) -> Vec<usize> {
        let Some((_, query)) = self.input.marker_query('$') else {
            return Vec::new();
        };
        let query = query.to_lowercase();
        skills
            .list()
            .iter()
            .enumerate()
            .filter(|(_, skill)| {
                format!("{} {} {}", skill.identifier, skill.name, skill.description)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
            .take(8)
            .collect()
    }
    fn handle_skill_key(&mut self, key: KeyEvent, skills: &SkillCatalog) -> bool {
        if self.skill_menu_dismissed || self.input.marker_query('$').is_none() {
            return false;
        }
        let matches = self.skill_matches(skills);
        match key.code {
            KeyCode::Esc => {
                self.skill_menu_dismissed = true;
                true
            }
            KeyCode::Up if !matches.is_empty() => {
                self.skill_selected = self
                    .skill_selected
                    .checked_sub(1)
                    .unwrap_or(matches.len() - 1);
                true
            }
            KeyCode::Down if !matches.is_empty() => {
                self.skill_selected = (self.skill_selected + 1) % matches.len();
                true
            }
            KeyCode::Tab | KeyCode::Enter
                if !matches.is_empty()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                let selected = matches[self.skill_selected.min(matches.len() - 1)];
                let skill = &skills.list()[selected];
                let (start, _) = self.input.marker_query('$').expect("skill query exists");
                self.input
                    .replace_before_cursor(start, &format!("${} ", skill.identifier));
                self.skill_selected = 0;
                self.skill_menu_dismissed = true;
                true
            }
            _ => false,
        }
    }
    fn finish_turn(&mut self) {
        if let Some(started) = self.turn_started.take() {
            self.last_elapsed = Some(started.elapsed());
        }
        self.running = false;
        self.last_progress_at = None;
        self.runtime_turn = false;
        self.transient_thought = None;
        self.activity_line = self.language.text("就绪", "Ready", "準備完了").to_owned();
    }

    fn begin_turn(&mut self, runtime_turn: bool, initial_progress: String) {
        let now = Instant::now();
        self.running = true;
        self.runtime_turn = runtime_turn;
        self.turn_started = Some(now);
        self.last_progress_at = Some(now);
        self.last_elapsed = None;
        self.progress_log.clear();
        self.record_progress(initial_progress);
    }

    fn ensure_runtime_turn(&mut self) {
        if self.running {
            self.runtime_turn = true;
            return;
        }
        self.tools.reset();
        self.begin_turn(
            true,
            self.language
                .text(
                    "已重新连接 Runtime · 正在恢复进度",
                    "Runtime reconnected · restoring progress",
                    "Runtime に再接続 · 進捗を復元中",
                )
                .to_owned(),
        );
    }

    fn observe_runtime_tasks(
        &mut self,
        tasks: &[crate::daemon::tui_bridge::RemoteTask],
        session_id: uuid::Uuid,
    ) {
        let has_active_task = tasks.iter().any(|task| {
            task.session_id == Some(session_id)
                && matches!(
                    task.status,
                    willdeep_runtime_protocol::TaskStatus::Queued
                        | willdeep_runtime_protocol::TaskStatus::Running
                        | willdeep_runtime_protocol::TaskStatus::Cancelling
                        | willdeep_runtime_protocol::TaskStatus::WaitingApproval
                        | willdeep_runtime_protocol::TaskStatus::WaitingAnswer
                )
        });
        if has_active_task && !self.running {
            self.ensure_runtime_turn();
        }
    }
    /// The Runtime's version when it differs from this binary's. The TUI is
    /// only a front end — a daemon started days ago keeps executing tools
    /// with its own (old) approval policy, so `willdeep --version` saying
    /// 0.22 proves nothing about what actually runs commands.
    pub(crate) fn stale_runtime_version(&self) -> Option<&str> {
        self.runtime_version
            .as_deref()
            .filter(|version| *version != willdeep_core::VERSION)
    }

    /// Record the Runtime version from a snapshot, announcing a mismatch in
    /// the transcript the first time it is seen.
    fn observe_runtime_version(&mut self, version: Option<String>) {
        if self.runtime_version.as_deref() != version.as_deref() {
            // A handoff to a different Runtime deserves a fresh warning.
            self.runtime_version_warned = false;
        }
        self.runtime_version = version;
        let Some(stale) = self.stale_runtime_version().map(str::to_owned) else {
            return;
        };
        if self.runtime_version_warned {
            return;
        }
        self.runtime_version_warned = true;
        let message = self
            .language
            .text(
                "⚠ Runtime {runtime} 与客户端 {client} 版本不一致。命令实际由 Runtime 执行，当前仍按旧版审批策略运行。请运行 `willdeep daemon upgrade` 后重试。",
                "⚠ Runtime {runtime} does not match client {client}. Commands execute inside the Runtime, which still applies its older approval policy. Run `willdeep daemon upgrade`, then retry.",
                "⚠ Runtime {runtime} とクライアント {client} のバージョンが不一致です。コマンドは Runtime 側で実行され、古い承認ポリシーが適用されます。`willdeep daemon upgrade` を実行してください。",
            )
            .replace("{runtime}", &stale)
            .replace("{client}", willdeep_core::VERSION);
        self.append_transcript(format!("System: {message}"));
    }

    /// Show an approval immediately, or queue it behind the one on screen.
    /// Returns true when the request became visible right now — the caller
    /// rings the terminal bell for that case, so a user looking elsewhere
    /// learns the turn is parked instead of watching it appear to hang.
    fn enqueue_approval(&mut self, request: ApprovalRequest) -> bool {
        let waiting = self
            .language
            .text("等待你确认", "Waiting for you", "確認待ち");
        self.record_progress(format!("{waiting} · {}", first_line(&request.0)));
        if self.approval.is_some() {
            self.approval_queue.push_back(request);
            return false;
        }
        self.approval = Some(request);
        true
    }

    /// Answer the visible approval and immediately promote the next queued
    /// one, so a turn that needs three confirmations asks three times in a
    /// row instead of stalling after the first.
    fn resolve_approval(&mut self, decide: impl FnOnce(bool) -> ApprovalDecision) {
        let Some((_, always, sender)) = self.approval.take() else {
            return;
        };
        let _ = sender.send(decide(always));
        self.approval = self.approval_queue.pop_front();
    }

    /// Show a question immediately, or queue it behind the visible one.
    /// Returns true when it became visible right now.
    ///
    /// A question pops even while the user is typing: the draft prompt in
    /// `self.input` is untouched (the dialog carries its own editor), so
    /// nothing already typed is lost — keystrokes are only redirected from
    /// the moment it appears.
    fn enqueue_question(&mut self, dialog: AskDialog) -> bool {
        let waiting = self
            .language
            .text("等待你回答", "Waiting for you", "回答待ち");
        self.record_progress(format!(
            "{waiting} · {}",
            first_line(&dialog.request.question)
        ));
        if self.question.is_some() {
            self.question_queue.push_back(dialog);
            return false;
        }
        self.question = Some(dialog);
        true
    }

    /// Promote the next queued question after the visible one is answered.
    fn promote_next_question(&mut self) {
        self.question = self.question_queue.pop_front();
    }

    /// Answer every parked question with "no answer", visibly.
    fn discard_pending_questions(&mut self) {
        let mut pending = Vec::new();
        if let Some(dialog) = self.question.take() {
            pending.push(dialog);
        }
        pending.extend(self.question_queue.drain(..));
        if pending.is_empty() {
            return;
        }
        for dialog in pending {
            let _ = dialog.sender.send(None);
        }
        self.notice = Some(
            self.language
                .text(
                    "切换会话已放弃待回答的提问",
                    "Pending questions dropped by session switch",
                    "セッション切り替えにより保留中の質問を破棄しました",
                )
                .to_owned(),
        );
    }

    /// Deny everything still parked, with a visible reason. Used when the
    /// user switches away from the session that raised them: dropping the
    /// senders would also deny, but silently.
    fn discard_pending_approvals(&mut self) {
        let mut pending = Vec::new();
        if let Some(request) = self.approval.take() {
            pending.push(request);
        }
        pending.extend(self.approval_queue.drain(..));
        if pending.is_empty() {
            return;
        }
        for (_, _, sender) in pending {
            let _ = sender.send(ApprovalDecision::Deny);
        }
        self.notice = Some(
            self.language
                .text(
                    "切换会话已拒绝待处理的审批",
                    "Pending approvals denied by session switch",
                    "セッション切り替えにより保留中の承認を拒否しました",
                )
                .to_owned(),
        );
    }

    fn record_progress(&mut self, value: String) {
        self.last_progress_at = Some(Instant::now());
        self.activity_line = value.clone();
        let elapsed = self
            .turn_started
            .map(|started| started.elapsed().as_secs_f32())
            .unwrap_or_default();
        self.progress_log
            .push_back(format!("{elapsed:>5.1}s · {value}"));
        while self.progress_log.len() > 12 {
            self.progress_log.pop_front();
        }
    }

    fn working_summary(&self) -> Option<String> {
        let started = self.turn_started?;
        let elapsed = started.elapsed();
        let idle = self
            .last_progress_at
            .map(|progress| progress.elapsed())
            .unwrap_or(elapsed);
        Some(format_working_summary(
            self.language,
            self.runtime_turn,
            &self.activity_line,
            elapsed,
            idle,
        ))
    }
    fn handle_question_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.question.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                if let Some(dialog) = self.question.take() {
                    let _ = dialog.sender.send(None);
                    self.promote_next_question();
                }
            }
            KeyCode::Up | KeyCode::BackTab => dialog.selected = dialog.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                if !dialog.request.options.is_empty() {
                    dialog.selected = (dialog.selected + 1) % dialog.request.options.len();
                }
            }
            KeyCode::Char(' ')
                if dialog.request.multi_select
                    && dialog.answer.is_empty()
                    && !dialog.checked.is_empty() =>
            {
                dialog.checked[dialog.selected] = !dialog.checked[dialog.selected];
            }
            KeyCode::Enter => {
                let dialog = self.question.take().expect("question dialog");
                let typed = dialog.answer.text().trim();
                let answer = if !typed.is_empty() {
                    typed.to_owned()
                } else if dialog.request.multi_select {
                    dialog
                        .request
                        .options
                        .into_iter()
                        .zip(dialog.checked)
                        .filter_map(|(option, checked)| checked.then_some(option))
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    dialog
                        .request
                        .options
                        .get(dialog.selected)
                        .cloned()
                        .unwrap_or_default()
                };
                let _ = dialog.sender.send(Some(answer));
                self.promote_next_question();
            }
            KeyCode::Left => dialog.answer.left(),
            KeyCode::Right => dialog.answer.right(),
            KeyCode::Home => dialog.answer.home(),
            KeyCode::End => dialog.answer.end(),
            KeyCode::Backspace => dialog.answer.backspace(),
            KeyCode::Delete => dialog.answer.delete(),
            KeyCode::Char(value)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                dialog.answer.insert(&value.to_string())
            }
            _ => {}
        }
    }
    fn max_scroll(&self) -> usize {
        self.transcript_height.saturating_sub(self.viewport_height)
    }
    fn scroll_up(&mut self, n: usize) {
        let max = self.max_scroll();
        if max == 0 {
            return self.scroll_to_bottom();
        }
        self.follow_bottom = false;
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(n).min(max);
    }
    fn scroll_down(&mut self, n: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n);
        if self.scroll_from_bottom == 0 {
            self.follow_bottom = true;
        }
    }
    fn scroll_to_top(&mut self) {
        let max = self.max_scroll();
        self.follow_bottom = max == 0;
        self.scroll_from_bottom = max;
    }
    fn scroll_to_bottom(&mut self) {
        self.follow_bottom = true;
        self.scroll_from_bottom = 0;
    }
    fn append_transcript(&mut self, v: String) {
        let previous_height = rendered_transcript_height(&self.transcript, self.transcript_width);
        self.transcript.push(v);
        self.transcript_height =
            rendered_transcript_height(&self.transcript, self.transcript_width);
        if !self.follow_bottom {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(self.transcript_height.saturating_sub(previous_height));
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
    }
    fn handle_paste(&mut self, value: String) {
        if value.contains('\n') || value.chars().count() > 200 {
            let n = self.attachments.len() + 1;
            self.attachments.push(DraftAttachment {
                message: MessageAttachment::Text {
                    name: format!("paste-{n}.txt"),
                    content: value,
                },
            });
            self.selected_attachment = self.attachments.len() - 1;
        } else {
            self.input.insert(&value);
        }
    }
    fn delete_selected_attachment(&mut self) {
        if self.attachments.is_empty() {
            return;
        }
        let index = self.selected_attachment.min(self.attachments.len() - 1);
        self.attachments.remove(index);
        self.selected_attachment = index.saturating_sub(1);
        self.notice = Some("Attachment removed".to_owned());
    }
    fn paste_clipboard_image(&mut self) {
        match clipboard_image() {
            Ok(value) => {
                self.attachments.push(value);
                self.selected_attachment = self.attachments.len() - 1;
                self.notice = Some("Clipboard image attached".to_owned());
            }
            Err(e) => self.notice = Some(format!("Clipboard image unavailable: {e}")),
        }
    }

    fn transcript_selection_point(
        &self,
        x: u16,
        y: u16,
        clamp_to_viewport: bool,
    ) -> Option<ChatSelectionPoint> {
        if self.transcript_rows.is_empty()
            || self.transcript_rect.width < 3
            || self.transcript_rect.height < 3
        {
            return None;
        }
        let left = self.transcript_rect.x.saturating_add(1);
        let right = self.transcript_rect.right().saturating_sub(2);
        let top = self.transcript_rect.y.saturating_add(1);
        let bottom = self.transcript_rect.bottom().saturating_sub(2);
        if !clamp_to_viewport && (x < left || x > right || y < top || y > bottom) {
            return None;
        }
        let visible_row = y.clamp(top, bottom).saturating_sub(top) as usize;
        let row = self
            .transcript_render_offset
            .saturating_add(visible_row)
            .min(self.transcript_rows.len().saturating_sub(1));
        let row_width = UnicodeWidthStr::width(self.transcript_rows[row].as_str());
        let column = x
            .clamp(left, right)
            .saturating_sub(left)
            .min(row_width.saturating_sub(1).min(u16::MAX as usize) as u16)
            as usize;
        Some(ChatSelectionPoint { row, column })
    }

    fn handle_chat_selection_mouse(&mut self, mouse: MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, false)
                else {
                    return self.selection_mode;
                };
                self.chat_selection = Some(ChatSelection {
                    anchor: point,
                    head: point,
                });
                self.focus = FocusPane::Chat;
                self.selection_mode
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(mut selection) = self.chat_selection else {
                    return false;
                };
                let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, true)
                else {
                    return false;
                };
                selection.head = point;
                self.chat_selection = Some(selection);
                self.selection_mode = true;
                self.focus = FocusPane::Chat;
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.chat_selection.is_some() => {
                if !self.selection_mode {
                    self.chat_selection = None;
                    return false;
                }
                if let Some(point) = self.transcript_selection_point(mouse.column, mouse.row, true)
                    && let Some(selection) = self.chat_selection.as_mut()
                {
                    selection.head = point;
                }
                true
            }
            MouseEventKind::ScrollUp if self.selection_mode => {
                self.scroll_up(3);
                true
            }
            MouseEventKind::ScrollDown if self.selection_mode => {
                self.scroll_down(3);
                true
            }
            _ => false,
        }
    }

    fn selected_chat_text(&self) -> String {
        let Some(selection) = self.chat_selection else {
            return String::new();
        };
        let (start, end) = selection.ordered_range();
        selected_text(&self.transcript_rows, start, end)
    }

    fn copy_chat_selection(&mut self) {
        let value = self.selected_chat_text();
        if value.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "请先在聊天区拖动选择文字",
                        "Drag to select chat text first",
                        "先にチャット内の文字をドラッグ選択してください",
                    )
                    .to_owned(),
            );
            return;
        }
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(value)) {
            Ok(()) => {
                self.notice = Some(
                    self.language
                        .text(
                            "已复制所选文字",
                            "Selected text copied",
                            "選択した文字をコピーしました",
                        )
                        .to_owned(),
                );
            }
            Err(error) => {
                self.notice = Some(format!(
                    "{}: {error}",
                    self.language.text(
                        "复制到剪贴板失败",
                        "Copy to clipboard failed",
                        "クリップボードへのコピーに失敗"
                    )
                ));
            }
        }
    }

    fn quote_chat_selection(&mut self) {
        let value = self.selected_chat_text();
        if value.is_empty() {
            self.notice = Some(
                self.language
                    .text(
                        "请先在聊天区拖动选择文字",
                        "Drag to select chat text first",
                        "先にチャット内の文字をドラッグ選択してください",
                    )
                    .to_owned(),
            );
            return;
        }
        if !self.input.is_empty() {
            self.input.insert("\n\n");
        }
        self.input.insert(&quote_selected_text(&value));
        self.selection_mode = false;
        self.chat_selection = None;
        self.focus = FocusPane::Prompt;
        self.notice = Some(
            self.language
                .text(
                    "已引用到输入框",
                    "Selection quoted into the prompt",
                    "選択範囲を入力欄に引用しました",
                )
                .to_owned(),
        );
    }

    fn activate_session_picker_at(&mut self, x: u16, y: u16) -> bool {
        if !self.session_picker_rect.contains((x, y).into()) {
            return false;
        }
        if y == self.session_picker_rect.y.saturating_add(1) {
            if let Some(picker) = self.session_picker.as_mut() {
                picker.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.session_picker_rect.x + 3) as usize,
                    self.session_picker_rect.width.saturating_sub(4).max(1) as usize,
                );
            }
            return true;
        }
        let Some(position) = self
            .session_picker_hits
            .iter()
            .find_map(|(row, position)| (*row == y).then_some(*position))
        else {
            return true;
        };
        let Some(picker) = self.session_picker.as_mut() else {
            return true;
        };
        picker.selected = position.min(picker.results.len().saturating_sub(1));
        if let Some(result) = picker.results.get(picker.selected) {
            self.pending_session_switch = Some(PendingSessionSwitch {
                id: result.id.to_string(),
                archived: result.status == willdeep_runtime_protocol::SessionStatus::Archived,
            });
            self.session_picker = None;
        }
        true
    }

    fn handle_mouse(
        &mut self,
        x: u16,
        y: u16,
        registry: &BackgroundTaskRegistry,
        skills: &SkillCatalog,
    ) {
        if self.search.is_some() && self.search_rect.contains((x, y).into()) {
            if let Some(search) = &mut self.search {
                search.editor.set_cursor_visual(
                    0,
                    x.saturating_sub(self.search_rect.x + 1) as usize,
                    self.search_rect.width.saturating_sub(2).max(1) as usize,
                );
            }
        } else if self.approval.is_some()
            && self.approval_rect.contains((x, y).into())
            && y >= self.approval_rect.bottom().saturating_sub(2)
        {
            let relative = x.saturating_sub(self.approval_rect.x) as usize;
            let width = self.approval_rect.width.max(1) as usize;
            self.resolve_approval(|always| {
                if always {
                    match relative.saturating_mul(3) / width {
                        0 => ApprovalDecision::AllowOnce,
                        1 => ApprovalDecision::AlwaysAllow,
                        _ => ApprovalDecision::Deny,
                    }
                } else if relative < width / 2 {
                    ApprovalDecision::AllowOnce
                } else {
                    ApprovalDecision::Deny
                }
            });
        } else if self.question.is_some() && self.question_rect.contains((x, y).into()) {
            if let Some((_, selected)) = self
                .question_hits
                .iter()
                .find(|(row, _)| *row == y)
                .copied()
            {
                let multi = self
                    .question
                    .as_ref()
                    .is_some_and(|dialog| dialog.request.multi_select);
                if multi {
                    if let Some(dialog) = &mut self.question {
                        dialog.selected = selected;
                        dialog.checked[selected] = !dialog.checked[selected];
                    }
                } else if let Some(dialog) = self.question.take() {
                    let answer = dialog.request.options.get(selected).cloned();
                    let _ = dialog.sender.send(answer);
                    self.promote_next_question();
                }
            } else if y >= self.question_rect.bottom().saturating_sub(2) {
                let code = if x < self.question_rect.x + self.question_rect.width / 2 {
                    KeyCode::Esc
                } else {
                    KeyCode::Enter
                };
                self.handle_question_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
        } else if self.palette.is_some() && self.palette_rect.contains((x, y).into()) {
            if let Some((_, position)) =
                self.palette_hits.iter().find(|(row, _)| *row == y).copied()
            {
                if let Some(palette) = &mut self.palette {
                    palette.selected = position;
                }
                self.activate_palette_selection(registry);
            }
        } else if self.command_rect.contains((x, y).into()) {
            if let Some((_, selected)) =
                self.command_hits.iter().find(|(row, _)| *row == y).copied()
            {
                self.command_selected = selected;
                self.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
        } else if self.skill_rect.contains((x, y).into()) {
            if let Some((_, selected)) = self.skill_hits.iter().find(|(row, _)| *row == y).copied()
            {
                self.skill_selected = selected;
                self.handle_skill_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), skills);
            }
        } else if self.sidebar_rect.contains((x, y).into()) {
            self.focus = FocusPane::Sidebar;
            if let Some((_, hit)) = self.sidebar_hits.iter().find(|(row, _)| *row == y).copied() {
                match hit {
                    SidebarHit::Section(section) => {
                        self.sidebar_selected = section;
                        self.sidebar_toggle();
                    }
                    SidebarHit::Attention(index) => {
                        self.sidebar_selected = 1;
                        self.attention_selected = index;
                        self.attention_activate(registry);
                    }
                    SidebarHit::NewAgent => self.prefill_new_agent(),
                }
            }
        } else if self.transcript_rect.contains((x, y).into()) {
            self.focus = FocusPane::Chat;
        } else if self.activity_rect.contains((x, y).into()) {
            self.focus = FocusPane::Activity;
        } else if self.prompt_rect.contains((x, y).into()) {
            self.focus = FocusPane::Prompt;
            let row = y.saturating_sub(self.prompt_rect.y + 1) as usize + self.prompt_scroll;
            let col = x.saturating_sub(self.prompt_rect.x + 1) as usize;
            self.input.set_cursor_visual(
                row,
                col,
                self.prompt_rect.width.saturating_sub(2) as usize,
            );
        }
    }
    fn handle_slash_command(&mut self, prompt: &str, skills: &SkillCatalog) -> bool {
        let value = prompt.trim();
        if !value.starts_with('/') {
            return false;
        }
        let (command, args) = value.split_once(' ').unwrap_or((value, ""));
        if matches!(
            command,
            "/agent"
                | "/compress"
                | "/daemon"
                | "/diff"
                | "/local"
                | "/mobile"
                | "/runtime"
                | "/session"
                | "/webapp"
                | "/workspace"
        ) {
            return false;
        }
        match command {
            "/help" => self.append_transcript(
                "System: prompts use Runtime by default · /local <task> · /goal <text>|off · /compress · /sidebar [on|off] (status sidebar, hidden by default; Ctrl+B too) · /daemon [status|start|stop|upgrade] · /webapp [status|start|stop|127.0.0.1:PORT] · /runtime <task> · /session <action> · /workspace list|switch <id> · /agent instruct <id> <text> · /mobile [show|hide|off] · /skills · /clear · /help · use $skill-name in prompts"
                    .to_owned(),
            ),
            "/goal" if args.trim().eq_ignore_ascii_case("off") => {
                self.goal = None;
                self.append_transcript("System: Goal mode disabled".to_owned());
            }
            "/goal" if !args.trim().is_empty() => {
                self.goal = Some(args.trim().to_owned());
                self.append_transcript(format!("System: Goal mode · {}", args.trim()));
            }
            "/goal" => self.append_transcript(format!(
                "System: Goal · {}",
                self.goal.as_deref().unwrap_or("not set")
            )),
            "/sidebar" => {
                let argument = args.trim().to_ascii_lowercase();
                let visible = match argument.as_str() {
                    "" | "toggle" => !self.sidebar_visible,
                    "on" | "show" | "open" => true,
                    "off" | "hide" | "close" => false,
                    other => {
                        self.append_transcript(format!(
                            "Error: usage: {command} [on|off] (got `{other}`)"
                        ));
                        return true;
                    }
                };
                self.sidebar_visible = visible;
                if !visible {
                    self.focus = FocusPane::Prompt;
                }
                self.append_transcript(format!(
                    "System: {}",
                    if visible {
                        self.language.text(
                            "状态栏已显示（Ctrl+B 隐藏）",
                            "Status sidebar shown (Ctrl+B hides it)",
                            "状態サイドバーを表示しました（Ctrl+B で非表示）",
                        )
                    } else {
                        self.language.text(
                            "状态栏已隐藏（/sidebar 或 Ctrl+B 显示）",
                            "Status sidebar hidden (/sidebar or Ctrl+B shows it)",
                            "状態サイドバーを非表示にしました（/sidebar または Ctrl+B で表示）",
                        )
                    }
                ));
            }
            "/skills" => {
                self.append_transcript(format!("System: Available skills\n{}", skills.summary()))
            }
            "/clear" => {
                self.transcript.clear();
                self.scroll_to_bottom();
            }
            _ => self.append_transcript(format!("Error: unknown command {command}; use /help")),
        }
        true
    }
    fn handle_mobile_command(
        &mut self,
        prompt: &str,
        home: &std::path::Path,
        bridge: &RelayBridge,
        mobile_tx: &mpsc::UnboundedSender<MobilePrompt>,
        session: &Session,
    ) -> bool {
        let value = prompt.trim();
        if !matches!(
            value,
            "/mobile" | "/mobile show" | "/mobile hide" | "/mobile off"
        ) {
            return false;
        }
        match value {
            "/mobile off" => {
                self.mobile_gateway = None;
                self.mobile_qr = None;
                self.append_transcript("System: Mobile relay disconnected".to_owned());
            }
            "/mobile hide" => self.mobile_qr = None,
            _ => {
                if self.mobile_gateway.is_none() {
                    match RelayGateway::start(
                        home,
                        bridge.clone(),
                        mobile_tx.clone(),
                        mobile_snapshot(session),
                    ) {
                        Ok(gateway) => {
                            self.append_transcript(format!(
                                "System: Mobile relay connected · room {}",
                                gateway.room
                            ));
                            self.mobile_gateway = Some(gateway);
                        }
                        Err(error) => {
                            self.append_transcript(format!("Error: start mobile relay: {error:#}"))
                        }
                    }
                }
                self.mobile_qr = self
                    .mobile_gateway
                    .as_ref()
                    .map(|gateway| gateway.qr.clone());
            }
        }
        true
    }
    fn enrich_prompt(&self, prompt: &str, skills: &SkillCatalog) -> String {
        let mut blocks = Vec::new();
        if let Some(goal) = &self.goal {
            blocks.push(format!(
                "<goal>\n{goal}\n</goal>\nContinue until this goal is genuinely complete."
            ));
        }
        for token in prompt
            .split_whitespace()
            .filter(|value| value.starts_with('$'))
        {
            let name = token
                .trim_start_matches('$')
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
            if !name.is_empty()
                && let Ok(body) = skills.read(name, None)
            {
                blocks.push(format!(
                    "<explicit_skill name=\"{name}\">\n{body}\n</explicit_skill>"
                ));
            }
        }
        if blocks.is_empty() {
            prompt.to_owned()
        } else {
            format!("{}\n\n{prompt}", blocks.join("\n\n"))
        }
    }
}

fn mobile_snapshot(session: &Session) -> serde_json::Value {
    serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "type": "state.snapshot",
        "session_id": session.id,
        "payload": {
            "active_session_id": session.id,
            "sessions": [{
                "id": session.id,
                "title": session.title,
                "workspace_name": session.workspace.file_name().and_then(|value| value.to_str()).unwrap_or("Workspace"),
                "workspace_path": session.workspace,
                "message_count": session.messages.len(),
                "is_active": true,
                "is_responding": false,
                "updated_at": session.updated_at,
            }],
            "messages": [],
        }
    })
}

fn clipboard_image() -> Result<DraftAttachment> {
    let image = arboard::Clipboard::new()?.get_image()?;
    encode_clipboard_image(image.width, image.height, image.bytes.into_owned())
}

fn encode_clipboard_image(width: usize, height: usize, bytes: Vec<u8>) -> Result<DraftAttachment> {
    const MAX_RGBA_BYTES: usize = 64 * 1024 * 1024;
    let expected = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(4))
        .context("clipboard image dimensions overflow")?;
    if expected > MAX_RGBA_BYTES {
        return Err(anyhow::anyhow!("clipboard image exceeds 64 MB RGBA limit"));
    }
    if bytes.len() != expected {
        return Err(anyhow::anyhow!("invalid clipboard RGBA byte count"));
    }
    let width = u32::try_from(width).context("clipboard image width too large")?;
    let height = u32::try_from(height).context("clipboard image height too large")?;
    let rgba = RgbaImage::from_raw(width, height, bytes).context("invalid clipboard RGBA data")?;
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(rgba).write_to(&mut png, ImageFormat::Png)?;
    let data = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    Ok(DraftAttachment {
        message: MessageAttachment::Image {
            name: "clipboard.png".to_owned(),
            media_type: "image/png".to_owned(),
            data,
            width,
            height,
        },
    })
}

const PROGRESS_WAITING_AFTER: Duration = Duration::from_secs(8);
const PROGRESS_STALE_AFTER: Duration = Duration::from_secs(30);
const PROGRESS_SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

fn progress_spinner(elapsed: Duration) -> &'static str {
    let index = elapsed.as_secs() as usize % PROGRESS_SPINNER.len();
    PROGRESS_SPINNER[index]
}

fn format_working_summary(
    language: Language,
    runtime_turn: bool,
    activity_line: &str,
    elapsed: Duration,
    idle: Duration,
) -> String {
    let phase = if idle >= PROGRESS_STALE_AFTER {
        format!(
            "{} {:.0}s",
            language.text(
                "暂未收到新事件 · 已等待",
                "No new event yet · waiting",
                "新しいイベントなし · 待機"
            ),
            idle.as_secs_f32()
        )
    } else if idle >= PROGRESS_WAITING_AFTER {
        language
            .text(
                if runtime_turn {
                    "等待 Runtime / 模型返回"
                } else {
                    "等待模型 / 工具返回"
                },
                if runtime_turn {
                    "Waiting for Runtime / model"
                } else {
                    "Waiting for model / tool"
                },
                if runtime_turn {
                    "Runtime / モデルの応答待ち"
                } else {
                    "モデル / ツールの応答待ち"
                },
            )
            .to_owned()
    } else if activity_line.is_empty() {
        language.text("正在处理", "Working", "処理中").to_owned()
    } else {
        activity_line.to_owned()
    };
    format!(
        "{} {phase} · {} {:.1}s",
        progress_spinner(elapsed),
        language.text("已运行", "elapsed", "経過"),
        elapsed.as_secs_f32()
    )
}

fn draw(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    skills: &SkillCatalog,
) -> Result<()> {
    term.draw(|f| {
        app.sidebar_wide = f.area().width >= 110;
        let wide_sidebar = app.sidebar_visible && app.sidebar_wide && !app.composer_expanded;
        let columns = if wide_sidebar {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(76), Constraint::Percentage(24)])
                .split(f.area())
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(100), Constraint::Length(0)])
                .split(f.area())
        };
        let canvas = columns[0];
        let activity = if app.composer_expanded {
            0
        } else if app.tools_expanded && app.tools.requested > 0 {
            8
        } else if app.running {
            5
        } else {
            3
        };
        let attach = if app.composer_expanded || app.attachments.is_empty() {
            0
        } else {
            3
        };
        let input_width = canvas.width.saturating_sub(2).max(1) as usize;
        let input_lines = app.input.visual_line_count(input_width).clamp(3, 6);
        let input_height = (input_lines + 2) as u16;
        let constraints = composer_layout_constraints(
            app.composer_expanded,
            activity,
            attach,
            input_height,
        );
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(canvas);
        let mut visible_transcript = app.transcript.clone();
        if let Some(thought) = &app.transient_thought {
            visible_transcript.push(format!(
                "WillDeep · {}: {thought}",
                app.language.text("思考中", "thinking", "思考中")
            ));
        }
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.transcript_rect = areas[0];
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        app.transcript_height =
            rendered_transcript_height(&visible_transcript, app.transcript_width);
        let max = app.max_scroll();
        app.scroll_from_bottom = app.scroll_from_bottom.min(max);
        let offset = max
            .saturating_sub(app.scroll_from_bottom)
            .min(u16::MAX as usize) as u16;
        let mut title = if app.selection_mode {
            app.language
                .text(
                    "WillDeep · 拖动选择 · Ctrl/Cmd+C 或 Y 复制 · Q 引用 · Esc 退出",
                    "WillDeep · drag to select · Ctrl/Cmd+C or Y copy · Q quote · Esc exits",
                    "WillDeep · ドラッグ選択 · Ctrl/Cmd+C / Y コピー · Q 引用 · Esc 終了",
                )
                .to_owned()
        } else if app.follow_bottom {
            if app.focus == FocusPane::Chat {
                app.language
                    .text("WillDeep [焦点]", "WillDeep [focused]", "WillDeep [フォーカス]")
                    .to_owned()
            } else {
                "WillDeep".to_owned()
            }
        } else {
            format!(
                "WillDeep{} · history ↑{}",
                if app.focus == FocusPane::Chat {
                    app.language.text(" [焦点]", " [focused]", " [フォーカス]")
                } else {
                    ""
                },
                app.scroll_from_bottom
            )
        };
        if app.running
            && let Some(started) = app.turn_started
        {
            let elapsed = started.elapsed();
            title.push_str(&format!(
                " · {} {} {:.0}s",
                progress_spinner(elapsed),
                app.language.text("工作中", "working", "作業中"),
                elapsed.as_secs_f32()
            ));
        }
        let search_query = app
            .search
            .as_ref()
            .map(|search| search.editor.text().trim())
            .filter(|query| !query.trim().is_empty());
        let mut colored = wrap_styled_text(
            colored_transcript_at_width(
                &visible_transcript,
                search_query,
                app.transcript_width,
            ),
            app.transcript_width,
        );
        app.transcript_rows = text_rows(&colored);
        app.transcript_render_offset = offset as usize;
        if let Some(selection) = app.chat_selection {
            let (start, end) = selection.ordered_range();
            highlight_text_selection(&mut colored, start, end);
        }
        f.render_widget(
            Paragraph::new(colored)
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(if app.focus == FocusPane::Chat {
                            Color::Cyan
                        } else {
                            Color::Blue
                        })),
                )
                .scroll((offset, 0)),
            areas[0],
        );
        if activity > 0 {
            app.activity_rect = areas[1];
            let text = if app.tools_expanded {
                let mut lines = Vec::new();
                if let Some(summary) = app.working_summary() {
                    lines.push(summary);
                }
                lines.push(format!(
                    "{} · {}",
                    app.activity_line,
                    app.tools.summary(app.language)
                ));
                lines.extend(
                    app.tools
                        .details
                        .iter()
                        .rev()
                        .take(4)
                        .rev()
                        .cloned(),
                );
                lines.join("\n")
            } else if let Some(summary) = app.working_summary() {
                let history = app
                    .progress_log
                    .iter()
                    .rev()
                    .take(2)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                if history.is_empty() {
                    summary
                } else {
                    format!("{summary}\n{history}")
                }
            } else if app.tools.requested == 0 {
                app.activity_line.clone()
            } else {
                format!(
                    "{} · {}",
                    app.activity_line,
                    app.tools.summary(app.language)
                )
            };
            f.render_widget(
                Paragraph::new(text).block(
                    Block::default()
                        .title(app.language.text(
                            if app.focus == FocusPane::Activity {
                                "活动 [焦点] · Enter 展开/收起"
                            } else {
                                "活动 · Ctrl+O 查看详情"
                            },
                            if app.focus == FocusPane::Activity {
                                "Activity [focused] · Enter expand/collapse"
                            } else {
                                "Activity · Ctrl+O details"
                            },
                            if app.focus == FocusPane::Activity {
                                "アクティビティ [フォーカス] · Enter で開閉"
                            } else {
                                "アクティビティ · Ctrl+O で詳細"
                            },
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(
                            if app.focus == FocusPane::Activity {
                                Color::Cyan
                            } else {
                                Color::DarkGray
                            },
                        )),
                ),
                areas[1],
            );
        }
        if attach > 0 {
            let items = app
                .attachments
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    format!(
                        "{}[{}]",
                        if i == app.selected_attachment {
                            "▶ "
                        } else {
                            "  "
                        },
                        v.summary()
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            f.render_widget(
                Paragraph::new(items).block(
                    Block::default()
                        .title(app.language.text(
                            "附件 · Ctrl+D 删除",
                            "Attachments · Ctrl+D remove",
                            "添付ファイル · Ctrl+D で削除",
                        ))
                        .borders(Borders::ALL),
                ),
                areas[2],
            );
        }
        app.prompt_rect = areas[3];
        let width = areas[3].width.saturating_sub(2).max(1) as usize;
        let (row, col) = app.input.cursor_visual(width);
        let visible = areas[3].height.saturating_sub(2).max(1) as usize;
        app.prompt_scroll = row.saturating_sub(visible - 1);
        let wrapped_input = app.input.wrapped_text(width);
        f.render_widget(
            Paragraph::new(wrapped_input)
                .block(
                    Block::default()
                        .title(if app.focus == FocusPane::Prompt {
                            if app.composer_expanded {
                                app.language.text(
                                    "输入 [大空间] · F2 恢复 · Shift/Alt+Enter 换行",
                                    "Prompt [expanded] · F2 restore · Shift/Alt+Enter newline",
                                    "入力 [拡大] · F2 で戻す · Shift/Alt+Enter で改行",
                                )
                            } else {
                                app.language.text(
                                    "输入 [焦点] · F2 展开 · Shift/Alt+Enter 换行",
                                    "Prompt [focused] · F2 expand · Shift/Alt+Enter newline",
                                    "入力 [フォーカス] · F2 で拡大 · Shift/Alt+Enter で改行",
                                )
                            }
                        } else {
                            app.language.text("输入", "Prompt", "入力")
                        })
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(if app.focus == FocusPane::Prompt {
                            Color::Cyan
                        } else {
                            Color::DarkGray
                        })),
                )
                // Same cyan the transcript gives `You:` lines, so what you are
                // typing and what you already said read as one voice instead of
                // falling back to the terminal's default foreground.
                .style(Style::default().fg(Color::Cyan))
                .scroll((app.prompt_scroll.min(u16::MAX as usize) as u16, 0)),
            areas[3],
        );
        let cursor_y = areas[3].y + 1 + (row.saturating_sub(app.prompt_scroll) as u16);
        let cursor_x = areas[3].x + 1 + (col.min(width.saturating_sub(1)) as u16);
        if app.focus == FocusPane::Prompt && !app.help_visible && app.task_detail.is_none() {
            f.set_cursor_position((cursor_x, cursor_y));
        }
        let status = if app.selection_mode {
            app.notice.take().unwrap_or_else(|| {
                app.language
                    .text(
                        "文本选择模式 · 鼠标拖选 · Ctrl/Cmd+C 或 Y 复制 · Q 引用 · Esc 退出",
                        "Text selection · drag · Ctrl/Cmd+C or Y copy · Q quote · Esc exits",
                        "テキスト選択 · ドラッグ · Ctrl/Cmd+C / Y コピー · Q 引用 · Esc 終了",
                    )
                    .to_owned()
            })
        } else {
            app.notice.take().unwrap_or_else(|| {
            let input = app.latest_usage.input_tokens.unwrap_or(0);
            let output = app.latest_usage.output_tokens.unwrap_or(0);
            let context_tokens = app.context_tokens.max(input);
            let context_pct = context_tokens.saturating_mul(100) / app.context_window.max(1);
            let elapsed = app
                .turn_started
                .map(|value| value.elapsed())
                .or(app.last_elapsed)
                .unwrap_or_default()
                .as_secs_f32();
            if app.running {
                format!(
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output} · Ctrl+S {} · F1",
                    app.working_summary().unwrap_or_else(|| app.language.text("运行中", "Running", "実行中").to_owned()),
                    app.language.text("焦点", "Focus", "フォーカス"),
                    focus_label(app.focus, app.language),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近"),
                    app.language.text("选择", "select", "選択")
                )
            } else {
                format!(
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output} · {elapsed:.1}s · {} · Ctrl+S {} · F1",
                    app.language.text("就绪", "Ready", "準備完了"),
                    app.language.text("焦点", "Focus", "フォーカス"),
                    focus_label(app.focus, app.language),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近"),
                    app.language
                        .text("Enter 发送", "Enter send", "Enter で送信"),
                    app.language.text("选择", "select", "選択")
                )
            }
        })};
        f.render_widget(Paragraph::new(status), areas[4]);
        app.sidebar_rect = Rect::default();
        if app.sidebar_visible && (wide_sidebar || app.focus == FocusPane::Sidebar) {
            let sidebar = if wide_sidebar {
                columns[1]
            } else {
                let width = f.area().width.min(46);
                Rect {
                    x: f.area().right().saturating_sub(width),
                    y: f.area().y,
                    width,
                    height: f.area().height,
                }
            };
            app.sidebar_rect = sidebar;
            if !wide_sidebar {
                f.render_widget(Clear, sidebar);
            }
            render_sidebar(f, app, sidebar);
        }
        app.palette_rect = Rect::default();
        app.palette_hits.clear();
        if let Some(palette) = &app.palette {
            let width = f.area().width.min(92);
            let height = f
                .area()
                .height
                .min((palette.filtered.len().min(16) as u16 + 3).max(6));
            let popup = centered_rect(width, height, f.area());
            app.palette_rect = popup;
            let visible = popup.height.saturating_sub(3).max(1) as usize;
            let start = palette.selected.saturating_sub(visible - 1);
            let mut lines = vec![Line::styled(
                format!("› {}", palette.editor.text()),
                Style::default().fg(Color::Yellow),
            )];
            for (position, item_index) in palette
                .filtered
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
            {
                let item = &palette.items[*item_index];
                let selected = position == palette.selected;
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightMagenta)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::styled(
                    format!("{} {} · {}", if selected { "▶" } else { " " }, item.label, item.description),
                    style,
                ));
                app.palette_hits
                    .push((popup.y + 2 + (position - start) as u16, position));
            }
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(format!(
                            "{} · {}/{} · ↑/↓/Tab · Enter · Esc",
                            app.language.text("命令面板", "Command palette", "コマンドパレット"),
                            if palette.filtered.is_empty() { 0 } else { palette.selected + 1 },
                            palette.filtered.len()
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::LightMagenta)),
                ),
                popup,
            );
            if popup.width > 3 {
                let cursor = UnicodeWidthStr::width(palette.editor.text())
                    .min(popup.width.saturating_sub(4) as usize) as u16;
                f.set_cursor_position((popup.x + 3 + cursor, popup.y + 1));
            }
        }
        app.session_picker_rect = Rect::default();
        app.session_picker_hits.clear();
        if let Some(picker) = &app.session_picker {
            let width = f.area().width.min(100);
            let height = f
                .area()
                .height
                .min((picker.results.len().min(16) as u16 + 3).max(7));
            let popup = centered_rect(width, height, f.area());
            app.session_picker_rect = popup;
            let visible = popup.height.saturating_sub(3).max(1) as usize;
            let start = picker.selected.saturating_sub(visible - 1);
            let mut lines = vec![Line::styled(
                format!("› {}", picker.editor.text()),
                Style::default().fg(Color::Yellow),
            )];
            if picker.results.is_empty() {
                lines.push(Line::styled(
                    app.language.text(
                        "没有匹配的历史会话",
                        "No historical Sessions match",
                        "一致する履歴セッションがありません",
                    ),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                for (position, result) in
                    picker.results.iter().enumerate().skip(start).take(visible)
                {
                    let selected = position == picker.selected;
                    let style = if selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::LightCyan)
                            .add_modifier(Modifier::BOLD)
                    } else if result.id == picker.current_session {
                        Style::default().fg(Color::LightGreen)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    lines.push(Line::styled(
                        session_picker_result_line(
                            result,
                            result.id == picker.current_session,
                            app.language,
                            selected,
                        ),
                        style,
                    ));
                    app.session_picker_hits
                        .push((popup.y + 2 + (position - start) as u16, position));
                }
            }
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(format!(
                            "{} · {}/{} · ↑/↓/Tab · Enter · Esc",
                            app.language.text(
                                "历史会话（当前工作区）",
                                "Session history (current Workspace)",
                                "履歴セッション（現在のワークスペース）"
                            ),
                            if picker.results.is_empty() {
                                0
                            } else {
                                picker.selected + 1
                            },
                            picker.results.len()
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::LightCyan)),
                ),
                popup,
            );
            if popup.width > 3 {
                let cursor = UnicodeWidthStr::width(picker.editor.text())
                    .min(popup.width.saturating_sub(4) as usize)
                    as u16;
                f.set_cursor_position((popup.x + 3 + cursor, popup.y + 1));
            }
        }
        app.search_rect = Rect::default();
        if let Some(search) = &app.search {
            let width = f.area().width.min(72);
            let popup = Rect {
                x: f.area().x + f.area().width.saturating_sub(width) / 2,
                y: f.area().y,
                width,
                height: 3.min(f.area().height),
            };
            app.search_rect = popup;
            let position = if search.matches.is_empty() {
                "0/0".to_owned()
            } else {
                format!("{}/{}", search.selected + 1, search.matches.len())
            };
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(search.editor.text()).block(
                    Block::default()
                        .title(format!(
                            "{} · {position} · Enter/Shift+Enter · Esc",
                            app.language.text("搜索聊天", "Search chat", "チャット検索")
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                popup,
            );
            if app.approval.is_none() && app.question.is_none() && popup.width > 2 {
                let cursor = UnicodeWidthStr::width(search.editor.text())
                    .min(popup.width.saturating_sub(3) as usize) as u16;
                f.set_cursor_position((popup.x + 1 + cursor, popup.y + 1));
            }
        }
        app.command_rect = Rect::default();
        app.command_hits.clear();
        let command_matches = app.command_matches();
        if !app.command_menu_dismissed
            && !command_matches.is_empty()
            && app.input.marker_query('/').is_some()
        {
            app.command_selected = app.command_selected.min(command_matches.len() - 1);
            let width = areas[3].width.min(76);
            let height = (command_matches.len() as u16 + 2).min(10);
            let popup = Rect {
                x: areas[3].x,
                y: areas[3].y.saturating_sub(height),
                width,
                height,
            };
            app.command_rect = popup;
            let lines = command_matches
                .iter()
                .enumerate()
                .map(|(position, (command, description))| {
                    let prefix = if position == app.command_selected {
                        "▶"
                    } else {
                        " "
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{prefix} {command} "),
                            Style::default()
                                .fg(Color::LightMagenta)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(*description, Style::default().fg(Color::White)),
                    ])
                })
                .collect::<Vec<_>>();
            app.command_hits = (0..command_matches.len())
                .map(|position| (popup.y + 1 + position as u16, position))
                .collect();
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(app.language.text(
                            "命令 · ↑/↓ 选择 · Enter/Tab 插入 · Esc 关闭",
                            "Commands · ↑/↓ select · Enter/Tab insert · Esc close",
                            "コマンド · ↑/↓ 選択 · Enter/Tab 挿入 · Esc 閉じる",
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Magenta)),
                ),
                popup,
            );
        }
        app.skill_rect = Rect::default();
        app.skill_hits.clear();
        let skill_matches = app.skill_matches(skills);
        if !app.skill_menu_dismissed
            && app.input.marker_query('$').is_some()
            && !skill_matches.is_empty()
        {
            app.skill_selected = app.skill_selected.min(skill_matches.len() - 1);
            let width = areas[3].width.min(76);
            let height = (skill_matches.len() as u16 + 2).min(10);
            let popup = Rect {
                x: areas[3].x,
                y: areas[3].y.saturating_sub(height),
                width,
                height,
            };
            app.skill_rect = popup;
            let lines = skill_matches
                .iter()
                .enumerate()
                .map(|(position, index)| {
                    let skill = &skills.list()[*index];
                    let prefix = if position == app.skill_selected {
                        "▶"
                    } else {
                        " "
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{prefix} ${} ", skill.identifier),
                            Style::default()
                                .fg(Color::LightCyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{} · {}", skill.name, skill.description),
                            Style::default().fg(Color::White),
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            app.skill_hits = (0..skill_matches.len())
                .map(|position| (popup.y + 1 + position as u16, position))
                .collect();
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(app.language.text(
                            "技能 · ↑/↓ 选择 · Enter/Tab 插入 · Esc 关闭",
                            "Skills · ↑/↓ select · Enter/Tab insert · Esc close",
                            "スキル · ↑/↓ 選択 · Enter/Tab 挿入 · Esc 閉じる",
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
                popup,
            );
        }
        if let Some(qr) = &app.mobile_qr {
            let width = qr.lines().map(UnicodeWidthStr::width).max().unwrap_or(40) as u16 + 4;
            let height = qr.lines().count() as u16 + 4;
            let popup = centered_rect(
                width.min(f.area().width),
                height.min(f.area().height),
                f.area(),
            );
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(qr.clone()).block(
                    Block::default()
                        .title(app.language.text(
                            "使用 WillDeep Mobile 扫码 · Esc 隐藏",
                            "Scan with WillDeep Mobile · Esc hides",
                            "WillDeep Mobile でスキャン · Esc で非表示",
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
                popup,
            );
        }
        if app.help_visible {
            let popup = centered_rect(
                f.area().width.min(88),
                f.area().height.min(28),
                f.area(),
            );
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(help_content(app.language))
                    .block(
                        Block::default()
                            .title(app.language.text(
                                "快捷键帮助 · F1/?/Esc 关闭",
                                "Keyboard help · F1/?/Esc closes",
                                "キーボードヘルプ · F1/?/Esc で閉じる",
                            ))
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::LightCyan)),
                    )
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        if let Some(detail) = &app.task_detail {
            let content = format!(
                "{}: {}\n{}: {:?}\n{}: {:?}\n{}: {:.1}s\n{}: {}\n{}: {}\n\n{}\n{}",
                app.language.text("任务", "Task", "タスク"),
                detail.snapshot.id,
                app.language.text("类型", "Kind", "種類"),
                detail.snapshot.kind,
                app.language.text("状态", "Status", "状態"),
                detail.snapshot.status,
                app.language.text("耗时", "Elapsed", "経過時間"),
                detail.snapshot.elapsed_millis as f64 / 1000.0,
                app.language.text("退出码", "Exit code", "終了コード"),
                detail
                    .snapshot
                    .exit_code
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".to_owned()),
                app.language.text("输出字节", "Output bytes", "出力バイト"),
                detail.snapshot.output_bytes,
                detail.snapshot.label,
                detail.output
            );
            let popup = centered_rect(
                f.area().width.min(92),
                f.area().height.min(30),
                f.area(),
            );
            let viewport = popup.height.saturating_sub(2).max(1) as usize;
            app.task_detail_scroll = app
                .task_detail_scroll
                .min(content.lines().count().saturating_sub(viewport));
            let title = if detail.snapshot.status == BackgroundTaskStatus::Running {
                app.language.text(
                    "后台任务详情 · ↑/↓/PgUp/PgDn 滚动 · K 停止 · Esc 关闭",
                    "Background task · ↑/↓/PgUp/PgDn scroll · K stop · Esc close",
                    "バックグラウンドタスク · ↑/↓/PgUp/PgDn · K 停止 · Esc 閉じる",
                )
            } else {
                app.language.text(
                    "后台任务详情 · ↑/↓/PgUp/PgDn 滚动 · Esc 关闭",
                    "Background task · ↑/↓/PgUp/PgDn scroll · Esc close",
                    "バックグラウンドタスク · ↑/↓/PgUp/PgDn · Esc 閉じる",
                )
            };
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(content)
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow)),
                    )
                    .scroll((app.task_detail_scroll.min(u16::MAX as usize) as u16, 0))
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        if let Some(review) = &mut app.diff_review {
            let popup = centered_rect(
                f.area().width.saturating_sub(4).min(120),
                f.area().height.saturating_sub(4).min(40),
                f.area(),
            );
            let title = if review.commit_preview.is_some() {
                app.language.text(
                    "Commit Preview · Esc 返回 · 仅预览，未执行",
                    "Commit Preview · Esc back · preview only",
                    "Commit Preview · Esc 戻る · プレビューのみ",
                ).to_owned()
            } else if review.preview_draft.is_some() {
                app.language.text(
                    "Commit Preview 参数 · Tab 切换 · Enter 生成 · Esc 取消",
                    "Commit Preview fields · Tab switch · Enter generate · Esc cancel",
                    "Commit Preview 入力 · Tab 切替 · Enter 生成 · Esc 取消",
                ).to_owned()
            } else if let Some((path, _)) = &review.content {
                let path = terminal_safe_diff_text(path);
                if review.confirm_revert {
                    format!("⚠ Revert {path} ({:?})? Y confirm · any key cancel",review.area)
                } else {
                let area = match review.area {
                    crate::daemon::diff_review::DiffArea::Combined => "Combined",
                    crate::daemon::diff_review::DiffArea::Staged => "Staged",
                    crate::daemon::diff_review::DiffArea::Unstaged => "Unstaged",
                };
                let view = match review.view {
                    DiffViewMode::Unified => "Unified",
                    DiffViewMode::SideBySide => "Side-by-side",
                };
                let search = review.search.as_ref().map_or_else(String::new, |editor| {
                    format!(
                        " · /{} · {}/{}",
                        editor.text(),
                        review.search_selected.saturating_add(1).min(review.search_matches.len()),
                        review.search_matches.len()
                    )
                });
                format!("Diff · {path} · {area} · {view}{search} · Wheel/↑↓ scroll · A accept · D reject · C changes · M reviewed · R revert · V/S/ search · Esc")
                }
            } else {
                format!(
                    "Diff Review · {} files · +{} -{} · {} checks · {} agents · ↑/↓ Enter · P Commit Preview · Esc",
                    review.snapshot.files.len(),
                    review.snapshot.additions,
                    review.snapshot.deletions,
                    review.verifications.len(),
                    review.attributions.iter().map(|record|record.agent_id).collect::<BTreeSet<_>>().len()
                )
            };
            let lines = if let Some(preview) = &review.commit_preview {
                commit_preview_lines(preview)
            } else if let Some(draft) = &review.preview_draft {
                commit_preview_draft_lines(draft)
            } else if let Some((_, content)) = &review.content {
                let query = review
                    .search
                    .as_ref()
                    .map(|editor| editor.text().trim())
                    .filter(|query| !query.is_empty());
                let lines = match review.view {
                    DiffViewMode::Unified => diff_review_lines(content, query),
                    DiffViewMode::SideBySide => {
                        diff_side_by_side_lines(content, popup.width.saturating_sub(2), query)
                    }
                };
                let viewport = popup.height.saturating_sub(2) as usize;
                review.scroll = review
                    .scroll
                    .min(lines.len().saturating_sub(viewport.max(1)));
                lines
            } else {
                diff_snapshot_lines(review)
            };
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if review.snapshot.has_conflicts {
                                Color::Red
                            } else {
                                Color::LightCyan
                            })),
                    )
                    .scroll((review.scroll.min(u16::MAX as usize) as u16, 0))
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
        render_agent_overlays(f, app);
        render_attention_detail(f, app);
        app.approval_rect = Rect::default();
        if let Some((description, always, _)) = &app.approval {
            let content = approval_content(description, *always, app.language);
            let popup = centered_rect(modal_width(f.area()), 9.min(f.area().height), f.area());
            app.approval_rect = popup;
            paint_modal_halo(f, popup);
            f.render_widget(
                Paragraph::new(content)
                    .block(modal_block(
                        approval_title(app.language, app.approval_queue.len()),
                        Color::LightYellow,
                    ))
                    .wrap(Wrap { trim: false }),
                popup,
            );
            let halo = modal_halo(popup, f.area());
            seal_modal_background(f.buffer_mut(), halo);
        }
        app.question_rect = Rect::default();
        app.question_hits.clear();
        if let Some(dialog) = &app.question {
            let options = dialog
                .request
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let marker = if dialog.request.multi_select {
                        if dialog.checked[index] { "[x]" } else { "[ ]" }
                    } else if index == dialog.selected {
                        "(*)"
                    } else {
                        "( )"
                    };
                    format!(
                        "{} {} {}",
                        if index == dialog.selected { "▶" } else { " " },
                        marker,
                        option
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let help = if dialog.request.multi_select {
                app.language.text(
                    "↑/↓ 选择 · Space 勾选 · 可输入其他答案 · Enter 发送 · Esc 跳过",
                    "↑/↓ select · Space toggle · type another answer · Enter send · Esc skip",
                    "↑/↓ 選択 · Space 切替 · その他を入力 · Enter 送信 · Esc スキップ",
                )
            } else {
                app.language.text(
                    "↑/↓ 选择 · 可输入其他答案 · Enter 发送 · Esc 跳过",
                    "↑/↓ select · type another answer · Enter send · Esc skip",
                    "↑/↓ 選択 · その他を入力 · Enter 送信 · Esc スキップ",
                )
            };
            let content = format!(
                "{}\n\n{}\n\n{}: {}\n{}",
                dialog.request.question,
                options,
                app.language
                    .text("其他答案", "Other answer", "その他の回答"),
                dialog.answer.text(),
                help
            );
            let popup_width = modal_width(f.area());
            // Borders take 2 columns, the block's horizontal padding another 2.
            let content_width = popup_width.saturating_sub(4).max(1) as usize;
            let height = (visual_lines(&content, content_width) as u16 + 2)
                .min(f.area().height)
                .max(8);
            let popup = centered_rect(popup_width, height, f.area());
            app.question_rect = popup;
            app.question_hits = (0..dialog.request.options.len())
                .map(|index| {
                    (
                        question_option_row(
                            popup.y,
                            &dialog.request.question,
                            content_width,
                            index,
                        ),
                        index,
                    )
                })
                .collect();
            paint_modal_halo(f, popup);
            f.render_widget(
                Paragraph::new(question_lines(dialog, &content))
                    .block(modal_block(
                        queued_title(
                            app.language.text(
                                "智能体提问",
                                "Question from Agent",
                                "エージェントからの質問",
                            ),
                            app.language,
                            app.question_queue.len(),
                        ),
                        Color::LightCyan,
                    ))
                    .wrap(Wrap { trim: false }),
                popup,
            );
            let halo = modal_halo(popup, f.area());
            seal_modal_background(f.buffer_mut(), halo);
        }
    })?;
    Ok(())
}

/// Styles the question modal's rows: bold question, a highlight bar on the row
/// the cursor is on, the free-text field picked out, and the key hints dimmed.
///
/// Takes the already-assembled `content` rather than rebuilding the text, so the
/// styled rows cannot drift from the string the height and mouse-row math use.
fn question_lines(dialog: &AskDialog, content: &str) -> Vec<Line<'static>> {
    let question_rows = dialog.request.question.split('\n').count();
    let first_option = question_rows + 1; // one blank line after the question
    let after_options = first_option + dialog.request.options.len();
    let other_answer_row = after_options + 1; // one blank line after the options
    content
        .split('\n')
        .enumerate()
        .map(|(index, row)| {
            let style = if index < question_rows {
                Style::default().add_modifier(Modifier::BOLD)
            } else if (first_option..after_options).contains(&index) {
                if index - first_option == dialog.selected {
                    Style::default()
                        .bg(Color::LightCyan)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }
            } else if index == other_answer_row {
                Style::default().fg(Color::LightYellow)
            } else if index > other_answer_row {
                Style::default().fg(Color::Gray)
            } else {
                Style::default()
            };
            Line::styled(row.to_owned(), style)
        })
        .collect()
}

/// The panel colours shared by the approval and question modals. One identity
/// for both — they are the same class of thing, a gate waiting on the human —
/// with the accent left to the caller.
const MODAL_PANEL: Style = Style::new().bg(Color::Blue).fg(Color::White);

/// How wide a modal gets. Deliberately most of the terminal: a narrow centered
/// popup leaves transcript text sitting to either side on the same rows, which
/// is what made the dialog read as tangled with the chat instead of as its own
/// region.
fn modal_width(area: Rect) -> u16 {
    area.width.saturating_sub(8).clamp(40, 110).min(area.width)
}

/// The one-cell ring around `popup`, clamped to `area`.
fn modal_halo(popup: Rect, area: Rect) -> Rect {
    let x = popup.x.saturating_sub(1);
    let y = popup.y.saturating_sub(1);
    Rect {
        x,
        y,
        width: popup
            .width
            .saturating_add(2)
            .min(area.width.saturating_sub(x)),
        height: popup
            .height
            .saturating_add(2)
            .min(area.height.saturating_sub(y)),
    }
}

/// Paints the ring in panel colour, so the modal reads as a raised,
/// self-contained block rather than text floating over the chat.
fn paint_modal_halo(f: &mut ratatui::Frame<'_>, popup: Rect) {
    let halo = modal_halo(popup, f.area());
    f.render_widget(Clear, halo);
    f.render_widget(Block::default().style(MODAL_PANEL), halo);
}

/// Re-applies the panel background over `area` once its contents are drawn.
///
/// Necessary because `Buffer::set_stringn` calls `Cell::reset` on the trailing
/// cell of every double-width grapheme, which knocks the background back to the
/// terminal default. A Chinese title or option label therefore punches
/// transparent holes in an otherwise solid panel. This pass only touches
/// colours, never symbols, so borders, text and the cursor-row highlight all
/// survive — only the missing background is filled back in.
fn seal_modal_background(buffer: &mut ratatui::buffer::Buffer, area: Rect) {
    let area = area.intersection(*buffer.area());
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell = &mut buffer[(x, y)];
            if cell.bg == Color::Reset {
                cell.bg = Color::Blue;
            }
        }
    }
}

/// Chrome for a modal panel: thick accent border, bold title, and horizontal
/// padding only — vertical padding would shift the option rows that
/// [`question_option_row`] uses for mouse hit-testing.
fn modal_block(title: String, accent: Color) -> Block<'static> {
    Block::default()
        .title(title)
        .title_style(
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(accent))
        .style(MODAL_PANEL)
        .padding(Padding::horizontal(1))
}

fn composer_layout_constraints(
    expanded: bool,
    activity: u16,
    attachments: u16,
    input_height: u16,
) -> [Constraint; 5] {
    if expanded {
        [
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Length(0),
            Constraint::Min(3),
            Constraint::Length(1),
        ]
    } else {
        [
            Constraint::Min(4),
            Constraint::Length(activity),
            Constraint::Length(attachments),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ]
    }
}

fn workspace_status(workspace: &std::path::Path, language: Language) -> String {
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "detached / non-git".to_owned());
    let status = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).lines().count())
        .unwrap_or(0);
    let worktrees = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count()
        })
        .unwrap_or(0);
    format!(
        "{}: {}\n{}: {branch}\n{}: {status}\n{}: {worktrees}",
        language.text("项目", "Project", "プロジェクト"),
        workspace
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace"),
        language.text("分支", "Branch", "ブランチ"),
        language.text("变更文件", "Diff files", "変更ファイル"),
        language.text("工作树", "Worktrees", "ワークツリー")
    )
}

/// Dialog title carrying how many more are queued behind this one, so the
/// user knows the turn is not done asking.
fn queued_title(base: &str, language: Language, queued: usize) -> String {
    if queued == 0 {
        return base.to_owned();
    }
    let more = language.text("还有", "more", "残り");
    format!("{base} · {more} {queued}")
}

fn approval_title(language: Language, queued: usize) -> String {
    queued_title(
        language.text("需要确认", "Approval required", "承認が必要"),
        language,
        queued,
    )
}

/// First non-empty line of an approval description, for one-line activity
/// reporting. Approval descriptions carry an optional label line plus the
/// full command; the log only needs the head.
fn first_line(description: &str) -> String {
    description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(description)
        .chars()
        .take(96)
        .collect()
}

fn approval_content(description: &str, always: bool, language: Language) -> Vec<Line<'static>> {
    let allow_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let deny_style = Style::default()
        .fg(Color::White)
        .bg(Color::Red)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let mut content = description
        .lines()
        .map(|line| Line::raw(line.to_owned()))
        .collect::<Vec<_>>();
    content.push(Line::raw(""));
    let mut actions = vec![
        Span::styled(" Y ", allow_style),
        Span::styled(
            language.text(" 允许一次  ", " Allow once  ", " 一度だけ許可  "),
            label_style,
        ),
    ];
    if always {
        actions.extend([
            Span::styled(" A ", allow_style),
            Span::styled(
                language.text(" 始终允许  ", " Always allow  ", " 常に許可  "),
                label_style,
            ),
        ]);
    }
    actions.extend([
        Span::styled(" N ", deny_style),
        Span::styled(language.text(" 拒绝", " Disallow", " 拒否"), label_style),
    ]);
    content.push(Line::from(actions));
    content
}

fn session_picker_result_line(
    result: &willdeep_runtime_protocol::SessionSearchResult,
    current: bool,
    language: Language,
    selected: bool,
) -> String {
    let marker = if selected { "▶" } else { " " };
    let current = if current {
        format!(" [{}]", language.text("当前", "current", "現在"))
    } else {
        String::new()
    };
    let title = result.title.replace(['\r', '\n'], " ");
    let snippet = result
        .snippet
        .as_deref()
        .map(|value| value.replace(['\r', '\n'], " "))
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" · {value}"))
        .unwrap_or_default();
    let short_id = result.id.simple().to_string();
    format!(
        "{marker} {title}{current} · {} · {} · {} {}{snippet}",
        &short_id[..8],
        session_status_label(result.status, language),
        result.message_count,
        language.text("条消息", "messages", "件のメッセージ")
    )
}

fn session_status_label(
    status: willdeep_runtime_protocol::SessionStatus,
    language: Language,
) -> &'static str {
    use willdeep_runtime_protocol::SessionStatus;
    match status {
        SessionStatus::Idle => language.text("空闲", "idle", "待機中"),
        SessionStatus::Queued => language.text("排队中", "queued", "待機列"),
        SessionStatus::Running => language.text("运行中", "running", "実行中"),
        SessionStatus::WaitingApproval => language.text("等待审批", "waiting approval", "承認待ち"),
        SessionStatus::WaitingAnswer => language.text("等待回答", "waiting answer", "回答待ち"),
        SessionStatus::Failed => language.text("失败", "failed", "失敗"),
        SessionStatus::Interrupted => language.text("已中断", "interrupted", "中断"),
        SessionStatus::Archived => language.text("已归档", "archived", "アーカイブ済み"),
    }
}

fn fuzzy_score(query: &str, value: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0;
    let mut cursor = 0;
    for needle in query.chars() {
        let relative = value[cursor..].find(needle)?;
        score += relative;
        cursor += relative + needle.len_utf8();
    }
    Some(score)
}

fn workspace_files(workspace: &std::path::Path, limit: usize) -> Vec<String> {
    const MAX_INSPECTED_ENTRIES: usize = 3_000;
    let mut output = Vec::new();
    let mut pending = VecDeque::from([workspace.to_path_buf()]);
    let mut inspected = 0;
    while let Some(directory) = pending.pop_front() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            inspected += 1;
            if inspected > MAX_INSPECTED_ENTRIES {
                return output;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "node_modules" | ".build")
                ) {
                    pending.push_back(path);
                }
                continue;
            }
            if kind.is_file()
                && let Ok(relative) = path.strip_prefix(workspace)
            {
                output.push(relative.to_string_lossy().replace('\\', "/"));
                if output.len() >= limit {
                    return output;
                }
            }
        }
    }
    output
}

fn focus_label(focus: FocusPane, language: Language) -> &'static str {
    match focus {
        FocusPane::Prompt => language.text("输入", "Prompt", "入力"),
        FocusPane::Chat => language.text("聊天", "Chat", "チャット"),
        FocusPane::Activity => language.text("活动", "Activity", "アクティビティ"),
        FocusPane::Sidebar => language.text("状态栏", "Status", "ステータス"),
    }
}

fn runtime_status_label(status: RuntimeStatus, language: Language) -> &'static str {
    match status {
        RuntimeStatus::Idle => language.text("空闲", "idle", "待機"),
        RuntimeStatus::Working => language.text("工作中", "working", "作業中"),
        RuntimeStatus::Blocked => language.text("已阻塞", "blocked", "ブロック中"),
        RuntimeStatus::WaitingApproval => language.text("等待审批", "waiting approval", "承認待ち"),
        RuntimeStatus::WaitingAnswer => language.text("等待回答", "waiting answer", "回答待ち"),
        RuntimeStatus::Failed => language.text("失败", "failed", "失敗"),
        RuntimeStatus::Done => language.text("已完成", "done", "完了"),
        RuntimeStatus::Cancelled => language.text("已取消", "cancelled", "キャンセル済み"),
        RuntimeStatus::Unknown => language.text("未知", "unknown", "不明"),
    }
}

fn attention_source_label(source: AttentionSource, language: Language) -> &'static str {
    match source {
        AttentionSource::Approval => language.text("审批", "Approval", "承認"),
        AttentionSource::Question => language.text("提问", "Question", "質問"),
        AttentionSource::BackgroundShell => {
            language.text("后台命令", "Background shell", "バックグラウンドシェル")
        }
        AttentionSource::Subagent => language.text("子 Agent", "Subagent", "サブエージェント"),
        AttentionSource::Worktree => language.text("Worktree", "Worktree", "Worktree"),
        AttentionSource::DiffReview => language.text("Diff 审查", "Diff review", "Diff レビュー"),
    }
}

fn attention_style(status: RuntimeStatus) -> Style {
    let color = match status {
        RuntimeStatus::WaitingApproval | RuntimeStatus::WaitingAnswer => Color::Yellow,
        RuntimeStatus::Blocked | RuntimeStatus::Failed => Color::LightRed,
        RuntimeStatus::Working => Color::LightBlue,
        RuntimeStatus::Done => Color::LightGreen,
        RuntimeStatus::Cancelled => Color::DarkGray,
        RuntimeStatus::Idle | RuntimeStatus::Unknown => Color::Gray,
    };
    Style::default().fg(color)
}

fn help_content(language: Language) -> &'static str {
    match language {
        Language::ZhCn => {
            "全局\n  F1 / 空输入时 ?  打开帮助    Ctrl+C 退出\n  Ctrl+P 全局命令面板           Ctrl+R 搜索历史会话并继续\n  Ctrl+W 输入/聊天/活动/状态栏切换\n  Ctrl+B 或 /sidebar 显示/隐藏状态栏（默认隐藏）\n  Ctrl+S 文本选择/复制模式\n\n输入\n  Enter 发送                    Shift/Alt+Enter 或 Ctrl+J 换行\n  F2 展开/恢复大输入空间         Ctrl+A/E 行首/行尾\n  / 命令候选                    $ 技能候选\n  ↑/↓ 选择候选                  Enter/Tab 插入，Esc 关闭\n  Ctrl/Command+Shift+V 粘贴图片 Ctrl+D 删除附件\n\n聊天与活动\n  直接拖动选择聊天文字           Ctrl/Cmd+C 或 Y 复制，Q 引用\n  Ctrl+F 搜索，Enter/Shift+Enter 前后跳转\n  PageUp/PageDown 翻页           Alt+↑/↓ 逐行滚动\n  Ctrl+Home/End 顶部/底部        Ctrl+O 展开工具活动\n  点击活动区聚焦，Enter/Space 展开或收起\n\n状态栏\n  Tab/Shift+Tab 选择分组         ↑/↓ 选择 Inbox 条目\n  Enter 详情，K 停止，R 重试     M 已读，Space 折叠，Esc 返回\n  点击标题折叠，点击条目看详情，滚轮滚动内容"
        }
        Language::En => {
            "Global\n  F1 / ? on empty prompt  Open help    Ctrl+C Exit\n  Ctrl+P Command palette     Ctrl+R Search and continue a historical Session\n  Ctrl+W Switch Prompt/Chat/Activity/Status\n  Ctrl+B or /sidebar Show/hide Status (hidden by default)\n  Ctrl+S Text selection mode\n\nPrompt\n  Enter Send                 Shift/Alt+Enter or Ctrl+J Newline\n  F2 Expand/restore composer Ctrl+A/E Line start/end\n  / Command suggestions      $ Skill suggestions\n  ↑/↓ Select                 Enter/Tab Insert, Esc Close\n  Ctrl/Command+Shift+V Paste image      Ctrl+D Remove attachment\n\nChat and activity\n  Drag to select chat text   Ctrl/Cmd+C or Y copy, Q quote\n  Ctrl+F Search, Enter/Shift+Enter Previous/next match\n  PageUp/PageDown Page        Alt+↑/↓ Scroll one line\n  Ctrl+Home/End Top/Bottom    Ctrl+O Expand tool activity\n  Click activity to focus, Enter/Space to expand or collapse\n\nStatus sidebar\n  Tab/Shift+Tab Select section     ↑/↓ Select Inbox item\n  Enter Details, K Stop, R Retry   M Read, Space Toggle, Esc Return\n  Click headers to toggle, items for details, wheel to scroll"
        }
        Language::Ja => {
            "グローバル\n  F1 / 空入力で ?  ヘルプ       Ctrl+C 終了\n  Ctrl+P コマンドパレット        Ctrl+R 履歴セッションを検索して再開\n  Ctrl+W 入力/チャット/アクティビティ/状態を切替\n  Ctrl+B または /sidebar で状態欄を表示/非表示（既定は非表示）\n  Ctrl+S テキスト選択モード\n\n入力\n  Enter 送信                     Shift/Alt+Enter または Ctrl+J 改行\n  F2 入力欄を拡大/復元            Ctrl+A/E 行頭/行末\n  / コマンド候補                 $ スキル候補\n  ↑/↓ 選択                       Enter/Tab 挿入、Esc 閉じる\n  Ctrl/Command+Shift+V 画像貼付   Ctrl+D 添付削除\n\nチャットとアクティビティ\n  ドラッグで文字選択              Ctrl/Cmd+C / Y コピー、Q 引用\n  Ctrl+F 検索、Enter/Shift+Enter 前後の一致へ\n  PageUp/PageDown ページ移動      Alt+↑/↓ 1 行スクロール\n  Ctrl+Home/End 先頭/末尾         Ctrl+O ツール詳細\n  アクティビティをクリックして、Enter/Space で開閉\n\n状態サイドバー\n  Tab/Shift+Tab セクション選択    ↑/↓ Inbox 項目選択\n  Enter 詳細、K 停止、R 再実行    M 既読、Space 開閉、Esc 入力へ\n  見出しで開閉、項目で詳細、ホイールでスクロール"
        }
    }
}

pub fn channel() -> (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
) {
    mpsc::unbounded_channel()
}
#[cfg(test)]
mod test_suite;
