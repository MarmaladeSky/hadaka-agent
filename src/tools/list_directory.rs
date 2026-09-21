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
mod tests {
    use super::*;

    #[tokio::test]
    async fn depth_limits_return_only_requested_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::create_dir(dir.path().join("tests")).unwrap();
        fs::write(dir.path().join("main.rs"), "x").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "xx").unwrap();
        fs::write(dir.path().join("src/nested/deep.rs"), "xxx").unwrap();
        fs::write(dir.path().join("tests/test.rs"), "xxxx").unwrap();

        for (depth, expected) in [
            (0, json!([{"path": ".", "type": "directory"}])),
            (
                1,
                json!([
                    {"path": "main.rs", "type": "file", "size": 1},
                    {"path": "src", "type": "directory"},
                    {"path": "tests", "type": "directory"}
                ]),
            ),
            (
                2,
                json!([
                    {"path": "main.rs", "type": "file", "size": 1},
                    {"path": "src", "type": "directory"},
                    {"path": "src/lib.rs", "type": "file", "size": 2},
                    {"path": "src/nested", "type": "directory"},
                    {"path": "tests", "type": "directory"},
                    {"path": "tests/test.rs", "type": "file", "size": 4}
                ]),
            ),
        ] {
            let result: Value = serde_json::from_str(
                &ListDirectory
                    .call(json!({"path": dir.path(), "depth": depth}))
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["entries"], expected, "depth {depth}");
            assert_eq!(result["truncated"], false);
        }
    }

    #[tokio::test]
    async fn lists_sorted_entries_and_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("z-dir")).unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::write(dir.path().join("z-dir").join("nested"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("z-dir"), dir.path().join("link")).unwrap();
        let result: Value = serde_json::from_str(
            &ListDirectory
                .call(json!({"path": dir.path(), "depth": 2}))
                .await
                .unwrap(),
        )
        .unwrap();
        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries[0]["path"], "a.txt");
        assert_eq!(entries[0]["type"], "file");
        assert_eq!(entries[0]["size"], 5);
        assert!(entries.iter().any(|entry| entry["path"] == "z-dir/nested"));
        #[cfg(unix)]
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry["path"] == "link")
                .unwrap()["type"],
            "symlink"
        );
    }

    #[tokio::test]
    async fn validates_paths_depth_and_file_limits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        for arguments in [
            json!({"path": file}),
            json!({"path": dir.path(), "depth": MAX_DEPTH + 1}),
            json!({"path": dir.path(), "depth": 0, "extra": true}),
        ] {
            assert!(ListDirectory.call(arguments).await.is_err());
        }
    }
}
