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

use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Tool};

const MAX_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Create,
    Delete,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    File,
    Directory,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    operation: Operation,
    kind: Kind,
    path: String,
    content: Option<String>,
    recursive: Option<bool>,
}

pub(super) struct Filesystem;

impl Tool for Filesystem {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn description(&self) -> &str {
        "Create or delete files and directories. Requires write permission. operation, kind and path are required. Creating a file requires content (UTF-8, at most 1 MiB; empty is allowed). Creation fails if the target exists; parent directories must exist. Deletion requires an existing target of the specified kind. Directory deletion defaults to empty directories only; recursive: true deletes a directory tree without following contained symlinks. Target symlinks are rejected. Relative paths resolve from the working directory."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string", "enum": ["create", "delete"]},
                "kind": {"type": "string", "enum": ["file", "directory"]},
                "path": {"type": "string", "minLength": 1},
                "content": {"type": "string", "description": "Required only for creating files; at most 1 MiB. Use an empty string for an empty file."},
                "recursive": {"type": "boolean", "default": false, "description": "Allowed only when deleting directories."}
            },
            "required": ["operation", "kind", "path"],
            "additionalProperties": false,
            "oneOf": [
                {"properties": {"operation": {"const": "create"}, "kind": {"const": "file"}}, "required": ["content"], "not": {"required": ["recursive"]}},
                {"properties": {"operation": {"const": "create"}, "kind": {"const": "directory"}}, "not": {"anyOf": [{"required": ["content"]}, {"required": ["recursive"]}]}},
                {"properties": {"operation": {"const": "delete"}, "kind": {"const": "file"}}, "not": {"anyOf": [{"required": ["content"]}, {"required": ["recursive"]}]}},
                {"properties": {"operation": {"const": "delete"}, "kind": {"const": "directory"}}, "not": {"required": ["content"]}}
            ]
        })
    }

    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments.clone())
                .context("invalid filesystem arguments")?;
            let creates_file = args.operation == Operation::Create && args.kind == Kind::File;
            if creates_file {
                let content = args
                    .content
                    .as_ref()
                    .context("creating a file requires content")?;
                ensure!(
                    content.len() <= MAX_CONTENT_BYTES,
                    "content exceeds the 1 MiB size limit"
                );
            } else {
                ensure!(
                    arguments.get("content").is_none(),
                    "content is allowed only when creating files"
                );
            }
            if args.operation == Operation::Delete && args.kind == Kind::Directory {
                ensure!(
                    arguments.get("recursive").is_none_or(Value::is_boolean),
                    "recursive must be a boolean"
                );
            } else {
                ensure!(
                    arguments.get("recursive").is_none(),
                    "recursive is allowed only when deleting directories"
                );
            }
            tokio::task::spawn_blocking(move || execute(args))
                .await
                .context("filesystem operation failed")?
        })
    }
}

fn execute(args: Arguments) -> Result<String> {
    ensure!(!args.path.trim().is_empty(), "path must not be empty");
    // Inspect the final entry itself, even when the caller appends a separator.
    let raw = args.path.trim_end_matches(std::path::is_separator);
    let name = raw.rsplit(std::path::is_separator).next().unwrap_or("");
    ensure!(
        !matches!(name, "" | "." | ".."),
        "path must name a file or directory entry"
    );
    let path = Path::new(raw);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let path = fs::canonicalize(parent)
        .context("parent directory must exist")?
        .join(path.file_name().context("path must have a filename")?);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("cannot inspect target"),
    };
    if let Some(metadata) = &metadata {
        ensure!(
            !metadata.file_type().is_symlink(),
            "target symlinks are unsupported"
        );
    }
    let message = match (args.operation, args.kind) {
        (Operation::Create, Kind::File) => {
            ensure!(metadata.is_none(), "target already exists");
            let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            temporary.write_all(args.content.unwrap().as_bytes())?;
            temporary
                .persist_noclobber(&path)
                .context("cannot create file")?;
            "File created"
        }
        (Operation::Create, Kind::Directory) => {
            ensure!(metadata.is_none(), "target already exists");
            fs::create_dir(&path).context("cannot create directory")?;
            "Directory created"
        }
        (Operation::Delete, Kind::File) => {
            ensure!(
                metadata.context("target does not exist")?.is_file(),
                "target must be a regular file"
            );
            fs::remove_file(&path).context("cannot delete file")?;
            "File deleted"
        }
        (Operation::Delete, Kind::Directory) => {
            ensure!(
                metadata.context("target does not exist")?.is_dir(),
                "target must be a directory"
            );
            if args.recursive.unwrap_or(false) {
                fs::remove_dir_all(&path).context("cannot delete directory tree")?;
            } else {
                fs::remove_dir(&path).context(
                    "cannot delete directory (non-empty directories require recursive: true)",
                )?;
            }
            "Directory deleted"
        }
    };
    Ok(format!("{message}: {}", path.display()))
}

#[cfg(test)]
mod tests;
