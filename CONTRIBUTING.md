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

The sequential takeover fixture supplies only its three loopback discovery hints, so transient links can reconnect without ambient LAN traffic. ActorStore scheduling regressions run on a single multithreaded-runtime worker with a held transaction and a waiting reader; control tasks must remain runnable. Peer channel tests retain the first read/write/authorization failure across shutdown.

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
CI runs the three-device sequential takeover case in a separate step, with
`DAAT_LOCUS_TEST_STAGE_TRACE=1`, after the other library and HTTP tests. Keep
this explicit execution when excluding it from the parallel test pool.
Multi-device fixtures use multithreaded Tokio runtimes, like the service;
one device's synchronous storage work must not block every device's heartbeats.

Public signaling and account-sync checks use disposable identities and mock models.
Set `DAAT_LOCUS_TEST_STAGE_SIGNALING` to an explicitly chosen WSS origin, then run
`cargo test --locked --lib public_ -- --ignored --nocapture`. These opt-in checks
exercise SDP forwarding, authenticated P2P connections, message replication and
private model-configuration sync; they do not establish reachability through every NAT.

For connection measurements, enable debug logs for `daat_locus::stage`. The
`native_dial`, `offer_candidates`, `answer_candidates` and `ice_dtls_sctp` phase
records report elapsed milliseconds and outcomes without SDP or credentials.
Content completion reports original bytes and elapsed time; transport counters
separate content payload bytes from encoded protocol bytes. Compare repeated
attempts on the same network using connection success rate, median/tail setup
latency and verified content throughput. Include initiation direction, IPv4/IPv6,
network changes and loss; local loopback tests do not measure cellular performance.

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

## Windows Virtual Workspace

The independent library is `crates/daat-locus-virtual-workspace`, with a companion
UMDF/IddCx driver in `driver/`. Library builds do not build the service/WebUI.
The Windows service integrates it through the `computer_use` App;
Core still exposes only Actors. The managed display backend requires Windows 10 1903+ x64. Build/check:

```powershell
cargo test --locked -p daat-locus-virtual-workspace --all-targets
cargo clippy --locked -p daat-locus-virtual-workspace --all-targets -- -D warnings
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- probe
```

Build the Rust companion driver with stable Rust, MSVC x64 tools, a Windows SDK
and libclang (for WDK binding generation). No C++ driver source is compiled:

```powershell
cargo run --locked -p xtask -- idd-driver
cargo run --locked -p xtask -- idd-driver --check
cargo fmt --manifest-path crates/daat-locus-virtual-workspace/driver/Cargo.toml -- --check
# Distribution builds require an existing trusted certificate in CurrentUser/My:
cargo run --locked -p xtask -- idd-driver --release --certificate-thumbprint '<thumbprint>'
```

The xtask restores a version/SHA256-pinned Microsoft WDK NuGet package beneath
`target/virtual-workspace-deps`, compiles the UMDF DLL and validates/generates its
catalog with Inf2Cat. By default it produces a locally signed development package
and matching setup helper in `target/virtual-workspace-driver-debug`. The first
build creates a code-signing certificate and private-key backup in the gitignored
`.dev/windows-signing/` directory; subsequent builds reuse the same identity.
The directory/files allow only the current Windows owner. The PFX password is
random and protected with Windows CurrentUser DPAPI; do not copy these materials
to another account/machine. A missing CurrentUser/My key is restored from the
backup. Incomplete, corrupt or expired identities fail rather than silently rotate.
Do not delete this directory during routine cleanup. `--check`/`--test` do not
generate signing identities. The driver uses optimized/static release CRT code
in both modes to satisfy Universal API validation; `--release` selects distribution
signing and output in `target/virtual-workspace-driver`, not development fallback.
Development signing uses SHA256 without a network timestamp. Supplying
`--certificate-thumbprint` overrides the development identity and embeds that
certificate into the debug helper. A direct debug build of the `driver-setup`
feature also reuses the local identity; release helpers always embed the checked-in
distribution certificate and ignore `DAAT_LOCUS_DEVELOPMENT_CERTIFICATE`.
The xtask also runs Universal InfVerif and ApiValidator; the DLL uses a static CRT.
The driver is a separate Cargo workspace with a checked-in lockfile: normal lib
builds never invoke WDK tools. `--wdk-root` can use an existing WDK content root.
Distribute the INF, DLL and signed CAT together with `LICENSE.microsoft`;
unsigned build output is not an installable release. Signing the DLL precedes
catalog generation, using SHA256 and RFC3161 timestamps. Bundled distributions use
the dedicated self-signed `Daat Locus Virtual Display` identity. Its public DER
certificate lives in `crates/daat-locus-virtual-workspace/certificates`; private
signing/backup materials belong in the owner's `.daat-locus-private/windows-idd`
secrets repository. The installer explicitly provisions trust; this does not
constitute Microsoft certification or eliminate first-download warnings.
Validate installation and loading on clean supported Windows with Secure Boot and
memory integrity enabled, starting without the certificate and exercising the
actual installer's trust provisioning. Record unsupported configurations accurately.
Builds import signing keys only into CurrentUser/My; they never install drivers,
provision Root/TrustedPublisher trust or change Windows signature enforcement.
Trust is provisioned only by the elevated setup helper, with a separate ownership
ledger for each certificate. Building does not replace an already installed driver.

