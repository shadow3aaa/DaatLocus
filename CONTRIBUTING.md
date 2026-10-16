# Contributing

[简体中文](CONTRIBUTING_zh-CN.md)

Read [AGENTS.md](AGENTS.md) and the
[architecture](docs/architecture.md) before changing product boundaries.
Actor and App are the only core concepts. Preserve identities, durable routing,
permissions and incremental client contracts. Do not restore retired Session,
TUI, Dashboard, Sleep, Workflow, SCOPE/LSP or sharing interfaces.

## Build and run

Install Rust, Bun and wasm-pack, then:

```sh
rustup target add wasm32-unknown-unknown
cd webui
bun install --frozen-lockfile
cd ..
cargo run -- serve
```

Normal Cargo builds compile and embed the WebUI and shared authentication WASM.
Open the printed service address; browser authentication and mutations use that
same origin. For frontend iterations, run `bun run build` in `webui` and rebuild
the embedded service. The retired simulated `webui_fixture` is not a development
entry point. Use a separate profile and temporary workspace for manual checks;
never substitute real user data for fixtures.

For an independent visual preview, run `bun run dev:mock` in `webui` and open
`http://127.0.0.1:5173/__mock` (use the port Vite prints). One Vite process serves
the actual shared WebUI and an in-memory HTTP/SSE mock; no daemon, account,
wasm-pack or Rust compiler is needed. The scene picker covers conversations,
activity, approvals, queue, attachments, long content, login and connection states.
Each scene also has 320px and 390px frame previews for responsive checks.
Use the regular UI to open settings or move an Actor dialog to the main window.
Switching scenes resets only mock data. Authentication is deliberately unavailable
in this preview; production builds retain the real OPAQUE/WASM path and omit the
preview routes. Validate fixtures with `bun run typecheck:mock` and
`bun test tests/mock-preview.test.ts`; use service contract tests for backend behavior.

For backend-only checks set `DAAT_LOCUS_BUILD_WEBUI=0`. These builds require
neither Bun nor wasm-pack. `DAAT_LOCUS_MODEL_CATALOG_PATH` can supply a validated
models.dev JSON snapshot for offline builds; Cargo `--offline` does not disable
build-script network requests. See [model catalog](docs/model-catalog.md).

## Source and tests

- `src/actors`: execution, persistence, permissions and approval.
- `src/*_app` and `src/app_instructions`: domain tools and all built-in App instructions.
- `src/accounts`, `src/stage`, `src/resources`: account authentication, device coordination and resources.
- `src/service`: bounded HTTP operations and resumable streams.
- `webui/src/components`: reusable UI, settings, IM and work presentations.
- `webui/src/locales`: matching Chinese/English catalogs; add keys to both files.
- `tests/http_api.rs`, `tests/http`, `tests/support`: one actual loopback HTTP
  harness, suites by contract, isolated real OPAQUE accounts and temporary stores.
- `workers/signaling`: optional public P2P signaling only; no business execution or password authority.
- `xtask`: native archives and installers. No Git submodules are required.

Relevant checks:

```sh
cargo fmt --all -- --check
# With DAAT_LOCUS_BUILD_WEBUI=0:
cargo test --locked --workspace -- --test-threads=4
cargo clippy --locked --workspace --all-targets -- -D warnings
# In webui:
bun run typecheck
bun test
bun run build
# In workers/signaling, when signaling changes:
bun install --frozen-lockfile
bun test
```

Tests must not contact real model providers. Keep useful positive/negative OS
sandbox checks and HTTP authorization, CSRF, queue freshness, resource and
stream-reconnection coverage. Tests depending on external services stay opt-in;
report prerequisites and failures explicitly. Do not weaken isolation or private
ACL checks to accommodate a restricted environment. Library checks do not prove
full product usability or cross-platform isolation.

Limit independent Rust test fixtures to four concurrent cases. Device contract
cases still run concurrent peer runtimes; this avoids OS I/O and cryptographic
setup in unrelated fixtures starving the real lease and heartbeat deadlines.

Public signaling and account-sync checks use disposable identities and mock models.
Set `DAAT_LOCUS_TEST_STAGE_SIGNALING` to an explicitly chosen WSS origin, then run
`cargo test --locked --lib public_ -- --ignored --nocapture`. These opt-in checks
exercise SDP forwarding, authenticated P2P connections, message replication and
private model-configuration sync; they do not establish reachability through every NAT.

