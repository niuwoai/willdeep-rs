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
    };
    let mut events = EventStream::new();
    loop {
        draw(terminal, &app)?;
        tokio::select! {
            event = events.next() => if let Some(Ok(Event::Key(key))) = event && key.kind == KeyEventKind::Press {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') { break; }
                if let Some((_, sender)) = app.approval.take() {
                    let allow = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
                    let _ = sender.send(allow);
                    continue;
                }
                match key.code {
                    KeyCode::Enter if !app.running && !app.input.trim().is_empty() => {
                        let prompt = std::mem::take(&mut app.input);
                        app.transcript.push(format!("You: {prompt}")); app.running = true;
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
                UiMessage::Agent(AgentEvent::AssistantText(text)) => app.transcript.push(format!("WillDeep: {text}")),
                UiMessage::Agent(AgentEvent::ToolRequested(call)) => app.transcript.push(format!("[tool] {}", call.name)),
                UiMessage::Agent(AgentEvent::ToolCompleted { call, is_error, .. }) => app.transcript.push(format!("[tool:{}] {}", if is_error {"error"} else {"done"}, call.name)),
                UiMessage::Agent(_) => {},
                UiMessage::Approval(description, sender) => app.approval = Some((description, sender)),
                UiMessage::Finished(Ok(outcome)) => { session.messages = outcome.messages; store.save(session)?; app.running = false; }
                UiMessage::Finished(Err(error)) => { app.transcript.push(format!("Error: {error}")); app.running = false; }
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

fn draw(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &App) -> Result<()> {
    terminal.draw(|frame| {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());
        let text = app
            .transcript
            .iter()
            .rev()
            .take(200)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::default().title("WillDeep").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            areas[0],
        );
        frame.render_widget(
            Paragraph::new(app.input.as_str())
                .block(Block::default().title("Prompt").borders(Borders::ALL)),
            areas[1],
        );
        let status = if let Some((description, _)) = &app.approval {
            format!("Approval: {description} — press y to allow, any other key to deny")
        } else if app.running {
            "Agent running… Ctrl-C to exit".to_owned()
        } else {
            "Enter to send · Ctrl-C to exit".to_owned()
        };
        frame.render_widget(Paragraph::new(status), areas[2]);
    })?;
    Ok(())
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
