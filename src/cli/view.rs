use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
    widgets::{List, ListItem, ListState, Paragraph, Widget, Wrap},
};

use super::{commands::Command, state::State};

const HEADER: u16 = 1;
const INPUT: u16 = 3;
const FOOTER: u16 = 1;
const MENU_MAX: u16 = 6;

pub(super) const MIN_HEIGHT: u16 = HEADER + INPUT + FOOTER;

pub(super) fn height(state: &State, size: Size) -> u16 {
    let transcript = if state.pending_output.is_empty() {
        0
    } else {
        transcript(state)
            .line_count(size.width)
            .min(u16::MAX.into()) as u16
    };
    (HEADER + INPUT + FOOTER)
        .saturating_add(transcript)
        .saturating_add(menu_rows(state))
        .max(MIN_HEIGHT)
        .min(size.height)
}

// An empty result set still needs its one row for "No matching commands".
fn menu_rows(state: &State) -> u16 {
    state.palette.as_ref().map_or(0, |palette| {
        palette.results.len().clamp(1, MENU_MAX.into()) as u16
    })
}

fn transcript(state: &State) -> Paragraph<'_> {
    Paragraph::new(state.pending_output.as_str()).wrap(Wrap { trim: false })
}

// Each complete line is emitted once, above the live UI. Ratatui scrolls excess
// rows into the terminal's own scrollback instead of overwriting old messages.
pub(super) fn flush_completed<B: Backend>(
    terminal: &mut Terminal<B>,
    pending: &mut String,
) -> Result<(), B::Error> {
    let width = terminal.get_frame().area().width;
    if width == 0 || terminal.get_frame().area().height == 0 {
        return Ok(());
    }
    while let Some(end) = pending.find('\n') {
        let paragraph = Paragraph::new(&pending[..end]).wrap(Wrap { trim: false });
        let rows = paragraph.line_count(width).max(1).min(u16::MAX.into()) as u16;
        terminal.insert_before(rows, |buffer| paragraph.render(buffer.area, buffer))?;
        pending.drain(..=end);
    }
    Ok(())
}

pub(super) fn render(state: &State, frame: &mut Frame) {
    // A clamped viewport gives the menu what is left after the fixed chrome.
    let menu_rows = menu_rows(state).min(frame.area().height.saturating_sub(MIN_HEIGHT));
    let areas = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(HEADER),
        Constraint::Length(menu_rows),
        Constraint::Length(INPUT),
        Constraint::Length(FOOTER),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!("hadaka-agent  |  {}", state.status))
            .style(Style::default().fg(Color::Cyan)),
        areas[1],
    );
    let transcript = transcript(state);
    let bottom = transcript
        .line_count(areas[0].width)
        .saturating_sub(usize::from(areas[0].height));
    let offset = bottom.min(u16::MAX as usize) as u16;
    frame.render_widget(transcript.scroll((offset, 0)), areas[0]);
    menu(state, frame, areas[2]);
    frame.render_widget(&state.input, areas[3]);
    frame.render_widget(
        Paragraph::new(if state.palette.is_some() {
            "↑/↓: select | Enter: execute | Esc: close"
        } else {
            "Enter: send | /exit, Ctrl-D: exit | Ctrl-C: interrupt"
        })
        .style(Style::default().fg(Color::DarkGray)),
        areas[4],
    );
}

