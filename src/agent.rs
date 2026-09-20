use std::io::Write;

use anyhow::{Result, bail, ensure};

use crate::{
    model::{DeepSeek, Message},
    tools::Tools,
};

pub struct Agent {
    model: DeepSeek,
    system_prompt: String,
    max_turns: usize,
}

impl Agent {
    pub fn new(model: DeepSeek, system_prompt: String, max_turns: usize) -> Self {
        Self {
            model,
            system_prompt,
            max_turns,
        }
    }

    pub async fn run(&mut self, task: &str, tools: &Tools, output: &mut impl Write) -> Result<()> {
        ensure!(!task.trim().is_empty(), "task must not be empty");
        let mut messages = vec![
            Message::text("system", &self.system_prompt),
            Message::text("user", task),
        ];
        for _ in 0..self.max_turns {
            let response = self
                .model
                .respond(&messages, tools.definitions(), output)
                .await?;
            if !response.content.is_empty() {
                writeln!(output)?;
                output.flush()?;
            }
            let done = response.tool_calls.is_empty();
            messages.push(response);
            if done {
                return Ok(());
            }

            let mut results = Vec::new();
            for call in &messages
                .last()
                .expect("assistant message was just appended")
                .tool_calls
            {
                eprintln!("tool: {}", call.function.name);
                let mut result = Message::text("tool", tools.call(call).await);
                result.tool_call_id = Some(call.id.clone());
                results.push(result);
            }
            messages.extend(results);
        }
        bail!("model turn limit reached ({})", self.max_turns)
    }
}
