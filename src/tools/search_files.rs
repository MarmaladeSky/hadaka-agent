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
mod tests;
