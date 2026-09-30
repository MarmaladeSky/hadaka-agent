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

use super::{
    AssistantTurn, Message, ModelProvider, ToolDefinition, chat_completions::ChatCompletions,
};
use crate::config::Provider;
use anyhow::Result;
use futures_util::future::LocalBoxFuture;
use std::time::Duration;

pub struct LlamaCpp {
    client: ChatCompletions,
}
impl LlamaCpp {
    pub fn new(config: &Provider) -> Result<Self> {
        config.validate_selected()?;
        Ok(Self {
            client: ChatCompletions::new(
                config.llamacpp_endpoint()?,
                config.model.clone(),
                config.api_key.clone(),
                Duration::from_secs(config.request_timeout_secs.unwrap_or(600)),
                "llama.cpp",
                Default::default(),
            )?,
        })
    }
}
impl ModelProvider for LlamaCpp {
    fn respond<'a>(
        &'a self,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        on_text: &'a mut dyn FnMut(&str) -> Result<()>,
    ) -> LocalBoxFuture<'a, Result<AssistantTurn>> {
        self.client.respond(messages, tools, on_text)
    }
}
