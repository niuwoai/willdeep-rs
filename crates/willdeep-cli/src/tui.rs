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

/// 连续多少份新鲜快照里都没有本会话的活动任务，才去向 Runtime 求证「工作中」是否残留。
/// 快照一秒一份，刚提交的轮次在队列里排队时任务是 `Queued`、快照会滤掉它，
/// 所以要留几秒余量，别把一条真在排队的轮次当成残留复位掉。
const STALE_RUNTIME_TURN_SNAPSHOTS: u8 = 3;
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
mod media_ui;
mod model_commands;
mod narration;
mod overlay_dismiss;
mod permission_commands;
mod plan_ui;
mod rendering;
mod rewind_ui;
mod routing_settings;
mod runtime_ui;
mod session_commands;
mod session_picker_ui;
mod sidebar;
mod webapp_commands;
mod workspace_attention;
mod workspace_commands;
mod workspace_picker_ui;
use activity::ToolActivity;
use agent_commands::handle_agent_command;
use agent_worktree_ui::render_agent_overlays;
use command_catalog::{command_candidates, help_text};
use diff_review_ui::*;
use dispatch::{
    dispatch_compress, dispatch_input_suggestion, dispatch_prompt, dispatch_retitle,
    wake_for_kernel_events,
};
use media_ui::{MediaAction, MediaState, render_media_overlay};
use model_commands::{
    ModelCommand, ModelPickerAction, ModelPickerState, render_model_picker, request_model_list,
    switch_model, switch_or_defer_model,
};
use narration::StreamKind;
use permission_commands::{
    PermissionCommand, PermissionPickerAction, PermissionPickerState, render_permission_picker,
};
use plan_ui::*;
use rendering::*;
use rewind_ui::{RewindPickerAction, RewindPickerState, render_rewind_picker, rewind_summary};
use routing_settings::{RoutingSettingsAction, RoutingSettingsState, render_routing_settings};
use runtime_ui::open_remote_gate;
use runtime_ui::{PromptExecution, prompt_execution};
use session_commands::{
    SessionPickerRequest, handle_new_session_command, handle_session_command,
    parse_session_picker_command,
};
use session_picker_ui::{
    PendingSessionSwitch, SessionPickerAction, SessionPickerState, refresh_session_picker,
    render_session_picker,
};
use sidebar::{render_attention_detail, render_sidebar};
use workspace_attention::workspace_attention;
use workspace_commands::handle_workspace_command;
use workspace_picker_ui::{WorkspacePickerAction, WorkspacePickerState, render_workspace_picker};

