# Native client

Windows and macOS use one Tauri application and the existing WebUI. The companion
`daat-locus` service supplies account login and incremental HTTP/SSE interfaces
over its verified loopback origin. Tauri serves the independently built
`webui/dist` through frontendDist and its built-in asset compression. The native
transport keeps cookies in the profile's WebView store (Android CookieManager),
never exposing Set-Cookie to JS, and pulls/cancels response chunks on demand.
Authorized resource URLs use a bounded dl-resource protocol and sandboxed previews. The client exposes no
JavaScript filesystem, shell or process commands; resources and approvals retain
the service's account and Actor checks.

A single profile/origin-bound native account command handles first registration,
pre-login connection settings and new-device OPAQUE enrollment over OS-owner IPC.
WebView network subprocesses cannot supply an unrestricted OS-owner socket proof.
This bridge accepts only bounded bootstrap operations while the profile has no
credentials; it never returns a session cookie or manages an existing account.
Passwords remain in the shared client WASM, and successful login creates the
ordinary HttpOnly cookie through HTTP. Resource preview windows cannot use the bridge.

```powershell
bun scripts/build-webui.ts
cargo build --locked -p daat-locus --no-default-features
cargo build --locked -p daat-locus-app --features custom-protocol
./target/debug/daat-locus-app
```

The two executables must be adjacent. `--profile NAME` selects the same isolated
profile as the maintenance CLI; `DAAT_LOCUS_HOME` retains its existing meaning.
The client validates the service's installation, profile and loopback origin
before enabling API requests. Duplicate launches raise that profile's existing
window. Each profile uses separate WebView storage on Windows and on macOS 14+;
on older macOS, server-side profile/session binding still enforces authorization.

Closing the window hides it and preserves execution. The Windows tray offers
Minimize to tray (or Open Daat Locus when hidden) and Quit Daat Locus, in a native
menu with larger rows and system keyboard/accessibility support. The macOS menu
keeps Open, Minimize to tray and Quit. Quitting leaves the independent service
running. Service maintenance uses the CLI's OS-owner IPC. CLI stop eventually
exits the client; restart reconnects it to the
verified service. Actor data is never removed by these operations.

The Windows menu has an opt-in native interaction check:
`cargo test -p daat-locus-app native_popup_keyboard_and_dismissal -- --ignored`.
Activate its isolated test window, then press Escape, Down/Enter, Down/Down/Enter,
and Escape as each menu appears. It checks light/dark rendering, cancellation and
both selections without starting a service or accessing an account.

The main window blends its title area into the shared UI: Windows uses custom
minimize/maximize/close controls and a drag area; macOS uses an overlay titlebar
with native traffic lights reserved inside the navigation rail. Content reaches
the top of the window; narrow layouts inset only the affected header controls.
The transparent drag region leaves buttons and inputs interactive.
Window commands are limited to the verified main
conversation WebView and cannot target resource previews or backend operations.
`--background` starts only in the tray. Browser entry has no native window controls.

External HTTP(S) links open in the system browser. Authorized resources use the
native resource protocol in sandboxed conversation dialogs; HTML/SVG have no
same-origin privileges or native command access. HTML file input and drag/drop use the existing attachment
upload path, with opaque resource IDs.

`cargo xtask package windows` builds the Windows installers;
`cargo xtask package macos` builds the app and pkg on macOS. Native installers
include both executables with assets only in the client. Binary archives contain
the standalone browser-serving CLI instead. Linux retains the headless service and
browser interface. Android uses an in-process backend; its installation does not require a companion executable or terminal.

The Windows setup EXE includes Microsoft's signed Evergreen WebView2 bootstrapper;
installing a bare MSI requires an existing WebView2 runtime. The standalone CLI
archive does not require WebView2. Building the setup downloads and verifies that bootstrapper. macOS uses
the system WebKit. Signing and notarization require the distributor's credentials.

Platform prerequisites: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

