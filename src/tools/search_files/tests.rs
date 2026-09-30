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

#[tokio::test]
async fn supports_glob_syntax_on_file_names() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    for name in [
        "main.rs",
        "test1.rs",
        "test2.rs",
        "testx.rs",
        "README.md",
        "nested/lib.rs",
    ] {
        fs::write(dir.path().join(name), "needle\n").unwrap();
    }
    for (pattern, expected) in [
        (
            "*.rs",
            vec![
                "main.rs",
                "nested/lib.rs",
                "test1.rs",
                "test2.rs",
                "testx.rs",
            ],
        ),
        ("test?.rs", vec!["test1.rs", "test2.rs", "testx.rs"]),
        ("test[12].rs", vec!["test1.rs", "test2.rs"]),
        ("{main,lib}.rs", vec!["main.rs", "nested/lib.rs"]),
        ("README.md", vec!["README.md"]),
        ("*.RS", vec![]),
        ("nested/*.rs", vec![]),
        ("", vec![]),
    ] {
        let result: Value = serde_json::from_str(
            &SearchFiles
                .call(json!({
                    "query": "needle", "path": dir.path(), "glob": pattern
                }))
                .await
                .unwrap(),
        )
        .unwrap();
        let mut paths: Vec<_> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["path"].as_str().unwrap())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, expected, "pattern {pattern:?}");
    }
}

#[tokio::test]
async fn rejects_invalid_glob_even_in_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    for pattern in ["[", "{a,b"] {
        let error = SearchFiles
            .call(json!({
                "query": "needle", "path": dir.path(), "glob": pattern
            }))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid glob pattern"));
    }
}

#[tokio::test]
async fn handles_many_wildcards_without_recursive_backtracking() {
    let dir = tempfile::tempdir().unwrap();
    let name = "a".repeat(64);
    fs::write(dir.path().join(&name), "needle\n").unwrap();
    for pattern in [
        format!("{}b", "*a".repeat(24)),
        format!("{}b", "*".repeat(24)),
    ] {
        let result: Value = serde_json::from_str(
            &SearchFiles
                .call(json!({
                    "query": "needle", "path": dir.path(), "glob": pattern
                }))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total_matches"], 0);
    }
    let result: Value = serde_json::from_str(
        &SearchFiles
            .call(json!({
                "query": "needle", "path": dir.path(), "glob": format!("{}*", "*a".repeat(24))
            }))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["total_matches"], 1);
    assert_eq!(result["matches"][0]["path"], name);
}

#[test]
fn requires_explicit_path() {
    assert!(serde_json::from_value::<Arguments>(json!({"query": "needle"})).is_err());
    let schema = SearchFiles.parameters();
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("path"))
    );
    assert!(schema["properties"]["path"].get("default").is_none());
}

#[tokio::test]
async fn finds_literal_matches_with_glob_and_context() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "before\nneedle here\nafter\n",
    )
    .unwrap();
    fs::write(dir.path().join("README.md"), "needle docs\n").unwrap();
    let result: Value = serde_json::from_str(
        &SearchFiles
            .call(json!({
                "query": "needle", "path": dir.path(), "glob": "*.rs", "context_lines": 1
            }))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["total_matches"], 1);
    assert_eq!(result["matches"][0]["path"], "src/main.rs");
    assert_eq!(result["matches"][0]["line"], 2);
    assert_eq!(
        result["matches"][0]["context"],
        json!(["1: before", "2: needle here", "3: after"])
    );
}

#[tokio::test]
async fn validates_limits_and_empty_queries() {
    let dir = tempfile::tempdir().unwrap();
    for arguments in [
        json!({"query": "", "path": dir.path()}),
        json!({"query": "x", "path": dir.path(), "max_results": 0}),
        json!({"query": "x", "path": dir.path(), "context_lines": MAX_CONTEXT_LINES + 1}),
    ] {
        assert!(SearchFiles.call(arguments).await.is_err());
    }
}
