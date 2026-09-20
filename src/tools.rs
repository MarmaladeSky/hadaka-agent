use std::{collections::HashMap, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, ResourceContents},
    service::RunningService,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    process::{Child, Command},
    time::timeout,
};

use crate::{config::McpServer, model::ToolCall};

const MCP_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

struct Session {
    child: Child,
    client: Option<RunningService<RoleClient, ()>>,
}

enum Route {
    Echo,
    Mcp { session: usize, name: String },
}

pub struct Tools {
    definitions: Vec<Value>,
    routes: HashMap<String, Route>,
    sessions: Vec<Session>,
}

impl Tools {
    pub fn new() -> Self {
        Self {
            definitions: vec![definition(
                "echo",
                "Return the supplied text unchanged.",
                json!({
                    "type": "object", "properties": {"text": {"type": "string"}},
                    "required": ["text"], "additionalProperties": false
                }),
            )],
            routes: HashMap::from([("echo".into(), Route::Echo)]),
            sessions: Vec::new(),
        }
    }

    pub fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    pub async fn connect(&mut self, servers: &[McpServer]) -> Result<()> {
        for server in servers {
            // Keep ownership of the child before awaiting initialization, so startup
            // errors and Ctrl-C follow the same explicit shutdown path.
            let mut child = Command::new(&server.command)
                .args(&server.args)
                .envs(&server.env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| format!("cannot start MCP server {}", server.name))?;
            let stdout = child.stdout.take().context("MCP stdout unavailable")?;
            let stdin = child.stdin.take().context("MCP stdin unavailable")?;
            let index = self.sessions.len();
            self.sessions.push(Session {
                child,
                client: None,
            });
            timeout(MCP_TIMEOUT, async {
                let client = ().serve((stdout, stdin)).await.context("MCP initialization failed")?;
                self.sessions[index].client = Some(client);
                let client = self.sessions[index]
                    .client
                    .as_ref()
                    .context("missing MCP session")?;
                let available = client
                    .list_all_tools()
                    .await
                    .context("MCP tool discovery failed")?;
                for tool in available {
                    let name = exposed_name(&server.name, &tool.name);
                    ensure!(
                        !self.routes.contains_key(&name),
                        "MCP tool name collision: {name}"
                    );
                    self.definitions.push(definition(
                        &name,
                        tool.description.as_deref().unwrap_or(""),
                        Value::Object((*tool.input_schema).clone()),
                    ));
                    self.routes.insert(
                        name,
                        Route::Mcp {
                            session: index,
                            name: tool.name.into_owned(),
                        },
                    );
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .with_context(|| format!("MCP startup timed out: {}", server.name))?
            .with_context(|| format!("cannot connect MCP server {}", server.name))?;
            eprintln!("connected MCP server: {}", server.name);
        }
        Ok(())
    }

    pub async fn call(&self, call: &ToolCall) -> String {
        match self.execute(call).await {
            Ok(result) => result,
            Err(error) => format!("Error: {error:#}"),
        }
    }

    async fn execute(&self, call: &ToolCall) -> Result<String> {
        let arguments: Value = serde_json::from_str(&call.function.arguments)
            .context("tool arguments must be valid JSON")?;
        let arguments = arguments
            .as_object()
            .context("tool arguments must be a JSON object")?;
        match self.routes.get(&call.function.name) {
            Some(Route::Echo) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Echo {
                    text: String,
                }
                let args: Echo = serde_json::from_value(Value::Object(arguments.clone()))
                    .context("echo expects a single string argument: text")?;
                Ok(args.text)
            }
            Some(Route::Mcp { session, name }) => {
                let client = self.sessions[*session]
                    .client
                    .as_ref()
                    .context("MCP session unavailable")?;
                let result = timeout(
                    MCP_TIMEOUT,
                    client.call_tool(
                        CallToolRequestParams::new(name.clone()).with_arguments(arguments.clone()),
                    ),
                )
                .await
                .context("MCP tool call timed out")?
                .context("MCP tool call failed")?;
                Ok(render_result(result))
            }
            None => bail!("unknown tool: {}", call.function.name),
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        for session in &mut self.sessions {
            if let Some(client) = &mut session.client {
                match client.close_with_timeout(SHUTDOWN_TIMEOUT).await {
                    Ok(Some(_)) => {}
                    Ok(None) => errors.push("MCP connection shutdown timed out".to_string()),
                    Err(error) => errors.push(format!("MCP connection shutdown failed: {error}")),
                }
            }
            match timeout(SHUTDOWN_TIMEOUT, session.child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => errors.push(format!("cannot wait for MCP process: {error}")),
                Err(_) => {
                    if let Err(error) = session.child.kill().await {
                        errors.push(format!("cannot terminate MCP process: {error}"));
                    }
                }
            }
        }
        self.sessions.clear();
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}

fn definition(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type": "function", "function": {
        "name": name, "description": description, "parameters": parameters
    }})
}

fn exposed_name(server: &str, tool: &str) -> String {
    format!("mcp_{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn render_result(result: CallToolResult) -> String {
    let mut parts = Vec::new();
    if result.is_error == Some(true) {
        parts.push("Error: MCP tool reported a failure".into());
    }
    for content in result.content {
        parts.push(match content {
            ContentBlock::Text(text) => text.text,
            ContentBlock::Resource(resource) => match resource.resource {
                ResourceContents::TextResourceContents { text, .. } => text,
                _ => "[Unsupported binary MCP resource]".into(),
            },
            ContentBlock::ResourceLink(link) => format!("{}: {}", link.name, link.uri),
            ContentBlock::Image(_) => "[Unsupported MCP image content]".into(),
            ContentBlock::Audio(_) => "[Unsupported MCP audio content]".into(),
            _ => "[Unsupported MCP content]".into(),
        });
    }
    if let Some(structured) = result.structured_content {
        parts.push(structured.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FunctionCall;

    #[tokio::test]
    async fn echo_and_invalid_calls() {
        let tools = Tools::new();
        for (name, arguments, expected) in [
            ("echo", r#"{"text":"hello"}"#, "hello"),
            ("echo", "{", "Error: tool arguments must be valid JSON"),
            ("echo", "[]", "Error: tool arguments must be a JSON object"),
            ("echo", r#"{"text":123}"#, "Error: echo expects"),
            (
                "echo",
                r#"{"text":"a","extra":true}"#,
                "Error: echo expects",
            ),
            ("missing", "{}", "Error: unknown tool"),
        ] {
            let call = ToolCall {
                id: "test".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: arguments.into(),
                },
            };
            assert!(tools.call(&call).await.starts_with(expected));
        }
    }

    #[test]
    fn preserves_text_and_structured_content_without_binary_payloads() {
        let result: CallToolResult = serde_json::from_value(json!({
            "isError": true,
            "content": [{"type":"text", "text":"first"}, {"type":"text", "text":"second"},
                {"type":"image", "data":"secret-binary-data", "mimeType":"image/png"}],
            "structuredContent": {"answer": 42}
        }))
        .unwrap();
        let output = render_result(result);
        assert!(output.starts_with("Error:"));
        assert!(output.contains("first\nsecond"));
        assert!(output.contains(r#"{"answer":42}"#));
        assert!(output.contains("Unsupported MCP image"));
        assert!(!output.contains("secret-binary-data"));
    }
}
