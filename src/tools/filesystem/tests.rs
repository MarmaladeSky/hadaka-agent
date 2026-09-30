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
use crate::{
    model::ToolCall,
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
        .call(json!({"operation":"delete","kind":"directory","path":directory,"recursive":true}))
        .await
        .unwrap();
    assert!(!directory.exists());
}

async fn call(tools: &Tools, args: Value) -> String {
    tools
        .call(&ToolCall {
            id: "filesystem-test".into(),
            name: "filesystem".into(),
            arguments: args.to_string(),
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
    let writable = Tools::with_policy(PermissionPolicy::new(vec![], vec![allowed.clone()], vec![]));
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
    let writable = Tools::with_policy(PermissionPolicy::new(vec![], vec![allowed.clone()], vec![]));
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
        assert!(
            Filesystem
                .call(json!({"operation":"delete","kind":"directory","path":path,"recursive":true}))
                .await
                .is_err()
        );
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