pub enum UiMessage {
    Agent(AgentEvent),
    Approval(String, bool, oneshot::Sender<ApprovalDecision>),
    Question(UserQuestion, oneshot::Sender<Option<String>>),
    Finished(
        Result<willdeep_core::AgentOutcome, willdeep_core::AgentError>,
        willdeep_core::checkpoint::ClaimedSessionCheckpointSink,
    ),
    Compressed(
        Result<Vec<Message>, willdeep_core::AgentError>,
        willdeep_core::checkpoint::ClaimedSessionCheckpointSink,
    ),
    RuntimeNotice(String),
    /// Runtime 操作的最终结果：同时进聊天记录与状态行。只进状态行的话，下一条
    /// 提示一来就被覆盖，用户看到的只剩「操作已提交」。
    RuntimeResult(String),
    ModelsLoaded(std::result::Result<Vec<String>, String>),
    MediaLoaded {
        target: String,
        result: std::result::Result<DynamicImage, String>,
    },
    MediaResized(std::result::Result<ratatui_image::thread::ResizeResponse, String>),
    /// 标题摘要跑完了（`Some` 才是有结果）。摘要是一次网络往返，不能在
    /// 事件循环里直接 await——那会让整个界面在轮次收尾时卡住。
    ///
    /// `requested` 区分「轮次收尾时自动跑的」和「人敲 `/session retitle` 要的」：
    /// 前者失败该静默（列表里还有 L1 标题），后者失败必须说出来，
    /// 否则一条命令按下去什么都没发生。
    Retitled {
        title: Option<String>,
        requested: bool,
    },
    /// 轮次结束后的下一句预测回来了。`epoch` 是发起时输入框的世代号：用户已经
    /// 开始打字、或新一轮已经开始，世代号就翻页了，晚到的结果原地丢弃。
    InputSuggested {
        suggestion: Option<String>,
        epoch: u64,
    },
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
    approval_selected: usize,
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
    /// 轮次结束后预测的「你可能想说的下一句」。只在空闲且输入框为空时以灰字显示，
    /// Tab 采用（只填入，不发送），打字 / Esc / 新一轮开始即清掉。只活在内存里。
    input_suggestion: Option<String>,
    /// 输入框世代号：每次清预测就加一。在途的预测带着旧世代号回来时对不上号，丢弃。
    input_suggestion_epoch: u64,
    /// Runtime 轮次刚正常收尾（completed / partial）。事件循环据此发起一次预测；
    /// 失败与中断的收尾不置位。
    runtime_turn_settled: bool,
    goal: Option<String>,
    mobile_gateway: Option<RelayGateway>,
    mobile_qr: Option<String>,
    /// 本轮在跑时收到的提示词。键盘和手机共用一条队列，本轮一结束就按顺序发出去；
    /// 中断当前轮次同样会让队列立刻续上。
    queued_prompts: VecDeque<QueuedPrompt>,
    /// 本轮进行中收到的 `/model`：本轮结束、排队的提示词发出之前切过去。
    pending_model: Option<String>,
    /// 进程内 Harness 当前轮次的句柄。Runtime 轮次由 Daemon 停，本地轮次只能靠
    /// 掐这个 Task——没有它，`/local` 跑飞了就只剩退出 TUI 一条路。
    local_turn: Option<tokio::task::JoinHandle<()>>,
    latest_usage: Usage,
    /// 本轮累计用量与第一次响应回来的时刻。
    ///
    /// `latest_usage` 是**最后一次请求**的用量，状态栏要的是那个；本轮账目要
    /// 的是整轮加起来，两者不能互相顶替。Runtime 路径没有 `AgentOutcome`，
    /// 只能靠这里累计。
    turn_input_tokens: u64,
    turn_output_tokens: u64,
    turn_first_reply: Option<Duration>,
    turn_started: Option<Instant>,
    last_progress_at: Option<Instant>,
    runtime_turn: bool,
    /// 界面显示 Runtime 轮次在跑、而新鲜快照里却找不到本会话活动任务的连续次数。
    /// 攒够 [`STALE_RUNTIME_TURN_SNAPSHOTS`] 次就去问 Runtime 一句「还有在途轮次吗」，
    /// 没有就把残留的「工作中」复位——否则排队的提示词会跟着一起死等。
    stale_runtime_turn_snapshots: u8,
    last_elapsed: Option<Duration>,
    context_window: u64,
    context_tokens: u64,
    activity_line: String,
    background_tasks: Vec<BackgroundTaskSnapshot>,
    workspace_attention: Vec<AttentionItem>,
    runtime_attention: Vec<AttentionItem>,
    /// 事件内核里仍待用户处理的那些，投影到 Inbox。只显示，不决策。
    kernel_attention: Vec<AttentionItem>,
    runtime_gates: Vec<crate::daemon::RemoteGate>,
    /// Version of the Runtime that actually executes tools, when one is
    /// reachable. `None` means no Runtime (everything runs in-process).
    runtime_version: Option<String>,
    /// A version mismatch is announced once in the transcript; the sidebar
    /// warning then stays up on its own.
    runtime_version_warned: bool,
    /// Runtime 比客户端旧、本会话还没试过自动升级：事件循环据此发起一次。
    runtime_auto_upgrade_pending: bool,
    runtime_auto_upgrade_tried: bool,
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
    workspace_status: String,
    progress_log: VecDeque<String>,
    language: Language,
    transient_thought: Option<String>,
    /// 本轮最近一段已落进聊天区的中途文字；收尾文字据此去重，不重复最后一段。
    turn_narration: Option<String>,
    /// `/tools`：临时展开聊天区里收起的工具行。只影响显示，记录不动。
    tool_rows_expanded: bool,
    /// 临时行此刻流的是思维链还是正文，决定标签与何时清缓冲。
    transient_kind: StreamKind,
    /// 本轮刚结束：下一帧画之前响一声铃，响过就清。
    bell_pending: bool,
    selection_mode: bool,
    native_selection_mode: bool,
    chat_selection: Option<ChatSelection>,
    transcript_rows: Vec<String>,
    transcript_render_offset: usize,
    skill_selected: usize,
    skill_menu_dismissed: bool,
    command_selected: usize,
    command_menu_dismissed: bool,
    /// `/exit` 请求退出。事件循环下一圈读到它就收尾——命令处理器只报告
    /// 「用户想走」，真正拆终端的活儿留在它原来的地方，免得两处都能关。
    quit_requested: bool,
    focus: FocusPane,
    sidebar_visible: bool,
    sidebar_selected: usize,
    sidebar_expanded: [bool; 4],
    sidebar_scroll: usize,
    sidebar_rect: Rect,
    sidebar_wide: bool,
    help_visible: bool,
    media: MediaState,
    sidebar_hits: Vec<(u16, SidebarHit)>,
    sidebar_manual_scroll: bool,
    attention_selected: usize,
    attention_read: BTreeSet<String>,
    task_detail: Option<TaskDetail>,
    task_detail_scroll: usize,
    attention_detail: Option<AttentionItem>,
    /// 当前详情对应的失败排查材料；打开另一条时作废。
    attention_diagnostics: Option<AttentionDiagnostics>,
    attention_diff_rect: Rect,
    attention_allow_rect: Rect,
    attention_deny_rect: Rect,
    search: Option<SearchState>,
    workspace: Option<PathBuf>,
    palette: Option<PaletteState>,
    palette_rect: Rect,
    palette_hits: Vec<(u16, usize)>,
    session_picker: Option<SessionPickerState>,
    rewind_picker: Option<RewindPickerState>,
    session_picker_rect: Rect,
    session_picker_hits: Vec<(u16, usize)>,
    model_picker: Option<ModelPickerState>,
    model_picker_rect: Rect,
    model_picker_hits: Vec<(u16, usize)>,
    workspace_picker: Option<WorkspacePickerState>,
    workspace_picker_rect: Rect,
    workspace_picker_hits: Vec<(u16, usize)>,
    permission_picker: Option<PermissionPickerState>,
    /// 当前审批档位，显示在输入框标题上。真正生效的是 Agent 与 Runtime 会话
    /// 手里的句柄，这里只是界面上的镜像。
    approval_mode: willdeep_core::ApprovalMode,
    /// 已经把当前档位同步过去的 Runtime 会话。
    approval_synced_session: Option<uuid::Uuid>,
    routing_settings: Option<RoutingSettingsState>,
    routing_settings_rect: Rect,
    pending_session_switch: Option<PendingSessionSwitch>,
    /// 面板里选中的工作区，等主循环空出手来真正切过去。
    pending_workspace_switch: Option<uuid::Uuid>,
    transcript_rect: Rect,
    command_rect: Rect,
    command_hits: Vec<(u16, usize)>,
    skill_rect: Rect,
    skill_hits: Vec<(u16, usize)>,
    approval_rect: Rect,
    approval_action_hits: Vec<(Rect, ApprovalDecision)>,
    question_rect: Rect,
    question_hits: Vec<(u16, usize)>,
    search_rect: Rect,
    // 弹层自身的外框。只有记下来，才知道一次点击是「落在弹层里」
    // 还是「落在弹层外」——后者按 Esc 处理。
    mobile_qr_rect: Rect,
    help_rect: Rect,
    task_detail_rect: Rect,
    attention_detail_rect: Rect,
    agent_detail_rect: Rect,
    worktree_review_rect: Rect,
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

/// Inbox 详情里附带的失败排查材料：哪条命令、退出码、错误输出。
/// 排版在取回时一次做完，渲染只管往下贴。
struct AttentionDiagnostics {
    item_id: String,
    text: String,
}

struct QueuedPrompt {
    text: String,
    attachments: Vec<DraftAttachment>,
    /// 手机来的提示词走进程内 Harness（与直接收到时的行为一致），
    /// 键盘输入按 `/local` 与 `/runtime` 的常规规则路由。
    from_phone: bool,
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

enum PaletteAction {
    Command(String),
    Skill(String),
    Session(String),
    Task(usize),
    File(String),
}

/// 本轮在跑时，一条输入该怎么处置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BusyInput {
    /// 只改本地显示，不碰会话、模型或 Runtime——没有任何理由让它等。
    RunNow,
    /// 提示词：先试着直接送进正在跑的这一轮（下一次调模型前注入）；送不进去
    /// 再排队，本轮结束（或被中断）后按顺序发出去。
    Queue,
    /// 其余斜杠命令：会改会话或 Runtime 状态，延迟几分钟再执行只会更意外。
    Refuse,
}

/// 运行中立即执行的命令白名单。判据是「只读或只改本地显示」——
/// `/history` 打开面板是只读查询，真正的切换在消费那一步另有运行中保护。
fn busy_input(prompt: &str) -> BusyInput {
    let value = prompt.trim();
    if !value.starts_with('/') {
        return BusyInput::Queue;
    }
    let command = value.split_whitespace().next().unwrap_or_default();
    match command {
        // 切档正是为了处理「这一轮在跑、又不停地弹审批」，必须当场生效。
        "/help" | "/version" | "/clear" | "/sidebar" | "/skills" | "/history" | "/permissions"
        | "/permission-mode" => BusyInput::RunNow,
        // 换模型本来就只作用于下一轮，没有理由让用户等到本轮结束再敲一遍：当场收下，
        // 记成待切换，本轮结束时再真正切（见 `switch_or_defer_model`）。
        "/model" => BusyInput::RunNow,
        "/session" => match value.split_whitespace().nth(1) {
            Some("search") => BusyInput::RunNow,
            _ => BusyInput::Refuse,
        },
        "/local" | "/runtime" => BusyInput::Queue,
        _ => BusyInput::Refuse,
    }
}

/// A pending approval: what is being asked, whether Always Allow applies,
/// and the channel the waiting harness is parked on.
type ApprovalRequest = (String, bool, oneshot::Sender<ApprovalDecision>);

const APPROVAL_DECISIONS: [ApprovalDecision; 2] =
    [ApprovalDecision::AllowOnce, ApprovalDecision::Deny];
const APPROVAL_DECISIONS_WITH_ALWAYS: [ApprovalDecision; 3] = [
    ApprovalDecision::AllowOnce,
    ApprovalDecision::AlwaysAllow,
    ApprovalDecision::Deny,
];

fn approval_decisions(always: bool) -> &'static [ApprovalDecision] {
    if always {
        &APPROVAL_DECISIONS_WITH_ALWAYS
    } else {
        &APPROVAL_DECISIONS
    }
}
pub type TuiRuntimeInputs = (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
    u64,
    Arc<BackgroundTaskRegistry>,
    crate::daemon::RuntimeSubmitOptions,
    willdeep_core::provider::ProviderConfig,
    Language,
    crate::notify::Notifier,
);

