# Native client

Windows and macOS use one Tauri application and the existing WebUI. The companion
`daat-locus` service supplies the embedded assets, account login and incremental
HTTP/SSE interfaces over its verified loopback origin. The client exposes no
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
cargo build --locked -p daat-locus -p daat-locus-app
./target/debug/daat-locus-app
```

The two executables must be adjacent. `--profile NAME` selects the same isolated
profile as the maintenance CLI; `DAAT_LOCUS_HOME` retains its existing meaning.
The client validates the service's installation, profile and loopback origin
before loading the window. Duplicate launches raise that profile's existing
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
with native traffic lights. Window commands are limited to the verified main
conversation WebView and cannot target resource previews or backend operations.
`--background` starts only in the tray. Browser entry has no native window controls.

External HTTP(S) links open in the system browser. Authorized resource popups
remain in native WebViews sharing their opener's environment; HTML/SVG keep the
backend's sandbox CSP. HTML file input and drag/drop use the existing attachment
upload path, with opaque resource IDs.

`cargo xtask package windows` builds the Windows installers;
`cargo xtask package macos` builds the app and pkg on macOS. Binary archives for
these targets include both executables. Linux retains the headless service and
browser interface. Android uses an in-process backend; its installation does not require a companion executable or terminal.

The Windows setup EXE includes Microsoft's signed Evergreen WebView2 bootstrapper;
installing a bare MSI or running the binary archive requires an existing WebView2
runtime. Building the setup downloads and verifies that bootstrapper. macOS uses
the system WebKit. Signing and notarization require the distributor's credentials.

Platform prerequisites: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).

## Android

The APK contains the same Actor core, account transport and embedded WebUI.
A phone can create an account and infer independently with Actors, app-private
Files, HTTP Browser, Artifacts and Study. Terminal and Chrome are unavailable.
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

Install Android SDK 36, NDK 27 and JDK 17+, with Android Rust targets and wasm-pack.
After `bun install --frozen-lockfile` in `clients/app`, run:

```sh
bun run android:dev
# Run packaging from the repository root:
DAAT_LOCUS_ANDROID_SIGNING=/private/release.properties cargo xtask package android
cargo xtask package android --target x86_64 --debug
```

Packaging prints the APK location and copies it to
`target/<Android-triple>/<release|debug>/package`; `--out-dir` overrides that location.
`--skip-build` reuses Gradle output after checking version, ABI and embedded WebUI.
The Bun `android:build` script remains available as the shared underlying build step.

The external properties file contains `storeFile`, `keyAlias` and `password`.
Use a private absolute keystore path; never commit this file or the keystore.
Release signing uses that key; debug builds use Android's debug key. Back up the
release key privately: future APK updates must use the same signing identity.
The Android project under `src-tauri/gen/android` is maintained source. Do not
re-run `android init` over its lifecycle, HTTPS, signing and manifest changes.
Only loopback WebUI traffic permits HTTP; public transports use authenticated TLS.
Resources open in a sandboxed dialog within the authenticated WebView, and system
file selection uses Android's picker without storage-wide permissions.
