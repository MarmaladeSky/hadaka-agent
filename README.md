# hadaka-agent

Hadaka Agent is a task-focused, security-first agent runtime. It is intentionally simple and reviewable, providing a practical alternative to overcomplicated agent systems whose behavior is difficult to inspect.

Configure a provider in `~/.config/hadaka-agent/config.toml`, then run a task:

```sh
cargo run -- "Read Cargo.toml and report its package version."
```

Use `-v` for execution details and `--format json` for newline-delimited JSON output. Permissions are granted explicitly with flags such as `--allow-read`, `--allow-write`, `--allow-exec`, and `--allow-net`.
