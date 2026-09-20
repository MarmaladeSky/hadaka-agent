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
