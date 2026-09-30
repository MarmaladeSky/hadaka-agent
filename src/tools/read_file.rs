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

use std::{fs::File, io::Read};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_LINES: usize = 1000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    offset: usize,
    limit: usize,
}

use super::{BoxFuture, Tool};

pub(super) struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file. Relative paths resolve from the working directory. offset is zero-based and limit is measured in lines (1–1000). Returns numbered text, total_lines, has_more, and next_offset (null at EOF). Files must be regular files of at most 1 MiB; output is capped at 64 KiB of numbered text, on whole-line boundaries. Binary files are unsupported."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LINES}
            },
            "required": ["path", "offset", "limit"],
            "additionalProperties": false
        })
    }
    fn call(&self, arguments: Value) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_value(arguments).context(
                "read_file expects path (string), offset and limit (nonnegative integers)",
            )?;
            ensure!(!args.path.trim().is_empty(), "path must not be empty");
            ensure!(
                (1..=MAX_LINES).contains(&args.limit),
                "limit must be between 1 and {MAX_LINES}"
            );
            tokio::task::spawn_blocking(move || read(args))
                .await
                .context("file reader failed")?
        })
    }
}

fn read(args: Arguments) -> Result<String> {
    let metadata = std::fs::metadata(&args.path)
        .with_context(|| format!("cannot inspect file {}", args.path))?;
    ensure!(metadata.is_file(), "path must refer to a regular file");
    ensure!(
        metadata.len() <= MAX_FILE_BYTES as u64,
        "file exceeds the 1 MiB size limit"
    );
    let file = File::open(&args.path).with_context(|| format!("cannot open file {}", args.path))?;
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read file {}", args.path))?;
    ensure!(
        bytes.len() <= MAX_FILE_BYTES,
        "file exceeds the 1 MiB size limit"
    );
    ensure!(!bytes.contains(&0), "binary files are unsupported");
    let text = String::from_utf8(bytes).context("file must contain UTF-8 text")?;
    let lines: Vec<_> = text.lines().collect();
    let mut content = String::new();
    let mut returned = 0;
    for (index, line) in lines.iter().enumerate().skip(args.offset).take(args.limit) {
        let numbered = format!("{}: {line}\n", index + 1);
        if content.len() + numbered.len() > MAX_OUTPUT_BYTES {
            ensure!(
                returned > 0,
                "line {} exceeds the 64 KiB output limit",
                index + 1
            );
            break;
        }
        content.push_str(&numbered);
        returned += 1;
    }
    let next = args.offset.saturating_add(returned);
    let has_more = next < lines.len();
    Ok(json!({
        "content": content,
        "total_lines": lines.len(),
        "has_more": has_more,
        "next_offset": if has_more { Some(next) } else { None }
    })
    .to_string())
}

#[cfg(test)]
mod tests;
