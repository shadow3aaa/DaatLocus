# Contributing

[简体中文](CONTRIBUTING_zh-CN.md)

Use a separate profile and temporary workspace for development. Tests must not use a personal account or contact real model providers. Repository rules are in [AGENTS.md](AGENTS.md).

## Requirements

- Stable Rust with rustfmt and Clippy.
- Bun; the WebUI's package manager version is pinned in [package.json](webui/package.json).
- wasm-pack and the `wasm32-unknown-unknown` Rust target for the real WebUI.
- Platform build tools: MSVC on Windows, Xcode tools on macOS, or a C/C++ toolchain on Linux. Linux sandbox tests also require bubblewrap and working user namespaces.

Native client and Android requirements are in [the client guide](clients/app/README.md).

## Build and run

From the repository root:

```sh
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run --locked -- --profile dev serve
```

The frontend script installs locked WebUI dependencies and builds the authentication WASM and UI. Cargo consumes that output; it does not run Bun or wasm-pack. Rebuild the UI before rebuilding a service with embedded assets after frontend changes.

Browser-only first registration uses the local CLI: stop the development service, run `daat-locus --profile dev account create NAME --password-stdin` with a password of at least 12 characters supplied on stdin, then restart that profile. Native clients can register in the app.

For a backend-only build, use `cargo build --locked --no-default-features --bin daat-locus`. It serves the API without a bundled UI.

To skip asset embedding during checks, set `DAAT_LOCUS_BUILD_WEBUI=0` in the current shell:

```powershell
$env:DAAT_LOCUS_BUILD_WEBUI='0'
```

```sh
export DAAT_LOCUS_BUILD_WEBUI=0
```

The build normally downloads the models.dev catalog. For an offline build, set `DAAT_LOCUS_MODEL_CATALOG_PATH` to an existing models.dev JSON snapshot. Cargo's `--offline` flag alone does not stop build-script HTTP requests. `DAAT_LOCUS_WEBUI_DIST` selects a different prebuilt UI directory.

## Frontend development

The mock preview runs without a service, account or WASM build:

```sh
cd webui
bun install --frozen-lockfile
bun run dev:mock
```

Open `/__mock` at the address Vite prints. Scenes use the actual UI components with in-memory data; the frame controls include 320px and 390px widths. Changing a scene resets mock data.

For hot reload against a development service, run `bun run dev` instead. The default API target is `http://127.0.0.1:45825`; set `DAAT_LOCUS_DAEMON_URL` to match the service's printed address. Set it before starting Vite. Use the same browser origin for account requests.

`DAAT_LOCUS_MOCK_MACOS=1` and `DAAT_LOCUS_MOCK_PHONE=1` select platform mocks. Mock rendering cannot establish native input, permission or WebView behavior.

## Checks

With `DAAT_LOCUS_BUILD_WEBUI=0`, run the relevant backend checks from the root:

```sh
cargo fmt --all -- --check
cargo test --locked -p daat-locus --no-default-features --lib --bins --test http_api -- --test-threads=4
cargo test --locked -p daat-locus-sandbox -p daat-locus-bash -p daat-locus-uutils -- --test-threads=4
cargo clippy --locked -p daat-locus --no-default-features --all-targets -- -D warnings
```

In `webui`:

```sh
bun run typecheck
bun run typecheck:mock
bun test
bun run build
```

Relay changes also need `bun run typecheck` and `bun test` in `workers/signaling`. The [CI workflow](.github/workflows/ci.yaml) lists the platform matrix, driver checks, separate device-takeover case and native opt-in tests.

Limit unrelated Rust fixtures to four concurrent cases. Native sandbox tests need their real OS prerequisites. Report unavailable coverage separately from passed tests; do not replace isolation with unrestricted execution to make a check pass.

Tests should verify behavior with independent expected results. Use temporary storage, mock providers and local fixtures. Actual desktop input, elevation, driver installation and public-relay checks are opt-in and need an explicitly chosen test environment.

## Packaging

Run packaging from the repository root. It prepares the shared frontend before building the selected distribution.

| Output | Command |
| --- | --- |
| Service archive | `cargo xtask package binary --target <triple>` |
| Windows installers, on Windows | `cargo xtask package windows` |
| macOS package, on macOS | `cargo xtask package macos` |
| Android debug APK | `cargo xtask package android --debug` |

Binary archives embed the browser UI. Native packages put the assets in the client and build the companion service without duplicate assets. Outputs go under `target/<triple>/<profile>/package`; `--out-dir` overrides the package directory. Use `cargo xtask package <format> --help` for signing and reuse options.

Windows GUI packaging needs a signed virtual-display package and matching helpers. macOS and Android release builds need their signing credentials. See [the client guide](clients/app/README.md) before building installers.

Do not delete `.dev/windows-signing` during cleanup. It contains a persistent local development signing identity. Never commit credentials, private keys, runtime data or build output. Preserve the attribution files described in [license notes](docs/license-attribution.md).

## Changes

Keep changes focused and describe the visible behavior, reason and checks. Update both locale catalogs when changing UI text. Put runtime prompts in `src/prompts` and bundled skills in `src/builtin_skills`; keep instructions short and tool-specific parameters in schemas.

Keep documentation about using, building and testing the current code. Release notes belong on GitHub Releases. Do not add architecture documents, task logs, changing test totals or screenshots of intermediate work to the repository.
