use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
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
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    context_lines: usize,
}

fn default_path() -> String {
    ".".into()
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
        "Search recursively for a literal, case-sensitive query in UTF-8 text files. path defaults to the working directory. glob optionally filters file names (simple * wildcards). max_results defaults to 100 and is limited to 1000. context_lines includes up to 10 surrounding lines. Symlinked directories are not followed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "path": {"type": "string", "default": "."},
                "glob": {"type": "string", "description": "Optional file-name filter, for example *.rs or Cargo.toml."},
                "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "default": 100},
                "context_lines": {"type": "integer", "minimum": 0, "maximum": MAX_CONTEXT_LINES, "default": 0}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments)
                .context("search_files expects query and optional path, glob, max_results, and context_lines")?;
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
    let root = Path::new(&args.path);
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "path must refer to a directory"
    );
    let mut matches = Vec::new();
    collect(root, root, &args, &mut matches)?;
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

fn collect(root: &Path, current: &Path, args: &Arguments, matches: &mut Vec<Match>) -> Result<()> {
    for entry in fs::read_dir(current)
        .with_context(|| format!("cannot read directory {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            if !metadata.file_type().is_symlink() {
                collect(root, &path, args, matches)?;
            }
            continue;
        }
        if !metadata.is_file()
            || args
                .glob
                .as_deref()
                .is_some_and(|pattern| !matches_glob(&entry.file_name().to_string_lossy(), pattern))
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

fn matches_glob(name: &str, pattern: &str) -> bool {
    wildcard(name.as_bytes(), pattern.as_bytes())
}

fn wildcard(value: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    if pattern[0] == b'*' {
        wildcard(value, &pattern[1..]) || (!value.is_empty() && wildcard(&value[1..], pattern))
    } else {
        !value.is_empty() && pattern[0] == value[0] && wildcard(&value[1..], &pattern[1..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            json!({"query": ""}),
            json!({"query": "x", "path": dir.path(), "max_results": 0}),
            json!({"query": "x", "path": dir.path(), "context_lines": MAX_CONTEXT_LINES + 1}),
        ] {
            assert!(SearchFiles.call(arguments).await.is_err());
        }
    }
}