On an authorized elevated local connection, provision the signed package once.
The distribution helper imports only its embedded public identity into machine
Root and TrustedPublisher stores, then creates/binds the persistent PnP adapter.
Normal Windows elevation applies. Runtime creates no software devices: it opens the installed
device interface and requests a per-file monitor, without elevation, pipes, heartbeats
or an extra service. No preexisting virtual screen, manual registry editing or
display-settings action is required. Elevate only the installation command:

```powershell
# idd-driver already built this helper with the matching development public pin:
sudo --chdir "$PWD" "$PWD/target/virtual-workspace-driver-debug/daat-locus-display-setup.exe" install
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- screen --width 1920 --height 1080 --seconds 10
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- lab
# Without the companion driver, test only native WM_POINTER delivery:
cargo run --locked -p daat-locus-virtual-workspace --example workspace-debug -- pointer-lab --self-test
```

Library consumers use `DriverPackage::open(...).install()` during deployment,
and service hosts retain `SharedDisplay::global()` independently of Apps. The Windows
service prepares the enabled workspace before readiness and worker activation to create and
settle one screen without reserving a window region. Missing desktop/driver
prerequisites are logged without blocking the account service; ComputerUse still
reports native failures when used. Account maintenance commands do not create a screen.
Windows exposes its enable switch and supported virtual-display resolutions on
Settings → GUI Operation, alongside the Android-specific PhoneUse page contents.
Preferences are local to the service profile (`computer-use.json`), defaulting to
enabled at 1920×1080. A disabled profile creates no screen at startup. Disabling
returns controlled windows and removes the output; changing resolution returns
the windows, invalidates references, then reconfigures the same output. Pending
handoff or display/persistence failures leave input fenced until Owner retries.
Ordinary library construction remains lazy: consumers may call `initialize()`
explicitly or let the first `reserve(width, height)` create it. `DisplayRegion`
guards retain placement/lifecycle leases that may overlap. New windows prefer free
space, then cascade at their own size; fit uses the whole work area. Releasing the last region leaves the screen active.
Attach per-App `VirtualWorkspace`s to `region.display().id`; keep each region until
its window has been returned. Dropping the last service/region owner removes the
screen. These are process-local service owners, not a cross-process profile broker.
Run `cargo test --locked -p daat-locus-virtual-workspace --test native_shared_display -- --ignored --nocapture --test-threads=1`
on the unlocked signed-driver test machine to verify idle reuse, concurrent regions,
cursor/topology stability after warmup and final-owner cleanup. Replacement regions
retain both possible positions until native placement is confirmed; the test also
checks that both leases remain valid until explicitly dropped, even when their
bounds overlap other windows.
The same opt-in test checks initialization before any region and repeated startup
initialization without topology changes. Run
`DAAT_LOCUS_BUILD_WEBUI=0 cargo test --locked -p daat-locus --test computer_use_startup -- --ignored --nocapture --test-threads=1`
on that desktop to verify the real service creates its output before reporting
readiness, retains it without any Actor tool call, and removes it on shutdown.

Window observations use on-demand Windows Graphics Capture, not a desktop crop.
Run `cargo test --locked -p daat-locus-virtual-workspace --test native_window_capture -- --ignored --nocapture --test-threads=1`
on an unlocked desktop with a stationary cursor to verify covered-window pixels,
window-local child hit testing, disabled/modal refusal, desktop-state preservation
and repeated capture cleanup. This test needs neither IDD nor the Hook DLL.
The native Actor and Calculator tests additionally cover input through an unrelated
covering window using the production Hook/UIA routes; they still require IDD.

For explicitly short-lived diagnostic workspaces use
`VirtualWorkspace::create(DisplayOptions::default())` for an owned screen,
`configure(...)` for size/position, and `close()` to wait for removal. Drop/process
exit also unplugs owned screens through WDF file cleanup; borrowed `attach` outputs
are never removed. Up to eight independent workspace files can own monitors on the
adapter. Device/power loss retires their generation, without automatic recreation.
Normal close/Drop deactivates its path before unplugging it, preserving other modes
instead of letting hot-unplug normalize their positions. Process termination still
releases the monitor through file cleanup, with layout handled by Windows.
Automatic placement chooses free space adjacent to a physical output, preferring
its right edge. Explicit positions remain available when those edges are occupied.
Keep a physical display active. `lab --display '<exact GDI name from probe>'`
remains an optional diagnostic for a borrowed existing output.

