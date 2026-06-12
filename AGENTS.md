# Daat Locus Agent Guidelines

This document defines the agent-facing boundaries that match the current Daat Locus implementation.

The goal is not to write abstract slogans. The goal is to give future changes to `app`, `events`, `runtime_tools`, `runtime_context`, `preturn_state`, `telegram_transport`, `workflow`, `memory`, `sleep`, and related modules a set of design constraints that match how the code actually works.

## Project Reality

- Daat Locus is a long-running, tool-driven agent.
- Its main loop is not a chat app where a user sends one message and the model sends one answer. It is a runtime where structured context is injected, the model decides what to do, and tools mutate the world.
- External input enters the current turn primarily through `Event`, `PendingWork`, background app notices, and automatic memory recall.
- Plain assistant text is normally an explanation or intermediate record inside the runtime. It is not automatically sent to Telegram or any other external system.
- Real-world changes must happen through explicit tool calls.

## Non-Negotiables

- Telegram is not an `App`; it is a transport and event source.
- Normal event completion must not be plain text only. It must explicitly call `finish_and_send`.
- Browser, Terminal, and Coding are built-in `App`s because they represent interactive surfaces that require focus and continued operation.
- `App` and `Event` are parallel concepts. Do not collapse one into the other.
- Let the model make semantic judgments. Do not make the model perform mechanical enumeration, lookup, deduplication, or freshness checks that code can already perform.

## Slash Command Design

Slash commands in interactive clients are entry points, not miniature CLIs.
When a TUI user types a slash command, the preferred behavior is to open a
bottom interactive surface where the next step is selected, searched, toggled,
or inspected.

Rules:

- Prefer one memorable top-level slash command over a tree of user-visible
  subcommands.
- Use bottom panels for selection, search, toggles, and detail inspection.
- Do not require users to type object names, ids, or paths when code can present
  a picker.
- Query flows should be list/detail views, not `show <target>` commands.
- State refresh flows should normally be internal behavior, not visible
  `reload` commands.
- Keep action menus short and stable. For example, `/skills` should expose only
  high-level actions such as `List skills` and `Enable/Disable Skills`.
- Slash completion should show only commands that users should remember as
  product-level entry points.
- Internal verbs may exist for manager APIs, tests, remote command execution, or
  Telegram text control, but interactive TUI completion must not expose them as
  the primary UX.
- Normal successful flows should stay inside panels. Use short feedback only for
  errors, unavailable actions, invalid input, or fire-and-forget operations.

Examples:

```text
/skills     -> action menu -> skill list or enable/disable toggle view
/telegram   -> action menu -> status detail or pending request picker
/debug      -> action menu -> readonly detail views
/app-status -> app picker -> app detail view
/status     -> readonly detail view
```

If the user's next step is to choose, browse, toggle, or inspect something, it
belongs in a bottom panel rather than in a visible slash subcommand.

## TUI Dashboard Architecture

The TUI dashboard is an immediate-mode, full-frame terminal renderer driven by
explicit draw requests. It is not a retained UI tree, and it must not grow a
parallel invalidation system, partial-paint system, or permanent 60fps render
loop.

The architectural boundary is:

```text
DashboardState
  cross-client session/runtime snapshot

TuiViewState
  one terminal client's local interaction state

dashboard/input_controller.rs
  pure-ish input reducer that mutates TuiViewState and returns local outcomes

DashboardCommandRunner / Manager API
  async side effects for real runtime actions

FrameRequester
  the only scheduler for future full-frame draws

render(DashboardState, TuiViewState)
  pure full-frame rendering, with performance caches only
```

All TUI dataflow should follow one direction:

```text
external state/input
  -> local state mutation or explicit async effect
  -> FrameRequester draw request
  -> pure full-frame render
```

Do not add a second hidden state path where rendering, command parsing, or async
callbacks mutate session/runtime state directly.

### TUI State Ownership

`DashboardState` is the shared runtime snapshot produced by the Manager/Session
side and consumed by WebUI, TUI, and other clients. It may contain session-visible
facts such as activity cells, live cells, runtime status, token usage, skills,
Telegram access requests, status text, and errors.

`DashboardState` must not contain state that belongs to one terminal client:

- command input text or cursor position
- slash-command popup selection or scroll
- bottom-panel selection, search query, or local detail view
- activity scroll offset or auto-scroll flag
- expanded/collapsed local display choices
- pending paste placeholders
- local command feedback
- history paging request state
- render caches
- animation scheduler state

`TuiViewState` owns all local interaction state for one TUI instance:

- command input text, cursor position, and pending pasted text
- slash-command completion popup state
- bottom command panel state, search, selection, and scroll
- activity scroll offset, max scroll, page height, and auto-scroll flag
- local display expansion state such as expanded thinking cells
- local command feedback for errors, unavailable actions, and invalid input
- activity history pagination cursors and in-flight load state
- render caches tied to terminal dimensions and visible content

Multiple TUI clients attached to the same session may have different
`TuiViewState` values while reading the same `DashboardState`. Do not solve local
TUI behavior by mutating shared dashboard state.

### TUI Input Controller

Terminal input should be handled as a reducer over current `DashboardState` and
`TuiViewState`. The reducer may mutate `TuiViewState` and return a small set of
local outcomes; it must not render directly.

Allowed outcome categories:

```text
Continue
Exit
SubmitText { text }
RunDashboardAction { action, quiet_success }
RunPanelAction { action, keep_panel }
```

Rules:

- Typing, cursor movement, popup movement, panel movement, and local scrolling
  only mutate `TuiViewState`. The event loop schedules the follow-up frame.
- Selecting an action may produce a Manager/API effect only when it represents a
  real runtime action, such as sending user text, clearing runtime state,
  reloading skills, approving Telegram access, or changing skill auto-use state.
- Panel key handling should return `CommandPanelAction`; input handling converts
  it into local panel changes or one of the action outcomes above.
- Normal successful interactive actions should usually be silent. Use local
  feedback for errors, unavailable actions, invalid input, or fire-and-forget
  operations whose result will not immediately appear in `DashboardState`.
- Do not expose implementation verbs as primary slash-command UX. If the next
  step is choosing, searching, toggling, or inspecting, use a panel.

### TUI Slash Command Surface

The slash command surface has three separate layers:

```text
command registry
  stable user-facing command vocabulary and completion metadata

command parser / input controller
  maps current input and key events into local panel changes or explicit actions

command panels
  local interactive state for selection, search, toggles, detail views, and
  short feedback
```

Rules:

- Slash commands are product entry points. They should open panels or invoke one
  obvious action; they should not become a typed subcommand tree.
- Completion must show only memorable top-level commands and short command
  forms that users are expected to type.
- Hidden or compatibility command strings may exist for remote command execution
  and tests, but they must not leak into normal TUI completion.
- Command parsing may produce local feedback for invalid input, unavailable
  actions, or destructive confirmations. Successful panel-local actions should
  usually stay silent.
- Command panels own only local UI state: selection, scroll, search text,
  detail-view scroll, toggle feedback, and picker position.
- Command panels must not call Manager APIs, mutate `DashboardState`, read or
  write persistent config, or render themselves.
- Panel key handling returns a `CommandPanelAction`; the input controller decides
  whether that action only changes local state or starts a Manager/API side
  effect.
- Manager/API effects belong behind `DashboardCommandRunner` and
  `DashboardAction`, not inside panel structs or render functions.
- Rendering a command panel is a pure projection of the current panel model.

Example command flow:

