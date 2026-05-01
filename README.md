<div align="center">

<img src="assets/logo.svg" alt="Daat Locus Logo" style="width:220px; height:auto;" />

# Daat Locus

[![简体中文][readme-cn-badge]][readme-cn-url]
[![Crates.io][crates-badge]][crates-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

An agent runtime that truly has experience.

</div>

[readme-cn-badge]: https://img.shields.io/badge/README-简体中文-blue.svg?style=for-the-badge
[readme-cn-url]: README_zh-CN.md
[crates-badge]: https://img.shields.io/crates/v/daat-locus?style=for-the-badge
[crates-url]: https://crates.io/crates/daat-locus
[ci-badge]: https://img.shields.io/github/actions/workflow/status/shadow3aaa/DaatLocus/ci.yml?style=for-the-badge&label=CI
[ci-url]: https://github.com/shadow3aaa/DaatLocus/actions/workflows/ci.yml
[license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=for-the-badge
[license-url]: LICENSE

## What Is This?

Daat Locus is a long-running local self-governing Agent Runtime.

It is built for work that becomes better through history: maintaining the same
project over time, repeatedly handling the same class of task, remembering your
preferences and practical experience, and distilling them to improve later
behavior.

## Core Ideas

## Apps For Agents

When humans use a computer, we rarely choose an action from a global list of
everything the machine can do. We open a terminal, read the current output,
enter a command, and wait for the result; or we open a browser, read the current
page, click, navigate, and continue from the new page.

Daat Locus gives agents a similar interaction model.

Apps provide stateful operating surfaces for the runtime. Each App renders the
current state the agent can see, explains when it should be used, explains how
it should be operated, and exposes a local set of tools when focused.

Compared with a flat tool list, this gives the model three things:

1. **Locality**: the agent only sees tools relevant to the current operating
   surface.
2. **State grounding**: actions are based on the state currently displayed by
   the App, instead of choosing tools out of context.
3. **Temporal continuity**: long-running surfaces such as Terminal and Browser
   can be safely continued.

Apps are how Daat Locus turns "tools" into "software operating surfaces".

Therefore, Daat Locus does not need `SKILLS.md` to explain how a group of tools
should be used. The App itself is self-describing.

### Workflow Self-Improvement

Daat Locus executes tasks with workflows as blueprints, then feeds execution
experience back into workflows during an independent sleep phase.

While awake, Daat Locus executes tasks and records practical experience. During
sleep, it organizes that experience, fixes recurring problems, and improves the
workflows that later tasks depend on.

Sleep optimization also attempts to merge similar workflows to avoid unbounded
growth.

## Quick Start

The recommended install path is `cargo-binstall`, which installs the prebuilt
GitHub Release binary for your platform. On first launch, Daat Locus downloads
the matching self-contained Hindsight sidecar from the project sidecar release,
then caches it locally. Normal installs do not need Python, `uv`, or
PyInstaller.

```bash
cargo install cargo-binstall
cargo binstall daat-locus
```

You can also download the matching archive directly from
[GitHub Releases][releases-url], extract it, and place `daat-locus` on your
`PATH`.

On first launch, Daat Locus opens an interactive setup flow.

### Source Builds

`cargo install daat-locus` is available from crates.io. Like the prebuilt
release binary, source builds download the matching Hindsight sidecar on first
launch when it is not already cached. Source builds require Node.js with
Corepack or Yarn available because `build.rs` builds and embeds the WebUI.

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run --locked
```

`cargo build` and `cargo run` build the WebUI through `build.rs` and embed the
generated assets into the daemon by default. Use `cargo xtask build` when you
need the release-style `cargo build -p daat-locus --release --locked` wrapper
used by release packaging.

[releases-url]: https://github.com/shadow3aaa/DaatLocus/releases

## Documentation

- [简体中文 README](README_zh-CN.md)
- [Architecture](docs/architecture.md)
- [Builtin workflows](workflows/README.md)

## License

Daat Locus is licensed under the [Apache License 2.0](LICENSE).
