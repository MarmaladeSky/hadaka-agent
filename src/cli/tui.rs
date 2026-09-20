use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    terminal::{Clear, ClearType, disable_raw_mode},
};
use ratatui::{
    DefaultTerminal, Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::{
    state::{Action, State},
    stream::{Output, StreamWriter, apply_output},
    view::{self, render},
};
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
    area: Rect,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        let mut session = Self {
            terminal: None,
            area: Rect::ZERO,
        };
        let mut terminal = ratatui::try_init_with_options(TerminalOptions {
            viewport: Viewport::Inline(view::MIN_HEIGHT),
        })?;
        session.area = terminal.get_frame().area();
        session.terminal = Some(terminal);
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(session)
    }

    fn draw(&mut self, state: &mut State) -> Result<()> {
        let terminal = self.terminal.as_mut().expect("terminal initialized");
        terminal.autoresize()?;
        view::flush_completed(terminal, &mut state.pending_output)?;
        self.area = terminal.get_frame().area();
        let size = self
            .terminal
            .as_ref()
            .expect("terminal initialized")
            .size()?;
        let height = view::height(state, size);
        if height != self.area.height {
            self.grow(height)?;
        }
        let terminal = self.terminal.as_mut().expect("terminal initialized");
        terminal.draw(|frame| render(state, frame))?;
        // A completed frame reports the whole terminal; only a Frame knows the viewport.
        self.area = terminal.get_frame().area();
        Ok(())
    }

    // Ratatui fixes the inline height at construction, so following the content
    // means clearing the old viewport and anchoring a new one at its origin.
    fn grow(&mut self, height: u16) -> Result<()> {
        execute!(
            io::stdout(),
            MoveTo(0, self.area.top()),
            Clear(ClearType::FromCursorDown)
        )?;
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        self.area = terminal.get_frame().area();
        self.terminal = Some(terminal);
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
            MoveTo(0, self.area.bottom().saturating_sub(1))
        );
        let _ = writeln!(io::stdout());
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
        terminal.draw(&mut state)?;
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
                terminal.draw(&mut state)?;
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
                        _ = redraw.tick() => terminal.draw(&mut state)?,
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
                if !state.pending_output.is_empty() && !state.pending_output.ends_with('\n') {
                    state.pending_output.push('\n');
                }
                terminal.draw(&mut state)?;
                result?;
            }
        }
    }
}