```text
user types /skills
  -> registry recognizes /skills
  -> parser opens a Skills action panel
  -> panel key handling selects List or Enable/Disable
  -> input controller replaces the local panel or runs an explicit action
  -> render draws the new local panel state
```

### TUI Event Loop

The main TUI loop consumes external events and draw notifications. Non-draw
branches only update state, start side effects, and request a frame. The draw
branch is the only branch that calls terminal rendering.

Required flow:

```text
terminal input
  -> dashboard/input_controller.rs mutates TuiViewState
  -> optional async effect
  -> FrameRequester::schedule_frame()
  -> continue loop

DashboardState stream update
  -> replace or merge latest DashboardState snapshot
  -> FrameRequester::schedule_frame()
  -> continue loop

async side-effect completion, such as history load
  -> update TuiViewState or receive new DashboardState through normal stream
  -> FrameRequester::schedule_frame()
  -> continue loop

draw_rx receives Draw
  -> terminal.draw(|frame| render(DashboardState, TuiViewState))
  -> if animation is still needed, FrameRequester::schedule_frame_in(...)
```

Rules:

- Render the whole frame when drawing.
- Do not render from terminal input, dashboard stream updates, command
  execution, history load completion, or resize handling.
- Do not keep a `needs_render` flag in the main loop when a draw scheduler
  already exists.
- Do not keep a second scheduled-draw path such as `TuiEvent::Draw` when
  `FrameRequester` already emits draw notifications.
- The initial frame is a normal `FrameRequester::schedule_frame()` request.
- Shutdown and exit handling may break the loop directly, but must not create a
  separate render path.

### FrameRequester

`FrameRequester` is the single draw scheduler. It accepts immediate and delayed
frame requests, coalesces them, applies frame-rate limiting, and emits draw
notifications through one channel consumed by the TUI loop.

Rules:

- `schedule_frame()` requests the next allowed frame.
- `schedule_frame_in(duration)` requests a future frame for animation or delayed
  UI work.
- When several requests are pending, the earliest allowed deadline wins and
  produces one draw notification.
- The scheduler may use a frame-rate limiter to avoid busy redraws, but the main
  loop must not run a fixed `16ms` interval.
- Do not maintain a user-visible or architectural taxonomy of redraw reasons.
  Profiling can record timings and cell counts, but reason labels must not
  become state or product behavior.

### TUI Animation

Animation is time-dependent render output plus a scheduled future frame. It is
not a separate render loop.

Rules:

- Render output derives animated visuals from current time or stable timestamps.
- After a draw, if visible state still needs animation, schedule the next frame
  with `FrameRequester::schedule_frame_in(animation_interval)`.
- When runtime/local state no longer needs animation, stop scheduling animation
  frames.
- Reduced-motion mode should stop animation scheduling or render static
  indicators.

Examples of animation-worthy state:

- runtime status such as `Working`
- live activity cells with spinner or progress affordances
- explicitly time-based local UI effects

### TUI Rendering

Rendering is a pure projection of `DashboardState` plus `TuiViewState` into one
terminal frame, except for performance caches updated by the render layer.

Rules:

- Render functions may read shared runtime state and local view state.
- Render functions must not call Manager APIs, mutate runtime/session state,
  spawn tasks, or execute commands.
- Render functions must not decide semantic behavior such as skill enablement,
  Telegram approval, session routing, or history pagination.
- Layout should be derived from the current terminal area and current state.
- Cursor placement is render output; the cursor position source of truth remains
  local input state.

### TUI History Paging

Activity history paging is a side effect triggered by local view state, usually
scrolling near the top of the currently loaded activity range.

Rules:

- Paging cursors and in-flight load state belong to `TuiViewState`.
- The async load request may call the Manager dashboard history API.
- Completion updates local loaded history state or receives new shared state
  through the normal dashboard stream, then schedules a frame.
- History paging must not call render directly from the async task.
- Paging state must not become global session state unless it represents real
  session-visible activity.

### Render Caches

Render caches are allowed only as performance caches for pure render products,
not as semantic state.

Allowed examples:

- cached rendered activity cell lines
- cached wrapped height for a cell at a specific terminal width
- cached syntax/markdown render output tied to source content and width

Rules:

- Cache keys must include source content and terminal width, plus any other input
  needed for correctness.
- Cache invalidation must follow source state changes and terminal dimension
  changes.
- Caches must not decide command behavior, runtime state, session state, manager
  routing, or animation scheduling.
- Clearing a render cache should only make rendering slower, not change product
  behavior.

### TUI Performance Work

When TUI performance is unclear, use a controlled mock-frame harness before
guessing from an interactive session:

1. Build a representative `DashboardState` and `TuiViewState`.
2. Run a fixed number of frames with known terminal dimensions and cell counts.
3. Measure total frame time, preparation time, activity rendering time, command
   bar rendering time, and cache hit behavior.
4. Fix the bottleneck.
5. Remove temporary commands and harnesses unless they become stable tests or an
   explicitly supported benchmark.

The stable harness is the hidden `dev tui-perf` command behind the non-default
`tui-perf-cmd` feature:

```bash
cargo run --features tui-perf-cmd -- dev tui-perf \
  --scenario mixed --frames 120 --warmup 10 --width 120 --height 40
```

Rules:

- Keep `tui-perf-cmd` non-default. The command is a developer tool, not product
  CLI surface, and it must not appear in normal help or completion.
- Keep scenarios deterministic. A scenario should construct fixed
  `DashboardState` and `TuiViewState` values, use `TestBackend`, and render the
  real dashboard frame path.
- Prefer comparing reported metrics across revisions over hard portable
  millisecond gates. Absolute timing differs across machines.
- Use `--json` when collecting machine-readable output in scripts or CI.
- Add or update scenarios when a TUI regression depends on a specific visible
  state, such as long history, live activity, command panels, or wide markdown.

Permanent profiling should be low-noise and opt-in or warning-based. It may log
frame rate, slow frame count, average/max frame time, activity cell counts, live
cell counts, and cache timing. It must not expose internal redraw reasons as
product state.

### TUI Refactor Direction

The desired module shape is:

```text
dashboard/mod.rs
  orchestration, stream wiring, lifecycle, and high-level loop

dashboard/view_state.rs
  TuiViewState and local-only state transitions

dashboard/input_controller.rs
  terminal key/paste/resize handling and returned local outcomes

dashboard/frame_requester.rs
  draw scheduling, coalescing, and frame-rate limiting

dashboard/command_input.rs
  command-line text wrapping, cursor projection, paste placeholders, and display
  text only

dashboard/command_registry.rs
  stable slash command vocabulary, completion metadata, and accepted command
  forms only

dashboard/command_flow.rs
  slash input parsing, command-to-panel construction, command-to-action
  invocation, live command feedback, remote control command text handling, and
  popup selection helpers; no ratatui rendering

dashboard/command_panels.rs
  command panel data models, panel-local state transitions, and panel actions;
  no ratatui rendering and no Manager/API calls

dashboard/command_text.rs
  readonly command detail text formatting and truncation helpers

dashboard/commands.rs
  DashboardAction, DashboardControlCommand, DashboardCommandRunner, and
  Manager/API-facing command effects

dashboard/render*.rs and dashboard/cells/*
  pure frame projection and render caches

dashboard/tui_animation.rs
  time-based animation policy that only schedules future frames
```

Do not split modules merely to move lines around. Split when it enforces one of
these boundaries: local view state, input-to-effect reduction, async runtime
effects, frame scheduling, or pure rendering.

