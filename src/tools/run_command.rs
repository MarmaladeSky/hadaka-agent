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
mod tests;
