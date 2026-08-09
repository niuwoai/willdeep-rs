use std::collections::BTreeMap;
use std::io::{self, Cursor};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use image::{DynamicImage, ImageFormat, RgbaImage};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::{mpsc, oneshot};
use unicode_width::UnicodeWidthChar;
use willdeep_core::{
    Agent, AgentEvent, Approver, EventSink, Message, MessageAttachment, Session, SessionStore,
};

use crate::editor::{DraftAttachment, PromptEditor};

pub enum UiMessage {
    Agent(AgentEvent),
    Approval(String, oneshot::Sender<bool>),
    Finished(Result<willdeep_core::AgentOutcome, willdeep_core::AgentError>),
}
pub struct TuiSink(pub mpsc::UnboundedSender<UiMessage>);
#[async_trait]
impl EventSink for TuiSink {
    async fn emit(&self, event: AgentEvent) {
        let _ = self.0.send(UiMessage::Agent(event));
    }
}
pub struct TuiApprover(pub mpsc::UnboundedSender<UiMessage>);
#[async_trait]
impl Approver for TuiApprover {
    async fn approve(&self, description: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .0
            .send(UiMessage::Approval(description.to_owned(), tx))
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }
}

struct App {
    input: PromptEditor,
    transcript: Vec<String>,
    running: bool,
    approval: Option<(String, oneshot::Sender<bool>)>,
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
    fn summary(&self) -> String {
        let calls = self
            .counts
            .iter()
            .map(|(n, c)| format!("{n}×{c}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let progress = if self.completed < self.requested {
            format!("{}/{} complete", self.completed, self.requested)
        } else {
            format!("{} calls", self.requested)
        };
        let failed = if self.failed > 0 {
            format!(" · {} failed", self.failed)
        } else {
            String::new()
        };
        format!("Tools: {progress}{failed} · {calls}")
    }
}

pub async fn run(
    agent: Arc<Agent>,
    mut session: Session,
    store: SessionStore,
    tx: mpsc::UnboundedSender<UiMessage>,
    mut rx: mpsc::UnboundedReceiver<UiMessage>,
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
    let result = event_loop(&mut term, agent, &mut session, &store, tx, &mut rx).await;
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

async fn event_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent: Arc<Agent>,
    session: &mut Session,
    store: &SessionStore,
    tx: mpsc::UnboundedSender<UiMessage>,
    rx: &mut mpsc::UnboundedReceiver<UiMessage>,
) -> Result<()> {
    let mut app = App::new(transcript(&session.messages));
    let mut events = EventStream::new();
    loop {
        draw(term, &mut app)?;
        tokio::select! {
            event=events.next()=>if let Some(Ok(event))=event { match event {
                Event::Paste(value)=>app.handle_paste(value),
                Event::Mouse(mouse) if matches!(mouse.kind,MouseEventKind::Down(_))=>app.handle_mouse(mouse.column,mouse.row),
                Event::Key(key) if key.kind==KeyEventKind::Press=>{
                    if key.modifiers.contains(KeyModifiers::CONTROL)&&key.code==KeyCode::Char('c'){break;}
                    if let Some((_,sender))=app.approval.take(){let _=sender.send(matches!(key.code,KeyCode::Char('y')|KeyCode::Char('Y')));continue;}
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
                            let prompt=app.input.take();app.append_transcript(format!("You: {prompt}"));app.running=true;app.tools.reset();
                            let history=session.messages.clone();let attachments=std::mem::take(&mut app.attachments).into_iter().map(|v|v.message).collect();let user=Message::user_with_attachments(&prompt,attachments);session.messages.push(user.clone());store.save(session)?;
                            let agent=agent.clone();let tx=tx.clone();tokio::spawn(async move{let _=tx.send(UiMessage::Finished(agent.run_with_history_message(history,user).await));});
                        }
                        KeyCode::Left=>app.input.left(),KeyCode::Right=>app.input.right(),KeyCode::Up=>app.input.up_visual(app.prompt_rect.width.saturating_sub(2).max(1) as usize),KeyCode::Down=>app.input.down_visual(app.prompt_rect.width.saturating_sub(2).max(1) as usize),KeyCode::Home=>app.input.home(),KeyCode::End=>app.input.end(),KeyCode::Backspace=>app.input.backspace(),KeyCode::Delete=>app.input.delete(),
                        KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL|KeyModifiers::SUPER)=>app.input.insert(&c.to_string()),_=>{}
                    }
                }
                _=>{}
            }},
            Some(message)=rx.recv()=>match message {UiMessage::Agent(AgentEvent::AssistantText(v))=>app.append_transcript(format!("WillDeep: {v}")),UiMessage::Agent(AgentEvent::ToolRequested(v))=>app.tools.requested(&v.name),UiMessage::Agent(AgentEvent::ToolCompleted{call,is_error,..})=>app.tools.completed(&call.name,is_error),UiMessage::Agent(_)=>{},UiMessage::Approval(v,s)=>app.approval=Some((v,s)),UiMessage::Finished(Ok(outcome))=>{session.messages=outcome.messages;store.save(session)?;app.running=false;}UiMessage::Finished(Err(e))=>{app.append_transcript(format!("Error: {e}"));app.running=false;}}
        }
    }
    Ok(())
}