`dashboard/mod.rs` should not remain the permanent home for command parsing,
panel models, panel key handling, command text formatting, and ratatui rendering
all at once. It may temporarily contain glue during a refactor, but the stable
shape should make each boundary easy to audit:

- state models do not render
- render functions do not execute effects
- input reducers do not call `terminal.draw`
- command registry does not perform parsing side effects
- command panels do not own Manager/session state
- async command execution does not mutate local view state except through the
  input-controller result path or normal dashboard stream updates

## Multi-Session Architecture

Daat Locus uses a client-server multi-session architecture.

The public client-server boundary is:

```text
WebUI / TUI / CLI / Telegram control
  -> Manager daemon on the configured public port
  -> Session processes over local IPC
```

Clients connect only to the Manager daemon's configured port. Session processes
are never client connection targets. Session process identifiers, IPC names,
socket paths, process ids, and local implementation details must not appear in
WebUI state, TUI state, CLI arguments, dashboard URLs, or public API contracts
except as opaque diagnostic summaries when explicitly requested.

The Manager is the only public server. A Session process is a local runtime
worker owned and controlled by the Manager.

### Manager Responsibilities

The Manager daemon owns:

- public HTTP/WebSocket API endpoints
- embedded WebUI serving
- daemon authentication and token validation
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

### Manager-Session IPC

Manager-to-Session communication uses `interprocess` Tokio local sockets. This
IPC layer is private to the local machine. It is not a public network protocol
and must not be exposed as a client integration surface.

The IPC transport is a local socket byte stream with explicit message framing.
Version 1 should use length-prefixed JSON messages because they are inspectable
and easy to debug. The framing and protocol types should be isolated behind
`SessionIpcClient` and `SessionIpcServer` so the encoding can evolve without
changing Manager or Session runtime boundaries.

Every IPC request carries:

```text
protocol_version
request_id
session_id
ipc_token
body
```

`ipc_token` is generated by the Manager when spawning a session and is included
in every Manager-to-Session request. This is defense in depth for local IPC; it
does not replace public daemon authentication, which belongs only to the
Manager.

Required IPC request bodies:

```text
Status
StatusSummary
SubmitUserInput { origin, text, attachments, wait_for_reply }
EnqueueTelegramEvent { event }
DashboardSnapshot
DashboardHistoryPage { before, after, limit }
DrainTelegramOutbox
RecordTelegramDelivery { event_id, status, note }
RequeueTelegramOutbound { message }
SubscribeDashboard
Shutdown { reason }
```

Required user input origins:

```text
WebUi
Tui
CliSend
```

Required IPC response bodies:

```text
Status { runtime_status }
StatusSummary { summary }
Submitted { event_id, reply_message, terminal_status }
DashboardSnapshot { state }
DashboardHistoryPage { page }
TelegramOutbox { messages }
DeliveryRecorded
TelegramOutboundRequeued
ShutdownAccepted
Error { code, message, retryable }
```

Dashboard subscriptions are long-lived IPC streams. Required stream events:

```text
DashboardSnapshot { state }
DashboardClosed { reason }
Error { code, message, retryable }
```

Public API mapping:

- `/send` maps to `SubmitUserInput { wait_for_reply: true }`
- dashboard/TUI text input maps to `SubmitUserInput { wait_for_reply: false }`
- Telegram input maps to `EnqueueTelegramEvent`
- Telegram output maps to `DrainTelegramOutbox`, Manager delivery, then
  `RecordTelegramDelivery` or `RequeueTelegramOutbound`
- dashboard websocket maps to `SubscribeDashboard`

The Session remains responsible for event registration, pending work, model
turns, tool calls, event completion, and dashboard state production. The Manager
only routes input and proxies output.

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
those summaries are user-facing runtime facts such as ready, focused app,
pending work count, active turn, dashboard metrics, and errors. They must not
leak registry lifecycle status or force clients to perform session process
management.

The Manager chooses the target session from an explicit `session_id`, active TUI
selection, Telegram default-session mapping, or the project scope selected by
`daat-locus code <project-dir>`.

### Persistence Boundary

Shared/global state:

- config
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
- Keep Manager handlers as public API, auth, session registry, lifecycle, and
  routing/proxy code only.
- Route send, command, dashboard snapshot/history, dashboard stream, and Telegram
  work through the target Session over IPC.
- Keep project-scoped `code <project-dir>` as a multi-session selector.
- Do not reintroduce any client direct-session connection path.

## Commit History

Commit history is a long-term engineering interface, not a temporary chat log. When rewriting history or adding commits, follow these rules:

- Commit messages must be in English. The title should use imperative mood or a clear action phrase, such as `Add ...`, `Fix ...`, `Refactor ...`, `Remove ...`, `Split ...`, or `Document ...`.
- The title must state the real subject and purpose of the change. Avoid uninformative titles such as `update`, `fix`, `u`, `misc`, `wip`, or `cleanup`.
- One commit should represent one logical concern. Split behavior changes, refactors, formatting, documentation, tests, and dependency updates unless they cannot compile or cannot be explained independently.
- Large refactor commits must name the boundary being split, such as `Split runtime turn scheduling modules`; do not write only `Refactor runtime`.
- Bug-fix commit titles should describe the fixed behavior rather than only the symptom, such as `Retry Telegram delivery instead of failing events`.
- Pure mechanical formatting should be its own commit, such as `Format Rust sources after refactor`.
- Do not commit local research directories, generated caches, runtime logs, or unconfirmed experiments.
- Before rewriting already-pushed history, create a local backup branch. Push rewritten `main` with `--force-with-lease`.

## Runtime Model

A runtime turn usually contains these layers:

1. System prompt contract
2. Memory and history messages
3. One-shot `afterclaim_context` for newly claimed work
4. Repeated `preturn_context` for current execution state
5. Model text output or tool calls
6. Tool results written back into history
7. Another tool cycle when needed

Runtime context is split by lifetime:

- `afterclaim_context`: claimed event/app-notice input and workflow primitive routing catalog
- `preturn_context`: memory recall, sensory state, plan, bound workflow state, and app surface state

Therefore, when adding an agent-facing interface, first decide which layer it belongs to. Do not directly pile it into some app state.

## Core Objects

### App

An `App` is an interactive surface that only makes sense to operate after it has been focused.

The current implementation has these built-in apps:

- `Browser`
- `Terminal`
- `Coding`

Something should be modeled as an `App` only if it satisfies all of these conditions:

- The model must first shift attention to it before later operations make sense.
- The visible information is naturally local and needs step-by-step exploration.
- Operations have temporal semantics, such as waiting for loading, continuing an interaction, handling a session, or reading after the state stabilizes.

Every `App` must expose three separate layers to the model:

- `state`: current structured visible facts
- `usage`: what the app is for and when it is worth focusing
- `how_to_use`: how to operate it correctly after focus

Do not mix these layers.

- `state` is not an operating manual.
- `usage` is not a full tutorial.
- `how_to_use` is not world state.

In the current code, this separation mainly appears in `App::render_state`, `usage()`, and `how_to_use()`.

Do not put self-optimizable task execution procedures into an app's supplemental instruction layer. Reusable methods across tasks should be modeled as `Workflow` SOP primitives, not as app-local explanatory text.

### Event

An `Event` is a structured external fact that the system has already received and that now requires the model to make a semantic judgment.

In the current implementation, the only payload that truly enters `EventStore` is:

- `TelegramIncoming`

