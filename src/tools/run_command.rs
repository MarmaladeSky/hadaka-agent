use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt, process::Command, task::JoinHandle, time::timeout};

use super::{BoxFuture, Tool};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Clone, Default)]
pub(super) struct RunCommand {
    cleanup: Arc<Mutex<Vec<JoinHandle<Result<()>>>>>,
}

impl RunCommand {
    pub(super) async fn shutdown(&self) -> Result<()> {
        let tasks = std::mem::take(&mut *self.cleanup.lock().unwrap());
        let mut errors = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(format!("{error:#}")),
                Err(error) => errors.push(error.to_string()),
            }
        }
        ensure!(
            errors.is_empty(),
            "command cleanup failed: {}",
            errors.join("; ")
        );
        Ok(())
    }
}

// Own the process tree independently of the cancellable output-reading future.
// Drop signals it synchronously; Tools::shutdown awaits the queued reaping work.
struct OwnedCommand {
    child: Option<Box<dyn ChildWrapper>>,
    owner: RunCommand,
}

fn start_termination(child: &mut dyn ChildWrapper) -> Result<()> {
    if let Err(error) = child.start_kill() {
        // A command that exited normally may no longer have a process group.
        if child.try_wait()?.is_none() {
            return Err(error).context("cannot terminate command tree");
        }
    }
    Ok(())
}

impl OwnedCommand {
    async fn finish(&mut self) -> Result<()> {
        let child = self.child.as_mut().unwrap();
        start_termination(child.as_mut())?;
        child.wait().await.context("cannot reap command")?;
        self.child.take();
        Ok(())
    }
}

impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let termination = start_termination(child.as_mut());
            let task = tokio::spawn(async move {
                termination?;
                child
                    .wait()
                    .await
                    .context("cannot reap cancelled command")?;
                Ok(())
            });
            self.owner.cleanup.lock().unwrap().push(task);
        }
    }
}

impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }
    fn description(&self) -> &str {
        "Run an explicitly approved executable without a shell. program is required; args and cwd are optional; timeout_ms defaults to 120000 and is limited to 300000. stdout and stderr are captured and each is limited to 64 KiB."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "program": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}, "default": []},
                "cwd": {"type": "string"},
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS}
            },
            "required": ["program"],
            "additionalProperties": false
        })
    }
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments)
                .context("run_command expects program and optional args, cwd, and timeout_ms")?;
            ensure!(!args.program.trim().is_empty(), "program must not be empty");
            ensure!(
                (1..=MAX_TIMEOUT_MS).contains(&args.timeout_ms),
                "timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"
            );
            let mut command = Command::new(&args.program);
            command
                .args(&args.args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(cwd) = args.cwd {
                command.current_dir(cwd);
            }
            let mut command = CommandWrap::from(command);
            command.wrap(KillOnDrop);
            #[cfg(unix)]
            command.wrap(ProcessGroup::leader());
            #[cfg(windows)]
            command.wrap(JobObject);
            let child = command
                .spawn()
                .with_context(|| format!("cannot execute {}", args.program))?;
            let mut owned = OwnedCommand {
                child: Some(child),
                owner: self.clone(),
            };
            let child = owned.child.as_mut().unwrap();
            let mut stdout_pipe = child
                .stdout()
                .take()
                .context("command stdout unavailable")?;
            let mut stderr_pipe = child
                .stderr()
                .take()
                .context("command stderr unavailable")?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let output = timeout(Duration::from_millis(args.timeout_ms), async {
                tokio::try_join!(
                    child.wait(),
                    stdout_pipe.read_to_end(&mut stdout),
                    stderr_pipe.read_to_end(&mut stderr)
                )
            })
            .await;
            owned.finish().await.context("command cleanup failed")?;
            let status = match output {
                Ok(result) => result.context("cannot collect command output")?.0,
                Err(_) => bail!("command timed out"),
            };
            let stdout = truncate(stdout);
            let stderr = truncate(stderr);
            Ok(json!({
                "exit_code": status.code(),
                "success": status.success(),
                "stdout": stdout.text,
                "stderr": stderr.text,
                "stdout_truncated": stdout.truncated,
                "stderr_truncated": stderr.truncated,
                "timed_out": false
            })
            .to_string())
        })
    }
}

struct Truncated {
    text: String,
    truncated: bool,
}

fn truncate(bytes: Vec<u8>) -> Truncated {
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_OUTPUT_BYTES)];
    Truncated {
        text: String::from_utf8_lossy(bytes).into_owned(),
        truncated,
    }
}

#[cfg(test)]
mod tests {
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
}
