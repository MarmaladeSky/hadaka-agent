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

use anyhow::Result;
use futures_util::future::LocalBoxFuture;
use serde_json::Value;

mod chat_completions;
pub mod deepseek;
pub mod llamacpp;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    System(String),
    User(String),
    Assistant(AssistantTurn),
    Tool { call_id: String, content: String },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistantTurn {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Generates one complete assistant turn. Text deltas are delivered immediately.
/// Errors (including callback failures) abort the turn; callers must not execute
/// tools until this future succeeds. Returned content contains all emitted text.
pub trait ModelProvider {
    fn respond<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        on_text: &'a mut dyn FnMut(&str) -> Result<()>,
    ) -> LocalBoxFuture<'a, Result<AssistantTurn>>;
}