An event answers these questions:

- What just happened?
- Does it require a response?
- With what disposition should it end?

An event is not a conversation cursor, an app-internal selected item, or a navigation process.

### PendingWork

`PendingWork` is the scheduling unit that drives the main loop. It is not the same thing as `Event`.

The current implementation has two variants:

- `PendingWork::Event { event_id }`
- `PendingWork::AppNotice { app, reason }`

Rules:

- Events have higher priority than app notices.
- The queue handles scheduling, not semantic judgment.
- The queue may claim, release, consume, or requeue work at the front.

Do not turn the queue into another business state machine. It is only the entry point that drives the next runtime turn.

### Plan

`Plan` is the latest step-by-step execution plan for the current task. It is not a backlog database.

The current implementation requires:

- A non-empty plan that is not fully completed must have exactly one `in_progress` step.
- When all steps are complete, the plan should be cleared directly instead of retaining a set of completed steps.
- Each `update_plan` submits the complete plan, not an incremental patch.

Do not use the plan as:

- a long-term knowledge base
- a mirror of the event list
- an implicit cursor

### Workflow

`Workflow` is the runtime binding and evolution layer over a self-optimizable SOP primitive library. Persisted specs are SOP primitives, not composite task templates, not innate model capability, and not app-local supplemental instruction.

A persisted `PrimitiveSpec` answers these questions:

- What stable procedure can be reused as a primitive?
- What inputs, artifacts, capabilities, or preconditions does the primitive need?
- What outputs, artifacts, completion evidence, or handoff does it produce?
- How should this primitive recover from failures or blockers?
- What boundary prevents it from absorbing neighboring work?

The runtime may temporarily compose several primitives into an execution graph for one task. That graph is runtime state, similar to a structured plan with explicit artifact handoff between steps. It must not be written back as a new persisted primitive spec just because it succeeded once.

`Workflow` must be split into three layers:

- `PrimitiveSpec`: a persisted SOP primitive asset exposed to the agent through a concise name, capability summary, and thin input/output contract
- `WorkflowBinding` / runtime composition: which primitive or temporary primitive graph the current task is using; runtime state only
- `PrimitiveRunRecord`: evidence automatically accumulated after daytime execution for sleep; the current implementation writes it directly at the work-completion boundary instead of generating it later by replaying sleep

Rules:

- All persisted specs are primitives. Do not add a `kind` field just to distinguish primitive versus composite workflows; composite workflows should not be persisted in the primitive library.
- `PrimitiveSpec` must not carry runtime selection state or transient state such as "active".
- `WorkflowBinding` only means the current task is using a primitive or a temporary composition. It must not write back to the primitive spec itself.
- `PrimitiveRunRecord` is recorded by code. The model must not manually write a daytime outcome log.
- The main workflow evolution actions are `patch` and `merge`.
- v1 does not introduce `deprecate`.
- Agent-facing workflow primitive routing catalogs should present the full loaded primitive ID vocabulary, plus thin IO/capability contracts only for the top relevant primitives. `when_to_use` may remain as metadata for filtering, sleep-time analysis, and human documentation, but it must not be the primary runtime surface.
- Do not show only the first N workflows by lexicographic id or only a filtered subset as if that were the whole primitive vocabulary. If the workspace contains many workflows, code should still expose all primitive IDs for composition awareness while expanding only relevant primitive details to avoid context explosion.
- Workflow evolution must depend only on workflow-bound execution evidence. It must not depend on error demos, failure patterns, or prompt evaluation artifacts.
- Use sleep to manufacture better primitives from successful reusable experience and merge duplicates. Do not manufacture composite task templates such as `modify-local-project-then-commit-and-push`.

Example primitive vocabulary:

- `inspect-local-project`
- `inspect-repository-status`
- `modify-local-project`
- `run-required-checks`
- `commit-and-push`
- `report-result`
- `ask-clarifying-question`
- `summarize-findings`

Example temporary graph for "modify, commit, and push":

1. `inspect-repository-status`
2. `modify-local-project`
3. `run-required-checks`
4. `commit-and-push`
5. `report-result`

Do not use workflow as:

- a long-term mirror of the plan
- an implicit runtime state slot
- a set of auto-generated default templates for blind model use
- a persisted composite template for every multi-step request
- a performance ledger that the model has to maintain manually

### Memory

`Memory` has two parts:

- runtime conversation: the current thread context
- hindsight queue: long-term memory items waiting to be retained or already retained

Memory serves thread continuity and long-term experience accumulation. It does not serve mechanical state synchronization.

### Sleep / Self-Improvement

Daat Locus has an explicit self-improvement loop:

- runtime error cases
- sleep
- runtime error correction compile
- compiled runtime contract additions

This means runtime design is not disposable. Any agent-facing interface that systematically induces bad behavior will pollute error cases and workflow run evidence, and then affect later compilation.

Agent-facing interfaces should therefore be stable, explicit, and reviewable. Do not rely on vague conventions.

Sleep internals must be separated into two independent pipelines:

- `Runtime Error Correction Pipeline`
- `Workflow Improvement Pipeline`

They may run in parallel during the same sleep cycle, but neither pipeline may depend on the other as input.

The `Runtime Error Correction Pipeline` is responsible for:

- fixing global runtime contract and tool protocol errors based only on code-detected daytime runtime error cases
- directly producing small compiled runtime contract additions
- clarifying existing invariants when the model violates them, such as event completion, app notice completion, tool argument shape, plan contract, terminal session continuation, browser reference freshness, and retry/overflow recovery

The `Runtime Error Correction Pipeline` must not:

- consume raw complete message streams as its primary input
- consume sleep-internal program traces as daytime evidence
- consume workflow run records directly
- infer successful task patterns from positive examples
- generate task procedures, workflow steps, style preferences, or domain tactics
- decide whether an arbitrary failure belongs to workflow optimization or prompt correction through semantic guessing

Its input unit should be a `RuntimeErrorCase`: one code-detected runtime or protocol error plus the minimum diagnostic context needed to correct the global contract. If one turn contains multiple errors, split them into multiple cases that share the same turn id.

A `RuntimeErrorCase` may include:

- `case_id`, `turn_id`, `occurred_at_ms`
- `error_kind`, `severity`, and `detected_by`
- task context: origin, event source, user request summary, claimed ids, bound workflow id, workflow origin
- runtime context: phase, available tool names, focused app, plan summary, compact context summary
- action context: assistant text summary, tool call summaries, tool result summaries, and a short previous-action window
- error observation: expected behavior, actual behavior, evidence, recoverability, retry counts, and terminal status
- relevant existing runtime contract references or hashes

Allowed `error_kind` values should be an explicit code-owned whitelist. Examples include:

- `missing_finish_and_send`
- `missing_notice_resolved`
- `invalid_tool_args`
- `tool_schema_error`
- `stale_browser_ref`
- `wrong_terminal_session_continuation`
- `plan_contract_violation`
- `event_id_missing_or_stale`
- `repeated_identical_tool_error`
- `context_overflow_after_recovery`
- `claimed_input_left_unresolved`
- `transport_completion_violation`

Do not feed ordinary task quality problems into runtime error correction, such as slow news search, weak source choice, incomplete summaries, missed code-review findings, or unclear task steps. Those may be workflow or task-quality issues, but code cannot reliably assign them to prompt correction.

The `Workflow Improvement Pipeline` is responsible for:

- fixing workspace SOP primitive specs based only on workspace `PrimitiveRunRecord`
- producing primitive spec patches and primitive merges