## Web page hosting

The Web App uses independent native pages: WebView2 on Windows, WKWebView on
macOS and Android WebView. No bundled Chromium download is needed. These pages
have no account-native command grants and never use the main UI's cookie store.
Windows persists a separate site profile per Actor. macOS currently uses isolated
nonpersistent stores, so site logins do not survive client exit. Android requires
a WebView provider supporting AndroidX MULTI_PROFILE and accepts navigation only
while the Activity is foregrounded. Unsupported isolation fails explicitly.

The desktop host connects to the companion's OS-owner IPC through the hidden
`web-host` command. Android consumes the same bounded mailbox in process through
JNI. The service remains free of a GUI event loop. Reconnection never replays an
interrupted navigation. Web documents remain readable independently of a live page.
The Web HTTP source also works without a native client, with public-destination
validation, no cookies and no page JavaScript.

BrowserUse shares the native infrastructure and operates one device-wide browser
workspace. Every Actor sees the same tabs and site logins. The navigation rail
and mobile drawer open `/browser`, a full content page in the same position as
Virtual Displays. It replaces the conversation instead of opening a separate
window or a concurrent conversation pane. Shared WebUI tabs, address/history
controls, history and downloads manage the embedded native viewport.
Downloads opens a scrollable popup beneath its toolbar button on wide screens;
narrow screens retain the full-page download list and Back navigation. The popup
uses the same native clipping as the browser menu, keeping the web page live.
Leaving the browser or opening its full-page library detaches that viewport;
popup menus leave it mounted and running. Both detachment and stopping an Actor
preserve tabs. The Owner and models can operate at any time;
Owner input invalidates old observations. BrowserUse site storage is isolated from
both the account UI and Web's per-Actor document capture.
The shell and visited pages are independent WebViews. Rounded popup regions are
clipped from the native page layer, with input passing through to the shell;
closing a popup clears its region. Pages continue running, without screenshot
stand-ins or shell UI injected into the visited document. The DOM-only mock keeps
its iframe visible; `?browser_scene=cursor&native_clip=1` also emulates native
clipping for overlay checks.

Desktop tabs support middle-click close and mouse drag reordering; narrow/mobile
layouts keep touch scrolling without drag sorting. Common shortcuts work in
chrome and focused pages: F5/Ctrl+R, Ctrl+F, Ctrl+L/Alt+D, Ctrl+T/W,
Ctrl+Tab/Ctrl+Shift+Tab, Ctrl+PageUp/PageDown, Ctrl+1–9, Alt+Left/Right,
Ctrl+plus/minus/0, and F3/Shift+F3 or Ctrl+G/Shift+Ctrl+G. macOS also accepts
Command for the shared shortcuts. Tab order is shared with BrowserUse, and
closing the current tab selects its right neighbor or the last tab on its left.

The menu captures the visible page without browser chrome or the virtual cursor:
desktop copies a PNG image to the native clipboard; Android saves it to Downloads
(Android 7–9 uses the system save dialog). Find supports highlights, next/previous,
Enter/Shift+Enter and Escape, clearing native matches when closed or leaving a tab.

Input uses screenshot pixels and a one-use observation ID. Native DOM/viewport
changes, user input and a newer observation reject stale operations. Windows uses
WebView2's native CDP methods, macOS local AppKit events and WKWebView snapshots,
and Android WebView native input and PixelCopy. No debug port, arbitrary evaluation
or system-wide input is exposed. PNGs are published in the calling Actor's resource
ledger. Interrupted effects are never replayed automatically.

Accepted inputs display a custom cursor without a text label, with subtle click,
drag, scroll and keyboard indicators. This transient decoration neither dispatches
input nor displays typed text. It is excluded from model screenshots and hidden
on Owner navigation input.

Opt-in local native contracts (no personal accounts):