fn menu(state: &State, frame: &mut Frame, area: Rect) {
    let Some(palette) = &state.palette else {
        return;
    };
    if area.is_empty() {
        return;
    }
    if palette.results.is_empty() {
        frame.render_widget(
            Paragraph::new("  No matching commands").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }
    let names = palette
        .results
        .iter()
        .map(|command| command.label().chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = palette
        .results
        .iter()
        .map(|command| ListItem::new(entry(command, area.width, names)))
        .collect();
    // A ListState scrolls its own rows to keep the highlighted command visible.
    let mut selection = ListState::default().with_selected(Some(palette.selected));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut selection,
    );
}

// Names keep their width; descriptions take what is left, if anything.
fn entry(command: &Command, width: u16, names: usize) -> String {
    let label = command.label();
    let room = usize::from(width).saturating_sub(names + 4);
    let description: String = command.description.chars().take(room).collect();
    format!("{label:<names$}  {description}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_follows_only_the_pending_output() {
        let mut state = State::new(true, "");
        assert_eq!(height(&state, Size::new(80, 40)), MIN_HEIGHT);
        state.note("one");
        state.note("two");
        assert_eq!(height(&state, Size::new(80, 40)), MIN_HEIGHT + 2);
        state.note(&"wrapped ".repeat(20));
        assert_eq!(height(&state, Size::new(80, 40)), MIN_HEIGHT + 4);
        for index in 0..100 {
            state.note(&index.to_string());
        }
        assert_eq!(height(&state, Size::new(80, 40)), 40);
        assert_eq!(height(&state, Size::new(80, 3)), 3);
        state.pending_output.clear();
        assert_eq!(height(&state, Size::new(80, 40)), MIN_HEIGHT);
    }

    #[test]
    fn messages_one_through_eight_are_preserved_in_terminal_scrollback() {
        use ratatui::{TerminalOptions, Viewport, backend::TestBackend};
        let mut terminal = Terminal::with_options(
            TestBackend::new(40, 12),
            TerminalOptions {
                viewport: Viewport::Inline(MIN_HEIGHT),
            },
        )
        .unwrap();
        let mut state = State::new(true, "");
        let mut expected = Vec::new();
        for number in 1..=8 {
            let message = format!("you> {number}");
            state.note(&message);
            state.note("Configure a provider first");
            expected.push(message);
            expected.push("Configure a provider first".into());
            flush_completed(&mut terminal, &mut state.pending_output).unwrap();
            terminal.draw(|frame| render(&state, frame)).unwrap();
            assert!(state.pending_output.is_empty());
        }
        terminal
            .backend()
            .assert_scrollback_lines(expected[..9].iter().map(|line| format!("{line:<40}")));
        for (row, line) in expected[9..].iter().enumerate() {
            let visible: String = terminal.backend().buffer().content[row * 40..(row + 1) * 40]
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert_eq!(visible.trim_end(), line);
        }
    }

    #[test]
    fn partial_stream_is_committed_once_when_its_line_finishes() {
        use ratatui::{TerminalOptions, Viewport, backend::TestBackend};
        let mut terminal = Terminal::with_options(
            TestBackend::new(8, 10),
            TerminalOptions {
                viewport: Viewport::Inline(MIN_HEIGHT),
            },
        )
        .unwrap();
        let mut pending = "abcdefgh".to_string();
        flush_completed(&mut terminal, &mut pending).unwrap();
        assert_eq!(terminal.get_frame().area().y, 0);
        pending.push_str("ijklmnop\nnext");
        flush_completed(&mut terminal, &mut pending).unwrap();
        assert_eq!(pending, "next");
        assert_eq!(terminal.get_frame().area().y, 2);
        let before = terminal.backend().buffer().clone();
        flush_completed(&mut terminal, &mut pending).unwrap();
        assert_eq!(terminal.backend().buffer(), &before);
        let output: String = before.content[..16]
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(output, "abcdefghijklmnop");
    }

    fn screen(width: u16, height: u16, state: &State) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(state, frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn palette(query: &str) -> State {
        let mut state = State::new(true, "");
        for character in query.chars() {
            state
                .handle(
                    ratatui::crossterm::event::Event::Key(
                        ratatui::crossterm::event::KeyEvent::new(
                            ratatui::crossterm::event::KeyCode::Char(character),
                            ratatui::crossterm::event::KeyModifiers::NONE,
                        ),
                    ),
                    false,
                )
                .unwrap();
        }
        state
    }

    #[test]
    fn menu_marks_the_selection_and_grows_with_the_matches() {
        let state = palette("/");
        assert_eq!(height(&state, Size::new(60, 40)), MIN_HEIGHT + 2);
        let rows = screen(60, MIN_HEIGHT + 2, &state).join("\n");
        assert!(rows.contains("› /settings  Configure providers"), "{rows}");
        assert!(
            rows.contains("  /exit      Exit the chat session"),
            "{rows}"
        );
        assert!(rows.contains("↑/↓: select"), "{rows}");

        let state = palette("/set");
        assert_eq!(
            height(&state, Size::new(60, 40)),
            MIN_HEIGHT + 1,
            "the menu shrinks with the matches"
        );
    }

    #[test]
    fn menu_reports_an_empty_result_set() {
        let state = palette("/zzz");
        assert_eq!(height(&state, Size::new(60, 40)), MIN_HEIGHT + 1);
        let rows = screen(60, MIN_HEIGHT + 1, &state).join("\n");
        assert!(rows.contains("No matching commands"), "{rows}");
    }

    #[test]
    fn menu_keeps_the_input_and_the_selection_visible_on_a_small_terminal() {
        let mut state = palette("/");
        state.palette.as_mut().unwrap().selected = 1;
        // One row short of showing both commands, so the list must scroll.
        let rows = screen(30, MIN_HEIGHT + 1, &state);
        let screen = rows.join("\n");
        assert!(screen.contains("› /exit"), "{screen}");
        assert!(!screen.contains("/settings"), "{screen}");
        assert!(screen.contains("you>"), "the input survives the squeeze");
    }

    #[test]
    fn renders_setup_guidance_and_input_at_small_sizes() {
        for (width, height) in [(80, 16), (20, 8), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(&State::new(false, "Configure a provider first"), frame))
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
