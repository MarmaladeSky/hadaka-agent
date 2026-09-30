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

use super::*;
use serde_json::json;

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
    assert_eq!(definitions.last().unwrap().name, "custom");
    assert!(tools.register(Custom).is_err());
    assert_eq!(tools.definitions(), definitions);
    let call = ToolCall {
        id: "custom-call".into(),
        name: "custom".into(),
        arguments: r#"{"value":42}"#.into(),
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
        name: "search_files".into(),
        arguments: json!({"query": "needle", "path": directory.path()}).to_string(),
    };
    let result = allowed.call(&call).await;
    let result: Value = serde_json::from_str(&result)
        .unwrap_or_else(|_| panic!("expected search results, got: {result}"));
    assert_eq!(result["total_matches"], 1);
    assert_eq!(result["matches"][0]["path"], "sample.txt");
    assert!(denied.call(&call).await.contains("permission denied: read"));

    // Missing and invalid paths must never select an implicit directory.
    for path in [Value::Null, json!(42)] {
        call.arguments = json!({"query": "needle", "path": path}).to_string();
        assert!(
            allowed
                .call(&call)
                .await
                .contains("file tool path must be a string")
        );
    }
    for name in ["search_files", "read_file", "list_directory"] {
        call.name = name.into();
        call.arguments = json!({"query": "needle"}).to_string();
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
        .map(|definition| definition.name.as_str())
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
            "filesystem",
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
        .map(|definition| definition.name.as_str())
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
            "filesystem",
            "fetch_url"
        ]
    );
    let call = ToolCall {
        id: "denied".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"Cargo.toml","offset":0,"limit":1}"#.into(),
    };
    assert!(
        denied.call(&call).await.contains("permission denied"),
        "permission errors should be returned as tool results"
    );
    let command = ToolCall {
        id: "command-denied".into(),
        name: "run_command".into(),
        arguments: r#"{"program":"printf","args":["ok"]}"#.into(),
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
            name: name.into(),
            arguments: arguments.into(),
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