Builtin primitives belong to the base capability layer:

- They are compiled from repository `workflows/*.md` by `build.rs`.
- They are read-only, not writable, and cannot be patched or merged by sleep.
- They may be selected and bound by the agent, but they are not self-optimization targets.

Explicitly forbidden:

- driving workflow patches directly from runtime reviews, error demos, or failure patterns
- using workflow merge or patch results as evidence for runtime error correction compile

Keep these two object classes separate:

- Runtime error correction compile changes global tool/protocol constraints.
- Workflow evolution changes the primitive SOP library used during task-time composition.

## Current App Semantics

### Terminal

`Terminal` is the interface for local command execution and persistent process interaction.

It is an `App` not because the command line is important, but because:

- sessions persist
- output may need to be awaited
- stdin may need further writes
- foreground/background attention matters

Operational constraints:

- Operate only through `terminal_exec`, `terminal_write_stdin`, and `terminal_terminate`.
- Do not treat interactive full-screen programs as the normal path.
- Do not hand interactive login or authentication flows to the model.
- Sessions are explicitly addressed; there is no hidden selected session.

### Static Runtime File Tools

Plain file reading and plain file editing are static runtime tools, not an
`App` and not a `global::*` namespace. They do not represent an interactive
surface that needs focus. They are exposed as ordinary runtime tools:

```text
read_file
edit_file
```

`read_file` is the model-visible primitive for explicit file/range reads:

```text
read_file({
  path: "src/dashboard/mod.rs",
  start_line: 1268,
  line_count: 53
})
-> 1268#7a|fn run_tui_dashboard(...) {
   1269#c1|    ...
```

Rules:

- `read_file` accepts a path plus optional `start_line` and `line_count`.
- Paths may be workspace-relative or absolute paths allowed by the sandbox.
- `read_file` output lines use the same `line#hash2|source line` format used by
  Coding reads.
- Line hashes are stale-edit guards. They are not long-term identities and
  should stay short.
- `read_file` is the fallback for imports, top-level code, search misses,
  user-specified locations, and non-source/config/document files.
- Do not put explicit path/range read compatibility into `read_code`; that
  belongs here.

`edit_file` is the model-visible primitive for plain file edits:

```text
edit_file({
  edits: [{
    path: "AGENTS.md",
    op: "replace",
    start: "708#4b",
    end: "724#d1",
    content: "..."
  }]
})
```

Rules:

- `edit_file` uses the same structured edit schema and `line#hash2` anchors as
  `edit_code`.
- `edit_file` verifies line hashes before writing.
- The model-visible edit schema should be flat and must not use JSON Schema
  `oneOf`/`anyOf`. Expose edit `content` as a string field; implementation may
  accept legacy array content, but the schema should not advertise it.
- `edit_file` handles ordinary non-SCOPE files such as Markdown, TOML, YAML,
  JSON, shell scripts, and unsupported file types.
- `edit_file` does not run SCOPE propagation analysis and does not produce
  propagation review events.
- When Coding is focused and the target is a SCOPE-owned source file,
  `edit_file` must be rejected with an instruction to use `edit_code`.

`apply_patch` must not be a normal model-facing editing API. Patch-envelope or
unified-diff parsing may remain as an internal implementation detail or
migration aid, but the agent-facing path is `read_file` plus `edit_file`, or
`search_code` plus `read_code` plus `edit_code` for SCOPE-owned source files.

### Browser

`Browser` is the interface for viewing and interacting with web pages.

It is an `App` because:

- page content is naturally local and time-dependent
- loading may need to be awaited
- interactions require reading a semantic snapshot before using `element_ref`
- later interactions depend on a persistent page session

Operational constraints:

- Operate only through browser tools.
- Interactions must explicitly provide `page_id + element_ref`.
- If a page change invalidates refs, reread the page instead of blindly retrying old refs.
- Search result pages are usually leads for locating sources, not final facts.

### Coding

`Coding` is the interface for semantic code operations powered by scope-engine.

It is an `App` because:

- project state (open project, LSP connections) persists across tool calls
- symbol lookups and propagation analysis require a focused project context
- the model needs to see propagation status to decide whether to continue editing

**State rendering design:**

Coding app must render its key state into `AppStateRender` so that:

1. **Turn re-entry sees critical state immediately** — after context compression or turn interruption, the model can read the current project, LSP status, and pending propagation events from `<preturn_context>` or `<afterclaim_context>` without relying on conversation history.
2. **Tool return values carry immediate feedback** — `search_code` returns stable read handles, `read_code` returns hash-anchored source lines, and `edit_code` returns propagation results so the model sees impact scope mid-turn.
3. **Notice is NOT for propagation review** — Coding app uses `notice_reason()` only for background events like "LSP server crashed" or "project index ready". Propagation review is handled through tool return values and state rendering, not through notice-triggered turn interrupts.

Operational constraints:

- Coding tools (`search_code`, `read_code`, `edit_code`, and review tools) must go through the Coding app, requiring `focus_app("coding")` first.
- Use `read_file` for explicit path/range reads. Do not make `read_code`
  support path/range compatibility.
- Use `edit_file` for non-SCOPE files. When Coding is focused, `edit_file` must
  reject SCOPE-owned source files and require `edit_code`.
- Coding app `render_state()` must include: project_root, open_languages, lsp_status, propagation_pending_count, and up to N recent propagation events.
- LSP process lifecycle (start, crash recovery, shutdown) belongs to Coding app internals, not to tool return values.

### Coding Search / Read / Edit Protocol

The Coding tool protocol must not require the model to write SCOPE positioning
syntax. Canonical target labels remain an internal and display-level positioning
format, but model operations should use stable handles produced by code.

`search_code` is the model-visible search primitive. It replaces separate
model-visible `grep` and `glob` tools.

Search input is content-oriented, with optional narrowing fields such as path,
include pattern, and limit. Search output must be a compact list of stable read
handles plus display labels:

```text
code::search("run_tui_dashboard")
-> 1268#k7Qp|src/dashboard/mod.rs::fn run_tui_dashboard #L1268-L1320
-> 286#b91Z|src/dashboard/mod.rs::trait DashboardHistoryLoader #L286-L302
-> 1#a0F2|src/dashboard/mod.rs#L1-L24
```

Rules:

- The handle format is `start_line#hash4`, for example `1268#k7Qp`.
- The handle is a read capability for a canonical target, not a content
  fingerprint.
- The hash input is only the canonical target label, such as
  `src/dashboard/mod.rs::fn run_tui_dashboard #L1268-L1320` or
  `src/dashboard/mod.rs#L1-L24`.
- The handle must not include the target body, search query, session salt, file
  mtime, read timestamp, line hashes, or any other freshness material.
- The line number is part of handle identity. Do not add salted collision
  fallback, random suffixes, or automatic hash extension logic.
- The same canonical target in the same project must produce the same handle.
- Search results inside an AST symbol should point at that canonical symbol
  target label.
- Search results outside an AST symbol, such as imports or top-level statements,
  should point at a small canonical line-range target.
- Multiple matches inside the same target should be deduplicated before
  rendering.
- The display label is for human/model reading and for copying the path into
  `edit_code`; it is not syntax the model is expected to author.

`read_code` reads a search handle only:

```text
code::read("1268#k7Qp")
-> 1268#7a|fn run_tui_dashboard(...) {
   1269#c1|    ...
```

Rules:

- `read_code` output should not repeat the read handle, canonical target label,
  or path when the model already obtained them from search.
