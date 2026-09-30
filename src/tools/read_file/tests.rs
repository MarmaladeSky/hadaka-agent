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
async fn pages_unicode_crlf_blank_lines_and_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text");
    std::fs::write(&path, "hello\r\n世界\r\n\r\nlast").unwrap();
    let result: Value = serde_json::from_str(
        &ReadFile
            .call(json!({"path": path, "offset": 1, "limit": 2}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["content"], "2: 世界\n3: \n");
    assert_eq!(result["next_offset"], 3);
    assert_eq!(result["has_more"], true);
    for offset in [4, usize::MAX] {
        let result: Value = serde_json::from_str(
            &ReadFile
                .call(json!({"path": path, "offset": offset, "limit": 2}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["content"], "");
        assert_eq!(result["has_more"], false);
        assert!(result["next_offset"].is_null());
    }
}

#[tokio::test]
async fn rejects_invalid_arguments_and_unreadable_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text");
    for args in [
        json!({"path": path, "offset": 0}),
        json!({"path": path, "offset": -1, "limit": 1}),
        json!({"path": path, "offset": 0, "limit": 0}),
        json!({"path": path, "offset": 0, "limit": 1001}),
        json!({"path": path, "offset": 0, "limit": 1}),
        json!({"path": dir.path(), "offset": 0, "limit": 1}),
    ] {
        assert!(ReadFile.call(args).await.is_err());
    }
    for bytes in [
        vec![0],
        vec![255],
        vec![b'a'; MAX_FILE_BYTES + 1],
        vec![b'a'; MAX_OUTPUT_BYTES],
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(
            ReadFile
                .call(json!({"path": path, "offset": 0, "limit": 1}))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn output_cap_preserves_whole_lines_and_next_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text");
    std::fs::write(&path, format!("{}\n", "a".repeat(40000)).repeat(2)).unwrap();
    let result: Value = serde_json::from_str(
        &ReadFile
            .call(json!({"path": path, "offset": 0, "limit": 2}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["next_offset"], 1);
    assert_eq!(result["total_lines"], 2);
    assert!(result["content"].as_str().unwrap().len() <= MAX_OUTPUT_BYTES);
}
