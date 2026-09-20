# hadaka-agent

A small Rust agent harness, inspired by [Tiny Agents](https://huggingface.co/blog/tiny-agents). One loop streams a model response, executes its tool calls, appends the results, and repeats until the model answers without tools.

## Run

Copy `agent.example.toml` to `~/.config/hadaka-agent/config.toml` (or `$XDG_CONFIG_HOME/hadaka-agent/config.toml`), review each setting, and set the DeepSeek provider's `api_key` and `enabled = true`. The endpoint is fixed to `https://api.deepseek.com/chat/completions`. The example selects `deepseek-flash`; set the provider's `model` to your chosen DeepSeek model. See the [DeepSeek API documentation](https://api-docs.deepseek.com/) for available models and API keys.

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/hadaka-agent"
cp agent.example.toml "${XDG_CONFIG_HOME:-$HOME/.config}/hadaka-agent/config.toml"
# Edit config.toml: set the provider's api_key and enabled = true before running.
cargo run -- run "Use echo to repeat hello, then tell me the result."
cargo run -- chat
```

Both modes accept `--config PATH` before or after the subcommand. In a terminal, `chat` opens a compact inline Ratatui console with a status line and editable input. Completed output is appended above the console into normal terminal scrollback, so earlier messages remain accessible with the terminal's mouse-wheel, trackpad, or scroll shortcuts (often Shift+Page Up/Down). History retention follows the terminal's scrollback limit. A partial streamed line is previewed above the input until it is complete. Enter submits a message; arrow keys, Home/End, and Backspace/Delete edit it. Pasted text is inserted without submitting. `/exit` or Ctrl-D on empty input exits; Ctrl-C interrupts even while a response is streaming. Input is paused during model responses, while terminal scrolling and exit keys remain available. The terminal is restored on exit or failure.

Chat retains history until exit. When input or output is redirected, chat uses the plain line-based console and also exits on EOF. `run` always uses plain output. In plain mode assistant text streams to stdout; prompts, tool names, and errors go to stderr. In the Ratatui console, assistant text, tool activity, and MCP server stderr are displayed in the transcript. Exit codes are 0 for success, 1 for runtime errors, 2 for CLI usage errors, and 130 for Ctrl-C.

## Tools and configuration

The root `providers` array holds provider configurations. `providers = []`, or an array containing only disabled providers, represents an unconfigured application. In this state, `run` reports that a provider needs to be configured first and exits with code 1. `chat` shows the same setup guidance and opens the console; user messages repeat that guidance, and the console stays open until `/exit`, EOF, or Ctrl-C. Neither mode starts MCP servers or makes model requests without an enabled provider. Configuration editing commands are not implemented yet; restart after editing the config file. Legacy blank files are also accepted as unconfigured. If the default config file is missing, startup creates its parent directories and writes the example configuration with a disabled DeepSeek placeholder and an invalid API key. Existing files are preserved. A missing file explicitly selected with `--config PATH` still produces a file-read error.

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

The built-in `echo` tool takes `{"text":"..."}` and returns that text unchanged. To connect local tools over stdio, replace `mcp_servers = []` with `[[mcp_servers]]` entries. Every entry requires `name`, `command`, `args`, and `env`; use `args = []` and `env = {}` when empty. Server names must be unique. Commands run directly, inherit the launch directory and environment, and apply the configured environment overrides. Configured tools execute automatically.

MCP tools appear as `mcp_SERVER__TOOL`. Characters outside ASCII letters, digits, `_`, and `-` become `_`, and names are limited to 64 characters. Any resulting collision fails startup. The original server tool name is used when invoking it. All discovery pages are loaded once at startup.

Provider credentials are stored directly in config; `DEEPSEEK_API_KEY` and `HADAKA_API_KEY` are ignored. To migrate an older config, move the root `model` and `api_key` fields into a `[[providers]]` entry with `provider_name = "deepseek"` and `enabled = true`. Keep workflow settings at the root, before any TOML table headers. Remove any legacy `base_url` field.

`system_prompt` supplies the system instruction, and `max_turns` sets the positive limit on model requests per user input. Tool calls within a response execute sequentially. Every call receives a result, including errors the model can recover from. Text blocks and structured MCP results are preserved; image, audio, and binary resources are reported as unsupported.

Model responses time out after 120 seconds; MCP startup and tool calls after 60 seconds. HTTP errors, malformed or incomplete streams, and exhausted turn limits end the invocation (and end the session in chat mode). Requests are not retried automatically. On exit, connections close and subprocesses get three seconds to finish before being terminated.

Sessions are text-only and in memory. There is no persistence, context compaction, remote MCP transport, or built-in shell/file access. Requests explicitly disable DeepSeek thinking mode; this harness streams answer text and tool calls without retaining reasoning history. See [DeepSeek's thinking-mode documentation](https://api-docs.deepseek.com/guides/thinking_mode/) for the API behavior.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Tests use mock DeepSeek HTTP responses and a Rust stdio MCP fixture, with no real API credentials or external MCP runtime required. Only test builds can inject a local model endpoint. Cargo builds the fixture example during `cargo test`; for a filtered test, build it first with `cargo build --example mcp_fixture`.
