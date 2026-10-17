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
bun scripts/build-webui.ts
cargo run -- serve
```

`bun scripts/build-webui.ts` builds the one shared WebUI/authentication WASM
bundle in `webui/dist`. Cargo consumes that bundle; build scripts never invoke
Bun or wasm-pack. The default `embedded-webui` feature zstd-compresses the assets
into a standalone browser-serving binary. `DAAT_LOCUS_WEBUI_DIST` can override
the input directory. Open the printed service address; browser authentication
and mutations use the same origin. Rebuild the frontend before rebuilding the
embedded service after UI changes. Tauri uses that same dist through frontendDist
and its own built-in compression. Native companion services use
`--no-default-features`, and Android's core disables default features so the APK
contains only one WebUI. Tauri/xtask packaging prepares the frontend automatically.
Use `cargo run --no-default-features -- serve` for a headless API service. The retired simulated `webui_fixture` is not a development
entry point. Use a separate profile and temporary workspace for manual checks;
never substitute real user data for fixtures.

For an independent visual preview, run `bun run dev:mock` in `webui` and open
`http://127.0.0.1:5173/__mock` (use the port Vite prints). One Vite process serves
the actual shared WebUI and an in-memory HTTP/SSE mock; no daemon, account,
wasm-pack or Rust compiler is needed. The scene picker covers conversations,
activity, approvals, queue, attachments, long content, login and connection states.
Each scene also has 320px and 390px frame previews for responsive checks.
Use the regular UI to open settings. Actor links open a dialog on desktop and a
full-screen conversation with a back button on narrow screens.
Switching scenes resets only mock data. Authentication is deliberately unavailable
in this preview; production builds retain the real OPAQUE/WASM path and omit the
preview routes. Validate fixtures with `bun run typecheck:mock` and
`bun test tests/mock-preview.test.ts`; use service contract tests for backend behavior.

For backend-only checks set `DAAT_LOCUS_BUILD_WEBUI=0`. These builds require
neither a dist bundle, Bun nor wasm-pack. `DAAT_LOCUS_MODEL_CATALOG_PATH` can supply a validated
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
`daat-locus-app`. Prepare the shared dist first, then build the native client
with `--features custom-protocol` and its companion service with
`--no-default-features`. The service has no GUI dependencies or duplicate assets.
Client-only development checks can omit custom-protocol and use the dev URL.
Run `cargo test -p daat-locus-app` and its strict Clippy checks; native CI covers
both platforms. Verify account login, attachments, resource popups, duplicate
launch, hide/reopen, independent client exit and orderly service stop on a desktop.
Native code uses fixed companion CLI operations and validates profile, executable
and loopback origin. Keep WebView process/filesystem/shell capabilities disabled.

## Actor orchestration evaluations

The shared model-facing policy is `src/app_instructions/orchestration.md`.
Core and hosted Actors with the management Actors App receive it automatically;
rebuild the backend after edits. Keep scenario guidance general and distinguish
orchestration mistakes from tool, environment, permission or runtime failures.
Fix the responsible layer rather than adding a prompt rule for every failure.

For model-quality benchmarks, compare a baseline and one policy change with the
same task revisions, model/provider settings, tools, approval mode, environment,
and total time/token budget. Include all collaborators and automatic reviews in
costs. Use disposable accounts/profiles and authorized isolated task workspaces;
do not reuse Actor history or modified checkouts across independent samples.
Within one sample, preserve related Actor context to exercise reuse. Real-model
runs are explicitly invoked external evaluations, never normal tests.

Record the source revision and dirty diff, policy hash, task IDs, outcomes,
elapsed time, usage, and delegation/verification failures. Retain failed and
timed-out samples. Compare task-level results over repeated runs and a held-out
task set before adopting a rule; do not put task-specific solutions, hidden test
expectations or benchmark identities into the model instructions. Keep generated
reports in an ignored output location, outside source commits. The repository
currently has no DeepSWE/model-quality evaluation adapter; the mock resource
harness below measures backend costs, not problem-solving quality.

## Backend resource stress measurements

`rust-script scripts/perf.rs` is a single-file harness that builds the CLI, then runs
the actual `daat-locus serve` binary with a disposable account, profile, home and
workspace. The script hosts a loopback OpenAI-compatible streaming mock, logs in
through OPAQUE and sends sequential messages to each of several concurrent
Actors. Fixture Actors are seeded through internal APIs before service startup;
this adds no product API or configuration controls. No real provider is used.

```sh
# Install the script runner once.
cargo install rust-script --locked
# Release build: 64 Actors x 50 messages, 16 KiB replies, 90-second cooldown.
rust-script scripts/perf.rs
# Scale concurrency with identical per-Actor work (also try --actors 32 or 64).
rust-script scripts/perf.rs --actors 128 --rounds 50 --timeout-seconds 3600
# Quick validation only; debug measurements are not a release baseline.
rust-script scripts/perf.rs --debug-build --actors 2 --rounds 2 --idle-seconds 2 --cooldown-seconds 2
```

