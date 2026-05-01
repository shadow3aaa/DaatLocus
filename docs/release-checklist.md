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
- Confirm terminal and workspace app worker processes do not inherit protected
  provider secret environment variables.

## State And Migration

- Start from an existing `~/.daat-locus` layout and confirm legacy files migrate
  to the current config, memory, state, artifact, journal, log, and runtime
  directories.
- Confirm persistent queues survive restart:
  - events
  - pending work
  - plan
  - Hindsight retain handoff queue
  - workflow run records
- Confirm config files are written with private permissions on Unix-like
  systems.

## Release Packaging

- Build local release candidates through `cargo xtask build`; the root
  `build.rs` builds WebUI assets before Rust compilation and embeds them into
  the daemon by default.
- Run the `Release Binaries` workflow for the release tag and confirm Linux,
  macOS, and Windows artifacts are uploaded to the GitHub Release.
- Confirm release binaries embed the WebUI assets but do not embed Hindsight
  sidecars; runtime sidecar downloads should continue to resolve from the
  pinned sidecar release.
- Confirm `cargo-binstall` resolves the release asset and does not fall back to
  source compilation.
- Confirm browser runtime download behavior is expected for the release.
- Record any unpinned or latest-version download behavior in release notes.
- Review dependency updates for license or attribution changes.

## Compatibility

- Load an existing config file from the previous release.
- Run the setup wizard on a clean machine profile.
- Load existing workspace app packages under `~/daat-locus-workspace/apps`.
- Confirm workspace app hook compatibility:
  - `config(ctx)`
  - `init(ctx, state)`
  - `render_state(ctx, state)`
  - `tools(ctx, state)`
  - `call_tool(ctx, state, input)`
  - `on_focus(ctx, state)`
  - `on_blur(ctx, state)`
  - `poll_notices(ctx, state)`

## Quality Gates

- Run `cargo fmt --check`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Run `cargo test --no-default-features`.
- Run `cargo deny check bans sources licenses`.
- Run targeted manual smoke tests for:
  - first-time setup
  - daemon start and attach
  - Telegram ACL approval flow when Telegram is enabled
  - terminal command execution
  - workspace app load, reload, timeout, and restart

## Release Notes

- Document user-visible config changes.
- Document state migration behavior.
- Document any supply-chain pinning gaps.
- Document backwards compatibility notes for workspace app behavior.
