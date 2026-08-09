use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::{mpsc, oneshot};
use unicode_width::UnicodeWidthChar;
use willdeep_core::{Agent, AgentEvent, Approver, EventSink, Session, SessionStore};

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
    input: String,
    transcript: Vec<String>,
    running: bool,
    approval: Option<(String, oneshot::Sender<bool>)>,
    scroll_from_bottom: usize,
    follow_bottom: bool,
    transcript_width: usize,
    viewport_height: usize,
    tools: ToolActivity,
    tools_expanded: bool,
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
        if let Some(detail) = self
            .details
            .iter_mut()
            .rev()
            .find(|line| line.as_str() == format!("… {name}"))
        {
            *detail = format!("{} {name}", if is_error { "✗" } else { "✓" });
        }
    }
    fn summary(&self) -> String {
        if self.requested == 0 {
            return "No tool activity".to_owned();
        }
        let calls = self
            .counts
            .iter()
            .map(|(name, count)| format!("{name}×{count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let progress = if self.completed < self.requested {
            format!("{}/{} complete", self.completed, self.requested)
        } else {
            format!("{} calls", self.requested)
        };
        let failure = if self.failed == 0 {
            String::new()
        } else {
            format!(" · {} failed", self.failed)
        };
        format!("Tools: {progress}{failure} · {calls}")
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
    execute!(stdout, terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = event_loop(&mut terminal, agent, &mut session, &store, tx, &mut rx).await;
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    agent: Arc<Agent>,
    session: &mut Session,
    store: &SessionStore,
    tx: mpsc::UnboundedSender<UiMessage>,
    rx: &mut mpsc::UnboundedReceiver<UiMessage>,
) -> Result<()> {
    let mut app = App {
        input: String::new(),
        transcript: transcript(&session.messages),
        running: false,
        approval: None,
        scroll_from_bottom: 0,
        follow_bottom: true,
        transcript_width: 78,
        viewport_height: 10,
        tools: ToolActivity::default(),
        tools_expanded: false,
    };
    let mut events = EventStream::new();
    loop {
        draw(terminal, &mut app)?;
        tokio::select! {
            event = events.next() => if let Some(Ok(Event::Key(key))) = event && key.kind == KeyEventKind::Press {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') { break; }
                if let Some((_, sender)) = app.approval.take() {
                    let allow = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
                    let _ = sender.send(allow);
                    continue;
                }
                match key.code {
                    KeyCode::Up => app.scroll_up(1),
                    KeyCode::Down => app.scroll_down(1),
                    KeyCode::PageUp => app.scroll_up(app.viewport_height.saturating_sub(1).max(1)),
                    KeyCode::PageDown => app.scroll_down(app.viewport_height.saturating_sub(1).max(1)),
                    KeyCode::Home => app.scroll_to_top(),
                    KeyCode::End => app.scroll_to_bottom(),
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => app.tools_expanded = !app.tools_expanded,
                    KeyCode::Enter if !app.running && !app.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut app.input);
                        app.append_transcript(format!("You: {prompt}")); app.running = true; app.tools.reset();
                        let history = session.messages.clone();
                        session.messages.push(willdeep_core::Message::user(&prompt));
                        store.save(session)?;
                        let agent = agent.clone();
                        let tx = tx.clone();
                        // The agent's own sink delivers detailed events; this task only reports completion.
                        tokio::spawn(async move { let _ = tx.send(UiMessage::Finished(agent.run_with_history(history, prompt).await)); });
                    }
                    KeyCode::Backspace => { app.input.pop(); }
                    KeyCode::Char(c) => app.input.push(c),
                    _ => {}
                }
            },
            Some(message) = rx.recv() => match message {
                UiMessage::Agent(AgentEvent::AssistantText(text)) => app.append_transcript(format!("WillDeep: {text}")),
                UiMessage::Agent(AgentEvent::ToolRequested(call)) => app.tools.requested(&call.name),
                UiMessage::Agent(AgentEvent::ToolCompleted { call, is_error, .. }) => app.tools.completed(&call.name, is_error),
                UiMessage::Agent(_) => {},
                UiMessage::Approval(description, sender) => app.approval = Some((description, sender)),
                UiMessage::Finished(Ok(outcome)) => { session.messages = outcome.messages; store.save(session)?; app.running = false; }
                UiMessage::Finished(Err(error)) => { app.append_transcript(format!("Error: {error}")); app.running = false; }
            }
        }
    }
    Ok(())
}

pub fn channel() -> (
    mpsc::UnboundedSender<UiMessage>,
    mpsc::UnboundedReceiver<UiMessage>,
) {
    mpsc::unbounded_channel()
}

