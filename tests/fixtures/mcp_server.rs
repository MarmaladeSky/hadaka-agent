//! A deterministic stdio MCP fixture; it needs no network or external runtimes.
use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

fn main() {
    if let Ok(path) = std::env::var("FIXTURE_PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str().unwrap() {
            "initialize" => json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fixture", "version": "1.0.0"}
            }),
            "ping" => json!({}),
            "tools/list" => {
                if request["params"]["cursor"] == "page-2" {
                    json!({"tools": [tool("second")]})
                } else if std::env::var_os("FIXTURE_COLLISION").is_some() {
                    json!({"tools": [tool("same.name"), tool("same_name")]})
                } else {
                    json!({"tools": [tool("echo")], "nextCursor": "page-2"})
                }
            }
            "tools/call" => {
                let params = &request["params"];
                let text = params["arguments"]["text"]
                    .as_str()
                    .unwrap_or("missing text");
                let tool_name = params["name"].as_str().unwrap();
                json!({"content": [
                    {"type": "text", "text": format!("{tool_name}: {text}")},
                    {"type": "text", "text": "second content block"}
                ], "structuredContent": {"tool": tool_name, "text": text}, "isError": text == "fail"})
            }
            method => panic!("unexpected MCP method: {method}"),
        };
        writeln!(
            output,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )
        .unwrap();
        output.flush().unwrap();
    }
    if std::env::var_os("FIXTURE_HANG_ON_EOF").is_some() {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn tool(name: &str) -> Value {
    json!({"name": name, "description": "Fixture tool", "inputSchema": {
        "type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]
    }})
}
