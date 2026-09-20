use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Layout, Size},
    style::{Color, Style},
    widgets::{Paragraph, Widget, Wrap},
};

use super::state::State;

const HEADER: u16 = 1;
const INPUT: u16 = 3;
const FOOTER: u16 = 1;

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
        .max(MIN_HEIGHT)
        .min(size.height)
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
    let areas = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(HEADER),
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
    frame.render_widget(&state.input, areas[2]);
    frame.render_widget(
        Paragraph::new("Enter: send | Scroll: terminal | /exit, Ctrl-D: exit | Ctrl-C: interrupt")
            .style(Style::default().fg(Color::DarkGray)),
        areas[3],
    );
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
