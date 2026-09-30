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
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Tool};

const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 1000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    #[serde(default = "default_depth")]
    depth: usize,
}

fn default_depth() -> usize {
    1
}

pub(super) struct ListDirectory;

impl Tool for ListDirectory {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List files and directories below a path. path is required and depth defaults to 1 (the directory's immediate children); depth 0 lists only the requested directory entry, and depth must be at most 8. Results are sorted by relative path and include type, size for files, and a truncated flag. Symlinks are reported but never followed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "depth": {"type": "integer", "minimum": 0, "maximum": MAX_DEPTH, "default": 1}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments)
                .context("list_directory expects path and optional depth")?;
            ensure!(args.depth <= MAX_DEPTH, "depth must be at most {MAX_DEPTH}");
            tokio::task::spawn_blocking(move || list(args))
                .await
                .context("directory lister failed")?
        })
    }
}

fn list(args: Arguments) -> Result<String> {
    ensure!(!args.path.trim().is_empty(), "path must not be empty");
    let root = Path::new(&args.path);
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("cannot inspect path {}", root.display()))?;
    ensure!(metadata.is_dir(), "path must refer to a directory");

    let mut entries = Vec::new();
    collect(root, root, args.depth, &mut entries)?;
    entries.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    let mut truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    let mut result = json!({
        "path": args.path,
        "entries": entries,
        "truncated": truncated
    });
    while result.to_string().len() > MAX_OUTPUT_BYTES
        && result["entries"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    {
        result["entries"].as_array_mut().unwrap().pop();
        truncated = true;
        result["truncated"] = json!(truncated);
    }
    Ok(result.to_string())
}

fn collect(root: &Path, current: &Path, depth: usize, entries: &mut Vec<Value>) -> Result<()> {
    if depth == 0 {
        entries.push(json!({"path": ".", "type": "directory"}));
        return Ok(());
    }
    for entry in fs::read_dir(current)
        .with_context(|| format!("cannot read directory {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(&path)?;
        let file_type = if metadata.file_type().is_symlink() {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        let mut item = json!({"path": relative, "type": file_type});
        if metadata.is_file() {
            item["size"] = json!(metadata.len());
        }
        entries.push(item);
        if depth > 1 && metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect(root, &path, depth - 1, entries)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
