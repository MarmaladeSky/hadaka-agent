<!--
Copyright 2026 David Akermann

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
-->

# hadaka-agent

Hadaka Agent is a task-focused, security-first agent runtime. It is intentionally simple and reviewable, providing a practical alternative to overcomplicated agent systems whose behavior is difficult to inspect.

Configure a provider in `~/.config/hadaka-agent/config.toml`, then run a task:

```sh
cargo run -- "Read Cargo.toml and report its package version."
```

Use `-v` for execution details and `--format json` for newline-delimited JSON output. Permissions are granted explicitly with flags such as `--allow-read`, `--allow-write`, `--allow-exec`, and `--allow-net`.

With Nix:

```sh
nix run github:MarmaladeSky/hadaka-agent -- "Read Cargo.toml and report its package version."
nix develop github:MarmaladeSky/hadaka-agent
```

## FAQ

### Was it vibe-coded?

Thoughtfully augmented.