Keep browser preferences local and account configuration private. UI strings
use translation keys; language switching must update existing views without
translating user/source content. Test catalog parity, browser defaults and
explicit language choices. Do not add inline bilingual selectors.

The Windows/macOS Tauri application is `clients/app/src-tauri`, package
`daat-locus-app`. Build it alongside `daat-locus`; the service embeds the shared
WebUI and has no GUI dependencies. Client-only checks need no Bun/WASM build.
Run `cargo test -p daat-locus-app` and its strict Clippy checks; native CI covers
both platforms. Verify account login, attachments, resource popups, duplicate
launch, hide/reopen, independent client exit and orderly service stop on a desktop.
Native code uses fixed companion CLI operations and validates profile, executable
and loopback origin. Keep WebView process/filesystem/shell capabilities disabled.

## Process management and packaging

`start --open`, `status --json`, `stop` and `restart` address the selected
`--profile` through local OS-owner IPC. Test process identity, profile isolation,
restart options and history preservation using temporary profiles.

```sh
cargo xtask package binary --target <triple>
cargo xtask package windows --target x86_64-pc-windows-msvc
cargo xtask package macos --target <macOS-triple>
cargo xtask package android --target aarch64
```

All package commands print Cargo-style progress and clickable artifact URLs (plain URLs in redirected logs). Use `--out-dir` to choose the destination.

Windows installers require WiX; macOS installers require macOS tools.
`--skip-build --release-dir <directory>` packages an existing verified embedded
binary and, on Windows/macOS, its desktop client. Packaging rejects backend-only builds. Release CI builds each platform
natively. Keep generated archives and installers under ignored `target` paths.
Review dependency attribution before distribution; see
[license notes](docs/license-attribution.md).

## Changes and review

Inspect Git status first. Preserve unrelated edits, dirty submodule work,
credentials and persistent Actor data. Resolve deletion targets and verify their
location before removing anything; use native PowerShell `-LiteralPath` on Windows.

Explain the concrete behavior, purpose, checks and material limits. Keep source,
interfaces, configuration, tests and documentation consistent. Useful contracts
belong in architecture/contribution documents; do not add task journals, screenshot
handoff records, changing test counts or progress checklists as permanent docs.
Keep release changelogs. Commit titles use English and identify the change.
Do not commit caches, generated artifacts, runtime logs, credentials or experiments.

## Android builds

Use JDK 17+, Android SDK 36 / NDK 27, Bun, wasm-pack and Rust Android targets.
The maintained Gradle project is `clients/app/src-tauri/gen/android`; generated
JNI sources, native libraries and build outputs are ignored. Run `bun install
--frozen-lockfile` in `clients/app`, then `bun run android:dev` or
`DAAT_LOCUS_ANDROID_SIGNING=/private/release.properties cargo xtask package android`
from the repository root for packaging. ARM64 is the default; use `--target x86_64`
for an emulator or `--debug` for debug signing. APKs are copied to
`target/<Android-triple>/<release|debug>/package`, or `--out-dir`.
`--skip-build` checks and copies existing Gradle output; `--apk-dir` can select
another directory containing `output-metadata.json`. Version, ABI and embedded
WebUI are checked before reporting success. The existing Bun build script remains
the shared Tauri/Gradle implementation.
The signing properties and keystore stay outside the repository; see
[the native client guide](clients/app/README.md#android). Verify release APKs with
Android build-tools `apksigner verify --verbose --print-certs`, and check 16 KiB
ELF page alignment on the native library. Do not commit signing credentials.

Validate the signed, minified release APK on a device; a debug launch does not
exercise R8. The `ReleaseStartupTest` instrumentation test loads the embedded
account login without creating an account. Use a dedicated test installation.
After building a release APK for that device's ABI, run from the Gradle directory:

```sh
DAAT_LOCUS_ANDROID_SIGNING=/private/release.properties DAAT_LOCUS_ANDROID_ABI=arm64-v8a ./gradlew :app:connectedUniversalReleaseAndroidTest -x :app:rustBuildUniversalRelease
```

Use `x86_64` for an x86 emulator and build with `cargo xtask package android --target
x86_64` first. The excluded Rust task uses the already-built native library;
do not run this against stale native output.
