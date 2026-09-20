use std::io::Write;

use anyhow::{Result, bail, ensure};

use crate::{
    model::{DeepSeek, Message},
    tools::Tools,
};

pub struct Agent {
    model: DeepSeek,
    messages: Vec<Message>,
    max_turns: usize,
}

impl Agent {
    pub fn new(model: DeepSeek, system_prompt: String, max_turns: usize) -> Self {
        Self {
            model,
            messages: vec![Message::text("system", system_prompt)],
            max_turns,
        }
    }

    pub async fn run(&mut self, task: &str, tools: &Tools, output: &mut impl Write) -> Result<()> {
        self.run_with_status(task, tools, output, |name| eprintln!("tool: {name}"))
            .await
    }

    pub async fn run_with_status(
        &mut self,
        task: &str,
        tools: &Tools,
        output: &mut impl Write,
        mut on_tool: impl FnMut(&str),
    ) -> Result<()> {
        ensure!(!task.trim().is_empty(), "task must not be empty");
        self.messages.push(Message::text("user", task));
        for _ in 0..self.max_turns {
            let response = self
                .model
                .respond(&self.messages, tools.definitions(), output)
                .await?;
            if !response.content.is_empty() {
                writeln!(output)?;
                output.flush()?;
            }
            let done = response.tool_calls.is_empty();
            self.messages.push(response);
            if done {
                return Ok(());
            }

            let mut results = Vec::new();
            for call in &self
                .messages
                .last()
                .expect("assistant message was just appended")
                .tool_calls
            {
                on_tool(&call.function.name);
                let mut result = Message::text("tool", tools.call(call).await);
                result.tool_call_id = Some(call.id.clone());
                results.push(result);
            }
            self.messages.extend(results);
        }
        bail!("model turn limit reached ({})", self.max_turns)
    }
}
