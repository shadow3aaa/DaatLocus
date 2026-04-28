# Hindsight Sidecar Build Assets

Daat Locus no longer starts Hindsight through `uvx` or runtime Python package
downloads. The sidecar is built as a self-contained archive and published to a
dedicated GitHub Release. Daat Locus downloads the matching archive on first
use, verifies it, extracts it once into `~/.daat-locus/cache/hindsight-sidecars`,
and then runs the cached executable directly.

The generated sidecar archives in this directory are local/CI staging artifacts
and are ignored by git.

Normal `cargo build`, `cargo install`, and release binary builds do not embed
sidecars. Runtime installation is always driven by the pinned sidecar release
manifest in `src/hindsight/managed.rs`.

Required archive layout:

```text
bin/
  hindsight-embed
```

On Windows the executable must be `bin/hindsight-embed.exe`.

The packaged `hindsight-embed` must be self-contained. It must not call `uv`,
`uvx`, pip, Poetry, or any other runtime package installer.

`cargo xtask build-hindsight-sidecar` writes a `tar.zst` archive for the
current host platform.

The default PyInstaller environment pins `hindsight-embed` and
`hindsight-api-slim[embedded-db,local-ml]` instead of the full `hindsight-api`
meta package. Release sidecars include the embedded database plus local
embedding/reranking dependencies, while still avoiding unrelated Hindsight
extras. The uv PyTorch backend is pinned to CPU so Windows and Linux CI do not
resolve GPU-specific Torch distributions.

## Maintainer Commands

Build the current host platform sidecar with PyInstaller:

```bash
cargo xtask build-hindsight-sidecar --spec hindsight-sidecar/hindsight-embed.spec
```

or:

```bash
cargo xtask build-hindsight-sidecar --entry-script path/to/entry.py
```

The `Release Binaries` GitHub Actions workflow only compiles and packages Daat
Locus binaries as `dist/daat-locus-<version>-<target>.tar.zst` for release
upload and `cargo-binstall`. Those binaries use runtime sidecar downloads.

The `Hindsight Sidecars` workflow builds the same sidecar archives without
embedding them into Daat Locus. It uploads the archives plus a generated
`manifest.toml` to a dedicated GitHub Release such as
`hindsight-sidecars-v0.5.5-1`. That release is the source for the downloadable
sidecar runtime path.

Hindsight 0.5.5 includes the Windows profile metadata locking fix that earlier
local builds patched in the PyInstaller entrypoint.

Verify generated archives and manifest checksums:

```bash
cargo xtask verify-hindsight-sidecars
```

Smoke-test the current host archive by extracting it and running
`hindsight-embed --help` plus profile create/delete:

```bash
cargo xtask smoke-hindsight-sidecar
```

After building a release binary, package it for GitHub Releases and
`cargo-binstall`:

```bash
cargo xtask package-release-binary
```
