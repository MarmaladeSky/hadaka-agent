# hadaka-agent

A small Rust agent harness, inspired by [Tiny Agents](https://huggingface.co/blog/tiny-agents). One loop streams a model response, executes its tool calls, appends the results, and repeats until the model answers without tools.

## Run

Copy `agent.example.toml` to `~/.config/hadaka-agent/config.toml` (or `$XDG_CONFIG_HOME/hadaka-agent/config.toml`), review each setting, and set the DeepSeek provider's `api_key` and `enabled = true`. The endpoint is fixed to `https://api.deepseek.com/chat/completions`. The example selects `deepseek-flash`; set the provider's `model` to your chosen DeepSeek model. See the [DeepSeek API documentation](https://api-docs.deepseek.com/) for available models and API keys.

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/hadaka-agent"
cp agent.example.toml "${XDG_CONFIG_HOME:-$HOME/.config}/hadaka-agent/config.toml"
# Edit config.toml: set the provider's api_key and enabled = true before running.
cargo run -- "Use echo to repeat hello, then tell me the result."
```

Running the program executes one task non-interactively and exits. It accepts `--config PATH` before or after the task. By default, stdout contains only the final assistant result. Add `-v` or `--verbose` for the full trace. Use `--format human` (the default) for readable output, or `--format json` for newline-delimited JSON. In verbose human mode, diagnostics include the input, system prompt, complete tool list and parameter schemas, model turns, tool calls, parameters, and results. In verbose JSON mode, events have `type` set to `input`, `system_prompt`, `tools`, `turn`, `output`, `tool_call`, or `tool_result`; the `tools` event contains the exact definitions sent to the model. Without `-v`, JSON mode emits only one `result` object. MCP server diagnostics and runtime errors remain on stderr. Exit codes are 0 for success, 1 for runtime errors, 2 for CLI usage errors, and 130 for Ctrl-C. Interrupting a task still shuts down MCP subprocesses.

The built-in `list_directory` tool lists entries below a path with an optional `depth` (default 1, maximum 8). It reports files, directories, symlinks, and file sizes; symlinks are never followed. It uses the same read permission as `read_file`.

The built-in `search_files` tool recursively searches UTF-8 files for a literal, case-sensitive query. Both `query` and `path` are required; explicitly pass `"path": "."` to search the working directory. Optional arguments include a filename glob, result limit, and surrounding context-line count. Globs use [globset syntax](https://docs.rs/globset/latest/globset/#syntax), including `*`, `?`, character classes such as `[ab]`, and alternatives such as `*.{rs,toml}`. Patterns are case-sensitive and match file names only, not relative paths. Each pattern is compiled once per search; malformed patterns return an error. It uses the same read permission and does not follow symlinked directories.

The built-in `run_command` tool executes an approved program directly, without a shell. It accepts `program`, optional `args`, optional `cwd`, and a timeout (default 120 seconds, maximum 300 seconds). Its working directory must also be covered by `--allow-read`. It captures stdout and stderr with 64 KiB limits. Programs require explicit repeatable `--allow-exec PROGRAM` permission.

File and executable permissions are task-scoped and deny-by-default. Use repeatable `--allow-read PATH`, `--allow-write PATH`, and `--allow-exec PROGRAM` flags; omitted flags grant no corresponding access. The model always receives the built-in tools, but each operation is checked at execution time and returns a permission error when access was not granted. Read and write permissions are independent. Commands are executed without a shell and must match an explicitly allowed program name. Paths are resolved relative to the launch directory, canonicalized, and checked before each operation. Path traversal and symlink escapes are denied. MCP permissions are not yet restricted by these flags.

## Tools and configuration

The built-in `fetch_url` tool reads public HTTPS URLs using GET. Grant each exact hostname with repeatable `--allow-net HOST`; subdomains and redirect destinations require their own grants. The tool remains visible without permission and returns an error if called. Only port 443 is supported, URL credentials are rejected, and every resolved address and redirect destination must be public. DNS addresses are pinned for the connection and environment proxies are disabled. These network restrictions apply to `fetch_url`, not subprocesses, MCP servers, or the model connection.

```sh
cargo run -- --allow-net github.com "Check the latest release at https://github.com/rust-lang/rust/releases"
```

Arguments are `url`, optional `representation` (`auto`, `text`, `json`, or `raw`), and optional `max_chars` (default 30000, range 1–100000). `auto` converts HTML into Markdown-like text, parses JSON, and preserves XML/plain text. `text` converts HTML and otherwise returns textual bodies; `json` requires valid JSON; `raw` returns the UTF-8 body unchanged apart from the character limit. HTML conversion removes scripts, styles, and comments, preserves headings, lists, tables, code, and links, and resolves links against the final URL. It does not run JavaScript or fetch linked resources. Responses must be UTF-8, without NUL bytes; other encodings and binary downloads are unsupported.

Results include `url`, `final_url`, `status`, `content_type`, `representation`, `title`, `content`, up to 100 distinct `links`, `truncated`, and `untrusted`. HTTP error bodies are returned with their status. Text is truncated at Unicode character boundaries; oversized JSON returns an error instead of broken JSON. Downloads are limited to 2 MiB, five redirects, and a 30-second operation timeout. Verbose human output shows readable content and source metadata; JSON output preserves structured results. Web content is treated as untrusted evidence in the system instructions.

Web titles are limited to 512 characters. Link URLs longer than 2,048 characters are omitted from the links list rather than shortened. The complete serialized result is limited to 512 KiB, including JSON escaping and metadata. To fit this budget, trailing links are removed first, then text content is shortened at UTF-8 boundaries. Trimming content or metadata sets `truncated = true`. Oversized structured JSON or fixed source metadata returns an error; source URLs are never shortened.

The root `providers` array holds provider configurations. `providers = []`, or an array containing only disabled providers, represents an unconfigured application. In this state, the program reports that a provider needs to be configured first and exits with code 1. No MCP servers start and no model requests are made without an enabled provider. Edit configuration in the file before running a task. Legacy blank files are also accepted as unconfigured. If the default config file is missing, startup creates its parent directories and writes the example configuration with a disabled DeepSeek placeholder and an invalid API key. Existing files are preserved. A missing file explicitly selected with `--config PATH` still produces a file-read error.

Each provider requires a unique `provider_name`, an `enabled` boolean, `model`, and `api_key`:

```toml
[[providers]]
provider_name = "deepseek"
enabled = false
model = "deepseek-flash"
api_key = "invalid-placeholder-key"
```

Replace the placeholder key and set `enabled = true` to run workflows. DeepSeek is currently the only supported enabled provider. Duplicate names are rejected even for disabled entries. Enabled providers require nonblank model names and keys.

Workflow settings remain at the root: `system_prompt`, `max_turns`, and `mcp_servers`. When omitted, these use the example's system prompt, 20 turns, and no MCP servers. An omitted `providers` array defaults to empty. Unknown fields are rejected.

The built-in `read_file` tool takes `{"path":"src/main.rs","offset":0,"limit":100}`. Both `offset` and `limit` are required and currently count lines: offset is zero-based and limit is 1–1000. Relative paths resolve from the launch directory; absolute paths are supported. Results contain numbered `content` (one-based line numbers), `total_lines`, `has_more`, and `next_offset` (null at EOF). Reading beyond EOF returns empty content. Files must be regular UTF-8 text files, without NUL bytes, of at most 1 MiB. Output is capped at 64 KiB of numbered text on whole-line boundaries; use `next_offset` to continue. A single line exceeding that cap returns an error.

The `text_editor` tool modifies files using Anthropic-style commands:

```json
{"command":"create","path":"notes.txt","file_text":"First line\n"}
{"command":"str_replace","path":"notes.txt","old_str":"First","new_str":"Updated"}
{"command":"insert","path":"notes.txt","insert_line":1,"insert_text":"Second line"}
```

`create` fails if the path already exists; parent directories must exist. `str_replace` requires exactly one occurrence of nonempty `old_str`, including whitespace; an empty `new_str` deletes it. `insert_line` means insert after that line: 0 prepends, and the last line number appends. Insertion supplies line separators when needed, using CRLF for files containing CRLF. Reading remains in `read_file`. Edits require regular UTF-8 files without NUL bytes; both original and resulting content must fit within 1 MiB. Writes use a temporary file in the destination directory; edits preserve file permissions and replace the destination after validation. Symlinks for existing files resolve to their target. Concurrent edits are not coordinated.

The built-in `echo` tool takes `{"text":"..."}` and returns that text unchanged. To connect local tools over stdio, replace `mcp_servers = []` with `[[mcp_servers]]` entries. Every entry requires `name`, `command`, `args`, and `env`; use `args = []` and `env = {}` when empty. Server names must be unique. Commands run directly, inherit the launch directory and environment, and apply the configured environment overrides. Configured tools execute automatically.

MCP tools appear as `mcp_SERVER__TOOL`. Characters outside ASCII letters, digits, `_`, and `-` become `_`, and names are limited to 64 characters. Any resulting collision fails startup. The original server tool name is used when invoking it. All discovery pages are loaded once at startup.

Provider credentials are stored directly in config; `DEEPSEEK_API_KEY` and `HADAKA_API_KEY` are ignored. To migrate an older config, move the root `model` and `api_key` fields into a `[[providers]]` entry with `provider_name = "deepseek"` and `enabled = true`. Keep workflow settings at the root, before any TOML table headers. Remove any legacy `base_url` field.

`system_prompt` supplies the system instruction, and `max_turns` sets the positive limit on model requests per task. Tool calls within a response execute sequentially. Every call receives a result, including errors the model can recover from. Text blocks and structured MCP results are preserved; image, audio, and binary resources are reported as unsupported.

Model responses time out after 120 seconds; MCP startup and tool calls after 60 seconds. HTTP errors, malformed or incomplete streams, and exhausted turn limits end the invocation. Requests are not retried automatically. On exit, connections close and subprocesses get three seconds to finish before being terminated.

Task context is text-only and in memory. There is no persistence, context compaction, remote MCP transport, or built-in shell access. Requests explicitly disable DeepSeek thinking mode; this harness streams answer text and tool calls without retaining reasoning history. See [DeepSeek's thinking-mode documentation](https://api-docs.deepseek.com/guides/thinking_mode/) for the API behavior.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Tests use mock DeepSeek HTTP responses and a Rust stdio MCP fixture, with no real API credentials or external MCP runtime required. Only test builds can inject a local model endpoint. Cargo builds the fixture example during `cargo test`; for a filtered test, build it first with `cargo build --example mcp_fixture`.
