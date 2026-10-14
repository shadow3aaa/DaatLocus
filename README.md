# Daat Locus

[简体中文](README_zh-CN.md)

Daat Locus is an Actor system with one conversation interface:

```text
Actor = Agent core + installed App instances
```

The account Actor accepts requests and delegates to persistent Actors. Devices
share its conversation and model context, provide tools, and can take over its
worker. Ordinary Actors keep their own host. Follow Actor links to open work
conversations; no permanent sidebar, TUI, Workflow or Sleep framework is needed.

The WebUI supplies messages, attachments, queue guidance, compact activity and
results. Account/settings live in a floating menu, conversation search in the
opposite corner. Chinese and English are supported; language follows the browser
by default and can be selected in Appearance. Guest sharing and public tunnels
are removed; same-account devices connect through authenticated P2P channels.

## Run

Install Rust, Bun and wasm-pack:

```sh
rustup target add wasm32-unknown-unknown
cd webui
bun install --frozen-lockfile
cd ..
cargo run -- serve
```

Normal builds compile and embed the WebUI, including authentication WASM. Open
the printed address and create an account locally, or sign in with the same
account name and password on another device. OPAQUE keeps passwords in the client;
browser sessions use HttpOnly cookies. `--profile NAME` isolates local accounts.
Discovery and synchronization are automatic. LAN and SSH need no external relay;
CF Worker/DO is an optional public rendezvous/signaling transport.

Use `daat-locus --help` for clap help and the original character logo.
`serve` runs in the foreground; `start --open`, `status --json`, `stop` and
`restart` manage the selected profile through local OS-owner IPC. Stopping or
restarting keeps Actor identity and history. Headless first-account creation is
`account create NAME --password-stdin`; never pass passwords as command arguments.

Windows and macOS use the [Tauri desktop client](clients/app/README.md), sharing
the same WebUI and account interfaces. Build with
`cargo build -p daat-locus -p daat-locus-app`, then run `daat-locus-app` next to the
service executable. `start --open` prefers the installed client. Closing its
window hides it; Quit client preserves background tasks; Stop and exit shuts the
service down normally. The independent backend has no GUI dependencies. Linux
retains the service and browser entry. Android embeds the same core and WebUI in an APK;
a terminal is not required. See [native client builds](clients/app/README.md#android).

## Permissions and recovery

| Mode | Behavior |
| --- | --- |
| Auto | Independent model review approves or denies; errors deny, without human fallback. |
| Ask | Owner reviews the immutable action and requested access. |
| Full access | Runs without approval review, within hard identity and access boundaries. |

Extra filesystem access applies to one action. Read-only identity, tool grants,
framework credentials and other Actors' private data remain protected. Native
sandbox requirements fail closed. Files use SHA256 freshness checks; resources
are authorized by Actor and have stable pathless descriptions across devices.

Execution has no fixed message, ReAct-round or tool-batch limit. A full context
window is cleared with an archive-recovery prompt rather than summarized or
slid. Original history persists; interrupted or uncertain effects are not replayed.
The account's worker keeps healthy affinity and hands off to a synchronized device.
New devices join as replicas without adding permanent voting dependencies. If
account authority becomes unreachable, an eligible synchronized device continues
in a durable local recovery branch. Reconnection merges the conversation while
retaining original branches and never replaying uncertain App effects.

## Develop and package

See [contribution instructions](CONTRIBUTING.md), [architecture](docs/architecture.md),
[model catalog](docs/model-catalog.md) and [license notes](docs/license-attribution.md).
Explicit `DAAT_LOCUS_BUILD_WEBUI=0` skips frontend toolchains for backend-only checks.

`cargo xtask package binary --target <triple>` creates a binstall archive.
`package windows` and `package macos` create native installers on their respective
platforms. Packages require an embedded WebUI binary. No Git submodules are needed.
