use std::io::{self, Write};

use anyhow::{Result, bail, ensure};
use serde_json::json;

use crate::{
    cli::OutputFormat,
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
        self.run_with_diagnostics(task, tools, output, &mut std::io::stderr())
            .await
    }

    pub async fn run_with_options(
        &mut self,
        task: &str,
        tools: &Tools,
        output: &mut impl Write,
        format: OutputFormat,
        verbose: bool,
    ) -> Result<()> {
        if !verbose {
            return self.run_quiet(task, tools, output, format).await;
        }
        if matches!(format, OutputFormat::Human) {
            return self.run(task, tools, output).await;
        }
        let mut events = JsonEvents { output };
        ensure!(!task.trim().is_empty(), "task must not be empty");
        events.emit("input", json!({"text": task}))?;
        events.emit("system_prompt", json!({"text": self.system_prompt}))?;
        events.emit("tools", json!({"tools": tools.definitions()}))?;
        let mut messages = vec![
            Message::text("system", &self.system_prompt),
            Message::text("user", task),
        ];
        for turn in 1..=self.max_turns {
            events.emit("turn", json!({"number": turn, "maximum": self.max_turns}))?;
            let response = self
                .model
                .respond(&messages, tools.definitions(), &mut events)
                .await?;
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
                let arguments = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                    .unwrap_or_else(|_| json!({"raw": call.function.arguments}));
                events.emit(
                    "tool_call",
                    json!({"name": call.function.name, "id": call.id, "parameters": arguments}),
                )?;
                let result = tools.call(call).await;
                events.emit(
                    "tool_result",
                    json!({"name": call.function.name, "id": call.id, "result": result}),
                )?;
                let mut message = Message::text("tool", result);
                message.tool_call_id = Some(call.id.clone());
                results.push(message);
            }
            messages.extend(results);
        }
        bail!("model turn limit reached ({})", self.max_turns)
    }

    async fn run_quiet(
        &mut self,
        task: &str,
        tools: &Tools,
        output: &mut impl Write,
        format: OutputFormat,
    ) -> Result<()> {
        ensure!(!task.trim().is_empty(), "task must not be empty");
        let mut messages = vec![
            Message::text("system", &self.system_prompt),
            Message::text("user", task),
        ];
        for _ in 1..=self.max_turns {
            let mut discarded_stream = Vec::new();
            let response = self
                .model
                .respond(&messages, tools.definitions(), &mut discarded_stream)
                .await?;
            let done = response.tool_calls.is_empty();
            let content = response.content.clone();
            messages.push(response);
            if done {
                match format {
                    OutputFormat::Human => {
                        write!(output, "{content}")?;
                        if !content.is_empty() {
                            writeln!(output)?;
                        }
                        output.flush()?;
                    }
                    OutputFormat::Json => {
                        let mut events = JsonEvents { output };
                        events.emit("result", json!({ "text": content }))?;
                    }
                }
                return Ok(());
            }
            let mut results = Vec::new();
            for call in &messages
                .last()
                .expect("assistant message was just appended")
                .tool_calls
            {
                let result = tools.call(call).await;
                let mut message = Message::text("tool", result);
                message.tool_call_id = Some(call.id.clone());
                results.push(message);
            }
            messages.extend(results);
        }
        bail!("model turn limit reached ({})", self.max_turns)
    }

    pub async fn run_with_diagnostics(
        &mut self,
        task: &str,
        tools: &Tools,
        output: &mut impl Write,
        diagnostics: &mut impl Write,
    ) -> Result<()> {
        ensure!(!task.trim().is_empty(), "task must not be empty");
        writeln!(
            diagnostics,
            "input:\n{task}\nsystem prompt:\n{}",
            self.system_prompt
        )?;
        write_tool_summary(diagnostics, tools.definitions())?;
        diagnostics.flush()?;
        let mut messages = vec![
            Message::text("system", &self.system_prompt),
            Message::text("user", task),
        ];
        for turn in 1..=self.max_turns {
            writeln!(diagnostics, "output (turn {turn}/{}):", self.max_turns)?;
            diagnostics.flush()?;
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
                writeln!(
                    diagnostics,
                    "tool call: {} (id: {})\nparams:\n{}",
                    call.function.name,
                    call.id,
                    pretty_arguments(&call.function.arguments)
                )?;
                diagnostics.flush()?;
                let mut result = Message::text("tool", tools.call(call).await);
                writeln!(
                    diagnostics,
                    "tool result: {} (id: {})\n{}",
                    call.function.name,
                    call.id,
                    human_tool_result(&result.content)
                )?;
                diagnostics.flush()?;
                result.tool_call_id = Some(call.id.clone());
                results.push(result);
            }
            messages.extend(results);
        }
        bail!("model turn limit reached ({})", self.max_turns)
    }
}

struct JsonEvents<'a, W: Write> {
    output: &'a mut W,
}

impl<W: Write> JsonEvents<'_, W> {
    fn emit(&mut self, event: &str, data: serde_json::Value) -> Result<()> {
        let mut object = match data {
            serde_json::Value::Object(map) => map,
            _ => unreachable!(),
        };
        object.insert("type".into(), serde_json::Value::String(event.into()));
        serde_json::to_writer(&mut self.output, &serde_json::Value::Object(object))?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(())
    }
}

impl<W: Write> Write for JsonEvents<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(bytes);
        let mut object = serde_json::Map::new();
        object.insert("type".into(), json!("output"));
        object.insert("text".into(), json!(text));
        serde_json::to_writer(&mut self.output, &serde_json::Value::Object(object))?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn pretty_arguments(arguments: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| arguments.to_owned())
}

fn write_tool_summary(output: &mut impl Write, definitions: &[serde_json::Value]) -> Result<()> {
    writeln!(output, "available tools:")?;
    for definition in definitions {
        let function = definition.get("function").unwrap_or(definition);
        let name = function
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let description = function
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        writeln!(output, "- {name}: {description}")?;
        if let Some(parameters) = function.get("parameters") {
            writeln!(
                output,
                "  parameters:\n{}",
                pretty_arguments(&parameters.to_string())
            )?;
        }
    }
    Ok(())
}

pub(crate) fn human_tool_result(result: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(result) else {
        return result.to_owned();
    };
    let Some(object) = value.as_object() else {
        return result.to_owned();
    };
    // read_file returns structured pagination metadata. The file content is
    // already line-oriented, so displaying the metadata as JSON obscures the
    // useful part of the result in human mode.
    if let Some(content) = object.get("content").and_then(serde_json::Value::as_str) {
        let mut output = content.to_owned();
        let total = object
            .get("total_lines")
            .and_then(serde_json::Value::as_u64);
        let has_more = object.get("has_more").and_then(serde_json::Value::as_bool);
        if let (Some(total), Some(has_more)) = (total, has_more) {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!(
                "({total} total lines{}.)",
                if has_more { ", more available" } else { "" }
            ));
        }
        return output;
    }
    result.to_owned()
}
