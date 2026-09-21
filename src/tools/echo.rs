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

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Tool};

pub(super) struct Echo;

impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Return the supplied text unchanged."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object", "properties": {"text": {"type": "string"}},
            "required": ["text"], "additionalProperties": false
        })
    }

    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                text: String,
            }
            let args: Arguments = serde_json::from_value(arguments)
                .context("echo expects a single string argument: text")?;
            Ok(args.text)
        })
    }
}