- `read_code` output lines use the existing `line#hash2|source line` format.
- Line hashes stay short. They are stale-edit guards, not identity handles.
- Read-handle freshness and edit freshness are separate. Search handles locate
  targets; line hashes guard edits against stale source.
- `read_code` must not accept `path`, `start_line`, or `line_count`. Those
  fields belong to `read_file`.
- Avoid JSON Schema `oneOf`/`anyOf`. `read_code` should expose one clear handle
  field such as `ref`/`handle`, not a flat schema that pretends to support
  multiple modes.

`edit_code` uses the same structured edit schema as `edit_file`, but with SCOPE
propagation analysis and review:

```text
code::edit({
  edits: [{
    path: "src/dashboard/mod.rs",
    op: "replace",
    start: "1268#7a",
    end: "1320#d4",
    content: "..."
  }]
})
```

The model copies `path` from the search display label and copies `start`/`end`
line anchors from `read_code`. Existing replace/append/prepend semantics, line
hash verification, parse validation, and propagation analysis remain unchanged.
`edit_code` must not accept read handles as edit targets.
Like `edit_file`, `edit_code` must expose a flat structured-edit schema without
JSON Schema `oneOf`/`anyOf`.

The read-handle registry belongs to the Coding session state, not shared/global
state. It is cleared when the project changes and is not persisted as a
long-term identity database. `read_file` does not use this registry.

### App Composition

An app may declare that it *contains* other apps, making their tools available when the composing app is focused.

When `Coding` is focused, the tool scope includes:

- Coding's own tools: `search_code`, `read_code`, `edit_code`, review tools
- Terminal's delegated tools: `terminal_exec`, `terminal_write_stdin`, `terminal_terminate`
- Browser's tools: **not** available unless the model explicitly focuses Browser
- Static runtime file tools such as `read_file` and `edit_file` are not owned by
  any app. Their availability is governed by runtime policy and the SCOPE
  boundary, not by app composition.

Implementation: each `App` can optionally expose `fn composed_apps() -> Vec<AppId>`. The runtime tool-scope check traverses this list so that focused-app restriction plus composition gives the correct tool availability.

Rationale:

- "I am coding" inherently includes "I need to run commands and edit non-SCOPE
  files."
- Forcing `focus_app("terminal")` back-and-forth would be an unnecessary interruption.
- Composition preserves the attention model: `focus_app("coding")` means "I am in coding mode," and all tools needed for that mode are available.

### SCOPE Current Boundary and Static File Tool Boundary

SCOPE (scope-engine) provides semantic code reading, searching, hash-anchored
editing, and propagation review. Do not document unimplemented refactoring
features as expected model-facing capabilities.

| Capability | SCOPE Status | Boundary |
|---|---|---|
| Target discovery | ✅ `search_code` | Content search returns stable read handles plus display labels; the model must not author target syntax. |
| Read code | ✅ `read_code` | Reads a search handle only and returns hash-anchored source lines. Explicit path/range reads belong to `read_file`. |
| Edit code | ✅ `edit_code` | Applies the same structured hash-anchored edits as `edit_file`, plus SCOPE parse validation and propagation review. |
| Propagation review | ✅ review tools | Edit impact is surfaced through propagation results and review events. |
| New source files | ⚠️ explicit supported creation paths | Use supported creation/edit paths; SCOPE has no template system. |
| Non-source/config files | Outside SCOPE | Use `read_file` and `edit_file` for `.toml`, `.yaml`, `.md`, `.json`, `.sh`, and other non-source files. |

**Static file edit boundary:**

When Coding is focused and `edit_file` targets a source-code file that SCOPE
owns (for example `.rs`, `.py`, `.go`, `.ts`, `.js`, `.java`, `.c`, `.cpp`,
`.rb`, `.php`), Coding rejects the call and requires `edit_code` instead.

For non-source-code files (`.toml`, `.yaml`, `.md`, `.json`, `.sh`, etc.) or
unsupported cases outside SCOPE responsibility, `edit_file` is allowed.
Propagation review is then limited to what Coding can observe through its own
semantic operations and explicit review events; do not assume plain file edits
silently receive the same propagation analysis as `edit_code`.

## Third-Party App Package

Future third-party `App` extensions use a source-first design. Do not copy Codex plugin or connector structure.

### Directory Placement

- Third-party app source directories are fixed under the runtime workspace: `~/daat-locus-workspace/apps/<app_id_snake_case>/`.
- The current runtime workspace is resolved by `resolve_runtime_workspace_dir()`, which defaults to `~/daat-locus-workspace`.
- `app_id` is exactly the folder name `<app_id_snake_case>`.
- `~/.daat-locus` is a protected runtime directory and must not store third-party app source code.
- This design exists because `~/.daat-locus` is treated as a protected runtime path inside the sandbox, while the workspace is the default editable area.

### Package Layout

Minimal directory structure:

```text
~/daat-locus-workspace/apps/<app_id_snake_case>/
  app.toml
  runtime/
    app.lua
  prompt/
    usage.md
    how_to_use.md
```

Rules:

- `runtime/app.lua` is the only Lua entry point.
- `prompt/usage.md` is the pre-focus app description.
- `prompt/how_to_use.md` is the post-focus app description.
- Third-party app packages do not carry self-optimizable workflow assets.

### `app.toml`

In v1, `app.toml` is intentionally minimal. It has one responsibility: specify the relative path to the Lua entry point.

Rules:

- It does not carry `id`.
- It does not carry permissions.
- It does not carry usage, how-to-use, or workflow metadata.
- By default, the entry point is `runtime/app.lua`.

Minimal example:

```toml
entry = "runtime/app.lua"
```

Identity comes from the directory name. Configuration comes from `app.toml`.

### Lua Runtime

The third-party app runtime stack is fixed as:

- Rust side uses `mlua`.
- Lua dialect is standard `Lua 5.4`.
- Do not use legacy `rlua`.
- Do not use `JS/TS` as the v1 app runtime.
- Do not use `Wasm` as the v1 app runtime.

Rationale:

- The agent needs to be able to directly write and modify apps.
- Source-first Lua plus Markdown is a better v1 authoring format than ABI-first Wasm.
- `mlua` has mature enough Lua 5.4 support in Rust for host embedding.

### Unified Lua Interface

Do not design an app as multiple independent Lua entry scripts.

The correct model is:

- One third-party `App` equals one unified Lua module instance.
- The host loads only `runtime/app.lua`.
- `render_state`, tool calls, and notice polling share the same app instance state.

Do not introduce additional IPC to synchronize tool results and render state.

This means the behavioral body of a third-party app is an object model, not a collection of scripts.

### Workflow Assets

Self-optimizable workflows do not belong to the app package. They are runtime-level SOP primitive assets.

Rules:

- Workflows are not attached to any app by default.
- Builtin primitive specs live in repository root `workflows/*.md` and are compiled into the program by `build.rs`.
- Evolvable workspace primitive specs live in `~/daat-locus-workspace/workflows/*.md`.
- Each primitive spec is one Markdown file, and the filename is the primitive id.
- Primitive filenames may contain only lowercase `a-z` and `-`; identity comes only from the file stem.
- Primitive Markdown content is unrestricted by the primitive id; any legacy frontmatter is ignored for identity, and runtime writes specs as Markdown bodies without frontmatter.
- A primitive spec file should describe one reusable SOP primitive, not a composite task class.
- Composite task execution is a temporary runtime graph assembled from primitives; it is not a workflow asset to save by default.
- `prompt/*.md` is for app descriptions; `workflows/*.md` is for self-optimizable execution processes. Do not mix them.
- Builtin primitive specs do not fall into a writable runtime directory and are not touched by optimization pipelines.

