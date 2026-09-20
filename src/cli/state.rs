use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{style::Style, widgets::Block};
use ratatui_textarea::TextArea;

use super::Interrupted;

pub(super) struct State {
    pub(super) input: TextArea<'static>,
    // Only output not yet committed to terminal scrollback lives in the UI.
    pub(super) pending_output: String,
    pub(super) status: String,
}

pub(super) enum Action {
    Continue,
    Submit(String),
    Exit,
}

impl State {
    pub(super) fn new(configured: bool, setup_message: &str) -> Self {
        let mut state = Self {
            input: Self::input(),
            pending_output: String::new(),
            status: if configured {
                "Ready"
            } else {
                "Provider setup required"
            }
            .into(),
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

    pub(super) fn note(&mut self, text: &str) {
        if !self.pending_output.is_empty() && !self.pending_output.ends_with('\n') {
            self.pending_output.push('\n');
        }
        self.pending_output.push_str(text);
        self.pending_output.push('\n');
    }

    pub(super) fn handle(&mut self, event: Event, busy: bool) -> Result<Action> {
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
                    // History scrolling belongs to the terminal emulator.
                    KeyCode::PageUp | KeyCode::PageDown => {}
                    KeyCode::Enter if !busy => {
                        let text = self.input.lines().join("\n");
                        self.input = Self::input();
                        if text.trim() == "/exit" {
                            return Ok(Action::Exit);
                        }
                        if text.trim() == "/settings" {
                            self.note(&format!("you> {text}"));
                            self.note("Not yet implemented");
                            return Ok(Action::Continue);
                        }
                        if !text.trim().is_empty() {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

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
}