The lab creates two disposable windows in the same process and GUI thread. Click
the human window to start the interactive experiment. `--self-test` activates only
that fixture window and runs bounded assertions. Use `--default-activation` to
measure default window activation instead of the fixture's PA_NOACTIVATE handler;
with `--self-test` this explicitly expects activation and verifies its detection.
No existing user window is moved, controlled or closed. Do not move the physical
mouse during automated assertions; interactive diagnostics allow human movement
and report it without claiming which source caused it. Windows foreground-lock
policy may deny automatic fixture activation; run from a focused local terminal
or use the click-to-start lab. An activation-denied run is a failed prerequisite.

To keep automated assertions while starting with an actual user click, run
`workspace-debug lab --self-test --click-to-start`. Click inside its Human window
within two minutes. The assertions then verify capture and native pointer delivery;
the process exits and releases its screen. Add `--default-activation` to measure
default activation on that owned screen instead of the pointer-aware fixture.
`--default-activation --noactivate-window` additionally gives the disposable
receiver `WS_EX_NOACTIVATE`. Its assertions still require detecting activation:
that window style does not suppress default `WM_POINTERACTIVATE` behavior.

Native integration tests are ignored by default, require an interactive unlocked
desktop, and fail with a prerequisite error instead of silently passing. Run them
one at a time. The physical-screen tests need no IDD. The owned-display lifecycle
and complete workspace tests require the signed companion driver already installed,
create their own screen without administrator access, and release it; no monitor environment variable
is needed. The lifecycle test asserts original outputs remain unchanged during
creation, resize and removal; the full workspace test also verifies fixture pixels:

```powershell
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace native_pointer_metadata_and_input_preservation -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace default_pointer_activation_is_detected -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace default_pointer_activation_on_owned_display_is_detected -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace noactivate_style_does_not_prevent_default_pointer_activation_on_owned_display -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace owned_display_lifecycle_preserves_existing_outputs -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace closing_one_workspace_preserves_another_on_the_same_adapter -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace owned_monitor_is_removed_when_client_process_is_terminated -- --ignored --exact --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_workspace same_process_same_thread_pointer_delivery_preserves_human_input -- --ignored --exact --nocapture --test-threads=1
```