impl App {
    fn new(transcript: Vec<String>) -> Self {
        Self {
            input: PromptEditor::default(),
            transcript,
            running: false,
            approval: None,
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
        let activity = if app.tools.requested == 0 {
            0
        } else if app.tools_expanded {
            8
        } else {
            3
        };
        let attach = if app.attachments.is_empty() { 0 } else { 3 };
        let input_width = f.area().width.saturating_sub(2).max(1) as usize;
        let input_lines = visual_lines(app.input.text(), input_width).clamp(1, 6);
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
            .split(f.area());
        let text = app.transcript.join("\n");
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        let max = visual_lines(&text, app.transcript_width).saturating_sub(app.viewport_height);
        app.scroll_from_bottom = app.scroll_from_bottom.min(max);
        let offset = max
            .saturating_sub(app.scroll_from_bottom)
            .min(u16::MAX as usize) as u16;
        let title = if app.follow_bottom {
            "WillDeep".to_owned()
        } else {
            format!("WillDeep · history ↑{}", app.scroll_from_bottom)
        };
        f.render_widget(
            Paragraph::new(text)
                .block(Block::default().title(title).borders(Borders::ALL))
                .scroll((offset, 0))
                .wrap(Wrap { trim: false }),
            areas[0],
        );
        if activity > 0 {
            let text = if app.tools_expanded {
                format!(
                    "{}\n{}",
                    app.tools.summary(),
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
            } else {
                app.tools.summary()
            };
            f.render_widget(
                Paragraph::new(text).block(
                    Block::default()
                        .title("Activity · Ctrl+O details")
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
                        .title("Attachments · Ctrl+D remove")
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
                        .title("Prompt · Shift/Alt+Enter newline")
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
            if app.running {
                "Agent running · PgUp/PgDn history · Ctrl+O tools · Ctrl-C exit".to_owned()
            } else {
                "Enter send · Shift/Alt+Enter newline · Ctrl+Shift+V image · Ctrl+D remove"
                    .to_owned()
            }
        });
        f.render_widget(Paragraph::new(status), areas[4]);
    })?;
    Ok(())
}

pub fn channel() -> (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
) {
    mpsc::unbounded_channel()
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_tools() {
        let mut a = ToolActivity::default();
        a.requested("read_file");
        a.completed("read_file", true);
        assert!(a.summary().contains("1 failed"));
    }
    #[test]
    fn long_paste_is_attachment_and_deletable() {
        let mut a = App::new(Vec::new());
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
}