struct AskDialog {
    request: UserQuestion,
    selected: usize,
    checked: Vec<bool>,
    answer: PromptEditor,
    sender: oneshot::Sender<Option<String>>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    agent: Arc<Agent>,
    mut session: Session,
    store: SessionStore,
    home: PathBuf,
    skills: Arc<SkillCatalog>,
    relay_bridge: RelayBridge,
    kernel: willdeep_core::EventKernel,
    kernel_store: willdeep_core::kernel_store::KernelStore,
    ui: TuiRuntimeInputs,
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
    ui.7.set_session(&session.id.to_string(), Some(session.title.as_str()));
    // 作业记录与事件日志同一个家目录。
    let kernel_store_home = home.clone();
    let mut runtime = TuiRuntime {
        home,
        notifier: ui.7,
        skills,
        relay_bridge,
        kernel,
        detached_jobs: willdeep_core::DetachedJobStore::new(&kernel_store_home),
        kernel_store,
        context_window: ui.2,
        background_tasks: ui.3,
        runtime_submit: ui.4,
        provider_config: ui.5,
        local_workspace: session.workspace.clone(),
        tx: ui.0,
        rx: ui.1,
    };
    let result = event_loop(&mut term, agent, &mut session, &store, &mut runtime, ui.6).await;
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
    /// 宿主事件内核。后台结果、入站通知都进这里，由主 Agent 在 turn 边界收走。
    kernel: willdeep_core::EventKernel,
    kernel_store: willdeep_core::kernel_store::KernelStore,
    /// 脱离父进程的后台作业。它们活得比这个进程久，所以完成与否只能靠轮询
    /// 磁盘上的记录，没有可等的句柄。
    detached_jobs: willdeep_core::DetachedJobStore,
    context_window: u64,
    background_tasks: Arc<BackgroundTaskRegistry>,
    runtime_submit: crate::daemon::RuntimeSubmitOptions,
    provider_config: willdeep_core::provider::ProviderConfig,
    local_workspace: PathBuf,
    tx: mpsc::UnboundedSender<UiMessage>,
    rx: mpsc::UnboundedReceiver<UiMessage>,
}