Requires Rust and rust-script. The script's embedded manifest references the local
DL/auth crates; rust-script manages its own dependency lockfile and build cache.
The CLI is built with the repository's lockfile and WebUI embedding disabled.
The script and measured CLI both disable default features; no frontend dist,
Bun or wasm-pack is needed for the stress driver.
`--skip-build` reuses the selected CLI binary; rebuild after source changes.
`--debug-build` selects the measured CLI profile; rust-script's own `--debug` flag
only changes how the script is compiled. The script currently expects
the standard `target/<profile>` output location. `--model-delay-ms`, `--sample-ms` and
`--timeout-seconds` control synthetic response latency, sampling and the measured
run deadline. Each setup subprocess also has a 60-second deadline. Builds are
outside the measurements. Use a quiet machine and repeat runs for comparisons.
The default deadline is 1,800 seconds; concurrency accepts 1–256 Actors. Each
Actor retains its conversation across rounds, so rounds also increase history
and persistence costs. The 90-second cooldown exceeds the 60-second worker idle
eviction window. Load progress is printed every 10 seconds.

Each measured run writes `target/perf/<unique-id>/charts.svg`, `summary.json`,
`samples.csv`, `latencies.csv` and synthetic service stdout/stderr. Open the SVG
in a browser or image viewer; it is a standalone, offline chart generated by
the Rust script with Plotters. It shows phase-shaded memory-footprint and CPU timelines and a
completed-message latency histogram with p95. CPU interval averages use cumulative
CPU deltas; short phase-boundary intervals are merged to avoid rounding spikes.
Failed/interrupted runs retain partial samples and produce a clearly marked
partial chart (provided measurement started and report writing succeeds).
`latencies.csv` contains sorted completed-turn durations, excluding unfinished
turns; the chart shows completed/expected counts. The report records the executable hash, current checkout
revision/dirty flag, parameters, OS, completed messages, throughput, p95 observed
turn latency and per-phase footprint/CPU (RSS is retained as auxiliary data).
Schema v2 adds `footprint_bytes` to CSV samples and `footprint_*_bytes` to phase
statistics; `footprint_metric` identifies the platform-specific accounting:

- Windows: Private Bytes (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`), private
  committed memory including paged-out pages; excludes uncommitted reservations
  and shared allocations. This is commit cost, not physical residency or macOS's
  `phys_footprint` definition.
- macOS: `proc_pid_rusage().ri_phys_footprint`, including compressed memory.
- Linux: `/proc/<pid>/smaps_rollup` PSS + SwapPss, a proportional resident-plus-swap
  estimate, requiring both fields and permission to read the target's mappings.

The primary chart always uses footprint. Sampling errors fail the run instead
of silently falling back to RSS or virtual address size. Compare runs using the
same OS/metric. Prior schema-v1 RSS-only reports cannot be converted to footprint.
The checkout revision is provenance, not proof that a reused binary was built
from it. Source archives without Git metadata report `git_revision` and
`git_dirty` as null; retain the source archive hash/manifest alongside those runs.
Auxiliary RSS means resident memory (Working Set on Windows); sampled
peaks/p95 for either memory metric can miss shorter spikes. CPU is process
user+kernel time; 100% means one logical CPU and multicore work can exceed 100%.
Latency includes the driver's 100 ms bounded message-status polling interval.
The driver checks completed replies and the exact mock request count; failures,
timeouts and forced shutdown cannot produce a successful report.

Only the CLI PID is measured, excluding the driver/mock and account provisioning.
This measures single-device backend persistence, inference streaming and HTTP
overhead, not model inference, App tool effects, desktop/WebView, remote sync or
browser event-stream fan-out. Actor identities/history remain durable during a
run; post-load footprint or RSS alone is not evidence of a leak. The script attempts graceful
OS-owner stop and reaps its exact child process on failure. It then validates and
deletes its owned `TempDir`, including credentials and
databases; reports remain under ignored `target`. An uncatchable process kill or
power loss can prevent cleanup. Do not point the driver at an existing profile.

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
`--skip-build --release-dir <directory>` packages existing verified artifacts.
Binary archives require the standalone embedded WebUI service. Windows/macOS
installers require a native WebUI client and an API-only companion service;
packaging rejects duplicate service assets or a missing native UI. Release CI
builds each platform natively. Keep generated archives and installers under ignored `target` paths.
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
another directory containing `output-metadata.json`. Version, ABI, native WebUI
and absence of duplicate backend assets are checked before reporting success. The existing Bun build script remains
the shared Tauri/Gradle implementation.
The signing properties and keystore stay outside the repository; see
[the native client guide](clients/app/README.md#android). Verify release APKs with
Android build-tools `apksigner verify --verbose --print-certs`, and check 16 KiB
ELF page alignment on the native library. Do not commit signing credentials.

Android release keeps the performance-oriented workspace profile and panic
unwinding. The linker uses API-23-compatible APS2 packed relocations and 16 KiB
ELF page alignment. Gradle compresses JNI libraries in the APK and extracts them
at install time; this reduces download size at the cost of retaining an extracted
library alongside the installed APK. Keep R8 and resource shrinking enabled.
The Android dependency graph excludes the desktop Chromium/Viewpoint runtime;
HTTP and device transports share ring while HTTPS retains platform certificate
verification. Host build dependencies may still use another crypto provider and
do not count toward the APK's runtime dependency graph.

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