CI builds/catalog-validates the driver on Windows and checks the Rust library
across its OS matrix. Hosted CI does not load an unsigned driver or provision an
interactive IDD desktop; do not
report ignored integration cases as passing. Successful pointer-aware fixture
delivery does not establish arbitrary-app compatibility or independent keyboard
focus. Windows owns pointer routing and activation. Never simulate pointer IDs
with PostMessage or restore human focus to hide a
failed experiment. See the [pointer API](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-injectsyntheticpointerinput)
and [activation contract](https://learn.microsoft.com/en-us/windows/win32/inputmsg/wm-pointeractivate).

The optional `hook/` Cargo member builds a Rust DLL that Windows loads into only
the selected target GUI thread's process. No target source changes or restarts are
required. This explicitly permits DLL injection for the Hook experiment. Build
the DLL and lab in the same profile/architecture, then run from a focused local
terminal with a stationary cursor:

```powershell
cargo build --locked -p daat-locus-workspace-hook
cargo build --locked -p daat-locus-virtual-workspace --example hook_lab
./target/debug/examples/hook_lab.exe --dll ./target/debug/daat_locus_workspace_hook.dll
# Automatically run the tests and leave the live preview open:
./target/debug/examples/hook_lab.exe --run-tests
# Automatically verify and close; no-driver diagnostic remains available:
./target/debug/examples/hook_lab.exe --self-test
./target/debug/examples/hook_lab.exe --physical-screen --self-test
cargo test --locked -p daat-locus-virtual-workspace --test native_hook -- --ignored --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_hosted -- --ignored --nocapture --test-threads=1
cargo test --locked -p daat-locus-virtual-workspace --test native_calculator -- --ignored --nocapture --test-threads=1
```

By default the lab creates its own 1280x720 IDD output and opens a live preview
with a Run tests button. The DLL defaults to the matching profile directory beside
the examples folder. The native GUI thread captures and paints actual BGRA frames
on a 50ms timer, preserving aspect ratio on resize. It reports test progress and
keeps showing the screen after success/failure until closed; closing releases the
fixture and owned screen. The preview does not forward mouse/keyboard input.
`--self-test` additionally requires a stationary physical cursor and exits after
verification; interactive mode allows physical cursor movement while checking
focus/capture. `--physical-screen` retains the separate-window no-driver lab.

The controller launches a separate disposable process containing two ordinary
Win32 windows on one GUI thread. It verifies CBT activation/focus vetoes, native
EDIT text/Unicode/navigation/Shift-selection, a native BUTTON message click,
unchanged human text/focus/cursor, sibling-target rejection, modifier restoration,
native trackbar dragging from 20 to approximately 80, hot detach/reattach,
timeout cancellation without delayed replay, and owner-process
death cleanup and rejection of mouse actions during human capture. Foreground
activation denial fails the prerequisite. The owned-display
case requires the installed signed driver; the physical-screen case does not.
These opt-in cases are not run by hosted CI. CI compiles the DLL on Windows.

`native_hosted` creates disposable frame/content processes on its own IDD output.
It verifies cross-process Chinese text/clicks, content-process replacement, sibling
rejection, and revocation of every route. `native_calculator` launches a new real
Calculator instance and only closes the new frame identified from that launch;
existing windows/processes are never reused or killed. UI Automation reads button
bounds and display results only: the production `WindowInput` API selects UIA,
invokes the actual controls, and must display 7 + 5 = 12 without changing
foreground/focus/cursor/capture. The test prepares its own Calculator/human windows
before measuring input; any temporary fixture style/input-queue setup is restored
before the baseline. It never restores focus after an action. It also verifies
unsupported caret/key input is refused and revoked routes cannot dispatch.
A separate native test verifies pure Hook's typed pre-input signature-policy error
and partial-lease cleanup. Do not disable OS/application mitigations to pass either
test. Normal runs ignore these environment-dependent cases; explicitly opting in
fails clearly when an installed app/driver or interactive prerequisite is missing.

`WindowInput` chooses Hook normally and UIA on a setup-time signature restriction.
UIA explicitly disables `AutoSetFocus`, retains available CBT guards, and supports
accessible click/vertical scroll/close patterns. It does not advertise arbitrary
keys, caret insertion or dragging. A failed/uncertain action never triggers backend
fallback or replay. Apps can still implement incompatible behavior; check actual
results and desktop receipts. `native_hosted` also verifies UIA on a real Win32
BUTTON as well as the cross-process Hook route.

Consumers hold `WindowHook::attach(&window, absolute_dll_path)` (or initially
`workspace.hook(...)`) while operating the target; `text`, `key` and `mouse` take
an explicit control HWND. `close`/Drop disables the policy and removes the hooks;
immediate complete DLL unloading is not promised. Only same-architecture targets
are supported. Protected/elevated targets may refuse hook installation/loading.
Input is synchronous message dispatch, not independent hardware input: no
Raw Input, accelerator-loop integration, `GetFocus`/`GetCursorPos` virtualization
or OLE drag/drop. `drag(control, path, timeout)` dispatches a complete left-button
gesture of 2..=128 signed-16-bit client points in one callback, without sleeps or
holding a button between commands. Start inside the client; later points may leave
it. Early cancellation sends WM_CANCELMODE and releases target capture. Existing
capture is refused; native controls can briefly take real capture inside a click
or drag. Do not claim simultaneous-drag isolation or arbitrary-app support.
`--native-touch` separately measures genuine touch with default window handling;
CBT can block activation while compatibility promotion still moves the system
cursor, so that path is not a cursor-preserving fallback. Unknown effects must be
observed, never replayed automatically.

Run `cargo xtask idd-driver --test` for driver unit tests, including teardown/error
classification, with the same WDK and static-CRT configuration as the driver.

For driver-stage diagnostics, enable its ETW provider only for the experiment:

```powershell
sudo logman start DaatLocusIdd -p '{70b75adc-3a20-4a68-99a4-bc5249203ca0}' -ets -f bincirc -max 64 -o "$PWD/target/idd-diagnostics.etl"
# Run the install or screen operation being diagnosed.
sudo logman stop DaatLocusIdd -ets
```

Events contain stages and status codes without application data. `EventWriteString`
payloads may appear as UTF-16 `EventPayload` in `Get-WinEvent -Path ... -Oldest`
XML on a machine without provider metadata. Keep traces under ignored `target`.
The circular capture retains at most 64 MiB. Power callbacks record entry/exit,
previous/target WDF power states, the diagnostic D0 cycle and lock acquisition.
Adapter initialization/completion, monitor departure, object deletion and frame
worker stop/join also have correlated stages. Driver object handles and per-pump
IDs correlate callbacks; ETW supplies timestamps, process and thread IDs.
Failures retain their NTSTATUS/HRESULT type and original code. Unexpected
swapchain failures additionally query the D3D device removal reason when tracing
is enabled. Stop signals, abandoned swapchains and stopped graphics devices are
normal teardown events; successful frames and E_PENDING waits are not logged.
With no capture enabled, events skip formatting/allocation. This instrumentation
does not change power/ownership/retry policy and requires an updated driver to be
installed before capturing the new events. It does not itself collect crash dumps.
To read string events even without a provider manifest (PowerShell 7):

```powershell
Get-WinEvent -Path "$PWD/target/idd-diagnostics.etl" -Oldest |
  Where-Object ProviderId -eq ([guid]'70b75adc-3a20-4a68-99a4-bc5249203ca0') |
  ForEach-Object {
    $record = $_
    $eventXml = [xml]$record.ToXml()
    $payload = $eventXml.Event.ProcessingErrorData.EventPayload
    $message = if ($payload) {
      [Text.Encoding]::Unicode.GetString([Convert]::FromHexString($payload)).TrimEnd([char]0)
    } else { $record.Message }
    [pscustomobject]@{ Time=$record.TimeCreated; PID=$record.ProcessId; TID=$record.ThreadId; Level=$record.Level; Message=$message }
  } | Format-Table -Wrap
```

`ProcessingErrorData` in this fallback describes missing reader metadata, not a
driver failure; inspect the decoded event's status and severity.

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
The default deadline is 1,800 seconds; concurrency accepts any positive Actor count.
Each Actor retains its conversation across rounds, so rounds also increase history
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

## macOS ComputerUse

The macOS client hosts `daat-locus-computer-use`; the independent service uses
only `daat-locus-client-protocol::computer_use`. Build with the macOS SDK and
Swift compiler supplied by Xcode or Command Line Tools. The client still targets
macOS 11; live window screenshots require macOS 14 or newer. Build the shared
WebUI/authentication WASM before the client, following the normal build steps.

```bash
cargo test --locked -p daat-locus-client-protocol -p daat-locus-computer-use
DAAT_LOCUS_BUILD_WEBUI=0 cargo test --locked -p daat-locus-app
DAAT_LOCUS_BUILD_WEBUI=0 cargo clippy --locked -p daat-locus-app -p daat-locus-computer-use --all-targets -- -D warnings
xcrun swift-format lint --strict crates/daat-locus-computer-use/native/macos.swift crates/daat-locus-computer-use/tests/fixtures/NativeFixture.swift
```

The native GUI check is explicitly opt-in and needs an interactive macOS desktop.
Use a separate installed test bundle and an isolated `mac-test-*` profile, create
a disposable account through its native WebUI, enable **GUI operation**, and
follow both system permission prompts. Grant Accessibility and Screen Recording
to the installed client bundle, rather than the service or a terminal. Complete
any system authentication yourself. If macOS requests it, quit and reopen the
client. Rebuilding an ad-hoc signed bundle can invalidate its previous TCC grant;
verify the actual permission state after each replacement. Do not edit TCC's
database, weaken OS protections or reuse a personal account for this check.

Build a separate AppKit fixture from
`crates/daat-locus-computer-use/tests/fixtures/NativeFixture.swift`, with executable
`native-fixture` in a normal `.app` bundle. Its bundle identifier must be outside
`io.daat-locus.*`, for example `io.daatlocus.fixture.macos`. Start exactly one fresh
fixture window, with its initial editor value and counter, and leave it behind
another application. The fixture contains no account or workspace access.

```bash
DAAT_LOCUS_BUILD_WEBUI=0 cargo build --locked --no-default-features -p daat-locus -p daat-locus-app --features daat-locus-app/custom-protocol,daat-locus-app/native-contract
# Copy these companion binaries into the separate test bundle and sign it.
open -n -a '/Applications/Daat Locus GUI Test.app' --args --profile mac-test-native --native-gui-contract /private/tmp/dl-native-result.json
```

The hidden flag exists only with `native-contract`. A test bundle may instead set
`DAAT_LOCUS_HOME` to an isolated directory whose name starts with `mac-test-`, and
`DAAT_LOCUS_NATIVE_CONTRACT` to the result path in its `LSEnvironment`. The check
waits up to five minutes for the actual settings and TCC prerequisites, then
writes a JSON report. Missing prerequisites or a failed assertion produce
`passed:false`; the application's successful launch is not a passing test.
Inspect the report's `passed` field explicitly. It verifies real PNG dimensions,
Unicode editing and button effects, native scrolling/slider/window geometry,
actual window closure and a fresh background application launch,
stale image/epoch refusal, cancellation, Actor scope isolation and reference
revocation. It also checks the frontmost process and physical pointer throughout
the run. Do not interact with the desktop while that measurement is running.
These native checks do not substitute for the App authorization/schema tests.
Normal CI runs the privilege-free IPC, coordinate and cookie regressions; a GUI
runner must be separately provisioned with the interactive session and grants.

## Process management and packaging

Windows ComputerUse is a device App for non-Core Actors. Its single remembered
desktop authorization explicitly covers GUI file access outside the project
sandbox; it does not expand Files/Terminal grants. Handing a window back revokes
its IDs before hooks are removed or physical placement begins. A running command
keeps handoff pending until it finishes. The native App test uses disposable
windows and the installed signed IDD; run from an unlocked foreground terminal
with a stationary cursor, after building the matching Hook DLL and hook_lab.
It checks complete-window PNG dimensions, magnified crop input routing, resizing,
moving, fitting, maximized-window recovery, stale image rejection and unchanged
foreground/focus/capture/cursor during window adjustment:

```powershell
$env:DAAT_LOCUS_BUILD_WEBUI='0'
cargo test --locked --no-default-features --lib computer_use_app
cargo test --locked --no-default-features --lib computer_use_app::windows::tests::native_actor_take_input_give_revocation_and_inflight_handoff -- --ignored --exact --nocapture --test-threads=1
```

For startup placement, also build the current CLI and `launch_target` example.
Run native checks serially:

```powershell
cargo build --locked --no-default-features --bin daat-locus
cargo build --locked -p daat-locus-virtual-workspace --example launch_target
cargo test --locked -p daat-locus-virtual-workspace --test native_launch initial_visible_window -- --ignored --nocapture --test-threads=1
cargo test --locked --no-default-features --lib computer_use_app::windows::launch_tests -- --ignored --nocapture --test-threads=1
```

The first check verifies the EXE's first `WM_SHOWWINDOW` rectangle, rather than a
later relocation. The App check uses the real clean launcher with a Unicode/spaced
executable path and verifies automatic acquisition, protected environment removal,
and preservation of an existing window. Shell monitor hints are advisory: the
separate opt-in `calculator_shell_activation_respects_the_virtual_monitor_hint`
compatibility check currently fails because Calculator shows on the physical
monitor and activates. Do not report this case as fixed or convert it to a passing
prerequisite skip; unconfirmed startup must retain its `outcome_unknown` result.

Windows x64 distributions also build and verify the Hook DLL, deployment helper,
and signed IDD package. Use `--windows-certificate-thumbprint <CurrentUser/My SHA1>`
to sign them, or `--idd-package <directory>` for an existing signed driver package
with already signed helpers. Bundled packaging requires the pinned distribution
identity (SHA1 `C2D252AEFAF27171C36AF1BD6DAF2FDC5441E462`); the helper rejects a
CAT signed by another identity before provisioning trust. The signing host must
already trust this public certificate for SignTool policy verification; on first
bootstrap a signed package can be installed by the helper before re-running that
verification. WiX Burn elevates the machine driver installer; the client MSI remains
per-user. Before driver/MSI work, a cached unelevated companion coordinates graceful
shutdown of this installation's running services in the namespace's default and
named profiles, waits for their clients to exit, and fences service startup until
the transaction restores the original launch options. Burn rollback stops restored
processes before MSI rollback and resumes the old installation afterwards. Only
previously running services/clients are restored. An installer crash cannot leave
a live startup fence permanently; retry retains saved restart parameters. Other
users or external workspace consumers can still cause Windows to require a reboot.
Run `cargo test --locked -p daat-locus --test installer_upgrade -- --test-threads=1`
for process shutdown, startup fencing and launch-option restoration. The ignored
case in that suite additionally requires a working signed IDD and checks actual
monitor removal; run it separately on an unlocked stationary desktop. Set
`DAAT_LOCUS_TEST_COMPANION` to a freshly built `daat-locus.exe`, then run
`cargo test --locked -p xtask -- service_lifecycle_surrounds_msi_and_restores_the_service_after_rollback --ignored --nocapture --test-threads=1`
with WiX 6/Bal installed to exercise real disposable MSI success and rollback.
Windows CI explicitly provisions and runs this bundle regression without installing
a driver or touching the account profile.
Use the setup EXE for coordinated upgrades; a standalone MSI does not execute the
bundle's service coordination or driver installation steps.
The install page discloses machine certificate trust. The CAT hash and
certificate presence detect an unchanged installed prerequisite. Its InstallCondition
skips the entire permanent prerequisite when already present during Install, while
Repair (Burn action 8) still executes the helper. Keep source caching for actual
deployments/maintenance; neither request machine caching nor unconditional cleanup
on the skip path. Do not relax ProgramData cache ACLs to work around a mixed-scope
bundle failure. Client uninstall
retains the shared adapter/trust; explicit complete removal uses elevated
`daat-locus-display-setup.exe uninstall`. This removes all matching driver versions
and only certificate entries recorded as installer-owned. If removal needs a reboot,
repeat it after restarting to finish trust cleanup. No background service or boot,
Secure Boot, HVCI, or signature-policy changes are added. Private keys are never bundled.
Windows may recenter the physical cursor when the IDD topology is created/removed,
even with unchanged physical output modes. Input cursor assertions therefore use
the configured-display baseline; lifecycle tests still check foreground/focus,
capture, references and physical output geometry. The backend does not reposition
the user's cursor to hide the topology behavior. A synchronous modal input/WM_CLOSE
handler can keep input/handoff pending until it exits; arbitrary apps remain
an explicit compatibility validation requirement.
Archive users run `sudo ./daat-locus-display-setup.exe install` from the extracted
directory once. `--skip-build` still verifies all required signatures.

Distribution driver and helper signing use SHA256 and RFC3161 timestamps. The default DigiCert
timestamp service falls back to Sectigo on failure. Override the endpoint with
`--windows-timestamp-server <URL>` when packaging, or `--timestamp-server <URL>`
for `idd-driver`; an explicit alternative endpoint fails without a fallback.
The service/client EXEs, MSI and outer setup EXE are not Authenticode-signed.

Trust rollback/ownership tests run with `--features driver-setup`, including in
Windows CI. For the opt-in elevated driver roundtrip, close all DL workspaces and
start with no bundled driver/certificate (use the explicit uninstall above):

```powershell
$env:DAAT_LOCUS_SIGNED_IDD_PACKAGE="$PWD/target/virtual-workspace-driver-debug"
cargo test --locked -p daat-locus-virtual-workspace --features driver-setup --lib windows::driver_trust::tests::native_bundled_installation_rejects_tampering_and_cleans_trust -- --ignored --exact --nocapture --test-threads=1
```

Run this test from an elevated terminal on a disposable Windows test environment.
It rejects a modified INF and checks trust rollback, installs/repairs a signed
package, creates/removes a real monitor, and removes driver/trust. It leaves the
bundled driver uninstalled; install it again afterward if needed. Missing elevation
or a signed package fails explicitly. This does not establish all target OS policies.

Also run the mixed-scope installer regression from a **non-elevated** Windows
terminal with WiX 6 and the Bal extension installed:

```powershell
cargo test --locked -p xtask computer_use::tests::present_machine_driver_does_not_require_machine_cache_for_user_install -- --ignored --exact --nocapture
```

It executes a real Burn install/uninstall with the production driver prerequisite
authoring, a controlled already-present driver state, and a unique disposable
per-user MSI. It checks the installed file contents and removal, without installing
a driver or changing machine trust/cache permissions. Full product installation
and Repair still need validation; archive/layout checks do not exercise Apply.

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
packaging rejects duplicate service assets or a missing native UI.

`.github/workflows` contains two workflows. `ci.yaml` checks debug builds, tests,
warnings and formatting without private signing keys. `release.yaml` builds and
validates release packages on Linux, macOS, Windows and Android on main pushes,
`v*` tag pushes, or manual runs on those refs. Main runs append
`-dev.YYYYMMDDHHMMSS` (UTC) to the patch version in the disposable checkout and
upload verified packages as Actions artifacts, without publishing a GitHub Release.
All platforms share one timestamp per workflow run. Program versions, Android
version names and package filenames carry this suffix; Windows/macOS numeric
installer fields and Android version codes retain the base version. Version tags
must match the checked-in package version; these builds keep the original version
and publish a GitHub Release after all platform builds pass.

Repository Actions Secrets must contain `WINDOWS_IDD_PFX_BASE64` and
`WINDOWS_IDD_PFX_PASSWORD` for the pinned distribution PFX, plus
`ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD` for the Android release
PKCS12 store. Encode the binary files as base64; keep their recoverable backups in
the private secrets repository. Missing secrets fail the release checks.
Windows imports the key into CurrentUser/My and trusts only the public certificate
in CurrentUser Root/TrustedPublisher for verification. It removes the imported
entries afterwards, without installing a driver or changing signature enforcement.
Packaging verifies the driver DLL/CAT, INF catalog membership, Hook/helper
signatures and pinned driver identity. Android uses temporary signing directories
with mode 0700 and files with mode 0600, signs with alias `daat-locus`, checks the
release certificate SHA256 and APK 16 KiB alignment, then deletes the signing files.
These checks do not establish Windows driver loading or Android startup on a device.
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

Use JDK 17+, Android SDK 37.0 / NDK 27.2.12479018, Bun, wasm-pack and Rust Android targets.
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

PhoneUse uses Shizuku API/provider 13.1.5 and librootkotlinx 2.0.0-beta02.
The latter requires SDK 37; the maintained project pins AGP 9.2.1, Kotlin 2.3.21
and Gradle 9.4.1. Runtime display isolation requires Android 15/API 35 or newer.
Use dedicated disposable emulators: one with a running shell-UID Shizuku service,
one with Magisk/another supported root provider and a working standard `su`.
Authorize the test-signed DL installation through the relevant manager. Do not
run against a personal account or delete existing device data to make tests pass.

Build the device APK, then `:phone-use-fixture:assembleDebug` and
`:app:assembleUniversalReleaseAndroidTest -x :app:rustBuildUniversalRelease` in
the Gradle directory with the same signing properties. Run
`python3 clients/app/scripts/test-phone-use.py --help` from the repository root.
Provide the device serial, method, APK paths, a `phone-test-*` account and
`DAAT_LOCUS_PHONE_TEST_PASSWORD`. The instrumentation creates that account through
the actual OPAQUE WebUI, or logs into the matching test account; it refuses other
accounts. The fixture is a separate unprivileged APK, never included in DL.
The test checks the Owner gate, actual privileged UID, PNG contents/dimensions,
Unicode replacement, key dispatch, main-screen focus/IME preservation, stale
state/epoch and cancellation rejection, Actor/window isolation, crop/scale,
release/reacquire, cross-Actor task preservation, immediate relaunch after task closure,
detach/reset and the disabled gate. It also exercises edge-to-edge geometry in
portrait/landscape, settings and drawer controls, dialog bounds, and real IME
show/hide transitions without residual padding. Display settings remain drafts until
Apply; the test verifies in-place resizing preserves display IDs and task contents,
rejects old observation references, and retains settings across a process restart.
`-PdaatTestBuildType=debug` selects debug instrumentation for iteration; validation
must also exercise R8. Tests fail when prerequisites are missing. A local deterministic model fixture also drives the real Core/Actor/App/JNI path and checks model image delivery plus the authenticated screenshot resource endpoint; no external model service is used.

The `android-phone-use` CI job runs both methods on the provisioned runner named
by `PHONE_USE_RUNNER`, with `PHONE_USE_SHIZUKU_SERIAL`, `PHONE_USE_ROOT_SERIAL`,
and private `PHONE_USE_TEST_SIGNING` (properties path) / `PHONE_USE_TEST_PASSWORD`.
Provision JDK 17+, SDK/NDK, emulator authorization and disposable test accounts on
that runner. An unconfigured/skipped runner is unavailable native coverage.

## MCP validation

Use isolated local MCP servers for protocol, schema, credential, revision and
uncertain-effect tests. Run `cargo test --lib mcp::tests` and
`cargo test --test http_api apps::`, alongside Actor permission/runner checks.
Windows native process tests require MXC/PSEC with native deny paths. They are
explicitly ignored on generic Windows hosts, where the availability test still
verifies that restrictive Auto mode fails closed. Configure the repository variable
`MXC_RUNNER` with a provisioned Windows runner label to enable the dedicated native
CI job. That job probes the capability and runs all native sandbox, Terminal and
MCP cases; an unavailable capability fails the job. Do not count a skipped native
job as positive isolation coverage. On a supported local Windows host run:

```powershell
$env:DAAT_LOCUS_BUILD_WEBUI = '0'
cargo test --locked --no-default-features --lib sandbox:: -- --include-ignored --test-threads=4
cargo test --locked --no-default-features --lib terminal_app::process::tests::terminal_shell_runs_inside_required_native_isolation -- --ignored --exact
cargo test --locked --no-default-features --lib mcp::tests::stdio_uses_native_sandbox_explicit_environment_and_reaps_processes -- --ignored --exact
```

Linux CI installs bubblewrap and enables unprivileged user namespaces, then runs
all ignored sandbox cases explicitly. The backend requires pidfd/openat2 (Linux
5.6 or later), bubblewrap's `--disable-userns`/`--seccomp` support and a reviewed
x86-64 or AArch64 syscall ABI. Native tests cover real Actor read/write scopes,
missing protections, concurrent mounts, private devices/procfs, syscall filtering
and descendant cleanup. The syscall fixture requires rustc. macOS runs its
Seatbelt native cases in the normal suite, including unusual path characters,
future private paths, symlink escapes, process cleanup and ancestor rename protection.
CI explicitly runs the ignored private control-socket case on macOS; it initializes
the OS-owner control directory and removes only its own unique socket. Linux also
checks that a sandbox's TCP connection cannot claim OS-owner authority while a
separate ordinary owner process can still bootstrap.

The Windows stdio integration verifies allowed workspace reads, denied private
storage, explicit environment and child cleanup. No real account or
public MCP server is required. The WebUI mock gallery includes Apps list, empty,
loading and failure scenes; its installation form can simulate connection failure
with `https://unavailable.example/mcp`. Mock installation never contacts that URL.
