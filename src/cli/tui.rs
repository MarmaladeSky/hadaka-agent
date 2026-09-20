use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    cursor::{MoveTo, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::disable_raw_mode,
};
use ratatui::{
    DefaultTerminal, Frame, TerminalOptions, Viewport,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Paragraph, Wrap},
};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::Interrupted;
use crate::{agent::Agent, tools::Tools};

// Keep polling and inline cursor-position queries on the same thread. An
// EventStream background reader can hold the input lock during a resize query.
struct TerminalEvents(tokio::time::Interval);

impl TerminalEvents {
    fn new() -> Self {
        Self(tokio::time::interval(Duration::from_millis(16)))
    }

    async fn next(&mut self) -> Result<Event> {
        loop {
            self.0.tick().await;
            if event::poll(Duration::ZERO)? {
                return Ok(event::read()?);
            }
        }
    }
}

// This guard also runs when main's Ctrl-C select cancels the UI future.
struct TerminalSession {
    terminal: Option<DefaultTerminal>,
    bottom: u16,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        let mut session = Self {
            terminal: None,
            bottom: 0,
        };
        session.terminal = Some(ratatui::try_init_with_options(TerminalOptions {
            viewport: Viewport::Inline(16),
        })?);
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(session)
    }

    fn draw(&mut self, state: &State) -> Result<()> {
        let frame = self
            .terminal
            .as_mut()
            .expect("terminal initialized")
            .draw(|frame| state.render(frame))?;
        self.bottom = frame.area.bottom().saturating_sub(1);
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            Show,
            MoveTo(0, self.bottom)
        );
        let _ = writeln!(io::stdout());
    }
}

struct State {
    input: TextArea<'static>,
    transcript: String,
    status: String,
    scroll_back: usize,
}

enum Action {
    Continue,
    Submit(String),
    Exit,
}

impl State {
    fn new(configured: bool, setup_message: &str) -> Self {
        let mut state = Self {
            input: Self::input(),
            transcript: String::new(),
            status: if configured {
                "Ready"
            } else {
                "Provider setup required"
            }
            .into(),
            scroll_back: 0,
        };
        if !configured {
            state.note(setup_message);
        }
        state
    }

    fn input() -> TextArea<'static> {
        let mut input = TextArea::default();
        input.set_block(Block::bordered().title(" you> "));
        input.set_cursor_line_style(Style::default());
        input.set_placeholder_text("Type a message or /exit");
        input
    }

    fn note(&mut self, text: &str) {
        if !self.transcript.is_empty() && !self.transcript.ends_with('\n') {
            self.transcript.push('\n');
        }
        self.transcript.push_str(text);
        self.transcript.push('\n');
    }

    fn handle(&mut self, event: Event, busy: bool) -> Result<Action> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => return Err(Interrupted.into()),
                        KeyCode::Char('d') if self.input.lines().iter().all(String::is_empty) => {
                            return Ok(Action::Exit);
                        }
                        _ => {}
                    }
                }
                match key.code {
                    KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(5),
                    KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(5),
                    KeyCode::Enter if !busy => {
                        let text = self.input.lines().join("\n");
                        self.input = Self::input();
                        if text.trim() == "/exit" {
                            return Ok(Action::Exit);
                        }
                        if !text.trim().is_empty() {
                            self.scroll_back = 0;
                            self.note(&format!("you> {text}"));
                            return Ok(Action::Submit(text));
                        }
                    }
                    _ if !busy => {
                        self.input.input(key);
                    }
                    _ => {}
                }
            }
            Event::Paste(text) if !busy => {
                // A paste inserts text; only an explicit Enter submits it.
                self.input.insert_str(text.replace(['\r', '\n'], " "));
            }
            _ => {}
        }
        Ok(Action::Continue)
    }

    fn render(&self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());
        frame.render_widget(
            Paragraph::new(format!("hadaka-agent  |  {}", self.status))
                .style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let transcript = Paragraph::new(self.transcript.as_str()).wrap(Wrap { trim: false });
        let bottom = transcript
            .line_count(areas[1].width)
            .saturating_sub(usize::from(areas[1].height));
        let offset = bottom
            .saturating_sub(self.scroll_back)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(transcript.scroll((offset, 0)), areas[1]);
        frame.render_widget(&self.input, areas[2]);
        frame.render_widget(
            Paragraph::new(
                "Enter: send | PgUp/PgDn: scroll | /exit, Ctrl-D: exit | Ctrl-C: interrupt",
            )
            .style(Style::default().fg(Color::DarkGray)),
            areas[3],
        );
    }
}