impl TuiRuntime {
    fn refresh_provider_config(&mut self) -> Result<()> {
        let loaded = crate::config::LoadedConfig::load(self.runtime_submit.config.as_deref())?;
        let profile_name = self
            .runtime_submit
            .profile
            .clone()
            .or_else(|| loaded.file.default_provider.clone())
            .or_else(|| {
                (loaded.file.providers.len() == 1)
                    .then(|| loaded.file.providers.keys().next().cloned())
                    .flatten()
            });
        let Some(profile_name) = profile_name else {
            return Ok(());
        };
        let mut config = crate::provider_config_from_profile(&loaded.file, &profile_name)?;
        if let Some(model) = &self.runtime_submit.model {
            config.model = model.clone();
        }
        self.provider_config = config;
        Ok(())
    }
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

fn is_clipboard_image_paste_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v' | 'V'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
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

/// 停掉当前这一轮。Runtime 轮次交给 Daemon 排空（它知道在途工具怎么收尾），
/// 进程内轮次只能掐 Task；两条路都要保证 `running` 落回去，否则界面会一直卡在
/// 「工作中」，排队的提示词也永远续不上。
async fn interrupt_turn(app: &mut App, session: &Session, runtime: &TuiRuntime) -> Result<String> {
    if !app.running {
        return Ok(app
            .language
            .text(
                "当前没有正在运行的轮次",
                "No turn is running",
                "実行中のターンはありません",
            )
            .to_owned());
    }
    if !app.runtime_turn
        && let Some(handle) = app.local_turn.take()
    {
        handle.abort();
        // Wait for cancellation before a queued prompt can read its final checkpoint.
        let _ = handle.await;
        app.finish_turn();
        app.append_transcript(format!(
            "System: {}",
            app.language.text(
                "已中断本地轮次",
                "Local turn interrupted",
                "ローカルターンを中断しました"
            )
        ));
        return Ok(app
            .language
            .text("已中断", "Interrupted", "中断しました")
            .to_owned());
    }
    let Some(active) = crate::daemon::remote_active_turn(&runtime.home, session.id).await? else {
        // Runtime 说没有在途轮次，那界面上的「工作中」是残留状态，就地清掉。
        app.finish_turn();
        return Ok(app
            .language
            .text(
                "Runtime 已无在途轮次，界面状态已复位",
                "Runtime has no active turn; the display was reset",
                "Runtime に進行中のターンはありません。表示を戻しました",
            )
            .to_owned());
    };
    crate::daemon::stop_remote_turn(&runtime.home, active.turn_id).await?;
    app.record_progress(
        app.language
            .text("已请求中断", "Interrupt requested", "中断を要求しました")
            .to_owned(),
    );
    Ok(app
        .language
        .text(
            "已请求中断当前轮次",
            "Interrupt requested for the current turn",
            "現在のターンの中断を要求しました",
        )
        .to_owned())
}

/// Inbox 里打开的如果是 Runtime 任务，就顺带把失败详情取回来。
/// 拿不到（旧版 Daemon 没有这个操作、或任务已被清理）就安静跳过——
/// 详情弹窗本身仍然有用，不该因为附加信息拉不到就打不开。
async fn load_attention_diagnostics(app: &mut App, runtime: &TuiRuntime) {
    let Some(item) = app.attention_detail.as_ref() else {
        app.attention_diagnostics = None;
        return;
    };
    if app
        .attention_diagnostics
        .as_ref()
        .is_some_and(|loaded| loaded.item_id == item.id)
    {
        return;
    }
    let Some(id) = item
        .id
        .strip_prefix("runtime-task:")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
    else {
        return;
    };
    let item_id = item.id.clone();
    if let Ok(diagnostics) = crate::daemon::remote_task_diagnostics(&runtime.home, id).await
        && let Some(text) = format_task_diagnostics(&diagnostics, app.language)
    {
        app.attention_diagnostics = Some(AttentionDiagnostics { item_id, text });
    }
}

/// 把诊断对象排成人能读的几行。没有任何失败痕迹时返回 `None`，
/// 免得在成功的任务详情下面挂一个空的「失败详情」标题。
fn format_task_diagnostics(
    diagnostics: &willdeep_runtime_protocol::RuntimeTaskDiagnostics,
    language: Language,
) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(exit_code) = diagnostics.task.exit_code {
        lines.push(format!(
            "{}: {exit_code}",
            language.text("退出码", "Exit code", "終了コード")
        ));
    }
    if let Some(domain) = diagnostics.task.failure_domain {
        lines.push(format!(
            "{}: {domain:?}",
            language.text("失败域", "Failure domain", "失敗ドメイン")
        ));
    }
    if let Some(failure) = &diagnostics.failure {
        // 事件原文形如 `task_id=… exit_code=1 error=…`，task_id 详情里已经有了。
        let failure = failure
            .split_once(' ')
            .map_or(failure.as_str(), |(_, rest)| rest);
        if !failure.trim().is_empty() {
            lines.push(format!(
                "{}: {failure}",
                language.text("失败原因", "Failure", "失敗理由")
            ));
        }
    }
    for tool in &diagnostics.failed_tools {
        lines.push(String::new());
        lines.push(format!(
            "{} {}",
            language.text("失败的工具", "Failed tool", "失敗したツール"),
            tool.name
        ));
        if let Some(arguments) = &tool.arguments {
            for (key, value) in tool_arguments(arguments) {
                lines.push(format!("  {key}: {value}"));
            }
        }
        if let Some(output) = tool
            .output
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("  {}:", language.text("输出", "Output", "出力")));
            lines.extend(output.lines().map(|line| format!("    {line}")));
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// 工具入参是 JSON 字符串。能解析成对象就按 `键: 值` 逐行摊开，
/// `run_command` 的 `command` 就落在这里；解析不了就原样给一行。
fn tool_arguments(arguments: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str(arguments) else {
        return vec![("arguments".to_owned(), arguments.replace('\n', " "))];
    };
    object
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::String(value) => value,
                other => other.to_string(),
            };
            (key, value)
        })
        .collect()
}