### Reload Strategy

Third-party apps should not be fully reparsed on every turn.

Recommended strategy:

- Perform one full scan of `~/daat-locus-workspace/apps` at startup.
- Scan and watch the primitive spec directory `~/daat-locus-workspace/workflows` separately.
- Use `notify` at runtime to watch supported directory changes.
- Map file events to the affected `<app_id_snake_case>`.
- Mark only that app as dirty and reload it incrementally.
- When primitive spec files change, mark only the affected primitive as dirty and reload it incrementally.
- If the watcher fails or directory state becomes untrusted, fall back to one full rescan.

Do not make full parsing the normal path.

### State and Cache

v1 does not define a dedicated third-party app cache directory.

Current conclusions:

- Define only the source directory: `~/daat-locus-workspace/apps`.
- Define workflow source separately as `~/daat-locus-workspace/workflows`.
- Do not define `cache/apps`.
- Do not define `cache/workflows`.
- If the host later truly needs to persist app runtime state, use the protected runtime state system, for example `~/.daat-locus/state/apps/<app_id_snake_case>/`.
- If workflow telemetry later needs host persistence, use the protected runtime state system, for example `~/.daat-locus/state/workflows/`.

Third-party apps and workspace primitive specs are agent-editable assets, but they are not runtime state owned directly by the agent.

## Current Event Semantics

### Telegram

In the current code, Telegram is:

- input side: `TelegramTransport` polls the Bot API and registers incoming events
- manager side: approved messages are routed through the per-chat default session mapping
- session side: each target Session owns its own Telegram event queue and outbound queue
- send side: completing an event enqueues a message into the session outbox; the Manager drains session outboxes and delivers them through the Bot API

Telegram is not an `App`.

Reasons:

- When a new message arrives, code already knows enough structured facts.
- Normal handling is "judge and respond", not "first navigate to a chat UI and explore".
- Standard runtime actions should bind `event_id` and explicit `chat_id`, not rely on hidden cursors.
- Session selection for Telegram is Manager routing state, not an agent-facing app cursor.

For approved Telegram chats, the Manager maintains a default session mapping:

- If `chat_id -> session_id` exists and the session still exists, ordinary messages route to that session.
- If no valid mapping exists, the first ordinary message creates a new `General` session, stores it as the chat default, and routes the event there.
- This automatic default is per Telegram chat. It is not a global main session.

Telegram-only session control commands are Manager-level commands. They must not
enter any session's `EventStore` as ordinary user events:

- `/session_list` lists sessions and marks the current chat attachment.
- `/session_new [title]` creates a new `General` session and attaches the current chat to it.
- `/session_attach <session_id_or_unique_prefix>` attaches the current chat to an existing session.
- `/session_delete <session_id_or_unique_prefix>` deletes the target session through Manager lifecycle deletion and removes Telegram default mappings that pointed to it.

These commands may accept a unique `session_id` prefix for Telegram usability.
If the prefix is ambiguous, code must ask for more characters instead of
guessing. `/session_attach` changes only the Manager's Telegram default mapping;
it does not inject an event into the target Session.

The standard path for an approved Telegram message:

1. The transport receives the message.
2. If the message is a Manager-level command, the Manager handles it directly and replies without creating a runtime event.
3. Otherwise, the Manager resolves or creates the chat's default session.
4. The Manager sends `EnqueueTelegramEvent` to the target Session over local IPC.
5. The Session registers the event in its own `EventStore`.
6. The Session enqueues `PendingWork::Event`.
7. The runtime claims the event.
8. The model judges and calls tools.
9. It ends the event with `finish_and_send`.
10. The Session writes the reply into its Telegram outbox.
11. The Manager drains the outbox, delivers through the Bot API, and records delivery back to the Session.

Unknown Telegram chats do not enter the normal event-processing path. They enter the ACL pending flow.

## Resolution Rules

All resolutions must bind to a specific event, not to a container.

Current minimum requirements:

- Operate on events through `event_id`.
- Disposition must be explicit: `resolved`, `dismissed`, or `failed`.
- `resolved` or `failed` must provide a non-empty `reply_message`.

Current event states include:

- `Pending`
- `Claimed`
- `AwaitingDelivery`
- `Resolved`
- `Dismissed`
- `Failed`

When designing a new event type, follow these principles:

- If stale/new event conflicts can exist in the world, actions must bind to a concrete version or equivalent freshness guard.
- Do not resolve only by container ids such as `chat_id`, `thread_id`, or `page_id`.
- Failure states should allow retry or revalidation rather than silently swallowing the problem.

## Tool Design Rules

### General

- Tools should explicitly mutate the world.
- Plain text must not implicitly trigger side effects.
- Tool parameters should use explicit identifiers as much as possible instead of hidden prior selection.
- A normal operation should complete in one explicit call when feasible.

### App-Scoped Tools

App-scoped tools may require `focus_app` first.

This is not pointless ceremony. It preserves attention discipline:

- The current foreground app determines available tool scope.
- `focus_app` and `put_away_app` trigger a turn boundary and require a fresh world-state render.

Therefore, do not design operations that clearly belong to Browser or Terminal but can secretly execute from any context.

### Event Tools

Event completion tools must:

- explicitly receive `event_id`
- explicitly receive `reply_message` when a final answer needs to be sent to the user
- use `dismissed` only for silent completion; `failed` should still send a failure explanation to the user

Do not design the final reply as assistant text itself.

### Plan Tools

`update_plan` maintains only the complete current plan.

Do not add tools such as `append_plan_step` or `select_plan_step` that introduce hidden cursors and incremental synchronization complexity unless there is strong evidence that the current contract is insufficient.

### Workflow Tools

Workflow's responsibility is to expose a reusable SOP primitive library for task-time composition, not to carry dynamic world state.

Current rules:

- The workflow primitive routing catalog appears directly in `afterclaim_context` as a full `primitive_ids` vocabulary plus `relevant_primitives` details for the top task-relevant primitives.
- `primitive_ids` should include every loaded primitive ID so runtime composition can see the available vocabulary; `relevant_primitives` entries should emphasize primitive name, capability, inputs, outputs, and constraints. `when_to_use` is supporting metadata, not the main interface.
- The currently bound primitive or temporary primitive graph is exposed to the model in fuller form.
- v1 only needs `create_primitive_spec` and `activate_composed_primitive`, or an equivalent bind tool.
- `create_primitive_spec` may only create workspace primitive specs. It must not overwrite builtin primitives.
- Do not make the model perform workflow semantic search or browse an expanded lexicographic workflow dump before continuing. The full ID vocabulary and relevant primitive details should be displayed directly in `afterclaim_context`.
- Do not introduce explicit `log_workflow_outcome`. Daytime evidence should be written automatically by code into `PrimitiveRunRecord`.
- Whether to bind a workflow is driven by task complexity and reusability, not by `focus_app`.

## What Code Should Do

Code is responsible for:

- polling and receiving Telegram updates
- deduplicating events
- persisting state
- loading builtin primitive specs and workspace primitive specs
- writing `PrimitiveRunRecord` directly at the work-completion boundary
- claiming, releasing, and requeueing pending work
- maintaining the outbox
- loading structured runtime context
- controlling tool scope
- recording traces
- running prompt compile and workflow evolution separately

