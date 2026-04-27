<div align="center">

<img src="assets/logo.svg" alt="Daat Locus Logo" style="width:220px; height:auto;" />

# Daat Locus

[![简体中文][readme-cn-badge]][readme-cn-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

A long-running local agent runtime with persistent memory, app-scoped tools,
Telegram event handling, and sleep-time self-improvement.

</div>

[readme-cn-badge]: https://img.shields.io/badge/README-简体中文-blue.svg?style=for-the-badge
[readme-cn-url]: docs/README_zh-CN.md
[ci-badge]: https://img.shields.io/github/actions/workflow/status/shadow3aaa/DaatLocus/ci.yml?style=for-the-badge&label=CI
[ci-url]: https://github.com/shadow3aaa/DaatLocus/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: LICENSE

## What It Is

Daat Locus is a daemon-first runtime for an agent that keeps working across
turns. It is not designed as a one-shot chat wrapper where every assistant
message is automatically sent to the outside world.

External input enters the runtime as structured events, app notices,
after-claim context, pre-turn context, and recalled memory. The model decides
what to do, while real-world effects go through explicit tools such as terminal
actions, browser actions, workflow binding, memory recall, or Telegram event
completion.

## Core Ideas

### Agent Apps

Flat tool lists do not scale well once an agent has many capabilities. Daat
Locus groups interactive capabilities into apps with state, lifecycle, usage
guidance, and focus semantics.

The built-in apps are currently `Terminal` and `Browser`. Third-party workspace
apps are also supported through source-first Lua app packages.

### Sleep-Time Improvement

Daat Locus improves during idle time instead of forcing self-improvement into
foreground task execution.

While awake, the runtime records code-detected runtime error cases and
workflow-bound execution evidence. During sleep, independent pipelines can
adjust global runtime contracts and workspace workflow specs.

## Features

- Foreground TUI plus background daemon runtime.
- Managed `Hindsight` integration for long-term memory and experience recall.
- Telegram as a transport and event source, not an app UI to navigate.
- Workflow binding and sleep-time workflow evolution for repeated task classes.
- App-scoped tools instead of one global, unstructured tool namespace.
- Interactive setup for providers, models, Telegram, and runtime config.

## Quick Start

Daat Locus currently runs from source:

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

On first run, if `~/.daat-locus/config.toml` does not exist, Daat Locus starts
the interactive setup wizard.

## Common Commands

```bash
cargo run                       # start or attach to the daemon-backed TUI
cargo run -- attach             # attach to an already-running daemon
cargo run -- daemon status      # show daemon status
cargo run -- daemon restart     # restart the background daemon
cargo run -- config             # open the interactive config menu
cargo run -- config show        # show config with secrets masked
```

## Configuration

The main config file is `~/.daat-locus/config.toml`. The persona file is
`~/.daat-locus/persona.md`.

Prefer the interactive config commands for normal setup:

```bash
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
cargo run -- config set-telegram
```

See [Configuration](docs/configuration.md) for the config shape, provider notes,
and a minimal TOML example.

## Documentation

- [简体中文 README](docs/README_zh-CN.md)
- [Configuration](docs/configuration.md)
- [Model catalog](docs/model-catalog.md)
- [Sandbox backend selection](docs/sandbox-backend-selection.md)
- [Builtin workflows](workflows/README.md)

## License

Daat Locus is licensed under the [Apache License 2.0](LICENSE).

Copyright 2026 shadow3 <shadow3aaaa@gmail.com>.
