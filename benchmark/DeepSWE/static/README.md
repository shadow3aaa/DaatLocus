# Static Daat Locus binary for DeepSWE

`Dockerfile` builds a self-contained Daat Locus binary once, so every DeepSWE
trial can reuse the same artifact instead of installing build tools and running
`cargo build` inside each sandbox.

## Why a dedicated build

The DeepSWE sandbox images (e.g. `public.ecr.aws/.../swe-bench-202605:*`) are
Debian bookworm without GTK. The default Linux build links the desktop tray
(GTK/AppIndicator), so it cannot run in those containers. This build:

- uses `--no-default-features` to drop the `desktop-tray` cargo feature (no GTK),
- targets `x86_64-unknown-linux-musl`, producing a fully static `static-pie`
  executable that runs on any x86-64 Linux, and
- exports only the stripped executable (roughly 60 MB).

## Build

From the repository root:

```bash
# .dockerignore at the repo root keeps the build context small.
docker build \
  -f benchmark/DeepSWE/static/Dockerfile \
  --target artifact \
  -o type=local,dest=benchmark/DeepSWE/static/out \
  .
```

The result is `benchmark/DeepSWE/static/out/daat-locus`.

The Dockerfile defaults to thin LTO and 2 parallel cargo jobs so it also fits a
small Docker Desktop VM. On a machine with more Docker RAM, override with
`--build-arg CARGO_PROFILE_RELEASE_LTO=fat --build-arg CARGO_BUILD_JOBS=<n>`.

## Use

Pass the built binary to the runner; every trial then just uploads it and skips
the in-sandbox toolchain/`cargo` build entirely:

```bash
cd benchmark/DeepSWE
uv run deepswe-daat-locus \
  -p <task-or-tasks-path> \
  --daat-locus-bin "$PWD/static/out/daat-locus"
```

`runner.py` forwards this as the `daat_locus_bin` agent kwarg, and
`daat_locus_agent.py` uploads it to the sandbox and marks it executable.
