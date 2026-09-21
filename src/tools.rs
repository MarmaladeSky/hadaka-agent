use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

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
mod fetch_url;
mod list_directory;
mod read_file;
mod run_command;
mod search_files;
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

#[derive(Clone, Debug)]
pub struct PermissionPolicy {
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    exec_programs: Vec<String>,
    net_hosts: Vec<String>,
}

impl PermissionPolicy {
    pub fn new(
        read_roots: Vec<PathBuf>,
        write_roots: Vec<PathBuf>,
        exec_programs: Vec<String>,
    ) -> Self {
        Self {
            read_roots: normalize_roots(read_roots),
            write_roots: normalize_roots(write_roots),
            exec_programs,
            net_hosts: Vec::new(),
        }
    }

    #[cfg(test)]
    fn allow_all() -> Self {
        Self::new(
            vec![PathBuf::from(".")],
            vec![PathBuf::from(".")],
            vec!["cargo".into(), "rustfmt".into(), "git".into()],
        )
    }

    pub fn with_network_hosts(mut self, hosts: Vec<String>) -> Self {
        self.net_hosts = hosts
            .into_iter()
            .map(|host| host.to_ascii_lowercase())
            .collect();
        self
    }

    fn check_host(&self, host: &str) -> Result<()> {
        ensure!(
            self.net_hosts
                .iter()
                .any(|allowed| allowed == &host.to_ascii_lowercase()),
            "permission denied: network access to `{host}` is not allowed (requires --allow-net {host})"
        );
        Ok(())
    }

    fn check(&self, path: &str, roots: &[PathBuf], operation: &str) -> Result<()> {
        let path = resolve_path(path)?;
        ensure!(
            roots.iter().any(|root| path.starts_with(root)),
            "permission denied: {operation} `{}` is not allowed",
            path.display()
        );
        Ok(())
    }

    fn check_read(&self, path: &str) -> Result<()> {
        self.check(path, &self.read_roots, "read")
    }

    fn check_write(&self, path: &str) -> Result<()> {
        self.check(path, &self.write_roots, "write")
    }

    fn check_exec(&self, program: &str) -> Result<()> {
        ensure!(
            self.exec_programs.iter().any(|allowed| allowed == program),
            "permission denied: execute `{program}` is not allowed"
        );
        Ok(())
    }
}

fn normalize_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots
        .into_iter()
        .filter_map(|root| {
            let root = if root.is_absolute() {
                root
            } else {
                std::env::current_dir().ok()?.join(root)
            };
            std::fs::canonicalize(root).ok()
        })
        .collect()
}

fn resolve_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }
    let parent = path.parent().context("path has no parent")?;
    let parent = std::fs::canonicalize(parent)?;
    Ok(parent.join(path.file_name().context("path has no filename")?))
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
    policy: PermissionPolicy,
}

impl Tools {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_policy(PermissionPolicy::allow_all())
    }

    pub fn with_policy(policy: PermissionPolicy) -> Self {
        let mut tools = Self {
            definitions: Vec::new(),
            routes: HashMap::new(),
            sessions: Vec::new(),
            policy: policy.clone(),
        };
        tools
            .register(echo::Echo)
            .expect("unique built-in tool name");
        tools
            .register(list_directory::ListDirectory)
            .expect("unique built-in tool name");
        tools
            .register(search_files::SearchFiles)
            .expect("unique built-in tool name");
        tools
            .register(run_command::RunCommand)
            .expect("unique built-in tool name");
        tools
            .register(read_file::ReadFile)
            .expect("unique built-in tool name");
        tools
            .register(text_editor::TextEditor)
            .expect("unique built-in tool name");
        tools
            .register(fetch_url::FetchUrl::new(policy))
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
        if call.function.name == "read_file"
            || call.function.name == "list_directory"
            || call.function.name == "search_files"
        {
            let arguments: Value = serde_json::from_str(&call.function.arguments)
                .context("tool arguments must be valid JSON")?;
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .context("file tool path must be a string")?;
            self.policy.check_read(path)?;
        } else if call.function.name == "text_editor" {
            let arguments: Value = serde_json::from_str(&call.function.arguments)
                .context("tool arguments must be valid JSON")?;
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .context("text_editor path must be a string")?;
            self.policy.check_write(path)?;
        } else if call.function.name == "run_command" {
            let arguments: Value = serde_json::from_str(&call.function.arguments)
                .context("tool arguments must be valid JSON")?;
            let program = arguments
                .get("program")
                .and_then(Value::as_str)
                .context("run_command program must be a string")?;
            self.policy.check_exec(program)?;
            let cwd = arguments.get("cwd").and_then(Value::as_str).unwrap_or(".");
            self.policy.check_read(cwd)?;
        }
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
    async fn search_files_explicit_path_respects_read_permissions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("sample.txt"), "needle\n").unwrap();
        let allowed = Tools::with_policy(PermissionPolicy::new(
            vec![directory.path().to_owned()],
            vec![],
            vec![],
        ));
        let denied = Tools::with_policy(PermissionPolicy::new(vec![], vec![], vec![]));
        let mut call = ToolCall {
            id: "search-explicit-path".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "search_files".into(),
                arguments: json!({"query": "needle", "path": directory.path()}).to_string(),
            },
        };
        let result = allowed.call(&call).await;
        let result: Value = serde_json::from_str(&result)
            .unwrap_or_else(|_| panic!("expected search results, got: {result}"));
        assert_eq!(result["total_matches"], 1);
        assert_eq!(result["matches"][0]["path"], "sample.txt");
        assert!(denied.call(&call).await.contains("permission denied: read"));

        // Missing and invalid paths must never select an implicit directory.
        for path in [Value::Null, json!(42)] {
            call.function.arguments = json!({"query": "needle", "path": path}).to_string();
            assert!(
                allowed
                    .call(&call)
                    .await
                    .contains("file tool path must be a string")
            );
        }
        for name in ["search_files", "read_file", "list_directory"] {
            call.function.name = name.into();
            call.function.arguments = json!({"query": "needle"}).to_string();
            assert!(
                allowed
                    .call(&call)
                    .await
                    .contains("file tool path must be a string")
            );
        }
    }

    #[tokio::test]
    async fn task_policy_exposes_tools_and_denies_unapproved_operations() {
        let denied = Tools::with_policy(PermissionPolicy::new(Vec::new(), Vec::new(), Vec::new()));
        let names: Vec<_> = denied
            .definitions()
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "echo",
                "list_directory",
                "search_files",
                "run_command",
                "read_file",
                "text_editor",
                "fetch_url"
            ]
        );

        let read_only = Tools::with_policy(PermissionPolicy::new(
            vec![PathBuf::from(".")],
            Vec::new(),
            Vec::new(),
        ));
        let names: Vec<_> = read_only
            .definitions()
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "echo",
                "list_directory",
                "search_files",
                "run_command",
                "read_file",
                "text_editor",
                "fetch_url"
            ]
        );
        let call = ToolCall {
            id: "denied".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"Cargo.toml","offset":0,"limit":1}"#.into(),
            },
        };
        assert!(
            denied.call(&call).await.contains("permission denied"),
            "permission errors should be returned as tool results"
        );
        let command = ToolCall {
            id: "command-denied".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "run_command".into(),
                arguments: r#"{"program":"printf","args":["ok"]}"#.into(),
            },
        };
        assert!(denied.call(&command).await.contains("permission denied"));
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
