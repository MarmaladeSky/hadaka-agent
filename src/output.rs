// Copyright 2026 David Akermann
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{
    cli::OutputFormat,
    model::{AssistantTurn, ToolCall, ToolDefinition},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::io::Write;

pub(crate) enum AgentEvent<'a> {
    Start {
        task: &'a str,
        system_prompt: &'a str,
        tools: &'a [ToolDefinition],
    },
    Turn {
        number: usize,
        maximum: usize,
    },
    Text(&'a str),
    AssistantComplete(&'a AssistantTurn),
    ToolCall(&'a ToolCall),
    ToolResult {
        call: &'a ToolCall,
        result: &'a str,
    },
    Complete(&'a str),
}

pub(crate) trait EventSink {
    fn emit(&mut self, event: AgentEvent<'_>) -> Result<()>;
}

pub(crate) struct Renderer<'a, W, D> {
    pub output: &'a mut W,
    pub diagnostics: &'a mut D,
    pub format: OutputFormat,
    pub verbose: bool,
}

impl<W: Write, D: Write> Renderer<'_, W, D> {
    fn json(&mut self, kind: &str, mut value: Value) -> Result<()> {
        value["type"] = json!(kind);
        serde_json::to_writer(&mut self.output, &value)?;
        writeln!(self.output)?;
        self.output.flush()?;
        Ok(())
    }
}

impl<W: Write, D: Write> EventSink for Renderer<'_, W, D> {
    fn emit(&mut self, event: AgentEvent<'_>) -> Result<()> {
        if !self.verbose {
            if let AgentEvent::Complete(text) = event {
                match self.format {
                    OutputFormat::Json => self.json("result", json!({"text":text}))?,
                    OutputFormat::Human => {
                        write!(self.output, "{text}")?;
                        if !text.is_empty() {
                            writeln!(self.output)?;
                        }
                        self.output.flush()?;
                    }
                }
            }
            return Ok(());
        }
        match self.format {
            OutputFormat::Json => match event {
                AgentEvent::Start {
                    task,
                    system_prompt,
                    tools,
                } => {
                    self.json("input", json!({"text":task}))?;
                    self.json("system_prompt", json!({"text":system_prompt}))?;
                    // Preserve the existing CLI event schema independently of provider serialization.
                    let tools: Vec<_> = tools.iter().map(|tool| json!({"type":"function", "function": {
                        "name":tool.name, "description":tool.description, "parameters":tool.parameters
                    }})).collect();
                    self.json("tools", json!({"tools":tools}))?;
                }
                AgentEvent::Turn { number, maximum } => {
                    self.json("turn", json!({"number":number,"maximum":maximum}))?
                }
                AgentEvent::Text(text) => {
                    if !text.is_empty() {
                        self.json("output", json!({"text":text}))?;
                    }
                }
                AgentEvent::ToolCall(call) => {
                    let parameters = serde_json::from_str::<Value>(&call.arguments)
                        .unwrap_or_else(|_| json!({"raw":call.arguments}));
                    self.json(
                        "tool_call",
                        json!({"name":call.name,"id":call.id,"parameters":parameters}),
                    )?;
                }
                AgentEvent::ToolResult { call, result } => self.json(
                    "tool_result",
                    json!({"name":call.name,"id":call.id,"result":result}),
                )?,
                AgentEvent::AssistantComplete(_) | AgentEvent::Complete(_) => {}
            },
            OutputFormat::Human => match event {
                AgentEvent::Start {
                    task,
                    system_prompt,
                    tools,
                } => {
                    writeln!(
                        self.diagnostics,
                        "input:\n{task}\nsystem prompt:\n{system_prompt}"
                    )?;
                    write_tool_summary(self.diagnostics, tools)?;
                    self.diagnostics.flush()?;
                }
                AgentEvent::Turn { number, maximum } => {
                    writeln!(self.diagnostics, "output (turn {number}/{maximum}):")?;
                    self.diagnostics.flush()?;
                }
                AgentEvent::Text(text) => {
                    self.output.write_all(text.as_bytes())?;
                    self.output.flush()?;
                }
                AgentEvent::AssistantComplete(turn) => {
                    if !turn.content.is_empty() {
                        writeln!(self.output)?;
                        self.output.flush()?;
                    }
                }
                AgentEvent::ToolCall(call) => {
                    writeln!(
                        self.diagnostics,
                        "tool call: {} (id: {})\nparams:\n{}",
                        call.name,
                        call.id,
                        pretty_arguments(&call.arguments)
                    )?;
                    self.diagnostics.flush()?;
                }
                AgentEvent::ToolResult { call, result } => {
                    writeln!(
                        self.diagnostics,
                        "tool result: {} (id: {})\n{}",
                        call.name,
                        call.id,
                        human_tool_result(result)
                    )?;
                    self.diagnostics.flush()?;
                }
                AgentEvent::Complete(_) => {}
            },
        }
        Ok(())
    }
}

fn pretty_arguments(arguments: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| arguments.to_owned())
}

fn write_tool_summary(output: &mut impl Write, definitions: &[ToolDefinition]) -> Result<()> {
    writeln!(output, "available tools:")?;
    for definition in definitions {
        writeln!(output, "- {}: {}", definition.name, definition.description)?;
        writeln!(
            output,
            "  parameters:\n{}",
            pretty_arguments(&definition.parameters.to_string())
        )?;
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
    if object.get("untrusted").and_then(serde_json::Value::as_bool) == Some(true)
        && let Some(url) = object.get("final_url").and_then(serde_json::Value::as_str)
    {
        let mut output = format!(
            "Source: {url}\nHTTP {} | {} | untrusted web content\n",
            object["status"],
            object["content_type"].as_str().unwrap_or("")
        );
        if let Some(title) = object.get("title").and_then(serde_json::Value::as_str) {
            output.push_str(&format!("{title}\n"));
        }
        output.push('\n');
        if let Some(content) = object["content"].as_str() {
            output.push_str(content);
        } else {
            output.push_str(&human_json_value(&object["content"], 0));
        }
        if let Some(links) = object.get("links").and_then(serde_json::Value::as_array)
            && !links.is_empty()
        {
            output.push_str("\n\nLinks:\n");
            for link in links.iter().filter_map(serde_json::Value::as_str) {
                output.push_str(&format!("  {link}\n"));
            }
        }
        if object.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
            output.push_str("\n(Content truncated.)");
        }
        return output;
    }
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

fn human_json_value(value: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    match value {
        serde_json::Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| format!("{indent}{key}:\n{}", human_json_value(value, depth + 1)))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                format!(
                    "{indent}Item {}:\n{}",
                    index + 1,
                    human_json_value(value, depth + 1)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::String(text) => format!("{indent}{text}"),
        value => format!("{indent}{value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_deltas_and_answers_preserve_output_shape() {
        for format in [OutputFormat::Human, OutputFormat::Json] {
            for verbose in [false, true] {
                let (mut output, mut diagnostics) = (Vec::new(), Vec::new());
                let mut renderer = Renderer {
                    output: &mut output,
                    diagnostics: &mut diagnostics,
                    format,
                    verbose,
                };
                renderer.emit(AgentEvent::Text("")).unwrap();
                renderer
                    .emit(AgentEvent::AssistantComplete(&AssistantTurn::default()))
                    .unwrap();
                renderer.emit(AgentEvent::Complete("")).unwrap();
                if matches!(format, OutputFormat::Json) && !verbose {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&output).unwrap(),
                        json!({"type":"result", "text":""})
                    );
                } else {
                    assert!(output.is_empty());
                }
                assert!(diagnostics.is_empty());
            }
        }
    }
}
