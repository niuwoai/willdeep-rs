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
use tokio::sync::{mpsc, oneshot};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use willdeep_core::types::Usage;
use willdeep_core::{
    Agent, AgentEvent, ApprovalDecision, Approver, BackgroundTaskRegistry, BackgroundTaskSnapshot,
    EventSink, Message, MessageAttachment, Session, SessionStore, SkillCatalog, UserQuestion,
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
        draw(term, &mut app)?;
        tokio::select! {
            _=refresh.tick()=>app.background_tasks=runtime.background_tasks.snapshots(),
            event=events.next()=>if let Some(Ok(event))=event { match event {
                Event::Paste(value)=>app.handle_paste(value),
                Event::Mouse(mouse)=>match mouse.kind {
                    MouseEventKind::Down(_)=>app.handle_mouse(mouse.column,mouse.row),
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
                        KeyCode::Left=>app.input.left(),KeyCode::Right=>app.input.right(),KeyCode::Up=>app.input.up_visual(app.prompt_rect.width.saturating_sub(2).max(1) as usize),KeyCode::Down=>app.input.down_visual(app.prompt_rect.width.saturating_sub(2).max(1) as usize),KeyCode::Home=>app.input.home(),KeyCode::End=>app.input.end(),KeyCode::Backspace=>app.input.backspace(),KeyCode::Delete=>app.input.delete(),
                        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>app.input.insert(&c.to_string()),_=>{}
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
        visual_lines(&self.transcript.join("\n"), self.transcript_width)
            .saturating_sub(self.viewport_height)
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
        if !self.follow_bottom {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(visual_lines(&v, self.transcript_width));
        }
        self.transcript.push(v);
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
    fn handle_mouse(&mut self, x: u16, y: u16) {
        if self.prompt_rect.contains((x, y).into()) {
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

fn draw(term: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    term.draw(|f| {
        let columns = if f.area().width >= 110 {
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
        let text = visible_transcript.join("\n");
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        let max = visual_lines(&text, app.transcript_width).saturating_sub(app.viewport_height);
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
        let colored = colored_transcript(&visible_transcript);
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
                        .title(app.language.text(
                            "输入 · Shift/Alt+Enter 换行",
                            "Prompt · Shift/Alt+Enter newline",
                            "入力 · Shift/Alt+Enter で改行",
                        ))
                        .borders(Borders::ALL),
                )
                .scroll((app.prompt_scroll.min(u16::MAX as usize) as u16, 0))
                .wrap(Wrap { trim: false }),
            areas[3],
        );
        let cursor_y = areas[3].y + 1 + (row.saturating_sub(app.prompt_scroll) as u16);
        let cursor_x = areas[3].x + 1 + (col.min(width.saturating_sub(1)) as u16);
        f.set_cursor_position((cursor_x, cursor_y));
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
                    "{} · {} {context_pct}% · {} ↑{input} ↓{output} · {elapsed:.1}s · Ctrl+O",
                    app.language.text("运行中", "Running", "実行中"),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近")
                )
            } else {
                format!(
                    "{} · {} {context_pct}% · {} ↑{input} ↓{output} · {elapsed:.1}s · {}",
                    app.language.text("就绪", "Ready", "準備完了"),
                    app.language.text("上下文", "context", "コンテキスト"),
                    app.language.text("最近", "latest", "直近"),
                    app.language
                        .text("Enter 发送", "Enter send", "Enter で送信")
                )
            }
        });
        f.render_widget(Paragraph::new(status), areas[4]);
        if columns[1].width > 0 {
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
            let jobs = app
                .background_tasks
                .iter()
                .take(8)
                .map(|task| {
                    format!(
                        "{} · {:?} · {:.1}s\n  {}",
                        task.id,
                        task.status,
                        task.elapsed_millis as f64 / 1000.0,
                        task.label
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let background = format!(
                "{}\n\n{}: {agent}\n{}: {relay}\n{}: {}\n{}: {}/{}\n{}: {}\n\n{}",
                app.workspace_status,
                app.language.text("智能体", "Agent", "エージェント"),
                app.language.text("中继", "Relay", "リレー"),
                app.language
                    .text("手机队列", "Phone queue", "モバイルキュー"),
                app.mobile_queue.len(),
                app.language
                    .text("工具完成", "Tools complete", "ツール完了"),
                app.tools.completed,
                app.tools.requested,
                app.language.text("失败", "Failed", "失敗"),
                app.tools.failed,
                if jobs.is_empty() {
                    app.language.text(
                        "没有后台任务",
                        "No background tasks",
                        "バックグラウンドタスクなし",
                    )
                } else {
                    &jobs
                }
            );
            f.render_widget(
                Paragraph::new(background)
                    .block(
                        Block::default()
                            .title(app.language.text(
                                "工作区 · 后台任务",
                                "Workspace · Background",
                                "ワークスペース · バックグラウンド",
                            ))
                            .borders(Borders::ALL),
                    )
                    .wrap(Wrap { trim: false }),
                columns[1],
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
fn colored_transcript(entries: &[String]) -> Text<'static> {
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
    Text::from(lines)
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
