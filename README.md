<div align="center">

<img src="assets/logo.svg" alt="Daat Locus Logo" style="width:220px; height:auto;" />

# Daat Locus

<p align="center">
  <img src="assets/preview-tui.png" alt="preview-tui" width="45%" />
  <img src="assets/preview-webui.png" alt="preview-webui" width="45%" />
</p>

[![简体中文][readme-cn-badge]][readme-cn-url]
[![Crates.io][crates-badge]][crates-url]
[![CI][ci-badge]][ci-url]
[![License][license-badge]][license-url]

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

Daat Locus is a long-running local agent runtime.

## Quick Start

The recommended install path is `cargo-binstall`, which installs the prebuilt
GitHub Release binary for your platform.

For now daat-locus is only tested on Windows and MacOS, but Linux should work as well.

```bash
cargo install cargo-binstall
cargo binstall daat-locus

# OR use cargo install directly, which builds from source and requires Bun
cargo install

daat-locus
```

On first launch, Daat Locus opens an interactive setup flow.

### Source Builds

Source builds require Bun because `build.rs` builds and embeds the WebUI.

Install Bun from <https://bun.sh/> and make sure it's in your `PATH` before building.

```bash
git clone https://github.com/shadow3aaa/DaatLocus
cd DaatLocus
cargo run
```

## Common Entry Points

```bash
daat-locus help                # show the help message
daat-locus run                 # open the foreground runtime flow
daat-locus code <project-dir>  # select or create a project-scoped session
daat-locus attach              # attach to an existing daemon
daat-locus send "..."          # send one message and wait for a reply
daat-locus config              # open the interactive config menu
```

## Documentation

- [简体中文 README](README_zh-CN.md)
- [Architecture](docs/architecture.md)
- [Configuration](docs/configuration.md)
- [Semantic Code Operation & Propagation Engine](crates/scope-engine/README.md)
- [Contributing](CONTRIBUTING.md)
- [Skills](skills/) — Reusable skill SKILL.md files for task automation

## License

Daat Locus is licensed under the [Apache License 2.0](LICENSE).
