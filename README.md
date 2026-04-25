<div align="center">

<img src="assets/logo.svg" alt="Daat Locus Logo" style="width:250px; height:auto;" />

# Daat Locus

[![简体中文][readme-cn-badge]][readme-cn-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

A long-running agent runtime with self-governance, persistent memory, app-scoped tools, and asynchronous self-improvement.

</div>

[readme-cn-badge]: https://img.shields.io/badge/README-简体中文-blue.svg?style=for-the-badge
[readme-cn-url]: docs/README_zh-CN.md
[ci-badge]: https://img.shields.io/github/actions/workflow/status/shadow3aaa/DaatLocus/ci.yml?style=for-the-badge&label=CI
[ci-url]: https://github.com/shadow3aaa/DaatLocus/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: LICENSE

## Features

- Long-term memory and experience accumulation powered by `Hindsight`
- Sleep-driven self-improvement
- App-oriented tool management instead of a flat tool list
- Prompt compilation that adapts to model capabilities
- Foreground TUI and background daemon runtime modes

## Philosophy

### Apps For Agents

As agent tooling grows, simple flat tool calling stops scaling. Hundreds or thousands of tools scatter the agent's attention, and the agent eventually falls back to a few generic tools such as the terminal.

Humans group mail lists, sending, and contacts into a mail app. We group friends, favorites, and feeds into messaging apps. Daat Locus applies the same idea to agents: agents need a native app ecosystem. Existing concepts such as MCP and workflows are adjacent, but they are not the same boundary.

A real agent app should satisfy these properties:

- Standardized: an app must follow a clear, fixed format that agents can manage, instead of loose scripts and scattered instructions.
- Stateful: an app should have its own state and lifecycle, not just a pile of tool calls and text.
- Interactive: when focused, an app should render structured state to the agent instead of forcing mechanical `list_xxx` exploration.
- Foreground/background aware: an app should be able to exist in the background and affect the runtime, for example by sending notices.
- Self-describing: an app should explain what it is for and how to use it, instead of pushing that burden into workflows or prompt fragments.

Daat Locus therefore raises tool management to the app level and provides a runtime environment with native support for agent apps. The current built-in system apps are `Terminal` and `Browser`, and third-party workspace apps are also supported.

### Sleep-Driven Asynchronous Self-Optimization

Daat Locus uses an asynchronous sleep mechanism to improve agent behavior during idle time. The design is inspired by [Hermes Agent](https://github.com/NousResearch/hermes-agent) and [EvoMap](https://github.com/EvoMap/evolver).

Daat Locus does not force self-improvement into the foreground runtime. Instead, self-improvement runs as a separate sleep phase.

While awake, the agent binds a suitable workflow for multi-step tasks, or creates a new workflow when no reusable process exists. Runtime traces and workflow run records are accumulated for sleep-time analysis.

The sleep phase is currently split into two independent pipelines:

- Prompt Improvement Pipeline: fixes system prompt and behavior constraints based on runtime traces
- Workflow Improvement Pipeline: fixes workspace workflows based on workflow run records

## Quick Start

Daat Locus currently runs from source:

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

On first run, if `~/.daat-locus/config.toml` does not exist, Daat Locus starts the interactive setup wizard.

## License

Daat Locus is licensed under the [Apache License 2.0](LICENSE).

Copyright 2026 shadow3 <shadow3aaaa@gmail.com>.

## Runtime Model

Daat Locus now defaults to a daemon model instead of a one-shot foreground process.

- `cargo run`
  Connects to an existing daemon first. If no daemon is running, it starts a background daemon and attaches the TUI.
- `cargo run -- attach`
  Attaches only to an already-running daemon.
- `cargo run -- daemon serve`
  Runs the daemon in the foreground, mainly for internal use and debugging.

The background daemon owns runtime state, the HTTP control interface, TUI synchronization state, and the Telegram transport. The daemon currently listens on the fixed local port `127.0.0.1:53825` by default.

## Configuration

The main config file is:

- `~/.daat-locus/config.toml`

The persona config file is:

- `~/.daat-locus/persona.md`

Prefer the interactive config commands:

```bash
cargo run -- config
cargo run -- config show
cargo run -- config add-provider
cargo run -- config add-model
cargo run -- config set-main-model
cargo run -- config set-hindsight-model
```

The core config structure is:

- `[providers]`: provider credential registry
- `[models]`: model definition registry
- `locale`: UI localization language
- `main_model`: main model reference
- `[daemon]`: daemon port
- `[judge]`: judge / pairwise evaluation config
- `[hindsight]`: Daat Locus-managed hindsight-embed config
- `[telegram]`: Telegram transport config

Minimal runnable example:

```toml
locale = "en-US"
main_model = "default"

[providers.openai]
type = "openai"
api_key = "your-api-key"

[models.default]
provider = "openai"
model_id = "gpt-4.1"
temperature = 1.0
request_timeout_secs = 300
stream_idle_timeout_secs = 45
context_window_tokens = 128000
effective_context_window_percent = 95
max_completion_tokens = 4000
tool_output_max_tokens = 2000

[daemon]
port = 53825

[hindsight]
namespace = "default"
bank_id = "daat-locus"
request_timeout_secs = 180
embed_version = ""
profile = "daat-locus"
port = 8888

[telegram]
enabled = true
bot_token = "your-telegram-bot-token"
poll_timeout_secs = 30
```

Notes:

- `hindsight` is now managed automatically by Daat Locus. You do not need to start Docker or run a separate service first.
- If `telegram.enabled = true` but `bot_token` is still a placeholder, the Telegram transport is not enabled.
- `hindsight.model = "xxx"` is optional. If unset, it falls back to `main_model`.
- `judge.model = "xxx"` is optional. If unset, it also falls back to `main_model`.
