# Contributing

[简体中文](CONTRIBUTING_zh-CN.md)

Use a separate profile and temporary workspace for development. Tests must not use a personal account or contact real model providers. Repository rules are in [AGENTS.md](AGENTS.md).

## Requirements

- Stable Rust with rustfmt and Clippy.
- Bun; the WebUI's package manager version is pinned in [package.json](webui/package.json).
- wasm-pack and the `wasm32-unknown-unknown` Rust target for the real WebUI.
- Platform build tools: MSVC on Windows, Xcode tools on macOS, or a C/C++ toolchain on Linux. Linux sandbox tests also require bubblewrap and working user namespaces.

Native client and Android requirements are in [the client guide](clients/app/README.md).

## Workspace filesystem

Workspace is available to non-Core Actors on devices with native mounting support. Use `default` to work in the original directory, or `fork` and `select` to work in a shared isolated branch. Save versions with `commit`, inspect them with `history`, `diff` and `read`, and fork a saved version to recover earlier work. `merge` combines two references into a fresh branch; `publish` integrates a branch or saved version into the original directory. Use the returned fixed versions and explicit path resolutions to resolve conflicts. Share a selected execution directory when creating collaborating Actors.

- Windows uses WinFsp 2.1 or newer in the 2.x series, matching the service architecture. Install its official signed driver with administrator authorization, then run the service as the ordinary OS user. File and directory symbolic links and junctions are persisted with their type and target. Absolute links into a project are relocated into its mounted branch; publishing relocates them back to default. Links outside the project retain native target semantics and remain subject to the device sandbox. Publishing symbolic links into native NTFS requires the OS user's normal symlink creation privilege or Developer Mode. Hard links and alternate data streams remain unsupported. Daat Locus neither imports a filesystem-driver certificate nor enables test signing. [WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos](https://github.com/winfsp/winfsp).
  Enable WinFsp's driver-side Mount Manager mode once from an administrator PowerShell before starting the service. This allows directory mounts and Win32 canonical paths without elevating the service:
  ```powershell
  reg.exe add HKLM\SOFTWARE\WinFsp /v MountUseMountmgrFromFSD /t REG_DWORD /d 1 /f /reg:32
  ```
- Linux requires an accessible `/dev/fuse`, a working `fusermount3` or `fusermount` helper, and fusectl mounted at `/sys/fs/fuse/connections` with owner access to the connection's abort control. Install the distribution's FUSE package through its normal administrator workflow. Workspace mounts do not request `allow_other`.
- macOS and Android currently have no Workspace projection backend and expose no Workspace App. Their ordinary execution directories continue to work.

Restart the service after changing native filesystem dependencies.

## Build and run

From the repository root:

```sh
rustup target add wasm32-unknown-unknown
bun scripts/build-webui.ts
cargo run --locked -- --profile dev serve
```

Rebuild the UI before rebuilding a service with embedded assets after frontend changes.

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

Open `/__mock` at the address Vite prints.

For hot reload against a development service, run `bun run dev` instead. The default API target is `http://127.0.0.1:45825`; set `DAAT_LOCUS_DAEMON_URL` to match the service's printed address. Set it before starting Vite. Use the same browser origin for account requests.

`DAAT_LOCUS_MOCK_MACOS=1` and `DAAT_LOCUS_MOCK_PHONE=1` select platform mocks.

## Packaging

Run packaging from the repository root. It prepares the shared frontend before building the selected distribution.

| Output | Command |
| --- | --- |
| Service archive | `cargo xtask package binary --target <triple>` |
| Windows installers, on Windows | `cargo xtask package windows` |
| macOS package, on macOS | `cargo xtask package macos` |
| Android debug APK | `cargo xtask package android --debug` |

Outputs go under `target/<triple>/<profile>/package`; `--out-dir` overrides the package directory. Use `cargo xtask package <format> --help` for signing and reuse options.

Windows GUI packaging needs a signed virtual-display package and matching helpers. macOS and Android release builds need their signing credentials. See [the client guide](clients/app/README.md) before building installers.

Do not delete `.dev/windows-signing` during cleanup. It contains a persistent local development signing identity. Never commit credentials, private keys, runtime data or build output. Preserve the attribution files described in [license notes](docs/license-attribution.md).

## Changes

Keep changes focused and describe the visible behavior, reason and checks. Update both locale catalogs when changing UI text. Put runtime prompts in `src/prompts` and bundled skills in `src/builtin_skills`; keep instructions short and tool-specific parameters in schemas.

Keep documentation about using, building and testing the current code. Release notes belong on GitHub Releases. Do not add architecture documents, task logs, changing test totals or screenshots of intermediate work to the repository.
