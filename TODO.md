# TODO

This file tracks hardening work for making Daat Locus a reliable long-running local agent runtime.

- [x] Add daemon authentication for local control endpoints
  - [x] Protect `/commands/run`, `/daemon/shutdown`, dashboard snapshot, and dashboard stream.
  - [x] Store a per-install random daemon token in the protected runtime directory.
  - [x] Make the CLI client send the token on every protected request.
  - [x] Keep `/health` minimal and unauthenticated for readiness probing.

- [x] Add daemon ready-state gating before accepting runtime commands
  - [x] Add explicit daemon states: `initializing`, `ready`, `stopping`, and `failed`.
  - [x] Expose daemon state through `/status`.
  - [x] Reject runtime commands until daemon initialization reaches `ready`.
  - [x] Make `attach` render startup progress without implying the runtime is ready.

- [x] Replace Hindsight retain flush no-op with real handoff ack tracking
  - [x] Track pending and inflight handoff jobs inside `HindsightRetainHandle`.
  - [x] Treat backend success as Hindsight accepting the async retain request.
  - [x] Make `flush()` wait for local handoff submission acks.

- [x] Remove Hindsight handoff items only after backend accepts submission
  - [x] Preserve pending and inflight handoff items across process exits.
  - [x] Drain submitted handoffs or preserve unfinished work during daemon shutdown.
  - [x] Avoid deleting local queue items until retain handoff is accepted.

- [x] Add hard memory bounds for terminal process output buffers
  - [x] Replace unbounded terminal `Vec<u8>` output storage with a bounded ring buffer.
  - [x] Track dropped byte counts when output exceeds the hard limit.
  - [x] Surface truncation/drop metadata in terminal tool results and dashboard cells.
  - [x] Add tests for high-output commands that exceed the buffer limit.

- [ ] Add timeout and interruption support for Lua workspace apps
  - [ ] Cover app hooks, tools, render, and notice polling.
  - [ ] Add Lua interruption through instruction hooks or equivalent cancellation.
  - [ ] Convert timeout and resource failures into stable app errors instead of blocking runtime.

- [ ] Tighten the default runtime sandbox policy
  - [ ] Move beyond protecting only `.daat-locus`.
  - [ ] Define explicit readable and writable runtime/workspace roots.
  - [ ] Add tests for protected runtime paths.

- [ ] Resolve symlinks and canonical paths for sandbox enforcement
  - [ ] Canonicalize paths before read/write checks where possible.
  - [ ] Add tests for symlink escape attempts.
  - [ ] Keep behavior deterministic when target paths do not exist yet.

- [ ] Add atomic write helpers for persistent runtime state
  - [ ] Use temp-file, fsync, and rename semantics.
  - [ ] Add corruption handling for partially written state files.
  - [ ] Add tests for interrupted writes where practical.

- [ ] Apply atomic persistence to state stores
  - [ ] Cover memory, events, pending work, plan, config, and ACL state.
  - [ ] Avoid direct `tokio::fs::write` or `std::fs::write` for durable state.

- [ ] Support env-based secret references for all provider credentials
  - [ ] Accept `env:NAME` or `$NAME` references consistently.
  - [ ] Resolve OpenAI and OpenAI-compatible API keys through the same resolver as Copilot.

- [ ] Write config files with private permissions
  - [ ] Set `0600` on Unix-like systems.
  - [ ] Preserve safe behavior when rewriting existing config files.

- [ ] Avoid leaking provider secrets in UI and logs
  - [ ] Mask provider secrets in config summaries.
  - [ ] Avoid printing raw credentials in errors and debug output.
  - [ ] Avoid persisting unmasked secrets outside config.

- [ ] Pin and verify managed uv downloads
  - [ ] Pin auto-downloaded uv versions instead of using GitHub `latest`.
  - [ ] Verify downloads with checksums or signatures before execution.

- [ ] Pin and verify browser runtime downloads
  - [ ] Pin browser runtime versions instead of always using latest stable.
  - [ ] Verify browser runtime downloads before extraction.
  - [ ] Document how to disable auto-downloads for locked-down environments.

- [ ] Add daemon shutdown drain for retain jobs and runtime persistence
  - [ ] Ensure shutdown completes outstanding retain and state flush work when possible.
  - [ ] Preserve unfinished work when clean drain is impossible.

- [x] Preserve pending Hindsight handoff items across process exits
  - [x] Reset only inflight state that is safe to retry.
  - [x] Keep pending but unsubmitted items visible in the local queue.

- [ ] Add bounded retry and recovery behavior for stuck app notices
  - [ ] Prevent app notices from spinning forever.
  - [ ] Surface stable failure reasons when notices are suppressed or dropped.

- [ ] Add tests for pending work recovery paths
  - [ ] Cover event claim, requeue, overflow fuse, and terminal resolution paths.
  - [ ] Audit turn-boundary behavior after focus/app/tool changes.
  - [ ] Ensure unresolved claimed inputs are requeued or failed with explicit reason.

- [ ] Document model catalog source and update process
  - [ ] Record source, generation process, and update date.
  - [ ] Add a refresh script or documented manual process.
  - [ ] Keep generated/catalog data clearly separated from handwritten provider logic.

- [ ] Add model catalog fallback tests
  - [ ] Cover unknown model IDs.
  - [ ] Cover stale or similar model names.
  - [ ] Confirm conservative defaults are used safely.

- [ ] Add CI quality gates
  - [ ] Run `cargo fmt --check`.
  - [ ] Run `cargo clippy --all-targets`.
  - [ ] Run `cargo test`.
  - [ ] Add dependency/license checks.

- [ ] Add release checklist
  - [ ] Cover daemon auth.
  - [ ] Cover state migration.
  - [ ] Cover supply chain pins.
  - [ ] Cover backwards compatibility for config and workspace app behavior.

- [ ] Track license and attribution obligations
  - [ ] Add a dependency license audit note for transitive dependencies.
  - [ ] Track dependencies with weak-copyleft or attribution-sensitive licenses.
  - [ ] Document when a project-level `NOTICE` file becomes necessary.