enum Output {
    Text(String),
    Tool(String),
}

struct StreamWriter(UnboundedSender<Output>);

impl Write for StreamWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.0
            .send(Output::Text(text.into()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console closed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn apply_output(state: &mut State, output: Output) {
    match output {
        Output::Text(text) => state.transcript.push_str(&text),
        Output::Tool(name) => {
            state.status = format!("Running tool: {name}");
            state.note(&format!("tool: {name}"));
        }
    }
}

pub async fn run(
    mut agent: Option<Agent>,
    tools: &Tools,
    setup_message: &str,
    mut diagnostics: UnboundedReceiver<String>,
) -> Result<()> {
    let mut terminal = TerminalSession::new()?;
    let mut state = State::new(agent.is_some(), setup_message);
    let mut events = TerminalEvents::new();
    loop {
        terminal.draw(&state)?;
        let event = tokio::select! {
            Some(message) = diagnostics.recv() => { state.note(&message); continue; }
            event = events.next() => event?,
        };
        match state.handle(event, false)? {
            Action::Continue => {}
            Action::Exit => return Ok(()),
            Action::Submit(task) => {
                let Some(agent) = &mut agent else {
                    state.note(setup_message);
                    continue;
                };
                state.status = "Responding…".into();
                state.note("assistant>");
                terminal.draw(&state)?;
                let (sender, mut output) = mpsc::unbounded_channel();
                let mut writer = StreamWriter(sender.clone());
                let response = agent.run_with_status(&task, tools, &mut writer, |name| {
                    let _ = sender.send(Output::Tool(name.into()));
                });
                tokio::pin!(response);
                let mut redraw = tokio::time::interval(Duration::from_millis(33));
                let result = loop {
                    tokio::select! {
                        result = &mut response => break result,
                        Some(chunk) = output.recv() => apply_output(&mut state, chunk),
                        Some(message) = diagnostics.recv() => state.note(&message),
                        _ = redraw.tick() => terminal.draw(&state)?,
                        event = events.next() => {
                            if matches!(state.handle(event?, true)?, Action::Exit) {
                                return Ok(());
                            }
                        }
                    }
                };
                // The response can complete before the receiver consumes its last chunks.
                while let Ok(chunk) = output.try_recv() {
                    apply_output(&mut state, chunk);
                }
                state.status = if result.is_ok() {
                    "Ready"
                } else {
                    "Response failed"
                }
                .into();
                terminal.draw(&state)?;
                result?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn streamed_text_and_tool_status_reach_the_transcript_in_order() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut writer = StreamWriter(sender.clone());
        writer.write_all("Hello, 世界!".as_bytes()).unwrap();
        writer.flush().unwrap();
        sender.send(Output::Tool("echo".into())).unwrap();
        writer.write_all(b"Done.\n").unwrap();
        let mut state = State::new(true, "");
        while let Ok(chunk) = receiver.try_recv() {
            apply_output(&mut state, chunk);
        }
        assert_eq!(state.transcript, "Hello, 世界!\ntool: echo\nDone.\n");
        assert_eq!(state.status, "Running tool: echo");
    }

    #[test]
    fn busy_console_rejects_edits_but_accepts_interrupts() {
        let mut state = State::new(true, "");
        state.handle(Event::Paste("ignored".into()), true).unwrap();
        let action = state
            .handle(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                true,
            )
            .unwrap();
        assert!(matches!(action, Action::Continue));
        assert_eq!(state.input.lines(), &[""]);
        let error = state
            .handle(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                true,
            )
            .err()
            .unwrap();
        assert!(error.is::<Interrupted>());
    }

    #[test]
    fn editing_paste_and_submission_preserve_unicode() {
        let mut state = State::new(false, "Configure a provider first");
        state
            .handle(Event::Paste("héllo\nworld".into()), false)
            .unwrap();
        state
            .handle(
                Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
                false,
            )
            .unwrap();
        state
            .handle(
                Event::Key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)),
                false,
            )
            .unwrap();
        let action = state
            .handle(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                false,
            )
            .unwrap();
        assert!(matches!(action, Action::Submit(text) if text == "héllo worl!d"));
        assert_eq!(state.input.lines(), &[""]);
    }

    #[test]
    fn renders_setup_guidance_and_input_at_small_sizes() {
        for (width, height) in [(80, 16), (20, 8), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| State::new(false, "Configure a provider first").render(frame))
                .unwrap();
            if width == 80 {
                let screen: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(screen.contains("Configure a provider first"));
                assert!(screen.contains("you>"));
            }
        }
    }
}
