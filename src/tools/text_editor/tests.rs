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
    TextEditor
        .call(json!({"command":"str_replace", "path":link, "old_str":"before", "new_str":"after"}))
        .await
        .unwrap();
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