mod event_loop;
use event_loop::event_loop;

fn dispatch_media_action(action: MediaAction, app: &mut App, runtime: &TuiRuntime) {
    match action {
        MediaAction::None => {}
        MediaAction::OpenUrl(target) => match media_ui::open_external_url(&target) {
            Ok(()) => {
                app.notice = Some(
                    app.language
                        .text(
                            "已交给系统浏览器打开",
                            "Opened in the system browser",
                            "システムブラウザで開きました",
                        )
                        .to_owned(),
                )
            }
            Err(error) => {
                app.notice = Some(format!(
                    "{}: {error}",
                    app.language.text(
                        "打开链接失败",
                        "Could not open link",
                        "リンクを開けませんでした"
                    )
                ))
            }
        },
        MediaAction::LoadImage(target) => {
            let workspace = runtime.runtime_submit.workspace.clone();
            let tx = runtime.tx.clone();
            tokio::spawn(async move {
                let result = media_ui::load_image(&target, &workspace).await;
                let _ = tx.send(UiMessage::MediaLoaded { target, result });
            });
        }
    }
}

mod app_state;

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

/// 过了 120 秒还用秒读数（293.2s）就得让人心算，换成分钟保留一位小数；
/// 过了 120 分钟同理换小时。`seconds_decimals` 沿用各显示点原有的秒精度。
/// 命令面板装不下时，从第几条开始画。
///
/// 选中项必须落在可视窗口里，否则用户按着 ↓ 却看不到光标去了哪——那正是让人
/// 以为「后面没有了」的原因。窗口贴着底走：只有选中项越过下沿才滚，向上回来
/// 时同样跟着走。
fn command_window_offset(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    selected
        .saturating_sub(visible.saturating_sub(1))
        .min(total - visible)
}

