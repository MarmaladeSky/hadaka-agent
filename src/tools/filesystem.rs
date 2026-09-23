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
mod tests {
    use super::*;
    use crate::{
        model::{FunctionCall, ToolCall},
        tools::{PermissionPolicy, Tools},
    };

    #[tokio::test]
    async fn creates_and_deletes_files_and_directories() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("nested");
        Filesystem
            .call(json!({"operation":"create","kind":"directory","path":directory}))
            .await
            .unwrap();
        for content in ["", "hello 世界\n"] {
            let file = directory.join("file");
            Filesystem
                .call(json!({"operation":"create","kind":"file","path":file,"content":content}))
                .await
                .unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), content);
            Filesystem
                .call(json!({"operation":"delete","kind":"file","path":file}))
                .await
                .unwrap();
            assert!(!file.exists());
        }
        Filesystem
            .call(json!({"operation":"delete","kind":"directory","path":directory}))
            .await
            .unwrap();
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn invalid_requests_preserve_existing_entries() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        let directory = root.path().join("directory");
        fs::write(&file, "original").unwrap();
        fs::create_dir(&directory).unwrap();
        for args in [
            json!({"operation":"create","kind":"file","path":file,"content":"replacement"}),
            json!({"operation":"create","kind":"directory","path":file}),
            json!({"operation":"create","kind":"file","path":directory,"content":"replacement"}),
            json!({"operation":"create","kind":"directory","path":directory}),
            json!({"operation":"delete","kind":"directory","path":file}),
            json!({"operation":"delete","kind":"file","path":directory}),
            json!({"operation":"delete","kind":"file","path":file,"recursive":false}),
            json!({"operation":"delete","kind":"file","path":file,"content":null}),
            json!({"operation":"delete","kind":"file","path":file,"unknown":true}),
            json!({"operation":"delete","kind":"directory","path":directory,"recursive":null}),
            json!({"operation":"delete","kind":"directory","path":directory,"recursive":"true"}),
        ] {
            assert!(
                Filesystem.call(args.clone()).await.is_err(),
                "accepted {args}"
            );
            assert_eq!(fs::read_to_string(&file).unwrap(), "original");
            assert!(directory.is_dir());
        }
    }

    #[tokio::test]
    async fn validates_create_arguments_and_missing_targets() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("new");
        for args in [
            json!({"operation":"create","kind":"file","path":path}),
            json!({"operation":"create","kind":"file","path":path,"content":null}),
            json!({"operation":"create","kind":"file","path":path,"content":123}),
            json!({"operation":"create","kind":"file","path":path,"content":"x".repeat(MAX_CONTENT_BYTES+1)}),
            json!({"operation":"create","kind":"directory","path":path,"content":""}),
            json!({"operation":"create","kind":"directory","path":path,"recursive":true}),
            json!({"operation":"create","kind":"file","content":""}),
            json!({"operation":"create","path":path,"content":""}),
            json!({"kind":"file","path":path,"content":""}),
            json!({"operation":"rename","kind":"file","path":path}),
            json!({"operation":"create","kind":"file","path":" ","content":""}),
            json!({"operation":"delete","kind":"file","path":path}),
            json!({"operation":"delete","kind":"directory","path":path,"recursive":true}),
        ] {
            assert!(Filesystem.call(args).await.is_err());
            assert!(!path.exists());
        }
        for kind in ["file", "directory"] {
            let mut args = json!({"operation":"create","kind":kind,"path":path.join("child")});
            if kind == "file" {
                args["content"] = json!("");
            }
            assert!(Filesystem.call(args).await.is_err());
            assert!(!path.exists());
        }
    }

    #[tokio::test]
    async fn recursive_deletion_is_explicit() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("tree");
        fs::create_dir_all(directory.join("nested")).unwrap();
        let file = directory.join("nested/file");
        fs::write(&file, "keep").unwrap();
        for recursive in [None, Some(false)] {
            let mut args = json!({"operation":"delete","kind":"directory","path":directory});
            if let Some(recursive) = recursive {
                args["recursive"] = json!(recursive);
            }
            assert!(Filesystem.call(args).await.is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
        }
        Filesystem
            .call(
                json!({"operation":"delete","kind":"directory","path":directory,"recursive":true}),
            )
            .await
            .unwrap();
        assert!(!directory.exists());
    }

    async fn call(tools: &Tools, args: Value) -> String {
        tools
            .call(&ToolCall {
                id: "filesystem-test".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "filesystem".into(),
                    arguments: args.to_string(),
                },
            })
            .await
    }

    #[tokio::test]
    async fn write_permission_is_required_and_sufficient() {
        let root = tempfile::tempdir().unwrap();
        let allowed = root.path().join("allowed");
        fs::create_dir(&allowed).unwrap();
        let denied = Tools::with_policy(PermissionPolicy::new(vec![], vec![], vec![]));
        let read_only = Tools::with_policy(PermissionPolicy::new(
            vec![root.path().to_owned()],
            vec![],
            vec![],
        ));
        let writable =
            Tools::with_policy(PermissionPolicy::new(vec![], vec![allowed.clone()], vec![]));
        for kind in ["file", "directory"] {
            for tools in [&denied, &read_only, &writable] {
                let path = if std::ptr::eq(tools, &writable) {
                    allowed.join("../outside")
                } else {
                    allowed.join("entry")
                };
                let mut args = json!({"operation":"create","kind":kind,"path":path});
                if kind == "file" {
                    args["content"] = json!("");
                }
                assert!(call(tools, args).await.contains("permission denied: write"));
                assert!(!path.exists());
                if kind == "file" {
                    fs::write(&path, "keep").unwrap();
                } else {
                    fs::create_dir(&path).unwrap();
                }
                assert!(
                    call(tools, json!({"operation":"delete","kind":kind,"path":path}))
                        .await
                        .contains("permission denied: write")
                );
                assert!(path.exists());
                if kind == "file" {
                    fs::remove_file(path).unwrap();
                } else {
                    fs::remove_dir(path).unwrap();
                }
            }
            let path = allowed.join("entry");
            let mut args = json!({"operation":"create","kind":kind,"path":path});
            if kind == "file" {
                args["content"] = json!("");
            }
            let result = call(&writable, args).await;
            assert!(!result.starts_with("Error:"), "{result}");
            assert!(path.exists());
            let result = call(
                &writable,
                json!({"operation":"delete","kind":kind,"path":path}),
            )
            .await;
            assert!(!result.starts_with("Error:"), "{result}");
            assert!(!path.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_redirect_mutations_or_recursive_deletion() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let allowed = root.path().join("allowed");
        let outside = root.path().join("outside");
        fs::create_dir(&allowed).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), "keep").unwrap();
        let link = allowed.join("link");
        symlink(&outside, &link).unwrap();
        let writable =
            Tools::with_policy(PermissionPolicy::new(vec![], vec![allowed.clone()], vec![]));
        for args in [
            json!({"operation":"create","kind":"file","path":link.join("new"),"content":"new"}),
            json!({"operation":"delete","kind":"file","path":link.join("keep")}),
            json!({"operation":"delete","kind":"directory","path":link,"recursive":true}),
        ] {
            assert!(
                call(&writable, args)
                    .await
                    .contains("permission denied: write")
            );
        }
        // Even with write permission for the target, final symlinks are rejected.
        for path in [link.clone(), link.join("")] {
            assert!(Filesystem.call(json!({"operation":"delete","kind":"directory","path":path,"recursive":true})).await.is_err());
        }
        let dangling = allowed.join("dangling");
        symlink(outside.join("missing"), &dangling).unwrap();
        assert!(
            Filesystem
                .call(json!({"operation":"create","kind":"file","path":dangling,"content":"new"}))
                .await
                .is_err()
        );
        let result = call(
            &writable,
            json!({"operation":"delete","kind":"directory","path":allowed,"recursive":true}),
        )
        .await;
        assert!(!result.starts_with("Error:"), "{result}");
        assert!(!allowed.exists());
        assert_eq!(fs::read_to_string(outside.join("keep")).unwrap(), "keep");
        assert!(!outside.join("new").exists());
        assert!(!outside.join("missing").exists());
    }
}
