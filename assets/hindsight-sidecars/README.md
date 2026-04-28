# Hindsight Sidecar Build Assets

Daat Locus no longer starts Hindsight through `uvx` or runtime package
downloads. Release builds generate a platform sidecar in this directory, embed
it into the final Daat Locus binary, and publish only that final binary archive.
The generated sidecar archives are local/CI staging artifacts and are ignored by
git.

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

Plain source builds can still compile without a sidecar unless
`DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR=1` is set. Release installs should use
the published binary archives, for example through `cargo-binstall`.

Required archive layout:

```text
bin/
  hindsight-embed
```

On Windows the executable must be `bin/hindsight-embed.exe`.

The embedded `hindsight-embed` must be self-contained. It must not call `uv`,
`uvx`, pip, Poetry, or any other runtime package installer.

`cargo xtask build-hindsight-sidecar` writes a `tar.zst` archive for the
current host platform. `tar.gz`, `tgz`, and `zip` remain supported import
formats for manual or externally built artifacts.

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

The `Release Binaries` GitHub Actions workflow builds sidecars on Linux, macOS,
and Windows runners, compiles release binaries with
`DAAT_LOCUS_REQUIRE_HINDSIGHT_SIDECAR=1`, and packages the final binaries as
`dist/daat-locus-<version>-<target>.tar.zst` for release upload and
`cargo-binstall`.

Verify generated archives and manifest checksums:

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

After building a release binary that has embedded the sidecar, package it for
GitHub Releases and `cargo-binstall`:

```bash
cargo xtask package-release-binary
```
