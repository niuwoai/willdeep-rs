use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Cursor};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use image::{DynamicImage, ImageFormat, RgbaImage};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use regex::RegexBuilder;
use tokio::sync::{mpsc, oneshot};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use willdeep_core::types::Usage;
use willdeep_core::{
    Agent, AgentEvent, ApprovalDecision, Approver, BackgroundTaskRegistry, BackgroundTaskSnapshot,
    BackgroundTaskStatus, EventSink, Message, MessageAttachment, Session, SessionStore,
    SkillCatalog, UserQuestion,
};

use crate::editor::{DraftAttachment, PromptEditor};
use crate::i18n::Language;
use crate::mobile::{MobilePrompt, RelayBridge, RelayGateway};

pub enum UiMessage {
    Agent(AgentEvent),
    Approval(String, bool, oneshot::Sender<ApprovalDecision>),
    Question(UserQuestion, oneshot::Sender<Option<String>>),
    Finished(Result<willdeep_core::AgentOutcome, willdeep_core::AgentError>),
    Compressed(Result<Vec<Message>, willdeep_core::AgentError>),
}
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

struct App {
    input: PromptEditor,
    transcript: Vec<String>,
    running: bool,
    approval: Option<(String, bool, oneshot::Sender<ApprovalDecision>)>,
    question: Option<AskDialog>,
    scroll_from_bottom: usize,
    follow_bottom: bool,
    transcript_width: usize,
    transcript_height: usize,
    viewport_height: usize,
    tools: ToolActivity,
    tools_expanded: bool,
    attachments: Vec<DraftAttachment>,
    selected_attachment: usize,
    prompt_rect: Rect,
    prompt_scroll: usize,
    notice: Option<String>,
    goal: Option<String>,
    mobile_gateway: Option<RelayGateway>,
    mobile_qr: Option<String>,
    mobile_queue: VecDeque<String>,
    latest_usage: Usage,
    turn_started: Option<Instant>,
    last_elapsed: Option<Duration>,
    context_window: u64,
    context_tokens: u64,
    activity_line: String,
    background_tasks: Vec<BackgroundTaskSnapshot>,
    background_notices: VecDeque<String>,
    workspace_status: String,
    progress_log: VecDeque<String>,
    language: Language,
    transient_thought: Option<String>,
    selection_mode: bool,
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
    task_detail: Option<TaskDetail>,
    task_detail_scroll: usize,
    search: Option<SearchState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusPane {
    Prompt,
    Sidebar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarHit {
    Section(usize),
    Task(usize),
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

struct AskDialog {
    request: UserQuestion,
    selected: usize,
    checked: Vec<bool>,
    answer: PromptEditor,
    sender: oneshot::Sender<Option<String>>,
}

#[derive(Default)]
struct ToolActivity {
    requested: usize,
    completed: usize,
    failed: usize,
    counts: BTreeMap<String, usize>,
    details: Vec<String>,
}
impl ToolActivity {
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn requested(&mut self, name: &str) {
        self.requested += 1;
        *self.counts.entry(name.to_owned()).or_default() += 1;
        self.details.push(format!("… {name}"));
    }
    fn completed(&mut self, name: &str, is_error: bool) {
        self.completed += 1;
        self.failed += usize::from(is_error);
        let pending = format!("… {name}");
        if let Some(v) = self.details.iter_mut().rev().find(|v| **v == pending) {
            *v = format!("{} {name}", if is_error { "✗" } else { "✓" });
        }
    }
    fn summary(&self, language: Language) -> String {
        let calls = self
            .counts
            .iter()
            .map(|(n, c)| format!("{n}×{c}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let progress = if self.completed < self.requested {
            format!(
                "{}/{} {}",
                self.completed,
                self.requested,
                language.text("完成", "complete", "完了")
            )
        } else {
            format!(
                "{} {}",
                self.requested,
                language.text("次调用", "calls", "回呼び出し")
            )
        };
        let failed = if self.failed > 0 {
            format!(
                " · {} {}",
                self.failed,
                language.text("失败", "failed", "失敗")
            )
        } else {
            String::new()
        };
        format!(
            "{}: {progress}{failed} · {calls}",
            language.text("工具", "Tools", "ツール")
        )
    }
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
        Language,
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
    let mut runtime = TuiRuntime {
        home,
        skills,
        relay_bridge,
        context_window: ui.2,
        background_tasks: ui.3,
        tx: ui.0,
        rx: ui.1,
    };
    let result = event_loop(&mut term, agent, &mut session, &store, &mut runtime, ui.4).await;
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
    skills: Arc<SkillCatalog>,
    relay_bridge: RelayBridge,
    context_window: u64,
    background_tasks: Arc<BackgroundTaskRegistry>,
    tx: mpsc::UnboundedSender<UiMessage>,
    rx: mpsc::UnboundedReceiver<UiMessage>,
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
    app.workspace_status = workspace_status(&session.workspace, language);
    app.context_window = runtime.context_window.max(1);
    app.background_tasks = runtime.background_tasks.snapshots();
    let mut background_rx = runtime.background_tasks.subscribe();
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    let (mobile_tx, mut mobile_rx) = mpsc::unbounded_channel::<MobilePrompt>();
    loop {
        draw(term, &mut app, &runtime.skills)?;
        tokio::select! {
            _=refresh.tick()=>app.background_tasks=runtime.background_tasks.snapshots(),
            event=events.next()=>if let Some(Ok(event))=event { match event {
                Event::Paste(value)=>app.handle_paste(value),
                Event::Mouse(mouse)=>match mouse.kind {
                    MouseEventKind::Down(_)=>app.handle_mouse(mouse.column,mouse.row,&runtime.background_tasks),
                    MouseEventKind::ScrollUp if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(-3),
                    MouseEventKind::ScrollDown if app.sidebar_rect.contains((mouse.column,mouse.row).into())=>app.sidebar_scroll_by(3),
                    MouseEventKind::ScrollUp=>app.scroll_up(3),
                    MouseEventKind::ScrollDown=>app.scroll_down(3),
                    _=>{}
                },
                Event::Key(key) if key.kind==KeyEventKind::Press=>{
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('c'){break;}
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('s'){
                        app.selection_mode = !app.selection_mode;
                        if app.selection_mode {execute!(term.backend_mut(),DisableMouseCapture)?;} else {execute!(term.backend_mut(),EnableMouseCapture)?;}
                        let notice=if app.selection_mode {language.text("文本选择模式 · 拖动选择，Ctrl+S 恢复交互","Text selection · drag to select, Ctrl+S restores interaction","テキスト選択 · ドラッグで選択、Ctrl+S で戻る")} else {language.text("已恢复鼠标滚动和点击","Mouse scrolling and clicks restored","マウス操作を復元しました")};
                        app.notice=Some(notice.to_owned());
                        continue;
                    }
                    if key.code==KeyCode::Esc&&app.mobile_qr.take().is_some(){continue;}
                    if app.question.is_some(){app.handle_question_key(key);continue;}
                    if let Some((_,always,sender))=app.approval.take(){
                        let decision=match key.code {KeyCode::Char('y')|KeyCode::Char('Y')=>ApprovalDecision::AllowOnce,KeyCode::Char('a')|KeyCode::Char('A') if always=>ApprovalDecision::AlwaysAllow,_=>ApprovalDecision::Deny};
                        let _=sender.send(decision);continue;
                    }
                    if app.task_detail.is_some(){app.handle_task_detail_key(key,&runtime.background_tasks);continue;}
                    if app.search.is_some(){app.handle_search_key(key);continue;}
                    if app.handle_help_key(key) {continue;}
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
                        app.focus=if app.focus==FocusPane::Prompt {FocusPane::Sidebar}else{FocusPane::Prompt};
                        continue;
                    }
                    if app.focus==FocusPane::Sidebar {
                        match key.code {
                            KeyCode::Esc=>app.focus=FocusPane::Prompt,
                            KeyCode::Up=>app.sidebar_move(-1),
                            KeyCode::Down=>app.sidebar_move(1),
                            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT)=>app.sidebar_move(-1),
                            KeyCode::Tab=>app.sidebar_move(1),
                            KeyCode::Enter=>app.sidebar_activate(&runtime.background_tasks),
                            KeyCode::Char(' ')=>app.sidebar_toggle(),
                            _=>{}
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
                            if prompt.trim()=="/compress" {dispatch_compress(&mut app,session,&agent,&runtime.tx);continue;}
                            if app.handle_slash_command(&prompt,&runtime.skills){continue;}
                            dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,prompt)?;
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
                UiMessage::Agent(AgentEvent::ToolCompleted{call,is_error,..})=>{app.record_progress(format!("{} {}",if is_error{language.text("失败","Failed","失敗")}else{language.text("已完成","Finished","完了")},call.name));app.tools.completed(&call.name,is_error);if matches!(call.name.as_str(),"create_file"|"edit_file"|"run_command"|"create_worktree"){app.workspace_status=workspace_status(&session.workspace,language);}},
                UiMessage::Agent(AgentEvent::Usage(v))=>{app.context_tokens=v.input_tokens.unwrap_or(app.context_tokens);app.latest_usage=v;},
                UiMessage::Agent(AgentEvent::CompressionStarted{estimated_tokens})=>{app.context_tokens=estimated_tokens;app.record_progress(language.text("正在压缩上下文","Compressing context","コンテキストを圧縮中").to_owned());},
                UiMessage::Agent(AgentEvent::CompressionCompleted{estimated_tokens})=>{app.context_tokens=estimated_tokens;app.record_progress(language.text("上下文已压缩","Context compressed","コンテキストを圧縮しました").to_owned());},
                UiMessage::Approval(v,a,s)=>app.approval=Some((v,a,s)),
                UiMessage::Question(request,sender)=>{let checked=vec![false;request.options.len()];app.question=Some(AskDialog{request,selected:0,checked,answer:PromptEditor::default(),sender});},
                UiMessage::Finished(Ok(outcome))=>{app.transient_thought=None;app.append_transcript(format!("WillDeep: {}",outcome.final_text));session.messages=outcome.messages;store.save(session)?;app.finish_turn();if let Some(notice)=app.background_notices.pop_front(){app.append_transcript("System: Background result returned to main harness".to_owned());dispatch_notification(&mut app,session,store,&agent,&runtime.tx,notice)?;}else if let Some(prompt)=app.mobile_queue.pop_front(){app.append_transcript(format!("Phone: {prompt}"));dispatch_prompt(&mut app,session,store,&runtime.skills,&agent,&runtime.tx,prompt)?;}},
                UiMessage::Finished(Err(e))=>{app.append_transcript(format!("Error: {e}"));app.finish_turn();},
                UiMessage::Compressed(Ok(messages))=>{let changed=messages.len()<session.messages.len();session.messages=messages;store.save(session)?;app.append_transcript(if changed{"System: Context compressed".to_owned()}else{"System: Context is too short to compress".to_owned()});app.finish_turn();},
                UiMessage::Compressed(Err(e))=>{app.append_transcript(format!("Error: context compression failed: {e}"));app.finish_turn();},
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
            question: None,
            scroll_from_bottom: 0,
            follow_bottom: true,
            transcript_width: 78,
            transcript_height: 0,
            viewport_height: 10,
            tools: ToolActivity::default(),
            tools_expanded: false,
            attachments: Vec::new(),
            selected_attachment: 0,
            prompt_rect: Rect::default(),
            prompt_scroll: 0,
            notice: None,
            goal: None,
            mobile_gateway: None,
            mobile_qr: None,
            mobile_queue: VecDeque::new(),
            latest_usage: Usage::default(),
            turn_started: None,
            last_elapsed: None,
            context_window: 128_000,
            context_tokens: 0,
            activity_line: String::new(),
            background_tasks: Vec::new(),
            background_notices: VecDeque::new(),
            workspace_status: String::new(),
            progress_log: VecDeque::new(),
            language,
            transient_thought: None,
            selection_mode: false,
            skill_selected: 0,
            skill_menu_dismissed: false,
            command_selected: 0,
            command_menu_dismissed: false,
            focus: FocusPane::Prompt,
            sidebar_visible: true,
            sidebar_selected: 0,
            sidebar_expanded: [true, true, true, true],
            sidebar_scroll: 0,
            sidebar_rect: Rect::default(),
            sidebar_wide: false,
            help_visible: false,
            sidebar_hits: Vec::new(),
            sidebar_manual_scroll: false,
            task_detail: None,
            task_detail_scroll: 0,
            search: None,
        }
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
        if self.sidebar_selected == 2
            && self.sidebar_expanded[2]
            && !self.background_tasks.is_empty()
        {
            self.open_task_detail(0, registry);
        } else {
            self.sidebar_toggle();
        }
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
                let suffix = if matches!(command, "/goal" | "/mobile") {
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
        self.last_elapsed = self.turn_started.take().map(|value| value.elapsed());
        self.running = false;
        self.transient_thought = None;
        self.activity_line = self.language.text("就绪", "Ready", "準備完了").to_owned();
    }
    fn record_progress(&mut self, value: String) {
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
    fn handle_question_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.question.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                if let Some(dialog) = self.question.take() {
                    let _ = dialog.sender.send(None);
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
    fn handle_mouse(&mut self, x: u16, y: u16, registry: &BackgroundTaskRegistry) {
        if self.sidebar_rect.contains((x, y).into()) {
            self.focus = FocusPane::Sidebar;
            if let Some((_, hit)) = self.sidebar_hits.iter().find(|(row, _)| *row == y).copied() {
                match hit {
                    SidebarHit::Section(section) => {
                        self.sidebar_selected = section;
                        self.sidebar_toggle();
                    }
                    SidebarHit::Task(index) => self.open_task_detail(index, registry),
                }
            }
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
        match command {
            "/help" => self.append_transcript(
                "System: /goal <text>|off · /compress · /mobile [show|hide|off] · /skills · /clear · /help · use $skill-name in prompts"
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

fn dispatch_prompt(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    skills: &SkillCatalog,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
    prompt: String,
) -> Result<()> {
    app.running = true;
    app.turn_started = Some(Instant::now());
    app.tools.reset();
    app.progress_log.clear();
    app.record_progress(
        app.language
            .text(
                "正在思考 · 理解你的请求",
                "Thinking · understanding your request",
                "思考中 · リクエストを理解しています",
            )
            .to_owned(),
    );
    let history = session.messages.clone();
    let attachments = std::mem::take(&mut app.attachments)
        .into_iter()
        .map(|value| value.message)
        .collect();
    let enriched = app.enrich_prompt(&prompt, skills);
    let user = Message::user_with_attachments(enriched, attachments);
    session.messages.push(user.clone());
    store.save(session)?;
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, user).await,
        ));
    });
    Ok(())
}

fn dispatch_compress(
    app: &mut App,
    session: &Session,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
) {
    app.running = true;
    app.turn_started = Some(Instant::now());
    app.progress_log.clear();
    app.record_progress("Compressing context".to_owned());
    let history = session.messages.clone();
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Compressed(agent.compress_history(history).await));
    });
}

fn dispatch_notification(
    app: &mut App,
    session: &mut Session,
    store: &SessionStore,
    agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<UiMessage>,
    notice: String,
) -> Result<()> {
    app.running = true;
    app.turn_started = Some(Instant::now());
    app.progress_log.clear();
    app.record_progress("Handling background result".to_owned());
    let history = session.messages.clone();
    let message = Message::user(notice);
    session.messages.push(message.clone());
    store.save(session)?;
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(UiMessage::Finished(
            agent.run_with_history_message(history, message).await,
        ));
    });
    Ok(())
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

fn draw(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    skills: &SkillCatalog,
) -> Result<()> {
    term.draw(|f| {
        app.sidebar_wide = f.area().width >= 110;
        let wide_sidebar = app.sidebar_visible && app.sidebar_wide;
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
        let activity = if app.tools_expanded && app.tools.requested > 0 {
            8
        } else if app.running {
            5
        } else {
            3
        };
        let attach = if app.attachments.is_empty() { 0 } else { 3 };
        let input_width = canvas.width.saturating_sub(2).max(1) as usize;
        let input_lines = visual_lines(app.input.text(), input_width).clamp(3, 6);
        let input_height = (input_lines + 2) as u16;
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(activity),
                Constraint::Length(attach),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(canvas);
        let mut visible_transcript = app.transcript.clone();
        if let Some(thought) = &app.transient_thought {
            visible_transcript.push(format!(
                "WillDeep · {}: {thought}",
                app.language.text("思考中", "thinking", "思考中")
            ));
        }
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        app.transcript_height =
            rendered_transcript_height(&visible_transcript, app.transcript_width);
        let max = app.max_scroll();
        app.scroll_from_bottom = app.scroll_from_bottom.min(max);
        let offset = max
            .saturating_sub(app.scroll_from_bottom)
            .min(u16::MAX as usize) as u16;
        let title = if app.selection_mode {
            app.language
                .text(
                    "WillDeep · 文本选择模式 · Ctrl+S 退出",
                    "WillDeep · text selection · Ctrl+S exits",
                    "WillDeep · テキスト選択 · Ctrl+S で終了",
                )
                .to_owned()
        } else if app.follow_bottom {
            "WillDeep".to_owned()
        } else {
            format!("WillDeep · history ↑{}", app.scroll_from_bottom)
        };
        let search_query = app
            .search
            .as_ref()
            .map(|search| search.editor.text().trim())
            .filter(|query| !query.trim().is_empty());
        let colored = colored_transcript(&visible_transcript, search_query);
        f.render_widget(
            Paragraph::new(colored)
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Blue)),
                )
                .scroll((offset, 0))
                .wrap(Wrap { trim: false }),
            areas[0],
        );
        if activity > 0 {
            let text = if app.tools_expanded {
                format!(
                    "{} · {}\n{}",
                    app.activity_line,
                    app.tools.summary(app.language),
                    app.tools
                        .details
                        .iter()
                        .rev()
                        .take(5)
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else if app.running {
                app.progress_log
                    .iter()
                    .rev()
                    .take(3)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
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
                            "活动 · Ctrl+O 查看详情",
                            "Activity · Ctrl+O details",
                            "アクティビティ · Ctrl+O で詳細",
                        ))
                        .borders(Borders::ALL),
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
        f.render_widget(
            Paragraph::new(app.input.text())
                .block(
                    Block::default()
                        .title(if app.focus == FocusPane::Prompt {
                            app.language.text(
                                "输入 [焦点] · Shift/Alt+Enter 换行",
                                "Prompt [focused] · Shift/Alt+Enter newline",
                                "入力 [フォーカス] · Shift/Alt+Enter で改行",
                            )
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
                .scroll((app.prompt_scroll.min(u16::MAX as usize) as u16, 0))
                .wrap(Wrap { trim: false }),
            areas[3],
        );
        let cursor_y = areas[3].y + 1 + (row.saturating_sub(app.prompt_scroll) as u16);
        let cursor_x = areas[3].x + 1 + (col.min(width.saturating_sub(1)) as u16);
        if app.focus == FocusPane::Prompt && !app.help_visible && app.task_detail.is_none() {
            f.set_cursor_position((cursor_x, cursor_y));
        }
        let status = app.notice.take().unwrap_or_else(|| {
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
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output} · {elapsed:.1}s · F1",
                    app.language.text("运行中", "Running", "実行中"),
                    app.language.text("焦点", "Focus", "フォーカス"),
                    focus_label(app.focus, app.language),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近")
                )
            } else {
                format!(
                    "{} · {}: {} · {} {context_pct}% · {} ↑{input} ↓{output} · {elapsed:.1}s · {} · F1",
                    app.language.text("就绪", "Ready", "準備完了"),
                    app.language.text("焦点", "Focus", "フォーカス"),
                    focus_label(app.focus, app.language),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近"),
                    app.language
                        .text("Enter 发送", "Enter send", "Enter で送信")
                )
            }
        });
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
        if let Some(search) = &app.search {
            let width = f.area().width.min(72);
            let popup = Rect {
                x: f.area().x + f.area().width.saturating_sub(width) / 2,
                y: f.area().y,
                width,
                height: 3.min(f.area().height),
            };
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
        if let Some((description, always, _)) = &app.approval {
            let content = approval_content(description, *always, app.language);
            let popup = centered_rect(f.area().width.min(76), 9.min(f.area().height), f.area());
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(content)
                    .block(
                        Block::default()
                            .title(
                                app.language
                                    .text("需要确认", "Approval required", "承認が必要"),
                            )
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow)),
                    )
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
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
            let height = (content.lines().count() as u16 + 2)
                .min(f.area().height)
                .max(8);
            let popup = centered_rect(f.area().width.min(84), height, f.area());
            f.render_widget(Clear, popup);
            f.render_widget(
                Paragraph::new(content)
                    .block(
                        Block::default()
                            .title(app.language.text(
                                "智能体提问",
                                "Question from Agent",
                                "エージェントからの質問",
                            ))
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    )
                    .wrap(Wrap { trim: false }),
                popup,
            );
        }
    })?;
    Ok(())
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

fn command_candidates(language: Language) -> [(&'static str, &'static str); 6] {
    [
        (
            "/help",
            language.text("查看帮助", "Show help", "ヘルプを表示"),
        ),
        (
            "/goal",
            language.text("设置持续目标", "Set persistent goal", "継続目標を設定"),
        ),
        (
            "/compress",
            language.text(
                "压缩会话上下文",
                "Compress conversation context",
                "会話コンテキストを圧縮",
            ),
        ),
        (
            "/mobile",
            language.text(
                "管理手机中继",
                "Manage mobile relay",
                "モバイルリレーを管理",
            ),
        ),
        (
            "/skills",
            language.text(
                "查看可用技能",
                "List available skills",
                "利用可能なスキルを表示",
            ),
        ),
        (
            "/clear",
            language.text("清空聊天显示", "Clear chat display", "チャット表示を消去"),
        ),
    ]
}

fn focus_label(focus: FocusPane, language: Language) -> &'static str {
    match focus {
        FocusPane::Prompt => language.text("输入", "Prompt", "入力"),
        FocusPane::Sidebar => language.text("状态栏", "Status", "ステータス"),
    }
}

fn help_content(language: Language) -> &'static str {
    match language {
        Language::ZhCn => {
            "全局\n  F1 / 空输入时 ?  打开帮助    Ctrl+C 退出\n  Ctrl+W  输入/状态栏切换      Ctrl+B 显示或隐藏状态栏\n  Ctrl+S  文本选择/复制模式\n\n输入\n  Enter 发送                    Shift/Alt+Enter 或 Ctrl+J 换行\n  / 命令候选                    $ 技能候选\n  ↑/↓ 选择候选                  Enter/Tab 插入，Esc 关闭\n  Ctrl/Command+Shift+V 粘贴图片 Ctrl+D 删除附件\n\n聊天与活动\n  Ctrl+F 搜索，Enter/Shift+Enter 前后跳转\n  PageUp/PageDown 翻页           Alt+↑/↓ 逐行滚动\n  Ctrl+Home/End 顶部/底部        Ctrl+O 展开工具活动\n\n状态栏\n  ↑/↓ 或 Tab/Shift+Tab 选择分组\n  Enter/Space 折叠或展开         Esc 返回输入\n  点击标题折叠，点击任务看详情，滚轮滚动内容"
        }
        Language::En => {
            "Global\n  F1 / ? on empty prompt  Open help    Ctrl+C Exit\n  Ctrl+W  Switch Prompt/Status          Ctrl+B Show or hide Status\n  Ctrl+S  Terminal text selection mode\n\nPrompt\n  Enter Send                 Shift/Alt+Enter or Ctrl+J Newline\n  / Command suggestions      $ Skill suggestions\n  ↑/↓ Select                 Enter/Tab Insert, Esc Close\n  Ctrl/Command+Shift+V Paste image      Ctrl+D Remove attachment\n\nChat and activity\n  Ctrl+F Search, Enter/Shift+Enter Previous/next match\n  PageUp/PageDown Page        Alt+↑/↓ Scroll one line\n  Ctrl+Home/End Top/Bottom    Ctrl+O Expand tool activity\n\nStatus sidebar\n  ↑/↓ or Tab/Shift+Tab Select section\n  Enter/Space Collapse or expand        Esc Return to Prompt\n  Click headers to toggle, tasks for details, wheel to scroll"
        }
        Language::Ja => {
            "グローバル\n  F1 / 空入力で ?  ヘルプ       Ctrl+C 終了\n  Ctrl+W 入力/状態を切替         Ctrl+B 状態欄を表示/非表示\n  Ctrl+S テキスト選択モード\n\n入力\n  Enter 送信                     Shift/Alt+Enter または Ctrl+J 改行\n  / コマンド候補                 $ スキル候補\n  ↑/↓ 選択                       Enter/Tab 挿入、Esc 閉じる\n  Ctrl/Command+Shift+V 画像貼付   Ctrl+D 添付削除\n\nチャットとアクティビティ\n  Ctrl+F 検索、Enter/Shift+Enter 前後の一致へ\n  PageUp/PageDown ページ移動      Alt+↑/↓ 1 行スクロール\n  Ctrl+Home/End 先頭/末尾         Ctrl+O ツール詳細\n\n状態サイドバー\n  ↑/↓ または Tab/Shift+Tab セクション選択\n  Enter/Space 折りたたみ          Esc 入力へ戻る\n  見出しで開閉、タスクで詳細、ホイールでスクロール"
        }
    }
}

fn render_sidebar(f: &mut ratatui::Frame<'_>, app: &mut App, area: Rect) {
    let relay = if app.mobile_gateway.is_some() {
        app.language.text("已连接", "connected", "接続済み")
    } else {
        app.language.text("关闭", "off", "オフ")
    };
    let agent = if app.running {
        app.language.text("运行中", "running", "実行中")
    } else {
        app.language.text("空闲", "idle", "待機中")
    };
    let titles = [
        app.language.text("工作区", "Workspace", "ワークスペース"),
        app.language.text("运行状态", "Runtime", "実行状態"),
        app.language
            .text("后台任务", "Background tasks", "バックグラウンドタスク"),
        app.language
            .text("移动中继", "Mobile relay", "モバイルリレー"),
    ];
    let mut lines = Vec::new();
    let mut headers = [0usize; 4];
    let mut logical_hits = Vec::new();
    for (section, title) in titles.into_iter().enumerate() {
        headers[section] = lines.len();
        logical_hits.push((lines.len(), SidebarHit::Section(section)));
        let selected = app.focus == FocusPane::Sidebar && app.sidebar_selected == section;
        let marker = if app.sidebar_expanded[section] {
            "▾"
        } else {
            "▸"
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        };
        lines.push(Line::styled(format!("{marker} {title}"), style));
        if !app.sidebar_expanded[section] {
            continue;
        }
        match section {
            0 => lines.extend(
                app.workspace_status
                    .lines()
                    .map(|line| Line::raw(format!("  {line}"))),
            ),
            1 => {
                lines.push(Line::raw(format!(
                    "  {}: {agent}",
                    app.language.text("智能体", "Agent", "エージェント")
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}/{}",
                    app.language
                        .text("工具完成", "Tools complete", "ツール完了"),
                    app.tools.completed,
                    app.tools.requested
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language.text("失败", "Failed", "失敗"),
                    app.tools.failed
                )));
            }
            2 if app.background_tasks.is_empty() => lines.push(Line::raw(format!(
                "  {}",
                app.language.text(
                    "没有后台任务",
                    "No background tasks",
                    "バックグラウンドタスクなし"
                )
            ))),
            2 => {
                for (task_index, task) in app.background_tasks.iter().take(8).enumerate() {
                    logical_hits.push((lines.len(), SidebarHit::Task(task_index)));
                    lines.push(Line::raw(format!(
                        "  {} · {:?} · {:.1}s",
                        task.id,
                        task.status,
                        task.elapsed_millis as f64 / 1000.0
                    )));
                    lines.push(Line::styled(
                        format!("    {}", task.label),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            3 => {
                lines.push(Line::raw(format!(
                    "  {}: {relay}",
                    app.language.text("中继", "Relay", "リレー")
                )));
                lines.push(Line::raw(format!(
                    "  {}: {}",
                    app.language
                        .text("手机队列", "Phone queue", "モバイルキュー"),
                    app.mobile_queue.len()
                )));
            }
            _ => {}
        }
        lines.push(Line::raw(""));
    }
    let viewport = area.height.saturating_sub(2).max(1) as usize;
    let selected_row = headers[app.sidebar_selected];
    if !app.sidebar_manual_scroll {
        if selected_row < app.sidebar_scroll {
            app.sidebar_scroll = selected_row;
        } else if selected_row >= app.sidebar_scroll + viewport {
            app.sidebar_scroll = selected_row.saturating_sub(viewport - 1);
        }
    }
    let max_scroll = lines.len().saturating_sub(viewport);
    app.sidebar_scroll = app.sidebar_scroll.min(max_scroll);
    app.sidebar_hits = logical_hits
        .into_iter()
        .filter_map(|(row, hit)| {
            (row >= app.sidebar_scroll && row < app.sidebar_scroll + viewport)
                .then_some((area.y + 1 + (row - app.sidebar_scroll) as u16, hit))
        })
        .collect();
    let border = if app.focus == FocusPane::Sidebar {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(app.language.text(
                        "状态 · Ctrl+W 聚焦 · Ctrl+B 隐藏",
                        "Status · Ctrl+W focus · Ctrl+B hide",
                        "状態 · Ctrl+W フォーカス · Ctrl+B 非表示",
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border)),
            )
            .scroll((app.sidebar_scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub fn channel() -> (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
) {
    mpsc::unbounded_channel()
}
fn colored_transcript(entries: &[String], search_query: Option<&str>) -> Text<'static> {
    let mut lines = Vec::new();
    for value in entries {
        if let Some(content) = value.strip_prefix("WillDeep: ") {
            lines.extend(render_assistant_markdown(content));
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

fn rendered_transcript_height(entries: &[String], width: usize) -> usize {
    Paragraph::new(colored_transcript(entries, None))
        .wrap(Wrap { trim: false })
        .line_count(width.max(1).min(u16::MAX as usize) as u16)
}

fn render_assistant_markdown(content: &str) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut code_block = false;
    for (index, raw) in content.lines().enumerate() {
        if raw.trim_start().starts_with("```") {
            code_block = !code_block;
            continue;
        }
        let prefix = (index == 0).then(|| {
            Span::styled(
                "WillDeep: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        });
        let mut spans = Vec::new();
        if let Some(prefix) = prefix {
            spans.push(prefix);
        }
        if code_block {
            spans.push(Span::styled(
                raw.to_owned(),
                Style::default().fg(Color::White).bg(Color::DarkGray),
            ));
        } else {
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
        }
        output.push(Line::from(spans));
    }
    if output.is_empty() {
        output.push(Line::styled("WillDeep:", Style::default().fg(Color::Green)));
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
fn compact_thought(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut compact = normalized.chars().take(180).collect::<String>();
    if normalized.chars().count() > 180 {
        compact.push('…');
    }
    compact
}
fn visual_lines(text: &str, width: usize) -> usize {
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

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn goal_command_enriches_future_prompts() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();

        assert!(app.handle_slash_command("/goal ship the CLI", &skills));
        let enriched = app.enrich_prompt("continue", &skills);

        assert!(enriched.contains("<goal>\nship the CLI\n</goal>"));
        assert!(enriched.ends_with("continue"));
        assert!(app.handle_slash_command("/goal off", &skills));
        assert_eq!(app.enrich_prompt("continue", &skills), "continue");
    }

    #[test]
    fn unknown_slash_command_is_handled_locally() {
        let mut app = App::new(Vec::new(), Language::En);
        let skills = SkillCatalog::default();

        assert!(app.handle_slash_command("/wat", &skills));
        assert!(app.transcript.last().unwrap().contains("unknown command"));
    }

    #[test]
    fn ordinary_prompt_is_not_treated_as_command() {
        let mut app = App::new(Vec::new(), Language::En);
        assert!(!app.handle_slash_command("please inspect /docs", &SkillCatalog::default()));
    }
}
fn transcript(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m.role {
            willdeep_core::Role::User => Some(format!(
                "You: {}{}",
                m.content,
                if m.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" [{} attachment(s)]", m.attachments.len())
                }
            )),
            willdeep_core::Role::Assistant if !m.content.trim().is_empty() => {
                Some(format!("WillDeep: {}", m.content))
            }
            _ => None,
        })
        .collect()
}

fn welcome_message(workspace: &std::path::Path, language: Language) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_tools() {
        let mut a = ToolActivity::default();
        a.requested("read_file");
        a.completed("read_file", true);
        assert!(a.summary(Language::En).contains("1 failed"));
    }

    #[test]
    fn welcome_mentions_workspace_without_entering_model_history() {
        let welcome = welcome_message(std::path::Path::new("/tmp/willdeep-rs"), Language::ZhCn);
        assert!(welcome.starts_with("WillDeep:"));
        assert!(welcome.contains("willdeep-rs"));
    }
    #[test]
    fn approval_shortcuts_are_colored_and_localized() {
        let lines = approval_content("run command", true, Language::Ja);
        let actions = lines.last().unwrap();
        assert_eq!(actions.spans[0].content, " Y ");
        assert_eq!(actions.spans[0].style.bg, Some(Color::Yellow));
        assert!(
            actions
                .spans
                .iter()
                .any(|span| span.content.contains("常に許可"))
        );
        let deny = actions
            .spans
            .iter()
            .find(|span| span.content == " N ")
            .unwrap();
        assert_eq!(deny.style.bg, Some(Color::Red));
        assert!(
            actions
                .spans
                .iter()
                .any(|span| span.content.contains("拒否"))
        );
    }
    #[test]
    fn long_paste_is_attachment_and_deletable() {
        let mut a = App::new(Vec::new(), Language::En);
        a.handle_paste("one\ntwo".to_owned());
        assert_eq!(a.attachments.len(), 1);
        a.delete_selected_attachment();
        assert!(a.attachments.is_empty());
    }
    #[test]
    fn cjk_wraps() {
        assert_eq!(visual_lines("中文", 2), 2);
    }
    #[test]
    fn transcript_height_uses_ratatui_word_wrapping() {
        let entries = vec!["WillDeep: 12345 12345 12345".to_owned()];

        assert_eq!(rendered_transcript_height(&entries, 10), 4);
        assert_eq!(visual_lines(&entries.join("\n"), 10), 3);
    }
    #[test]
    fn skill_menu_filters_and_inserts_selected_skill() {
        let workspace =
            std::env::temp_dir().join(format!("willdeep-tui-skill-menu-{}", uuid::Uuid::new_v4()));
        let skill_dir = workspace.join(".willdeep/skills/image-processing");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Image Processing\ndescription: Edit images\n---\n# Instructions",
        )
        .unwrap();
        let skills = SkillCatalog::discover(&workspace, &[]);
        let mut app = App::new(Vec::new(), Language::En);
        app.input.insert("use $image-pro");

        assert!(app.handle_skill_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &skills));
        assert_eq!(app.input.text(), "use $image-processing ");

        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn command_menu_filters_and_inserts_without_executing() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.input.insert("/com");

        assert!(app.handle_command_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.input.text(), "/compress");
        assert!(app.transcript.is_empty());
    }
    #[test]
    fn sidebar_navigation_wraps_and_toggles_sections() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        app.sidebar_move(-1);
        assert_eq!(app.focus, FocusPane::Sidebar);
        assert_eq!(app.sidebar_selected, 3);

        app.sidebar_toggle();
        assert!(!app.sidebar_expanded[3]);
        app.sidebar_move(1);
        assert_eq!(app.sidebar_selected, 0);
    }
    #[test]
    fn clicking_sidebar_focuses_it_and_clicking_prompt_restores_prompt_focus() {
        let mut app = App::new(Vec::new(), Language::En);
        let registry = BackgroundTaskRegistry::default();
        app.sidebar_rect = Rect::new(80, 0, 20, 30);
        app.prompt_rect = Rect::new(0, 20, 80, 8);

        app.handle_mouse(85, 5, &registry);
        assert_eq!(app.focus, FocusPane::Sidebar);
        app.handle_mouse(5, 22, &registry);
        assert_eq!(app.focus, FocusPane::Prompt);
    }
    #[test]
    fn clicking_sidebar_hits_toggles_sections_and_opens_task_detail() {
        let registry = BackgroundTaskRegistry::default();
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_rect = Rect::new(80, 0, 20, 30);
        app.sidebar_hits = vec![(2, SidebarHit::Section(1)), (5, SidebarHit::Task(0))];
        app.background_tasks.push(BackgroundTaskSnapshot {
            id: "job_test".to_owned(),
            kind: willdeep_core::BackgroundTaskKind::Shell,
            label: "Run tests".to_owned(),
            status: BackgroundTaskStatus::Completed,
            elapsed_millis: 1200,
            exit_code: Some(0),
            output_bytes: 12,
        });

        app.handle_mouse(85, 2, &registry);
        assert_eq!(app.sidebar_selected, 1);
        assert!(!app.sidebar_expanded[1]);
        app.handle_mouse(85, 5, &registry);
        assert_eq!(
            app.task_detail
                .as_ref()
                .map(|detail| detail.snapshot.id.as_str()),
            Some("job_test")
        );
    }
    #[test]
    fn sidebar_wheel_scrolls_content_without_changing_selected_section() {
        let mut app = App::new(Vec::new(), Language::En);
        app.sidebar_selected = 2;
        app.sidebar_scroll_by(3);
        assert_eq!(app.sidebar_selected, 2);
        assert_eq!(app.sidebar_scroll, 3);
        assert!(app.sidebar_manual_scroll);
    }
    #[test]
    fn help_opens_globally_but_question_mark_remains_typable_in_a_prompt() {
        let mut app = App::new(Vec::new(), Language::ZhCn);
        assert!(app.handle_help_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
        assert!(app.help_visible);
        assert!(app.handle_help_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.help_visible);

        app.input.insert("这是什么");
        assert!(!app.handle_help_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)));
    }
    #[test]
    fn help_documents_current_focus_and_sidebar_shortcuts() {
        assert_eq!(focus_label(FocusPane::Sidebar, Language::ZhCn), "状态栏");
        let help = help_content(Language::ZhCn);
        assert!(help.contains("Ctrl+W"));
        assert!(help.contains("Enter/Space"));
        assert!(help.contains("Ctrl+F"));
    }
    #[test]
    fn chat_search_filters_cycles_and_scrolls_to_matching_entries() {
        let mut app = App::new(
            vec![
                "You: first".to_owned(),
                "WillDeep: Alpha result".to_owned(),
                "You: middle".to_owned(),
                "WillDeep: alpha again".to_owned(),
            ],
            Language::En,
        );
        app.transcript_width = 40;
        app.viewport_height = 2;
        app.search = Some(SearchState::default());
        for character in "ALPHA".chars() {
            app.handle_search_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let search = app.search.as_ref().unwrap();
        assert_eq!(search.matches, vec![1, 3]);
        assert_eq!(search.selected, 0);
        assert!(!app.follow_bottom);

        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().selected, 1);
        app.handle_search_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.search.as_ref().unwrap().selected, 0);
    }
    #[test]
    fn chat_search_highlights_matches_without_removing_markdown_styles() {
        let text = colored_transcript(&["WillDeep: **Alpha** and alpha".to_owned()], Some("alpha"));
        let highlighted = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.bg == Some(Color::Yellow))
            .collect::<Vec<_>>();

        assert_eq!(highlighted.len(), 2);
        assert!(highlighted[0].style.add_modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn transient_thought_is_single_line_and_bounded() {
        let value = compact_thought(&format!("first\n{}", "x".repeat(300)));
        assert!(!value.contains('\n'));
        assert!(value.chars().count() <= 181);
    }
    #[test]
    fn renders_common_markdown_for_terminal() {
        let lines = render_assistant_markdown(
            "# Title\n- **bold** and `code`\n[Docs](https://example.com)",
        );
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("■ Title"));
        assert!(rendered.contains("• bold and code"));
        assert!(rendered.contains("Docs (https://example.com)"));
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }
    #[test]
    fn encodes_clipboard_rgba_as_deletable_image() {
        let value = encode_clipboard_image(1, 1, vec![255, 0, 0, 255]).unwrap();
        assert!(matches!(
            value.message,
            MessageAttachment::Image {
                width: 1,
                height: 1,
                ..
            }
        ));
    }
    #[tokio::test]
    async fn ask_dialog_accepts_custom_text() {
        let mut app = App::new(Vec::new(), Language::En);
        let (sender, receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: false,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        });
        for value in "Other".chars() {
            app.handle_question_key(KeyEvent::new(KeyCode::Char(value), KeyModifiers::NONE));
        }
        app.handle_question_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(receiver.await.expect("answer").as_deref(), Some("Other"));
    }
    #[tokio::test]
    async fn ask_dialog_supports_multiple_selected_options() {
        let mut app = App::new(Vec::new(), Language::En);
        let (sender, receiver) = oneshot::channel();
        app.question = Some(AskDialog {
            request: UserQuestion {
                question: "Choose".to_owned(),
                options: vec!["A".to_owned(), "B".to_owned()],
                multi_select: true,
            },
            selected: 0,
            checked: vec![false, false],
            answer: PromptEditor::default(),
            sender,
        });
        app.handle_question_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_question_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(receiver.await.expect("answer").as_deref(), Some("A, B"));
    }
}
