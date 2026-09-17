# Release Checklist

Use this checklist before tagging a Daat Locus release.

## Runtime Safety

- Confirm daemon control endpoints still require authentication:
  - `/commands/run`
  - `/daemon/shutdown`
  - dashboard snapshot
  - dashboard stream
- Confirm `/health` remains minimal and unauthenticated.
- Confirm daemon startup gates runtime commands until the lifecycle state is
  `ready`.
- Confirm terminal worker processes do not inherit protected provider secret
  environment variables.

## State And Migration

- Start from an existing `~/.daat-locus` layout and confirm legacy files migrate
  to the current config, memory, state, artifact, journal, log, and runtime
  directories.
- Confirm persistent queues survive restart:
  - events
  - pending work
  - plan
  - workflow run records
- Confirm config files are written with private permissions on Unix-like
  systems.

## Release Packaging

- Build local release candidates through
  `cargo build -p daat-locus --release --locked`; the root `build.rs` builds
  WebUI assets before Rust compilation and embeds them into the daemon by
  default.
- Run the `Release Binaries` workflow for the release tag and confirm Linux,
  macOS, and Windows `.tar.zst` archives from
  `target/<target>/release/package/`, plus the Windows `*-setup.exe`, are
  uploaded to the GitHub Release.
- Confirm release binaries, the generated Windows MSI, and the Windows
  bootstrapper embed the WebUI assets.
- Confirm the Windows MSI was generated from `assets/logo.svg` and shows the
  expected Add/Remove Programs and Start Menu shortcut icons.
- Confirm the Windows `*-setup.exe` bootstrapper wraps the MSI, uses the product
  icon as its file icon, and shows the standard bootstrapper UI.
- Install the Windows bootstrapper on a clean Windows user profile and confirm
  `daat-locus --help` works from a new terminal without administrator rights.
- Confirm the Windows MSI updates the user `PATH`, supports upgrade installs,
  blocks downgrades, and leaves `~/.daat-locus` runtime data intact on
  uninstall.
- Confirm `cargo-binstall` resolves the release asset and does not fall back to
  source compilation.
- Confirm browser runtime download behavior is expected for the release.
- Record any unpinned or latest-version download behavior in release notes.
- Review dependency updates for license or attribution changes.

## Compatibility

- Load an existing config file from the previous release.
- Run the setup wizard on a clean machine profile.

## Quality Gates

- Run `cargo fmt --all -- --check`.
- Run WebUI tests with `cd webui && bun install --frozen-lockfile && bun run test`.
- Run `cargo clippy --locked --all-targets -- -D warnings`.
- Run `cargo test --locked`.
- Run `cargo deny --locked check bans sources licenses`.
- Run targeted manual smoke tests for:
  - first-time setup
  - daemon start and attach
  - Telegram ACL approval flow when Telegram is enabled
  - terminal command execution

## Release Notes

- Write `docs/releases/<tag>.md` before tagging; use the exact tag name such as
  `docs/releases/v0.2.0.md`. The `Release Binaries` workflow reads this file as
  the GitHub Release body and fails if it is missing.
- Document user-visible config changes.
- Document state migration behavior.
- Document any supply-chain pinning gaps.
