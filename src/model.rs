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

use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Provider;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Serialize)]
pub struct Message {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn text(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
}

pub struct DeepSeek {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: String,
}

impl DeepSeek {
    pub fn new(config: &Provider) -> Result<Self> {
        ensure!(
            !config.api_key.trim().is_empty(),
            "api_key must not be empty"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint: "https://api.deepseek.com/chat/completions".into(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
        })
    }

    #[cfg(test)]
    pub fn for_test(config: &Provider, endpoint: String) -> Result<Self> {
        let mut model = Self::new(config)?;
        model.endpoint = endpoint;
        Ok(model)
    }

    pub async fn respond(
        &self,
        messages: &[Message],
        tools: &[Value],
        output: &mut impl Write,
    ) -> Result<Message> {
        let request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "messages": messages,
                "tools": tools,
                "tool_choice": "auto",
                "stream": true,
                "thinking": {"type": "disabled"},
            }));
        let response = request
            .send()
            .await
            .context("DeepSeek request failed")?
            .error_for_status()
            .context("DeepSeek returned an HTTP error")?;
        let mut events = response.bytes_stream().eventsource();
        let mut turn = StreamedTurn::default();
        while let Some(event) = events.next().await {
            let event = event.context("failed to read model stream")?;
            if event.data.trim() == "[DONE]" {
                return turn.finish();
            }
            turn.push(
                serde_json::from_str(&event.data).context("invalid JSON in model stream")?,
                output,
            )?;
        }
        bail!("incomplete model stream: missing [DONE]")
    }
}

#[derive(Default)]
struct StreamedTurn {
    content: String,
    calls: BTreeMap<usize, ToolCall>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Chunk {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    index: usize,
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolDelta>>,
}

#[derive(Deserialize)]
struct ToolDelta {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Default, Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

impl StreamedTurn {
    fn push(&mut self, value: Value, output: &mut impl Write) -> Result<()> {
        if let Some(error) = value.get("error") {
            bail!("model stream error: {error}");
        }
        let chunk: Chunk = serde_json::from_value(value).context("invalid model stream chunk")?;
        for choice in chunk.choices {
            ensure!(choice.index == 0, "unexpected multiple model choices");
            ensure!(
                self.finish_reason.is_none(),
                "received model data after finish_reason"
            );
            if let Some(text) = choice.delta.content {
                output
                    .write_all(text.as_bytes())
                    .context("cannot write model output")?;
                output.flush()?;
                self.content.push_str(&text);
            }
            for delta in choice.delta.tool_calls.unwrap_or_default() {
                let call = self.calls.entry(delta.index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    kind: "function".into(),
                    function: FunctionCall::default(),
                });
                if let Some(id) = delta.id {
                    call.id.push_str(&id);
                }
                if let Some(kind) = delta.kind {
                    ensure!(kind == "function", "unsupported tool-call type: {kind}");
                }
                let function = delta.function.unwrap_or_default();
                if let Some(name) = function.name {
                    call.function.name.push_str(&name);
                }
                if let Some(args) = function.arguments {
                    call.function.arguments.push_str(&args);
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Message> {
        let reason = self
            .finish_reason
            .context("incomplete model stream: missing finish_reason")?;
        match reason.as_str() {
            "stop" => ensure!(
                self.calls.is_empty(),
                "tool calls received with stop finish_reason"
            ),
            "tool_calls" => ensure!(
                !self.calls.is_empty(),
                "tool_calls finish_reason without tool calls"
            ),
            _ => bail!("model response did not complete (finish_reason: {reason})"),
        }
        let mut ids = HashSet::new();
        for call in self.calls.values() {
            ensure!(
                !call.id.is_empty() && !call.function.name.is_empty(),
                "incomplete tool call: missing ID or name"
            );
            ensure!(ids.insert(&call.id), "duplicate tool-call ID: {}", call.id);
        }
        Ok(Message {
            role: "assistant",
            content: self.content,
            tool_calls: self.calls.into_values().collect(),
            tool_call_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_interleaved_calls_and_streams_text() {
        let mut turn = StreamedTurn::default();
        let mut output = Vec::new();
        turn.push(
            json!({"choices": [{"index": 0, "delta": {"content": "Checking…", "tool_calls": [
                {"index": 1, "id": "b", "function": {"name": "echo", "arguments": "{\"text\":"}},
                {"index": 0, "id": "a", "function": {"name": "ec", "arguments": "{"}}
            ]}}]}),
            &mut output,
        )
        .unwrap();
        assert_eq!(String::from_utf8(output.clone()).unwrap(), "Checking…");
        turn.push(
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"name": "ho", "arguments": "\"text\":\"one\"}"}},
            {"index": 1, "function": {"arguments": "\"two\"}"}}
        ]}, "finish_reason": "tool_calls"}]}),
            &mut output,
        )
        .unwrap();
        let result = turn.finish().unwrap();
        assert_eq!(result.tool_calls[0].id, "a");
        assert_eq!(result.tool_calls[0].function.name, "echo");
        assert_eq!(result.tool_calls[0].function.arguments, r#"{"text":"one"}"#);
        assert_eq!(result.tool_calls[1].function.arguments, r#"{"text":"two"}"#);
    }

    #[test]
    fn rejects_unfinished_or_truncated_responses() {
        assert!(StreamedTurn::default().finish().is_err());
        let turn = StreamedTurn {
            finish_reason: Some("length".into()),
            ..Default::default()
        };
        assert!(turn.finish().unwrap_err().to_string().contains("length"));
    }

    #[test]
    fn accepts_null_optional_tool_fields() {
        let mut turn = StreamedTurn::default();
        let mut output = Vec::new();
        turn.push(
            json!({"choices": [{"index": 0, "delta": {
            "content": "hello", "tool_calls": null
        }, "finish_reason": "stop"}]}),
            &mut output,
        )
        .unwrap();
        assert_eq!(turn.finish().unwrap().content, "hello");

        let mut turn = StreamedTurn::default();
        turn.push(
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call", "function": null}
            ]}}]}),
            &mut output,
        )
        .unwrap();
        turn.push(
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"name": "echo", "arguments": "{}"}}
        ]}, "finish_reason": "tool_calls"}]}),
            &mut output,
        )
        .unwrap();
        assert_eq!(turn.finish().unwrap().tool_calls[0].id, "call");
    }
}
