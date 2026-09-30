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
