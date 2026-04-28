# Vendored Hindsight Sidecars

Daat Locus no longer starts Hindsight through `uvx` or runtime package
downloads. Source releases vendor platform archives under this directory so a
normal `cargo build` or `cargo install --path .` can embed the current target's
sidecar without fetching Python packages.

`build.rs` resolves sidecars in this order:

1. `DAAT_LOCUS_HINDSIGHT_SIDECAR=/path/to/archive`
2. `assets/hindsight-sidecars/manifest.toml`
3. `assets/hindsight-sidecars/<target>.tar.zst`
4. `assets/hindsight-sidecars/<target>.tzst`
5. `assets/hindsight-sidecars/<target>.tar.gz`
6. `assets/hindsight-sidecars/<target>.tgz`
7. `assets/hindsight-sidecars/<target>.zip`

The archive is embedded into the Daat Locus binary at build time and extracted
once into `~/.daat-locus/cache/hindsight-sidecars`.

Required archive layout:

```text
bin/
  hindsight-embed
```

On Windows the executable must be `bin/hindsight-embed.exe`.

The embedded `hindsight-embed` must be self-contained. It must not call `uv`,
`uvx`, pip, Poetry, or any other runtime package installer.

`cargo xtask build-hindsight-sidecar` writes `tar.zst` archives for every
platform. `tar.gz`, `tgz`, and `zip` remain supported import formats for
manual or CI-built artifacts.

## Maintainer Commands

Build the current host platform sidecar with PyInstaller:

```bash
cargo xtask build-hindsight-sidecar --spec hindsight-sidecar/hindsight-embed.spec
```

or:

```bash
cargo xtask build-hindsight-sidecar --entry-script path/to/entry.py
```

Import a CI-built archive for another platform:

```bash
cargo xtask import-hindsight-sidecar \
  --target x86_64-unknown-linux-gnu \
  --archive /path/to/x86_64-unknown-linux-gnu.tar.zst
```

The `Hindsight Sidecars` GitHub Actions workflow can be triggered manually to
build sidecar artifacts on Linux, macOS, and Windows runners. Download each
artifact, then import its archive with `cargo xtask import-hindsight-sidecar`.

Verify committed archives and manifest checksums:

```bash
cargo xtask verify-hindsight-sidecars
```

Smoke-test the current host archive by extracting it and running
`hindsight-embed --help` plus profile create/delete:

```bash
cargo xtask smoke-hindsight-sidecar
```

Release builds can set `DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR=1` to fail fast
when no sidecar is available for the target.
