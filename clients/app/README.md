# Native client

Run commands from the repository root unless a different directory is shown. See the [contribution guide](../../CONTRIBUTING.md) for the shared WebUI build.

## Windows and macOS

Install Rust, Bun, wasm-pack and the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for the host platform. Windows uses WebView2; macOS uses the system WebKit.

```sh
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo build --locked -p daat-locus --no-default-features
cargo build --locked -p daat-locus-app --features custom-protocol
```

Run `target/debug/daat-locus-app` (`.exe` on Windows). The client and service executables must be adjacent. Use `--profile dev` for an isolated development account. The client starts or reconnects to that profile's service.

Closing the window hides it; quitting the client leaves the service running. Use `daat-locus stop` or `daat-locus restart` for service maintenance.

For Tauri hot reload, first build the companion service, then run in `clients/app`:

```sh
bun install --frozen-lockfile
bun run tauri dev
```

## Desktop packaging

Build on the target platform:

```sh
cargo xtask package windows
cargo xtask package macos
```

Outputs are written to `target/<triple>/release/package`. Windows distributes the Setup executable, which installs WebView2 if needed; its MSI is an internal payload, not a standalone installer. The service archive works without WebView2 when only the HTTP web reader is needed. Windows includes BrowserUse but not desktop GUI control or virtual displays.

macOS packaging uses `--macos-signing-identity <identity>` when selecting a Keychain identity. Sign and notarize distributed packages with your own credentials. ComputerUse needs macOS privacy permissions for the app bundle you run.

## Android

Install JDK 17, Android SDK platform 37.0, build-tools 36.0.0, NDK 27.2.12479018, Bun and wasm-pack. Configure `ANDROID_HOME` and `NDK_HOME` for the installed SDK and NDK.

```sh
rustup target add wasm32-unknown-unknown aarch64-linux-android x86_64-linux-android
```

In `clients/app`:

```sh
bun install --frozen-lockfile
bun run android:dev
```

To make a debug APK, return to the repository root:

```sh
cargo xtask package android --debug
cargo xtask package android --target x86_64 --debug
```

For release builds, set `DAAT_LOCUS_ANDROID_SIGNING` to an external properties file with `storeFile`, `keyAlias` and `password`, then run `cargo xtask package android`. Keep the keystore and properties file private. APK updates must use the same signing identity.

The APK is copied to `target/<Android-triple>/<release|debug>/package`; `--out-dir` overrides this location. The maintained Android project is in `src-tauri/gen/android`. Do not run `android init` over it.

Android includes its own service and needs no companion executable. Terminal and Sudo are unavailable. PhoneUse requires Owner enablement and Shizuku or Root authorization. Work pauses in the background, and Android may terminate the process.
