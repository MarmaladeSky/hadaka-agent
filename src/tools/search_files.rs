use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
use globset::{Glob, GlobMatcher};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Tool};

const MAX_RESULTS: usize = 1000;
const MAX_CONTEXT_LINES: usize = 10;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    query: String,
    path: String,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    context_lines: usize,
}

fn default_max_results() -> usize {
    100
}

pub(super) struct SearchFiles;

impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search recursively for a literal, case-sensitive query in UTF-8 text files. query and path are required; use path '.' explicitly to search the working directory. glob optionally filters file names, not relative paths, using case-sensitive globset syntax (*, ?, [ab], {a,b}); invalid patterns return an error. max_results defaults to 100 and is limited to 1000. context_lines includes up to 10 surrounding lines. Symlinked directories are not followed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "path": {"type": "string"},
                "glob": {"type": "string", "description": "Optional case-sensitive globset pattern matched against file names, not relative paths; for example *.rs, test?.rs, or *.{rs,toml}."},
                "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "default": 100},
                "context_lines": {"type": "integer", "minimum": 0, "maximum": MAX_CONTEXT_LINES, "default": 0}
            },
            "required": ["query", "path"],
            "additionalProperties": false
        })
    }

    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments)
                .context("search_files expects query and path, with optional glob, max_results, and context_lines")?;
            ensure!(!args.query.is_empty(), "query must not be empty");
            ensure!(
                (1..=MAX_RESULTS).contains(&args.max_results),
                "max_results must be between 1 and {MAX_RESULTS}"
            );
            ensure!(
                args.context_lines <= MAX_CONTEXT_LINES,
                "context_lines must be at most {MAX_CONTEXT_LINES}"
            );
            tokio::task::spawn_blocking(move || search(args))
                .await
                .context("file search failed")?
        })
    }
}

#[derive(Debug)]
struct Match {
    path: String,
    line: usize,
    text: String,
    context: Vec<String>,
}

fn search(args: Arguments) -> Result<String> {
    let matcher = args
        .glob
        .as_deref()
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .context("invalid glob pattern")
        })
        .transpose()?;
    let root = Path::new(&args.path);
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "path must refer to a directory"
    );
    let mut matches = Vec::new();
    collect(root, root, &args, matcher.as_ref(), &mut matches)?;
    let total_matches = matches.len();
    let truncated = total_matches > args.max_results;
    matches.truncate(args.max_results);
    let mut output: Vec<Value> = Vec::new();
    for item in matches {
        let value = json!({"path": item.path, "line": item.line, "text": item.text, "context": item.context});
        let serialized = serde_json::to_string(&value)?;
        if output
            .iter()
            .map(|item| item.to_string().len())
            .sum::<usize>()
            + serialized.len()
            + 1
            > MAX_OUTPUT_BYTES
        {
            break;
        }
        output.push(value);
    }
    let output_count = output.len();
    Ok(json!({
        "matches": output,
        "total_matches": total_matches,
        "truncated": truncated || output_count < total_matches.min(args.max_results)
    })
    .to_string())
}

fn collect(
    root: &Path,
    current: &Path,
    args: &Arguments,
    matcher: Option<&GlobMatcher>,
    matches: &mut Vec<Match>,
) -> Result<()> {
    for entry in fs::read_dir(current)
        .with_context(|| format!("cannot read directory {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            if !metadata.file_type().is_symlink() {
                collect(root, &path, args, matcher, matches)?;
            }
            continue;
        }
        if !metadata.is_file()
            || matcher.is_some_and(|matcher| !matcher.is_match(Path::new(&entry.file_name())))
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<_> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains(&args.query) {
                continue;
            }
            let start = index.saturating_sub(args.context_lines);
            let end = (index + args.context_lines + 1).min(lines.len());
            let context = lines[start..end]
                .iter()
                .enumerate()
                .map(|(offset, value)| format!("{}: {value}", start + offset + 1))
                .collect();
            matches.push(Match {
                path: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                line: index + 1,
                text: (*line).into(),
                context,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
