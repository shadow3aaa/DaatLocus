# Sandbox Backend Selection

This document records the technology selection for optional strong sandbox
backends. The default runtime policy remains the current lightweight
self-protection guard: Daat Locus should assume the user owns the machine, while
still protecting Daat Locus private state, source, config, and provider secrets.

Strong sandboxing is for future app filesystem permissions and stricter tool
execution. It must be opt-in and must not become a normal requirement for local
use.

Network access is explicitly out of scope for this selection. Daat Locus does
not need to restrict web access for this milestone.

## Scope

Strong backends should apply to child processes, not the daemon process:

- Terminal command processes.
- Workspace app worker processes.

Workspace app workers need a narrower policy than Terminal commands:

- read the app source directory
- write only the app's own protected runtime state directory
- avoid write access to the app source, app config, Daat Locus runtime state,
  and provider secret environment variables

## Reference Implementation

OpenAI Codex is the reference implementation for this selection because it is a
Rust agent runtime with mature cross-platform sandbox work.

The local research clone lives under `tmp_research/openai-codex` and must not be
committed. If Daat Locus later copies or vendors Codex code, review Apache-2.0
license and NOTICE obligations before merging.

## Linux Selection

Use a Codex-style Linux bubblewrap backend as the primary design:

- Bubblewrap constructs the sandbox before `exec`.
- Bubblewrap is the primary filesystem and namespace backend.
- The helper should prefer a system `bwrap` outside the working directory.
- A vendored or bundled bubblewrap path can be considered later, but it has
  release and license packaging implications.
- Use mount namespace behavior to construct the filesystem view.
- Use `--die-with-parent` or equivalent behavior so sandboxed descendants do not
  survive the launcher unexpectedly.

Landlock should not be the primary Linux backend. It is useful as a legacy or
fallback reference, but it does not map as cleanly to restricted reads, masked
paths, and writable roots with read-only subpaths.

The Daat Locus policy should map to a bubblewrap filesystem view:

- read-only root or minimal readable roots
- explicit writable roots
- protected subpaths rebound read-only or masked
- denied read paths masked
- denied write paths protected even under writable parents

## macOS Selection

Keep the existing Seatbelt `sandbox-exec` backend. Future work should only align
its policy mapping with the same cross-platform backend interface.

## Windows Selection

Use Codex's Windows sandbox as the reference design, not a simpler Job Object
only model.

The selected direction is:

- elevated setup prepares sandbox identities and ACLs
- restricted tokens run sandboxed children
- capability SIDs express read/write access
- ACLs grant only selected read and write roots
- protected source, config, and runtime paths receive deny-write rules
- Job Objects are used for lifecycle cleanup, especially kill-on-close

Rejected first choices:

- Job Objects alone: useful for process lifetime, not filesystem permission
  enforcement.
- Restricted tokens alone: not enough without matching ACL preparation.
- AppContainer first: powerful but too much packaging and capability complexity
  for arbitrary local tools and workspace app workers.

Windows should be treated as a later implementation milestone than Linux because
the practical backend includes identity setup, ACL refresh, credential handling,
and process spawning.

## Integration Shape

Add a platform-neutral strong sandbox layer before implementing per-platform
details:

- `StrongFilesystemSandboxMode::Off`: default; current lightweight guard only.
- `StrongFilesystemSandboxMode::Auto`: use a supported strong backend when available and
  report when it is not available.
- `StrongFilesystemSandboxMode::Required`: fail child-process launch if the
  requested backend cannot be applied.

The runtime should keep one normalized policy model and transform it into
platform backend commands at spawn time. Terminal and workspace app workers
should share this path so app permissions are not a separate ad hoc sandbox.

Current implementation:

- `sandbox.strong_filesystem = "off" | "auto" | "required"` controls the
  optional strong filesystem backend.
- Terminal processes use the runtime filesystem policy.
- macOS Terminal keeps the existing Seatbelt self-protection behavior.
- Workspace app workers use a narrower worker policy that allows writes to the
  app's protected state directory while keeping app source read-only.
- Linux uses `bwrap` when available and enabled; `required` fails when `bwrap`
  is missing.

## Conformance Tests

When a backend is enabled, add platform-gated tests for:

- denied read access to Daat Locus private runtime state
- denied write access to Daat Locus source and config
- denied write access to workspace app source and `app.toml`
- allowed write access to the app's own state directory
- provider secret environment variables not inherited
- descendant process cleanup after timeout or parent termination
