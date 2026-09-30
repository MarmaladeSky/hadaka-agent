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

// A portable subprocess fixture that also launches a descendant. Both wait
// for the test to release them before attempting their observable writes.
#[test]
#[ignore = "subprocess fixture"]
fn process_cleanup_fixture() {
    let dir = std::path::PathBuf::from(std::env::var_os("HADAKA_PROCESS_FIXTURE_DIR").unwrap());
    let role = std::env::var("HADAKA_PROCESS_FIXTURE_ROLE").unwrap();
    let mut child = if role == "parent" {
        Some(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "tools::run_command::tests::process_cleanup_fixture",
                ])
                .env("HADAKA_PROCESS_FIXTURE_ROLE", "descendant")
                .spawn()
                .unwrap(),
        )
    } else {
        None
    };
    std::fs::write(
        dir.join(format!("{role}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    if role == "parent" && std::env::var("HADAKA_PROCESS_FIXTURE_EXIT").as_deref() == Ok("1") {
        while !dir.join("descendant.pid").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Deliberately orphan the descendant to exercise cleanup after the
        // group leader exits. The test's command owner must terminate it.
        drop(child);
        return;
    }
    while !dir.join("release").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(dir.join(format!("{role}.wrote")), "survived").unwrap();
    if let Some(child) = &mut child {
        child.wait().unwrap();
    }
}

#[cfg(unix)]
async fn wait_for_fixture(dir: &std::path::Path) {
    timeout(Duration::from_secs(5), async {
        while !dir.join("parent.pid").exists() || !dir.join("descendant.pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture must start before testing cleanup");
}

#[cfg(unix)]
async fn check_process_cleanup(cancel: bool, parent_exits: bool) {
    let dir = tempfile::tempdir().unwrap();
    // Launch the fixture through a shell solely to set its environment;
    // exec ensures the fixture itself is the tool's direct child.
    let arguments = json!({
        "program": "sh",
        "args": ["-c", "export HADAKA_PROCESS_FIXTURE_DIR=\"$1\" HADAKA_PROCESS_FIXTURE_ROLE=parent HADAKA_PROCESS_FIXTURE_EXIT=\"$3\"; exec \"$2\" --ignored --exact tools::run_command::tests::process_cleanup_fixture", "fixture", dir.path(), std::env::current_exe().unwrap(), if parent_exits { "1" } else { "0" }],
        "timeout_ms": if cancel { 10_000 } else { 1_500 }
    });
    let runner = RunCommand::default();
    let task_runner = runner.clone();
    let task = tokio::spawn(async move { task_runner.call(arguments).await });
    wait_for_fixture(dir.path()).await;
    if parent_exits {
        let pid = std::fs::read_to_string(dir.path().join("parent.pid")).unwrap();
        timeout(Duration::from_secs(5), async {
            while std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("group leader must exit before testing cleanup");
    }
    if cancel {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    } else {
        let error = task.await.unwrap().unwrap_err();
        assert!(format!("{error:#}").contains("command timed out"));
    }
    runner.shutdown().await.unwrap();
    std::fs::write(dir.path().join("release"), "go").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    for role in ["parent", "descendant"] {
        assert!(
            !dir.path().join(format!("{role}.wrote")).exists(),
            "{role} wrote after cleanup"
        );
    }
    #[cfg(unix)]
    {
        let pid = std::fs::read_to_string(dir.path().join("parent.pid")).unwrap();
        let status = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "direct child must be terminated and reaped"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_terminates_command_and_descendants() {
    check_process_cleanup(false, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_terminates_command_and_descendants() {
    check_process_cleanup(true, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_terminates_descendants_after_parent_exits() {
    check_process_cleanup(false, true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_terminates_descendants_after_parent_exits() {
    check_process_cleanup(true, true).await;
}

#[tokio::test]
async fn runs_argv_without_a_shell_and_captures_output() {
    let result: Value = RunCommand::default()
        .call(json!({
            "program": "printf", "args": ["hello"]
        }))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "hello");
    assert_eq!(result["stderr"], "");
    assert_eq!(result["timed_out"], false);
}

#[tokio::test]
async fn validates_timeout_and_reports_failures() {
    assert!(
        RunCommand::default()
            .call(json!({"program":"printf", "timeout_ms": 0}))
            .await
            .is_err()
    );
    let result: Value = RunCommand::default()
        .call(json!({"program":"sh", "args":["-c", "exit 7"]}))
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["success"], false);
}
