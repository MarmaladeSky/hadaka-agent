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

use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Tool};

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Arguments {
    Create {
        path: String,
        file_text: String,
    },
    StrReplace {
        path: String,
        old_str: String,
        new_str: String,
    },
    Insert {
        path: String,
        insert_line: usize,
        insert_text: String,
    },
}

pub(super) struct TextEditor;

impl Tool for TextEditor {
    fn name(&self) -> &str {
        "text_editor"
    }
    fn description(&self) -> &str {
        "Modify UTF-8 text files (at most 1 MiB). create requires file_text and fails if the path exists. str_replace requires nonempty old_str matching exactly once, and new_str (empty deletes). insert requires insert_line (0 prepends; N inserts after line N; last line appends) and insert_text. Insertion adds line separators when needed and preserves CRLF in CRLF files. Relative paths use the working directory. Parent directories must exist. Use read_file to inspect content before editing."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "enum": ["create", "str_replace", "insert"]},
                "path": {"type": "string"},
                "file_text": {"type": "string", "description": "Required only for create."},
                "old_str": {"type": "string", "description": "Required only for str_replace; exact text including whitespace."},
                "new_str": {"type": "string", "description": "Required only for str_replace."},
                "insert_line": {"type": "integer", "minimum": 0, "description": "Required only for insert; insert after this line, or 0 to prepend."},
                "insert_text": {"type": "string", "description": "Required only for insert."}
            },
            "required": ["command", "path"],
            "additionalProperties": false
        })
    }
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments =
                serde_json::from_value(arguments).context("invalid text_editor arguments")?;
            tokio::task::spawn_blocking(move || execute(args))
                .await
                .context("file editor failed")?
        })
    }
}

fn validate(text: &str) -> Result<()> {
    ensure!(text.len() <= MAX_BYTES, "file exceeds the 1 MiB size limit");
    ensure!(!text.contains('\0'), "binary files are unsupported");
    Ok(())
}

