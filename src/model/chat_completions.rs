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

use super::{AssistantTurn, Message, ToolCall, ToolDefinition};
use anyhow::{Context, Result, bail, ensure};
use eventsource_stream::Eventsource;
use futures_util::{StreamExt, future::LocalBoxFuture};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

fn wire_message(message: &Message) -> Value {
    match message {
        Message::System(content) => json!({"role":"system", "content":content}),
        Message::User(content) => json!({"role":"user", "content":content}),
        Message::Tool { call_id, content } => {
            json!({"role":"tool", "content":content, "tool_call_id":call_id})
        }
        Message::Assistant(turn) => {
            let mut value = json!({"role":"assistant", "content":turn.content});
            if !turn.tool_calls.is_empty() {
                value["tool_calls"] = json!(turn.tool_calls.iter().map(|call| json!({
                    "id":call.id, "type":"function", "function":{"name":call.name, "arguments":call.arguments}
                })).collect::<Vec<_>>());
            }
            value
        }
    }
}

pub(super) struct ChatCompletions {
    client: reqwest::Client,
    pub(super) endpoint: String,
    model: String,
    api_key: String,
    provider: &'static str,
    extra: serde_json::Map<String, Value>,
}

impl ChatCompletions {
    pub(super) fn new(
        endpoint: String,
        model: String,
        api_key: String,
        timeout: Duration,
        provider: &'static str,
        extra: serde_json::Map<String, Value>,
    ) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            model,
            api_key,
            provider,
            extra,
        })
    }

    pub(super) fn respond<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        on_text: &'a mut dyn FnMut(&str) -> Result<()>,
    ) -> LocalBoxFuture<'a, Result<AssistantTurn>> {
        Box::pin(async move {
            let result = self.complete(messages, tools, on_text).await;
            result.with_context(|| format!("{} response failed", self.provider))
        })
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        on_text: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<AssistantTurn> {
        let messages: Vec<Value> = messages.iter().map(wire_message).collect();
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({"type":"function", "function":{
                    "name":tool.name,"description":tool.description,"parameters":tool.parameters
                }})
            })
            .collect();
        let mut body = json!({"model":self.model,"messages":messages,"tools":tools,"tool_choice":"auto","stream":true});
        body.as_object_mut().unwrap().extend(self.extra.clone());
        let mut request = self.client.post(&self.endpoint).json(&body);
        if !self.api_key.trim().is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let response = request
            .send()
            .await
            .context("request failed")?
            .error_for_status()
            .context("server returned an HTTP error")?;
        ensure!(
            response.status().is_success(),
            "server returned HTTP {}",
            response.status()
        );
        let mut events = response.bytes_stream().eventsource();
        let mut turn = StreamedTurn::default();
        while let Some(event) = events.next().await {
            let event = event.context("failed to read model stream")?;
            if event.data.trim() == "[DONE]" {
                return turn.finish();
            }
            turn.push(
                serde_json::from_str(&event.data).context("invalid JSON in model stream")?,
                on_text,
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
    fn push(&mut self, value: Value, on_text: &mut dyn FnMut(&str) -> Result<()>) -> Result<()> {
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
                on_text(&text).context("cannot write model output")?;
                self.content.push_str(&text);
            }
            for delta in choice.delta.tool_calls.unwrap_or_default() {
                let call = self.calls.entry(delta.index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = delta.id {
                    call.id.push_str(&id);
                }
                if let Some(kind) = delta.kind {
                    ensure!(kind == "function", "unsupported tool-call type: {kind}");
                }
                let function = delta.function.unwrap_or_default();
                if let Some(name) = function.name {
                    call.name.push_str(&name);
                }
                if let Some(args) = function.arguments {
                    call.arguments.push_str(&args);
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<AssistantTurn> {
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
                !call.id.is_empty() && !call.name.is_empty(),
                "incomplete tool call: missing ID or name"
            );
            ensure!(ids.insert(&call.id), "duplicate tool-call ID: {}", call.id);
        }
        Ok(AssistantTurn {
            content: self.content,
            tool_calls: self.calls.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests;
