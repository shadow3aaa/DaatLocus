# Native client

Run commands from the repository root unless a different directory is shown. The [contribution guide](../../CONTRIBUTING.md) covers the shared WebUI and backend checks.

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

Native browser and GUI checks need a real client. UI mocks do not test page capture, keyboard focus, system permissions or download dialogs.

## Desktop packaging

Build on the target platform:

```sh
cargo xtask package windows
cargo xtask package macos
```

Outputs are written to `target/<triple>/release/package`. Windows setup includes the WebView2 bootstrapper; installing the bare MSI needs WebView2 already installed. The service archive works without WebView2 when only the HTTP web reader is needed.

Windows x64 GUI packaging also needs the signed virtual-display driver, Hook DLL and setup helper. Supply `--windows-certificate-thumbprint <thumbprint>` for the required signing identity, or `--idd-package <directory>` for an existing matching package. Run `cargo xtask package windows --help` for all options.

For local driver development, install MSVC x64 tools, a Windows SDK and libclang, then use:

```sh
cargo xtask idd-driver --check
cargo xtask idd-driver --test
cargo xtask idd-driver
```

The build downloads the pinned WDK dependencies and creates or reuses the local signing identity in `.dev/windows-signing`. It does not install a driver. Preserve that directory; `--check` and `--test` do not create keys. Distribution signing uses `cargo xtask idd-driver --release --certificate-thumbprint <thumbprint>`.

Windows ComputerUse needs Windows 10 1903+ x64 and the matching installed driver. Driver installation and trust changes need an explicitly authorized elevated session. Native ComputerUse tests also need an unlocked test desktop.

macOS packaging uses `--macos-signing-identity <identity>` when selecting a Keychain identity. Sign and notarize distributed packages with your own credentials. ComputerUse needs macOS privacy permissions for the actual app bundle used in the test.

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

## Native checks

Run the backend contracts with `DAAT_LOCUS_BUILD_WEBUI=0`:

```sh
cargo test --locked -p daat-locus --no-default-features --lib web_
cargo test --locked -p daat-locus --no-default-features --lib browser_use_app
```

On Windows or macOS, native browser checks are opt-in:

```sh
cargo test --locked -p daat-locus-app --features web-contract --test browser_live -- --ignored --test-threads=1
```

These checks open disposable native pages. Use a separate test environment for input or focus checks. Android instrumentation uses the runner in `src-tauri/gen/android/app/src/androidTest`; run `python clients/app/scripts/test-phone-use.py --help` from the repository root for the provisioned PhoneUse test. Use a disposable emulator or device, not a personal account.