fn execute(args: Arguments) -> Result<String> {
    let path = match &args {
        Arguments::Create { path, .. }
        | Arguments::StrReplace { path, .. }
        | Arguments::Insert { path, .. } => path,
    };
    ensure!(!path.trim().is_empty(), "path must not be empty");
    if let Arguments::Create { path, file_text } = &args {
        validate(file_text)?;
        let path = Path::new(path);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).context("cannot create temporary file")?;
        temporary.write_all(file_text.as_bytes())?;
        temporary
            .persist_noclobber(path)
            .context("cannot create file (path may already exist)")?;
        return Ok("File created.".into());
    }
    let path = fs::canonicalize(path).context("cannot resolve file path")?;
    let metadata = fs::metadata(&path).context("cannot inspect file")?;
    ensure!(metadata.is_file(), "path must refer to a regular file");
    ensure!(
        metadata.len() <= MAX_BYTES as u64,
        "file exceeds the 1 MiB size limit"
    );
    ensure!(!metadata.permissions().readonly(), "file is read-only");
    let mut original = String::new();
    fs::File::open(&path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_string(&mut original)
        .context("cannot read UTF-8 file")?;
    validate(&original)?;
    let updated = match args {
        Arguments::StrReplace {
            old_str, new_str, ..
        } => {
            ensure!(!old_str.is_empty(), "old_str must not be empty");
            // Count overlapping occurrences too, so the target is unambiguous.
            let matches = original
                .char_indices()
                .filter(|(i, _)| original[*i..].starts_with(&old_str))
                .count();
            ensure!(
                matches == 1,
                "old_str must match exactly once; found {matches} matches"
            );
            original.replacen(&old_str, &new_str, 1)
        }
        Arguments::Insert {
            insert_line,
            insert_text,
            ..
        } => {
            ensure!(!insert_text.is_empty(), "insert_text must not be empty");
            let lines: Vec<_> = original.split_inclusive('\n').collect();
            ensure!(
                insert_line <= lines.len(),
                "insert_line exceeds the {} lines in the file",
                lines.len()
            );
            let offset: usize = lines.iter().take(insert_line).map(|line| line.len()).sum();
            let (before, after) = original.split_at(offset);
            let newline = if original.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let insert_text = if newline == "\r\n" {
                insert_text.replace("\r\n", "\n").replace('\n', "\r\n")
            } else {
                insert_text
            };
            let mut result = before.to_owned();
            if !before.is_empty() && !before.ends_with('\n') {
                result.push_str(newline);
            }
            result.push_str(&insert_text);
            if !after.is_empty() && !insert_text.ends_with('\n') {
                result.push_str(newline);
            }
            result.push_str(after);
            result
        }
        Arguments::Create { .. } => unreachable!(),
    };
    validate(&updated)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("file has no parent")?)?;
    temporary.write_all(updated.as_bytes())?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())?;
    temporary.persist(&path).context("cannot replace file")?;
    Ok("File updated.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_replaces_deletes_and_inserts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        for args in [
            json!({"command":"create", "path":path, "file_text":"one\ntwo"}),
            json!({"command":"insert", "path":path, "insert_line":0, "insert_text":"zero"}),
            json!({"command":"insert", "path":path, "insert_line":3, "insert_text":"three"}),
            json!({"command":"str_replace", "path":path, "old_str":"two", "new_str":"世界"}),
            json!({"command":"str_replace", "path":path, "old_str":"one\n", "new_str":""}),
        ] {
            TextEditor.call(args).await.unwrap();
        }
        assert_eq!(fs::read_to_string(path).unwrap(), "zero\n世界\nthree");
    }

    #[tokio::test]
    async fn invalid_edits_leave_original_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, "aaa\n").unwrap();
        for args in [
            json!({"command":"create", "path":path, "file_text":"new"}),
            json!({"command":"str_replace", "path":path, "old_str":"aa", "new_str":"new"}),
            json!({"command":"str_replace", "path":path, "old_str":"missing", "new_str":"new"}),
            json!({"command":"str_replace", "path":path, "old_str":"", "new_str":"new"}),
            json!({"command":"insert", "path":path, "insert_line":2, "insert_text":"new"}),
            json!({"command":"insert", "path":path, "insert_line":0, "insert_text":"\u{0000}"}),
            json!({"command":"insert", "path":path, "insert_line":0, "insert_text":"a".repeat(MAX_BYTES)}),
            json!({"command":"insert", "path":path, "insert_line":0}),
            json!({"command":"insert", "path":path, "insert_line":-1, "insert_text":"new"}),
        ] {
            assert!(TextEditor.call(args).await.is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "aaa\n");
        }
    }

    #[tokio::test]
    async fn rejects_missing_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let args = json!({"command":"insert", "path":path, "insert_line":0, "insert_text":"text"});
        assert!(TextEditor.call(args.clone()).await.is_err());
        for bytes in [vec![255], vec![0], vec![b'a'; MAX_BYTES + 1]] {
            fs::write(&path, &bytes).unwrap();
            assert!(TextEditor.call(args.clone()).await.is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edits_preserve_permissions_and_follow_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let link = dir.path().join("link");
        fs::write(&path, "before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&path, &link).unwrap();
        TextEditor.call(json!({"command":"str_replace", "path":link, "old_str":"before", "new_str":"after"})).await.unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "after");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[tokio::test]
    async fn inserts_into_empty_files_and_preserves_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        for (source, line, inserted, expected) in [
            ("", 0, "hello", "hello"),
            ("a\r\nb\r\n", 1, "x\ny", "a\r\nx\r\ny\r\nb\r\n"),
            ("a\n", 1, "b", "a\nb"),
        ] {
            fs::write(&path, source).unwrap();
            TextEditor.call(json!({"command":"insert", "path":path, "insert_line":line, "insert_text":inserted})).await.unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        }
    }
}