impl App {
    fn max_scroll(&self) -> usize {
        visual_lines(&self.transcript.join("\n"), self.transcript_width)
            .saturating_sub(self.viewport_height)
    }
    fn scroll_up(&mut self, amount: usize) {
        let max_scroll = self.max_scroll();
        if max_scroll == 0 {
            self.scroll_to_bottom();
            return;
        }
        self.follow_bottom = false;
        self.scroll_from_bottom = self
            .scroll_from_bottom
            .saturating_add(amount)
            .min(max_scroll);
    }
    fn scroll_down(&mut self, amount: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(amount);
        if self.scroll_from_bottom == 0 {
            self.follow_bottom = true;
        }
    }
    fn scroll_to_top(&mut self) {
        let max_scroll = self.max_scroll();
        self.follow_bottom = max_scroll == 0;
        self.scroll_from_bottom = max_scroll;
    }
    fn scroll_to_bottom(&mut self) {
        self.follow_bottom = true;
        self.scroll_from_bottom = 0;
    }
    fn append_transcript(&mut self, value: String) {
        if !self.follow_bottom {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(visual_lines(&value, self.transcript_width));
        }
        self.transcript.push(value);
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
    }
}

fn draw(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    terminal.draw(|frame| {
        let activity_height = if app.tools.requested == 0 {
            0
        } else if app.tools_expanded {
            8
        } else {
            3
        };
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(activity_height),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());
        let text = app.transcript.join("\n");
        app.transcript_width = areas[0].width.saturating_sub(2).max(1) as usize;
        app.viewport_height = areas[0].height.saturating_sub(2).max(1) as usize;
        let total_lines = visual_lines(&text, app.transcript_width);
        let max_scroll = total_lines.saturating_sub(app.viewport_height);
        app.scroll_from_bottom = app.scroll_from_bottom.min(max_scroll);
        let offset = max_scroll
            .saturating_sub(app.scroll_from_bottom)
            .min(u16::MAX as usize) as u16;
        let title = if app.follow_bottom {
            "WillDeep".to_owned()
        } else {
            format!("WillDeep · history ↑{}", app.scroll_from_bottom)
        };
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::default().title(title).borders(Borders::ALL))
                .scroll((offset, 0))
                .wrap(Wrap { trim: false }),
            areas[0],
        );
        if activity_height > 0 {
            let activity = if app.tools_expanded {
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
            frame.render_widget(
                Paragraph::new(activity)
                    .block(
                        Block::default()
                            .title("Activity · Ctrl+O details")
                            .borders(Borders::ALL),
                    )
                    .wrap(Wrap { trim: true }),
                areas[1],
            );
        }
        frame.render_widget(
            Paragraph::new(app.input.as_str())
                .block(Block::default().title("Prompt").borders(Borders::ALL)),
            areas[2],
        );
        let status = if let Some((description, _)) = &app.approval {
            format!("Approval: {description} — press y to allow, any other key to deny")
        } else if app.running {
            "Agent running… ↑/↓ scroll · PgUp/PgDn · End follow · Ctrl+O tools · Ctrl-C exit"
                .to_owned()
        } else {
            "Enter send · ↑/↓ scroll · PgUp/PgDn · End follow · Ctrl+O tools · Ctrl-C exit"
                .to_owned()
        };
        frame.render_widget(Paragraph::new(status), areas[3]);
    })?;
    Ok(())
}

fn visual_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.split('\n')
        .map(|line| {
            let columns = line
                .chars()
                .map(|character| UnicodeWidthChar::width(character).unwrap_or(0))
                .sum::<usize>();
            columns.max(1).div_ceil(width)
        })
        .sum()
}
fn transcript(messages: &[willdeep_core::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m.role {
            willdeep_core::Role::User => Some(format!("You: {}", m.content)),
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
    fn visual_lines_accounts_for_wrapping_and_cjk_width() {
        assert_eq!(visual_lines("abcd", 4), 1);
        assert_eq!(visual_lines("abcde", 4), 2);
        assert_eq!(visual_lines("中文", 2), 2);
        assert_eq!(visual_lines("one\ntwo", 20), 2);
    }

    #[test]
    fn tool_activity_is_aggregated_and_reports_failures() {
        let mut activity = ToolActivity::default();
        activity.requested("read_file");
        activity.completed("read_file", false);
        activity.requested("read_file");
        activity.completed("read_file", true);
        let summary = activity.summary();
        assert!(summary.contains("read_file×2"));
        assert!(summary.contains("1 failed"));
        assert_eq!(activity.details, ["✓ read_file", "✗ read_file"]);
    }

    #[test]
    fn short_transcript_does_not_enter_fake_history_mode() {
        let mut app = App {
            input: String::new(),
            transcript: vec!["short".to_owned()],
            running: false,
            approval: None,
            scroll_from_bottom: 0,
            follow_bottom: true,
            transcript_width: 80,
            viewport_height: 20,
            tools: ToolActivity::default(),
            tools_expanded: false,
        };
        app.scroll_up(10);
        assert!(app.follow_bottom);
        assert_eq!(app.scroll_from_bottom, 0);
    }
}
