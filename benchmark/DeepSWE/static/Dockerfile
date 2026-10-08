# syntax=docker/dockerfile:1

# Clean builder for a self-contained Daat Locus binary.
#
# The DeepSWE sandbox containers are Debian bookworm images without GTK, so the
# binary must not depend on the desktop tray stack. We build a static musl
# binary with the tray feature disabled and export just the executable, which can
# then be reused by every DeepSWE trial via `--daat-locus-bin`.

FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        musl-tools build-essential pkg-config ca-certificates git curl unzip xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl

# Bun is required by build.rs to build the embedded WebUI.
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

WORKDIR /src
COPY . .

# Keep the compile within the Docker VM's memory budget. The project's own
# release profile uses fat LTO with a single codegen unit, which needs far more
# RAM than a typical Docker Desktop VM has; thin LTO produces an equivalent
# benchmark binary at a fraction of the peak memory. Override with
# `--build-arg CARGO_BUILD_JOBS=<n>` / `--build-arg CARGO_PROFILE_RELEASE_LTO=fat`
# on machines with more Docker RAM.
ARG CARGO_BUILD_JOBS=2
ARG CARGO_PROFILE_RELEASE_LTO=thin
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} \
    CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO}

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --no-default-features \
        --target x86_64-unknown-linux-musl -p daat-locus \
    && cp target/x86_64-unknown-linux-musl/release/daat-locus /daat-locus \
    && strip /daat-locus \
    && file /daat-locus

FROM scratch AS artifact
COPY --from=builder /daat-locus /daat-locus
