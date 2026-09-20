use std::{collections::HashMap, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, ResourceContents},
    service::{Peer, RunningService},
};
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

mod echo;
mod read_file;
mod text_editor;

use futures_util::future::BoxFuture;

/// A callable tool with its model-facing metadata and JSON arguments.
/// Implementations validate their arguments and return text or an error.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>>;
}

struct McpTool {
    name: String,
    remote_name: String,
    description: String,
    parameters: Value,
    peer: Peer<RoleClient>,
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let arguments = arguments
                .as_object()
                .context("tool arguments must be a JSON object")?;
            let result = timeout(
                MCP_TIMEOUT,
                self.peer.call_tool(
                    CallToolRequestParams::new(self.remote_name.clone())
                        .with_arguments(arguments.clone()),
                ),
            )
            .await
            .context("MCP tool call timed out")?
            .context("MCP tool call failed")?;
            Ok(render_result(result))
        })
    }
}

pub struct Tools {
    definitions: Vec<Value>,
    routes: HashMap<String, Box<dyn Tool>>,
    sessions: Vec<Session>,
}

impl Tools {
    pub fn new() -> Self {
        let mut tools = Self {
            definitions: Vec::new(),
            routes: HashMap::new(),
            sessions: Vec::new(),
        };
        tools
            .register(echo::Echo)
            .expect("unique built-in tool name");
        tools
            .register(read_file::ReadFile)
            .expect("unique built-in tool name");
        tools
            .register(text_editor::TextEditor)
            .expect("unique built-in tool name");
        tools
    }

    pub fn register(&mut self, tool: impl Tool + 'static) -> Result<()> {
        let name = tool.name().to_owned();
        ensure!(
            !self.routes.contains_key(&name),
            "tool name collision: {name}"
        );
        self.definitions
            .push(definition(&name, tool.description(), tool.parameters()));
        self.routes.insert(name, Box::new(tool));
        Ok(())
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
                let peer = client.peer().clone();
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
                    self.register(McpTool {
                        name,
                        remote_name: tool.name.into_owned(),
                        description: tool.description.as_deref().unwrap_or("").to_owned(),
                        parameters: Value::Object((*tool.input_schema).clone()),
                        peer: peer.clone(),
                    })?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .with_context(|| format!("MCP startup timed out: {}", server.name))?
            .with_context(|| format!("cannot connect MCP server {}", server.name))?;
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
        let Some(tool) = self.routes.get(&call.function.name) else {
            bail!("unknown tool: {}", call.function.name);
        };
        tool.call(Value::Object(arguments.clone())).await
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
    async fn registers_custom_tools_and_rejects_duplicates_without_changes() {
        struct Custom;
        impl Tool for Custom {
            fn name(&self) -> &str {
                "custom"
            }
            fn description(&self) -> &str {
                "A custom tool"
            }
            fn parameters(&self) -> Value {
                json!({"type": "object"})
            }
            fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    Ok(arguments.to_string())
                })
            }
        }
        let mut tools = Tools::new();
        tools.register(Custom).unwrap();
        let definitions = tools.definitions().to_vec();
        assert_eq!(definitions.last().unwrap()["function"]["name"], "custom");
        assert!(tools.register(Custom).is_err());
        assert_eq!(tools.definitions(), definitions);
        let call = ToolCall {
            id: "custom-call".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "custom".into(),
                arguments: r#"{"value":42}"#.into(),
            },
        };
        assert_eq!(tools.call(&call).await, r#"{"value":42}"#);
    }

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
