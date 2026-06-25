# Multi-Session Architecture Rules

## Multi-Session Architecture

Daat Locus uses a client-server multi-session architecture.

The public client-server boundary is:

```text
WebUI / TUI / CLI / Telegram control
  -> Manager daemon on the boot-configured public port
  -> Session processes over local IPC
```

Clients connect only to the Manager daemon's boot-configured port. Session
processes are never client connection targets. Session process identifiers, IPC
names, socket paths, process ids, and local implementation details must not
appear in WebUI state, TUI state, CLI arguments, dashboard URLs, or public API
contracts except as opaque diagnostic summaries when explicitly requested.

The Manager is the only public server. A Session process is a local runtime
worker owned and controlled by the Manager.

### Manager Responsibilities

The Manager daemon owns:

- public HTTP/WebSocket API endpoints
- embedded WebUI serving
- daemon authentication and token validation
- config readiness classification and setup/configuration endpoints
- session registry and lifecycle management
- session spawning, stopping, deletion, restart, and health checks
- routing for `/send`, `/commands/run`, dashboard requests, and Telegram input
- Telegram polling/input transport, default-session mapping, and Telegram-only session control commands
- dashboard snapshot/history/stream proxying from target sessions
- session list and status aggregation for WebUI, TUI, and CLI clients

The Manager must not:

- create a runtime `Context`
- run `daat_locus_loop`
- own per-session `EventStore`, `PendingWorkQueue`, `Memory`, `Plan`, or app instances
- interpret session-local runtime state beyond routing, lifecycle, and compact status summaries

### Session Responsibilities

Each Session process owns exactly one runtime:

- one `Context`
- one `EventStore`
- one `PendingWorkQueue`
- one runtime conversation and memory state
- one `Plan`
- one `AppManager` and app instance set
- one dashboard state stream
- one model loop

A Session process exposes only private IPC handlers to the Manager. It must not:

- serve public HTTP or WebUI
- expose `/sessions`
- load, mutate, or persist the global session registry
- manage any other session
- poll Telegram
- require direct client access

### Session Registry

The Manager owns a persisted registry of session metadata. Registry entries
should include:

```text
session_id
scope
pid
status
ipc_name
ipc_token_hash
project_dir
title
started_at_ms
last_seen_at_ms
```

`session_id` is an opaque stable identifier assigned by the Manager. It must not
be derived from project paths just to force uniqueness.

`scope` defines where the session is shown and what workspace it uses:

```text
General
Project { project_dir }
```

General sessions are shown in `daat-locus run`. Project-scoped sessions are
shown in `daat-locus code <project-dir>` only when the canonical project
directory matches. A single project directory may have multiple sessions.

### Session Titles

`session_id` is an operation handle, not the normal user-facing label.
Non-command clients should display a session title and avoid falling back to
raw session ids.

Title generation belongs to the Session process because only the Session owns
its event store, memory, and runtime context. Before a generated title exists,
the Session should publish a placeholder title derived from the first external
user/event sentence.

Generated titles should use the configured efficient model. Do not use the main
runtime model or judge model for routine title refreshes. Regenerate only when
session activity changed, and no more frequently than every five minutes. If
there is no new activity since the last generated title, do not regenerate.

The Manager may cache the latest title in `SessionRegistry` from session
status/dashboard snapshots. It must not inspect per-session memory or event
files to compute titles.

Command surfaces may expose `session_id` or a unique id prefix when the user
needs an explicit operation handle, such as attach, switch, delete, or
debugging.

### Code Mode

`daat-locus code <project-dir>` is a project-scoped session selector, not a
project-to-single-session mapping.

Rules:

- canonicalize `<project-dir>` before querying or creating project-scoped sessions
- show only sessions with `scope = Project { project_dir: canonical_project_dir }`
- creating a new code session creates a new opaque `session_id`
- the code session workspace is the canonical project directory itself
- the Coding app project root is the canonical project directory
- Terminal default cwd for that session is the canonical project directory
- sandbox writable roots must include the canonical project directory
- do not map code session workspaces to `~/daat-locus-workspace`

### Session Lifecycle

1. Manager creates an opaque `session_id`.
2. Manager creates a private IPC name and IPC token.
3. Manager spawns a Session child process with `--session-id`, `--ipc-name`,
   `--ipc-token`, and optional workspace/scope arguments.
4. Session binds its local socket and initializes its runtime.
5. Manager polls `StatusSummary` over IPC until the Session is ready or failed.
6. Manager routes all client work to the target Session over IPC.
7. On delete, Manager sends `Shutdown`, waits for process exit, removes registry
   metadata, and deletes session state only when explicitly requested by the
   delete operation.
8. On Manager restart, Manager reloads the registry and attempts to reconnect to
   live session IPC endpoints. Unreachable sessions become dormant or dead based
   on health-check policy.

### Public API Shape

Clients use only Manager endpoints:

```text
GET    /sessions
POST   /sessions
DELETE /sessions/{session_id}
POST   /sessions/{session_id}/title

GET    /config/readiness
POST   /config/setup
POST   /config/probe

POST   /send
POST   /commands/run

GET    /status/summary
GET    /dashboard/snapshot?session_id=...
GET    /dashboard/activity-history?session_id=...
GET    /dashboard/stream?session_id=...
```

`GET /sessions` returns session identity, title, and scope only. It must not
expose Manager-internal process lifecycle states such as dormant, starting,
dead, pid, IPC name, or IPC token metadata. Clients should select a session and
send normal Manager requests; the Manager transparently starts or reconnects the
target Session when needed.

`GET /status/summary` may include compact per-session runtime summaries, but
those summaries are user-facing runtime facts such as ready, active surface,
pending work count, active turn, dashboard metrics, and errors. They must not
leak registry lifecycle status or force clients to perform session process
management.

The Manager chooses the target session from an explicit `session_id`, active TUI
selection, Telegram default-session mapping, or the project scope selected by
`daat-locus code <project-dir>`.

### Persistence Boundary

Shared/global state:

- Manager boot config and agent config readiness
- config file, config backup, and setup/recovery metadata
- daemon auth tokens
- Telegram ACL
- Telegram default-session mapping
- session registry
- builtin primitive specs
- workspace primitive specs when they are intentionally global assets

Per-session state:

- events
- pending work
- runtime conversation and memory
- plan
- dashboard activity history
- app instances and app-local runtime state
- context compaction state

Sleep status and primitive run evidence must be explicitly classified as either
global optimization input or session-scoped evidence. Do not let those records
become accidentally mixed because Manager and Session code share helper paths.

### Maintenance Rules

- Keep `SessionIpcClient` and `SessionIpcServer` as the only Manager-Session IPC
  boundary.
- Keep Manager serve and Session serve separate. Runtime initialization belongs
  wholly to Session serve.
- Keep Manager boot independent from complete agent config. Missing,
  incomplete, or damaged agent config must disable agent operations, not WebUI
  or Manager startup.
- Keep Manager handlers as public API, auth, session registry, lifecycle, and
  routing/proxy code only.
- Route send, command, dashboard snapshot/history, dashboard stream, and Telegram
  work through the target Session over IPC.
- Keep project-scoped `code <project-dir>` as a multi-session selector.
- Do not reintroduce any client direct-session connection path.
