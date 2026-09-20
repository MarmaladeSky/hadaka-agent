use std::io::{self, Write};

use tokio::sync::mpsc::UnboundedSender;

use super::state::State;

pub(super) enum Output {
    Text(String),
    Tool(String),
}

pub(super) struct StreamWriter(pub(super) UnboundedSender<Output>);

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

pub(super) fn apply_output(state: &mut State, output: Output) {
    match output {
        Output::Text(text) => state.pending_output.push_str(&text),
        Output::Tool(name) => {
            state.status = format!("Running tool: {name}");
            state.note(&format!("tool: {name}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

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
        assert_eq!(state.pending_output, "Hello, 世界!\ntool: echo\nDone.\n");
        assert_eq!(state.status, "Running tool: echo");
    }
}
