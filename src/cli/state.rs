use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{style::Style, widgets::Block};
use ratatui_textarea::TextArea;

use super::{
    Interrupted,
    commands::{self, Command, CommandId},
};

pub(super) struct State {
    pub(super) input: TextArea<'static>,
    // Only output not yet committed to terminal scrollback lives in the UI.
    pub(super) pending_output: String,
    pub(super) status: String,
    pub(super) palette: Option<Palette>,
    // Esc holds the palette shut until the input changes again.
    dismissed: bool,
}

pub(super) struct Palette {
    pub(super) results: Vec<&'static Command>,
    pub(super) selected: usize,
}

pub(super) enum Action {
    Continue,
    Submit(String),
    Command(CommandId),
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
            palette: None,
            dismissed: false,
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

    fn text(&self) -> String {
        self.input.lines().join("\n")
    }

    // The palette mirrors the input: it follows every edit and closes with it.
    fn edited(&mut self) {
        self.dismissed = false;
        self.palette = self.text().strip_prefix('/').map(|query| Palette {
            results: commands::search(query),
            selected: 0,
        });
    }

    fn clear(&mut self) {
        self.input = Self::input();
        self.palette = None;
        self.dismissed = false;
    }

    fn execute(&mut self, command: &Command) -> Action {
        let label = command.label();
        self.clear();
        self.note(&format!("you> {label}"));
        Action::Command(command.id)
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
                    KeyCode::Esc if !busy => {
                        self.palette = None;
                        self.dismissed = true;
                    }
                    KeyCode::Down | KeyCode::Up if !busy && self.palette.is_some() => {
                        self.move_selection(key.code == KeyCode::Down);
                    }
                    KeyCode::Enter if !busy => return Ok(self.submit()),
                    _ if !busy => {
                        self.input.input(key);
                        self.edited();
                    }
                    _ => {}
                }
            }
            Event::Paste(text) if !busy => {
                // A paste inserts text; only an explicit Enter submits it.
                self.input.insert_str(text.replace(['\r', '\n'], " "));
                self.edited();
            }
            _ => {}
        }
        Ok(Action::Continue)
    }

    fn move_selection(&mut self, forward: bool) {
        if let Some(palette) = &mut self.palette {
            let last = palette.results.len().saturating_sub(1);
            palette.selected = if forward {
                palette.selected.saturating_add(1).min(last)
            } else {
                palette.selected.saturating_sub(1)
            };
        }
    }

    fn submit(&mut self) -> Action {
        if let Some(palette) = &self.palette {
            // Without a match the query stays on screen for further editing.
            let Some(command) = palette.results.get(palette.selected).copied() else {
                return Action::Continue;
            };
            return self.execute(command);
        }
        let text = self.text();
        if let Some(command) = commands::find(&text) {
            return self.execute(command);
        }
        let trimmed = text.trim().to_owned();
        self.clear();
        if trimmed.starts_with('/') {
            self.note(&format!("you> {trimmed}"));
            self.note(&format!("Unknown command: {trimmed}"));
            return Action::Continue;
        }
        if trimmed.is_empty() {
            return Action::Continue;
        }
        self.note(&format!("you> {text}"));
        Action::Submit(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn press(state: &mut State, code: KeyCode) -> Action {
        state
            .handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), false)
            .unwrap()
    }

    fn type_text(state: &mut State, text: &str) {
        for character in text.chars() {
            press(state, KeyCode::Char(character));
        }
    }

    fn results(state: &State) -> Vec<String> {
        state.palette.as_ref().map_or_else(Vec::new, |palette| {
            palette
                .results
                .iter()
                .map(|command| command.label())
                .collect()
        })
    }

    #[test]
    fn palette_opens_filters_dismisses_and_reopens_on_the_next_edit() {
        let mut state = State::new(true, "");
        assert!(state.palette.is_none());
        type_text(&mut state, "/");
        assert_eq!(results(&state), ["/settings", "/exit"]);
        type_text(&mut state, "set");
        assert_eq!(results(&state), ["/settings"]);
        press(&mut state, KeyCode::Esc);
        assert!(state.palette.is_none());
        assert_eq!(state.text(), "/set", "Esc retains the typed input");
        type_text(&mut state, "t");
        assert_eq!(
            results(&state),
            ["/settings"],
            "an edit reopens the palette"
        );
        for _ in 0..5 {
            press(&mut state, KeyCode::Backspace);
        }
        assert_eq!(state.text(), "");
        assert!(state.palette.is_none(), "input no longer starts with /");
    }

    #[test]
    fn selection_moves_within_the_results_and_stops_at_both_ends() {
        let mut state = State::new(true, "");
        type_text(&mut state, "/");
        assert_eq!(state.palette.as_ref().unwrap().selected, 0);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.palette.as_ref().unwrap().selected, 0);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.palette.as_ref().unwrap().selected, 1);
        type_text(&mut state, "e");
        assert_eq!(
            state.palette.as_ref().unwrap().selected,
            0,
            "a changed query resets the highlight"
        );
    }

    #[test]
    fn enter_executes_the_highlighted_command_rather_than_chatting() {
        let mut state = State::new(true, "");
        type_text(&mut state, "/s");
        press(&mut state, KeyCode::Down);
        let action = press(&mut state, KeyCode::Enter);
        assert!(matches!(action, Action::Command(CommandId::Exit)));
        assert_eq!(state.text(), "", "the query is cleared");
        assert!(state.palette.is_none());
        assert!(
            state.pending_output.contains("you> /exit"),
            "the canonical command is recorded, not the query: {}",
            state.pending_output
        );
    }

    #[test]
    fn enter_without_matches_keeps_the_query_and_the_palette() {
        let mut state = State::new(true, "");
        type_text(&mut state, "/zzz");
        assert!(results(&state).is_empty());
        let action = press(&mut state, KeyCode::Enter);
        assert!(matches!(action, Action::Continue));
        assert_eq!(state.text(), "/zzz");
        assert!(state.palette.is_some());
    }

    #[test]
    fn a_dismissed_unknown_command_is_reported_without_reaching_the_model() {
        let mut state = State::new(true, "");
        type_text(&mut state, "/zzz");
        press(&mut state, KeyCode::Esc);
        let action = press(&mut state, KeyCode::Enter);
        assert!(matches!(action, Action::Continue));
        assert!(state.pending_output.contains("Unknown command: /zzz"));
        assert_eq!(state.text(), "");
    }

    #[test]
    fn a_dismissed_exact_command_still_executes_and_plain_text_still_chats() {
        let mut state = State::new(false, "Configure a provider first");
        type_text(&mut state, "/settings");
        press(&mut state, KeyCode::Esc);
        let action = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(action, Action::Command(CommandId::Settings)),
            "/settings does not need an enabled provider"
        );
        type_text(&mut state, "hello");
        let action = press(&mut state, KeyCode::Enter);
        assert!(matches!(action, Action::Submit(text) if text == "hello"));
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
}
