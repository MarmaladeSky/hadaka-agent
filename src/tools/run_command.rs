use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{process::Command, time::timeout};

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

pub(super) struct RunCommand;

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
            let output = timeout(Duration::from_millis(args.timeout_ms), command.output())
                .await
                .context("command timed out")?
                .with_context(|| format!("cannot execute {}", args.program))?;
            let stdout = truncate(output.stdout);
            let stderr = truncate(output.stderr);
            Ok(json!({
                "exit_code": output.status.code(),
                "success": output.status.success(),
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

    #[tokio::test]
    async fn runs_argv_without_a_shell_and_captures_output() {
        let result: Value = RunCommand
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
            RunCommand
                .call(json!({"program":"printf", "timeout_ms": 0}))
                .await
                .is_err()
        );
        let result: Value = RunCommand
            .call(json!({"program":"sh", "args":["-c", "exit 7"]}))
            .await
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["success"], false);
    }
}