Do not push these responsibilities onto the model.

In particular, do not make the model repeatedly perform:

- list
- select
- open
- read latest state
- dedupe
- freshness check
- delivery bookkeeping

## What The Model Should Do

The model is responsible for:

- understanding event semantics
- judging whether a response is needed
- choosing whether to focus an app
- judging whether to create or bind a workflow
- planning steps
- choosing tools
- calling `deep_recall` when needed
- producing the final `reply_message`

If a new interface mainly makes the model perform mechanical lookup, it is probably designed incorrectly.

## Runtime Context Rules

The old unified `snapshot` concept is too broad. Do not treat it as the long-term context model.

Future context assembly should split the old snapshot responsibilities into two structured injection hooks:

- `AfterClaim Context`
- `PreTurn Context`

This does not mean deleting structured rendering. The structured unit rendering model must remain. Keep using document/part/block-style assembly such as `PromptDocument`, `PromptNode`, `PromptGroupDoc`, `PromptStateDoc`, `PromptUnitDoc`, `PromptBlock`, and `LlmPromptRenderer`. The goal is to replace one monolithic snapshot assembler with separate structured context assemblers, not to return to ad hoc string concatenation.

### AfterClaim Context

`AfterClaim Context` is injected after runtime work has been claimed.

It contains one-shot context for the currently claimed work:

- claimed event input
- claimed app notice input
- event/app notice source metadata needed to handle the claimed work
- workflow primitive routing catalog needed to choose or create primitives and compose a temporary execution graph for this claimed work

It is not a per-turn status dump.

Rules:

- Inject it when new work is claimed.
- Runtime history compaction is a turn boundary. If compaction happens while claimed work is still in progress, release/reclaim the work and inject `AfterClaim Context` in the new turn instead of trying to restore it inside the old turn.
- Do not repeat it every turn unless compaction made reinjection necessary.
- Event input belongs here, not in a generic world snapshot.
- Workflow primitive routing catalog belongs here, not in per-turn execution state.
- It should enter runtime history as structured context so later tool calls and assistant messages can build on a stable prefix.

### PreTurn Context

`PreTurn Context` is injected before each model turn, using the same broad trigger point that the old snapshot used.

It contains current execution state that may change after tools run:

- memory recall for the turn
- sensory state such as current time and machine status
- current plan state
- current app surface state, focused app state, and background app hints
- current bound primitive or temporary primitive graph execution context, including workflow id/origin when applicable and concise execution excerpt

Rules:

- Inject it before every model turn.
- Preserve turn boundaries for operations that change the next context view, such as `activate_composed_primitive`, `focus_app`, `put_away_app`, `update_primitive_spec`, and workspace app dynamic tools that return a turn boundary.
- Treat runtime history compaction as the same kind of boundary: compact, end the current turn, and build a fresh `PreTurn Context` for the next turn.
- It should enter runtime history as structured context, rather than being appended as a transient final user message outside history.
- Keep it concise and structurally compressible; do not persist full raw app screens, huge memory excerpts, or repeated low-value status dumps.

### Capability Docs

Do not mix capability manuals into state when a stable instruction layer is better.

Examples:

- event completion rules belong in system/tool contract or a small event contract block
- app usage and `how_to_use` are capability docs, not app state
- workflow primitive routing catalog rules belong in workflow contract / AfterClaim routing context

### Workflow Context Split

Workflow context has three distinct roles:

- `WorkflowRouting`: workflow primitive routing catalog plus choose/create/compose guidance for the currently claimed work; belongs in `AfterClaim Context`
- `WorkflowState`: the currently bound primitive or temporary primitive graph execution context; belongs in `PreTurn Context`
- `WorkflowHistoryMetadata`: which workflow a past task used; belongs in turn metadata/history, not in the primitive routing catalog

Do not make the model infer past task workflow ownership from adjacent tool logs if code can record it directly.

### Historical Metadata

When context needs to support future edits to past task workflows, preserve a small structured turn metadata record rather than saving full snapshots.

Turn metadata should be able to record facts such as:

- turn id
- claimed event or app notice ids
- user request summary
- bound workflow id and origin
- workflow run id when available
- focused app
- final disposition or outcome
- completed event ids
- concise tool summary

This metadata should enter runtime history and be compressible. It is separate from sleep-only `PrimitiveRunRecord` evidence, which may be consumed by sleep and must not be the only source of daytime historical workflow attribution.

### Legacy Snapshot Mapping

When refactoring old snapshot parts, use this mapping:

- old `recall_memories` -> `PreTurn.Recall`
- old `sensory` -> `PreTurn.Environment`
- old `plan` -> `PreTurn.TaskState`
- old focused app state and background hints -> `PreTurn.AppSurface`
- old app usage / how-to-use -> `CapabilityDocs.App`
- old claimed events -> `AfterClaim.ClaimedInput`
- old event queue summary -> `AfterClaim.EventQueueContext`
- old delivery reminder -> `EventContract`
- old workflow list and selection hint -> `AfterClaim.WorkflowRouting` workflow primitive routing catalog and composition hint
- old bound workflow id, origin, steps, done criteria, and recovery -> `PreTurn.WorkflowState`

Avoid these designs:

- persisting the full old snapshot on every turn
- using one context blob to carry state, routing, manuals, memory recall, and history metadata
- repeatedly injecting event input or the workflow primitive routing catalog every turn when no compaction occurred
- dropping structured rendering in favor of hand-built strings

## Anti-Patterns

Avoid these designs:

- Modeling transports such as Telegram, email, or notification centers as `App`s by default.
- Forcing the model to "open a chat" before handling a known new message.
- Storing `selected_chat`, `selected_thread`, or `opened_message` in app state.
- Making send or resolve depend on hidden viewport state.
- Binding events only to container ids instead of event ids.
- Designing workflow as app-local supplemental instruction or innate model capability.
- Forcing the model to perform workflow semantic search before continuing.
- Auto-generating generic default workflow templates for blind model use.
- Persisting composite workflows for every recurring combination of primitives.
- Treating `when_to_use` text as the primary workflow runtime interface instead of exposing primitive capabilities and IO contracts.
- Showing only the first few lexicographically sorted workflow ids, or only filtered relevant ids, when task-time composition needs the full primitive ID vocabulary plus relevant details.
- Writing the current workflow binding back into the primitive spec itself.
- Making the model manually submit workflow result logs.
- Treating long-term memory as an immediate state cache.
- Treating plan as a backlog database.
- Letting the model implicitly submit final send actions through plain text.

## Design Checklist

Before adding an agent-facing interface, ask:

1. Is this an interactive surface or a structured fact that has already arrived?
2. Would a human describe it as "go operate that interface" or as "something happened; decide how to handle it"?
3. Does code already have the facts the model needs?
4. Does the action bind to a concrete object and freshness guard?
5. Will this interface induce mechanical enumeration by the model?
6. Is it compatible with the trace, workflow-run-record, and sleep evaluation loop?

If the answer leans toward exploration and focus, model it as an `App`.

If the answer leans toward arrived fact and resolution, model it as an `Event`.

If the answer leans toward driving the next round of processing, it usually belongs to `PendingWork`, not `Event` or `App`.

## In Short

- `App` decides where attention goes and how to operate after focus.
- `Event` decides what happened, whether to respond, and how to complete it.
- `PendingWork` decides what should drive the next turn.
- `Workflow` supplies SOP primitives that runtime can compose for the current task and sleep can keep corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.