fn format_elapsed_span(seconds: f32, seconds_decimals: usize) -> String {
    if seconds > 7200.0 {
        format!("{:.1}h", seconds / 3600.0)
    } else if seconds > 120.0 {
        format!("{:.1}m", seconds / 60.0)
    } else {
        format!("{seconds:.seconds_decimals$}s")
    }
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
            "{} {}",
            language.text(
                "暂未收到新事件 · 已等待",
                "No new event yet · waiting",
                "新しいイベントなし · 待機"
            ),
            format_elapsed_span(idle.as_secs_f32(), 0)
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
        "{} {phase} · {} {}",
        progress_spinner(elapsed),
        language.text("已运行", "elapsed", "経過"),
        format_elapsed_span(elapsed.as_secs_f32(), 1)
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
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.transcript_rect = areas[0];
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        let mut visible_transcript = app.display_transcript().rows;
        // 思维链只占最后几行、灰色；正文一开始这行就换成回复预览。宽度要先算好，
        // 截取才知道几行是几行。
        if let Some(row) = app.transient_row() {
            visible_transcript.push(row);
        }
        app.transcript_height =
            rendered_transcript_height(&visible_transcript, app.transcript_width);
        let max = app.max_scroll();
        app.scroll_from_bottom = app.scroll_from_bottom.min(max);
        let offset = max
            .saturating_sub(app.scroll_from_bottom)
            .min(u16::MAX as usize) as u16;
        let mut title = if app.native_selection_mode {
            app.language
                .text(
                    "WillDeep · 终端原生选择 · 拖选后右键或 Cmd+C 复制 · Esc 退出",
                    "WillDeep · native terminal selection · drag, then right-click or Cmd+C · Esc exits",
                    "WillDeep · 端末の標準選択 · ドラッグ後に右クリック / Cmd+C · Esc 終了",
                )
                .to_owned()
        } else if app.selection_mode {
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
                " · {} {} {}",
                progress_spinner(elapsed),
                app.language.text("工作中", "working", "作業中"),
                format_elapsed_span(elapsed.as_secs_f32(), 0)
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
        // 空输入框里的灰字预测：顶替正文而不是叠加，「Tab 采用」的提示跟在后面。
        let suggestion_visible = app.visible_input_suggestion().is_some();
        let composer_body = match app.visible_input_suggestion() {
            Some(suggestion) => Text::from(Line::from(vec![
                Span::styled(suggestion.to_owned(), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "  {}",
                        app.language.text("Tab 采用", "Tab to accept", "Tab で採用")
                    ),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ])),
            None => Text::from(wrapped_input),
        };
        let composer = Paragraph::new(composer_body)
                .block(
                    Block::default()
                        .title(Line::from(vec![
                            Span::raw(if app.focus == FocusPane::Prompt {
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
                            }),
                            Span::raw(" · "),
                            // 轮到谁了必须一眼看出来：跑的时候回车只是排队，空闲时才轮到用户。
                            {
                                let (state, color) = app.composer_state();
                                Span::styled(state, Style::default().fg(color))
                            },
                            Span::raw(" · "),
                            // 档位常驻在输入框上：用户在按 Enter 之前就该知道这一轮
                            // 会不会弹审批。完全访问用红色，免得忘了自己开着它。
                            Span::styled(
                                format!(
                                    "{} (Shift+Tab)",
                                    permission_commands::label(app.approval_mode, app.language)
                                ),
                                Style::default()
                                    .fg(permission_commands::mode_color(app.approval_mode))
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]))
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
                .scroll((app.prompt_scroll.min(u16::MAX as usize) as u16, 0));
        // 正文由编辑器按宽度预先折行；灰字预测没走编辑器，交给 Paragraph 折。
        let composer = if suggestion_visible {
            composer.wrap(Wrap { trim: false })
        } else {
            composer
        };
        f.render_widget(composer, areas[3]);
        let cursor_y = areas[3].y + 1 + (row.saturating_sub(app.prompt_scroll) as u16);
        let cursor_x = areas[3].x + 1 + (col.min(width.saturating_sub(1)) as u16);
        if app.focus == FocusPane::Prompt && !app.help_visible && app.task_detail.is_none() {
            f.set_cursor_position((cursor_x, cursor_y));
        }
        let status = if app.native_selection_mode {
            app.notice.take().unwrap_or_else(|| {
                app.language
                    .text(
                        "终端原生选择 · 拖选后可用右键复制 · Esc 恢复 WillDeep 鼠标操作",
                        "Native terminal selection · drag, then right-click to copy · Esc restores WillDeep mouse controls",
                        "端末の標準選択 · ドラッグ後に右クリックでコピー · Esc でマウス操作を復元",
                    )
                    .to_owned()
            })
        } else if app.selection_mode {
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
            let input_tokens = app.latest_usage.input_tokens.unwrap_or(0);
            let output_tokens = app.latest_usage.output_tokens.unwrap_or(0);
            let input = format_token_count(input_tokens);
            let output = format_token_count(output_tokens);
            let cache = cache_hit_rate(&app.latest_usage)
                .map(|rate| {
                    format!(
                        " · {} {rate:.2}%",
                        app.language.text("缓存", "cache", "キャッシュ")
                    )
                })
                .unwrap_or_default();
            let context_tokens = app.context_tokens.max(input_tokens);
            let context_pct = context_tokens.saturating_mul(100) / app.context_window.max(1);
            let elapsed = format_elapsed_span(
                app.turn_started
                    .map(|value| value.elapsed())
                    .or(app.last_elapsed)
                    .unwrap_or_default()
                    .as_secs_f32(),
                1,
            );
            if app.running {
                format!(
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output}{cache}{queued} · Esc {} · F1",
                    app.working_summary().unwrap_or_else(|| app.language.text("运行中", "Running", "実行中").to_owned()),
                    app.language.text("焦点", "Focus", "フォーカス"),
                    focus_label(app.focus, app.language),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近"),
                    // 运行中最该被看见的是「怎么停下来」，不是文本选择的快捷键。
                    app.language.text("中断", "interrupt", "中断"),
                    queued = if app.queued_prompts.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} {}", app.language.text("待发", "queued", "送信待ち"), app.queued_prompts.len())
                    }
                )
            } else {
                format!(
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output}{cache} · {elapsed} · {} · Ctrl+S {} · F1",
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
        render_session_picker(f, app);
        render_rewind_picker(f, app);
        render_model_picker(f, app);
        render_workspace_picker(f, app);
        render_permission_picker(f, app);
        render_routing_settings(f, app);
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
            // 装不下就跟着选中项滚。此前这里把全部命中一次性交给 Paragraph，
            // 超出高度的部分被静默裁掉：18 条命令只看得见前 8 条，↓ 到第 9 条
            // 之后连箭头都跑到可视区外，界面看起来就像「后面没有了」。
            let visible = height.saturating_sub(2) as usize;
            let offset = command_window_offset(app.command_selected, command_matches.len(), visible);
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
                .skip(offset)
                .take(visible)
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
            // 鼠标点击按屏幕行找命令，所以命中表必须跟着同一个窗口走，否则
            // 滚动之后点第一行会插入一条根本没显示的命令。
            app.command_hits = (offset..command_matches.len().min(offset + visible))
                .map(|position| (popup.y + 1 + (position - offset) as u16, position))
                .collect();
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        // 装不下时把「第几条 / 共几条」写进标题：光有滚动，
                        // 人还是不知道下面还有多少。
                        .title(if command_matches.len() > visible {
                            format!(
                                "{} · {}/{}",
                                app.language.text(
                                    "命令 · ↑/↓ 选择 · Enter/Tab 插入 · Esc 关闭",
                                    "Commands · ↑/↓ select · Enter/Tab insert · Esc close",
                                    "コマンド · ↑/↓ 選択 · Enter/Tab 挿入 · Esc 閉じる",
                                ),
                                app.command_selected + 1,
                                command_matches.len()
                            )
                        } else {
                            app.language
                                .text(
                                    "命令 · ↑/↓ 选择 · Enter/Tab 插入 · Esc 关闭",
                                    "Commands · ↑/↓ select · Enter/Tab insert · Esc close",
                                    "コマンド · ↑/↓ 選択 · Enter/Tab 挿入 · Esc 閉じる",
                                )
                                .to_owned()
                        })
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
        app.mobile_qr_rect = Rect::default();
        if let Some(qr) = &app.mobile_qr {
            let width = qr.lines().map(UnicodeWidthStr::width).max().unwrap_or(40) as u16 + 4;
            let height = qr.lines().count() as u16 + 4;
            let popup = centered_rect(
                width.min(f.area().width),
                height.min(f.area().height),
                f.area(),
            );
            app.mobile_qr_rect = popup;
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
        app.help_rect = Rect::default();
        if app.help_visible {
            let popup = centered_rect(
                f.area().width.min(88),
                f.area().height.min(28),
                f.area(),
            );
            app.help_rect = popup;
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
        app.task_detail_rect = Rect::default();
        if let Some(detail) = &app.task_detail {
            let content = format!(
                "{}: {}\n{}: {:?}\n{}: {:?}\n{}: {}\n{}: {}\n{}: {}\n\n{}\n{}",
                app.language.text("任务", "Task", "タスク"),
                detail.snapshot.id,
                app.language.text("类型", "Kind", "種類"),
                detail.snapshot.kind,
                app.language.text("状态", "Status", "状態"),
                detail.snapshot.status,
                app.language.text("耗时", "Elapsed", "経過時間"),
                format_elapsed_span(detail.snapshot.elapsed_millis as f32 / 1000.0, 1),
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
            app.task_detail_rect = popup;
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
        render_media_overlay(f, app);
        app.approval_rect = Rect::default();
        app.approval_action_hits.clear();
        if let Some((description, always, _)) = &app.approval {
            let description = description.clone();
            let always = *always;
            let action_count = approval_decisions(always).len() as u16;
            let popup_height = if always { 12 } else { 11 }.min(f.area().height);
            let popup = centered_rect(modal_width(f.area()), popup_height, f.area());
            app.approval_rect = popup;
            paint_modal_halo(f, popup);
            let block = modal_block(
                approval_title(app.language, app.approval_queue.len()),
                Color::LightYellow,
            );
            let inner = block.inner(popup);
            f.render_widget(block, popup);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(1),
                    Constraint::Length(action_count),
                ])
                .split(inner);
            f.render_widget(
                Paragraph::new(description).wrap(Wrap { trim: false }),
                rows[0],
            );
            for (index, decision) in approval_decisions(always).iter().copied().enumerate() {
                let row = Rect::new(
                    rows[2].x,
                    rows[2].y.saturating_add(index as u16),
                    rows[2].width,
                    1.min(rows[2].height.saturating_sub(index as u16)),
                );
                if row.height == 0 {
                    continue;
                }
                let selected = app.approval_selected == index;
                f.render_widget(
                    Paragraph::new(approval_action_text(decision, app.language, selected))
                        .style(approval_action_style(decision, selected)),
                    row,
                );
                app.approval_action_hits.push((row, decision));
            }
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

/// 弹窗与文字选区的底色和字色。
///
/// 故意用 256 色色立方里的固定色，而不是 ANSI 的 `Blue` / `White`：后者是调色板
/// 0–15 号，会被终端主题改写。Catppuccin Mocha 把 Blue 设成浅蓝 #89B4FA、White
/// 设成浅灰，审批弹窗成了浅灰字压浅蓝底，几乎读不出来。16–231 号几乎所有终端
/// 都不改：24 号 #005f87 深蓝配 231 号纯白，对比度约 6.9:1，与主题无关。
pub(crate) const MODAL_BG: Color = Color::Indexed(24);
pub(crate) const MODAL_FG: Color = Color::Indexed(231);

/// The panel colours shared by the approval and question modals. One identity
/// for both — they are the same class of thing, a gate waiting on the human —
/// with the accent left to the caller.
const MODAL_PANEL: Style = Style::new().bg(MODAL_BG).fg(MODAL_FG);

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
                cell.bg = MODAL_BG;
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

fn approval_action_text(decision: ApprovalDecision, language: Language, selected: bool) -> String {
    let marker = if selected { "▶" } else { " " };
    let (shortcut, label) = match decision {
        ApprovalDecision::AllowOnce => (
            "Y / Enter",
            language.text("允许一次", "Allow once", "一度だけ許可"),
        ),
        ApprovalDecision::AlwaysAllow => {
            ("A", language.text("始终允许", "Always allow", "常に許可"))
        }
        ApprovalDecision::Deny => ("N / Esc", language.text("拒绝", "Disallow", "拒否")),
    };
    format!("{marker}  {shortcut:<10}  {label}")
}

fn approval_action_style(decision: ApprovalDecision, selected: bool) -> Style {
    if selected {
        return Style::default()
            .fg(Color::Black)
            .bg(Color::LightCyan)
            .add_modifier(Modifier::BOLD);
    }
    Style::default()
        .fg(if decision == ApprovalDecision::Deny {
            Color::LightRed
        } else {
            Color::LightYellow
        })
        .add_modifier(Modifier::BOLD)
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
        RuntimeStatus::Partial => language.text("部分完成", "partial", "部分完了"),
        RuntimeStatus::Cancelled => language.text("已取消", "cancelled", "キャンセル済み"),
        RuntimeStatus::Unknown => language.text("未知", "unknown", "不明"),
    }
}

/// Runtime 任务借用了 `BackgroundShell` 这个来源（核心枚举没有「任务」一档），
/// 可它是一整轮对话，不是一条 shell 命令——照直显示成「后台命令」会误导人。
fn attention_item_label(item: &AttentionItem, language: Language) -> &'static str {
    if item.id.starts_with("runtime-task:") {
        language.text("任务", "Task", "タスク")
    } else {
        attention_source_label(item.source, language)
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
        AttentionSource::RuntimeEvent => {
            language.text("运行时事件", "Runtime event", "ランタイムイベント")
        }
    }
}

fn attention_style(status: RuntimeStatus) -> Style {
    let color = match status {
        RuntimeStatus::WaitingApproval | RuntimeStatus::WaitingAnswer => Color::Yellow,
        RuntimeStatus::Blocked | RuntimeStatus::Failed => Color::LightRed,
        RuntimeStatus::Working => Color::LightBlue,
        RuntimeStatus::Done => Color::LightGreen,
        RuntimeStatus::Partial => Color::Yellow,
        RuntimeStatus::Cancelled => Color::DarkGray,
        RuntimeStatus::Idle | RuntimeStatus::Unknown => Color::Gray,
    };
    Style::default().fg(color)
}

fn help_content(language: Language) -> String {
    let content = match language {
        Language::ZhCn => {
            "全局\n  F1 / 空输入时 ?  打开帮助    Ctrl+C 退出\n  Ctrl+P 全局命令面板           Ctrl+R 或 /history 搜索历史会话并继续\n  Esc 中断当前轮次（运行中）      Ctrl+W 输入/聊天/活动/状态栏切换\n  Ctrl+B 或 /sidebar 显示/隐藏状态栏（默认隐藏）\n  Ctrl+S 文本选择/复制模式\n\n输入\n  Enter 发送                    Shift/Alt+Enter 或 Ctrl+J 换行\n  F2 展开/恢复大输入空间         Ctrl+A/E 行首/行尾\n  / 命令候选                    $ 技能候选\n  ↑/↓ 选择候选                  Enter/Tab 插入，Esc 关闭\n  Ctrl/Command+Shift+V 粘贴图片 Ctrl+D 删除附件\n\n聊天与活动\n  直接拖动选择聊天文字           Ctrl/Cmd+C 或 Y 复制，Q 引用\n  Ctrl+F 搜索，Enter/Shift+Enter 前后跳转\n  PageUp/PageDown 翻页           Alt+↑/↓ 逐行滚动\n  Ctrl+Home/End 顶部/底部        Ctrl+O 展开工具活动\n  点击活动区聚焦，Enter/Space 展开或收起\n\n状态栏\n  Tab/Shift+Tab 选择分组         ↑/↓ 选择 Inbox 条目\n  Enter 详情，K 停止，R 重试     M 已读，Space 折叠，Esc 返回\n  点击标题折叠，点击条目看详情，滚轮滚动内容"
        }
        Language::En => {
            "Global\n  F1 / ? on empty prompt  Open help    Ctrl+C Exit\n  Ctrl+P Command palette     Ctrl+R or /history Search and continue a Session\n  Esc Interrupt the running turn   Ctrl+W Switch Prompt/Chat/Activity/Status\n  Ctrl+B or /sidebar Show/hide Status (hidden by default)\n  Ctrl+S Text selection mode\n\nPrompt\n  Enter Send                 Shift/Alt+Enter or Ctrl+J Newline\n  F2 Expand/restore composer Ctrl+A/E Line start/end\n  / Command suggestions      $ Skill suggestions\n  ↑/↓ Select                 Enter/Tab Insert, Esc Close\n  Ctrl/Command+Shift+V Paste image      Ctrl+D Remove attachment\n\nChat and activity\n  Drag to select chat text   Ctrl/Cmd+C or Y copy, Q quote\n  Ctrl+F Search, Enter/Shift+Enter Previous/next match\n  PageUp/PageDown Page        Alt+↑/↓ Scroll one line\n  Ctrl+Home/End Top/Bottom    Ctrl+O Expand tool activity\n  Click activity to focus, Enter/Space to expand or collapse\n\nStatus sidebar\n  Tab/Shift+Tab Select section     ↑/↓ Select Inbox item\n  Enter Details, K Stop, R Retry   M Read, Space Toggle, Esc Return\n  Click headers to toggle, items for details, wheel to scroll"
        }
        Language::Ja => {
            "グローバル\n  F1 / 空入力で ?  ヘルプ       Ctrl+C 終了\n  Ctrl+P コマンドパレット        Ctrl+R または /history 履歴セッションを検索して再開\n  Esc 実行中のターンを中断          Ctrl+W 入力/チャット/アクティビティ/状態を切替\n  Ctrl+B または /sidebar で状態欄を表示/非表示（既定は非表示）\n  Ctrl+S テキスト選択モード\n\n入力\n  Enter 送信                     Shift/Alt+Enter または Ctrl+J 改行\n  F2 入力欄を拡大/復元            Ctrl+A/E 行頭/行末\n  / コマンド候補                 $ スキル候補\n  ↑/↓ 選択                       Enter/Tab 挿入、Esc 閉じる\n  Ctrl/Command+Shift+V 画像貼付   Ctrl+D 添付削除\n\nチャットとアクティビティ\n  ドラッグで文字選択              Ctrl/Cmd+C / Y コピー、Q 引用\n  Ctrl+F 検索、Enter/Shift+Enter 前後の一致へ\n  PageUp/PageDown ページ移動      Alt+↑/↓ 1 行スクロール\n  Ctrl+Home/End 先頭/末尾         Ctrl+O ツール詳細\n  アクティビティをクリックして、Enter/Space で開閉\n\n状態サイドバー\n  Tab/Shift+Tab セクション選択    ↑/↓ Inbox 項目選択\n  Enter 詳細、K 停止、R 再実行    M 既読、Space 開閉、Esc 入力へ\n  見出しで開閉、項目で詳細、ホイールでスクロール"
        }
    };
    content
        .replace(
            "Ctrl/Command+Shift+V",
            "Alt+V / Ctrl+V / Ctrl/Command+Shift+V",
        )
        .replace(
            "Ctrl+S 文本选择/复制模式",
            "Ctrl+S 文本选择/复制模式        Ctrl+L 链接与图片面板",
        )
        .replace(
            "Ctrl+S Text selection mode",
            "Ctrl+S Text selection mode      Ctrl+L Links and images",
        )
        .replace(
            "Ctrl+S テキスト選択モード",
            "Ctrl+S テキスト選択モード       Ctrl+L リンクと画像",
        )
}

pub fn channel() -> (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
) {
    mpsc::unbounded_channel()
}
#[cfg(test)]
mod test_suite;