```powershell
cargo build -p daat-locus-app --features web-contract
python clients/app/scripts/test-web-host.py target/debug/daat-locus-app.exe
python clients/app/scripts/test-browser-use.py target/debug/daat-locus-app.exe --embedded
python clients/app/scripts/test-browser-use.py target/debug/daat-locus-app.exe --hidden
python clients/app/scripts/test-browser-use.py target/debug/daat-locus-app.exe --embedded --reduced-motion
python clients/app/scripts/test-browser-downloads.py target/debug/daat-locus-app.exe
# With the standalone WebUI mock listening on 5177:
python clients/app/scripts/test-browser-ui.py target/debug/daat-locus-app.exe target/browser-ui-review
python clients/app/scripts/test-browser-page-actions.py target/debug/daat-locus-app.exe
```

The BrowserUse contract checks trusted click/hover/drag, Unicode input, modified
keys, real form submission, history, scrolling, screenshot pixels, stale-state
rejection, painted cursor/model-screenshot separation, shared cookies/tab
preservation and concurrent Owner input fencing. The download contract verifies
original file bytes and visit persistence against a local fixture. The UI contract
exercises mock menus, history filtering, omnibox navigation and tab preservation,
and saves desktop/narrow screenshots without using a personal account.

Mock entry points: `/browser`, `/browser?view=history`, `/browser?view=downloads`,
`/browser?view=settings`, `/browser?browser_scene=empty` and
`/browser?browser_scene=cursor&motion=full` (or `motion=reduce`). The cursor page
uses the same native decoration script and has controls for movement, click,
double click, drag, scroll, keyboard feedback and viewport-edge clipping.

## Android

The APK contains the same Actor core and account transport, with the WebUI
embedded once by Tauri. Its backend disables embedded-webui.
A phone can create an account and infer independently with Actors, app-private
Files, Web and Artifacts. Terminal is unavailable.
Joining an existing account shares its core Actor. Android uses the same
execution and recovery rules as other devices, without a phone-specific role.
New devices join as synchronized replicas; voting authority transfers through
joint membership. A fresh phone-only account initializes durable local authority.
If existing authority becomes unreachable, a synchronized eligible device can
continue in a local recovery branch; reconnecting merges the conversation without
replaying uncertain App effects.
Background transitions stop inference and preserve interrupted-effect records;
foreground resumes queued work. Android can kill the process, so this is not a
permanent background execution service. Transport reconnects and resumes journals.

Install Android SDK 37.0, NDK 27.2.12479018 and JDK 17+, with Android Rust targets and wasm-pack.
After `bun install --frozen-lockfile` in `clients/app`, run:

```sh
bun run android:dev
# Run packaging from the repository root:
DAAT_LOCUS_ANDROID_SIGNING=/private/release.properties cargo xtask package android
cargo xtask package android --target x86_64 --debug
```

Packaging prints the APK location and copies it to
`target/<Android-triple>/<release|debug>/package`; `--out-dir` overrides that location.
`--skip-build` reuses Gradle output after checking version, ABI, native WebUI and
absence of duplicate backend assets.
The Bun `android:build` script remains available as the shared underlying build step.

The external properties file contains `storeFile`, `keyAlias` and `password`.
Use a private absolute keystore path; never commit this file or the keystore.
Release signing uses that key; debug builds use Android's debug key. Back up the
release key privately: future APK updates must use the same signing identity.
The Android project under `src-tauri/gen/android` is maintained source. Do not
re-run `android init` over its lifecycle, HTTPS, signing and manifest changes.
Only loopback service traffic permits HTTP; pages are Tauri assets and public transports use authenticated TLS.
Resources open in a sandboxed dialog within the authenticated WebView, and system
file selection uses Android's picker without storage-wide permissions.

Native downloads use the system save dialog (Android SAF) from Rust. Only the
authorized resource path and a suggested name cross IPC; the selected destination
and session cookies never reach JavaScript. Filesystem/dialog plugins have no
JavaScript permission grants.
