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
- Browser, Terminal, and Coding are built-in `App`s because they represent stateful capability domains with their own tools, state, and lifecycle.
- `App` and `Event` are parallel concepts. Do not collapse one into the other.
- Let the model make semantic judgments. Do not make the model perform mechanical enumeration, lookup, deduplication, or freshness checks that code can already perform.

## Model-Facing JSON Schema Design

Model-facing schemas are an engineering contract, not a provider-specific
cleanup target. This section applies to runtime tool inputs, app tool inputs,
structured model outputs, and fallback schemas for freeform tools.

Daat uses a conservative portable schema dialect. The
`daat-locus-macros::model_schema` attribute macro is the normal compile-time
entry point for Rust model-facing input and output types. The dialect is the
source of truth, and the macro must implement it exactly. Runtime normalization
must not be relied on for correctness. Provider boundaries should receive
already-valid schemas. Dynamic schemas from workspace apps or other external
sources cannot be compile-time checked, so they must be validated at load time
instead of silently rewritten.

Rules:

- Root schemas must be JSON objects.
- Every object schema must contain `properties`, `required`, and
  `additionalProperties: false`.
- Every property listed in `properties` must also be listed in `required`.
- Optional model-visible values are represented as nullable required fields,
  such as `type: ["string", "null"]`. Do not represent optionality by omitting a
  required entry.
- Rust deserializers may accept missing fields for backward compatibility, but
  the model-facing schema must not advertise missing fields as valid input.
- Do not emit JSON Schema `default`. Defaults are local deserialization or
  implementation behavior, not part of the model contract.
- Do not use `skip_serializing_if`, serde defaults, or provider normalization to
  define model-visible optionality.
- Use simple scalar types only: string, integer, number, boolean, object,
  homogeneous array, and null unions.
- Use string enums for finite choices. Avoid tagged, untagged, adjacently
  tagged, or payload-carrying enums in model-facing schemas.
- Avoid maps, dictionaries, dynamic object keys, tuple arrays, `prefixItems`,
  and schema-valued `additionalProperties`.
- Avoid composition and conditional keywords: `oneOf`, `allOf`, `anyOf`, `not`,
  `if`, `then`, `else`, `dependentRequired`, and `dependentSchemas`.
- Avoid validation keywords that are not consistently supported across strict
  structured-output implementations: string `minLength`, `maxLength`,
  `pattern`, `format`; numeric `minimum`, `maximum`, `multipleOf`; array
  `minItems`, `maxItems`, `uniqueItems`, `contains`.
- Prefer inlined schemas over `$defs`/`$ref` for model-facing tool schemas. If a
  provider-specific path later allows references, a `$ref` schema must not have
  sibling keywords such as `default` or `description` next to the `$ref`.
- Keep schema names, property names, enum values, and descriptions concise.
  Large schemas cost context and can hit provider schema limits.
- Schema generation tests must inspect the final model-facing JSON, not only
  Rust type definitions.

When adding a model-facing Rust input/output type:

- Use `#[model_schema]` on the Rust type and call
  `model_schema_for::<Type>()`.
- Do not expose raw `schemars` output directly unless the test proves it already
  conforms to this section.
- Do not add provider-specific normalization to make a bad schema pass. Fix the
  type shape or the schema macro instead.
- If the type cannot express the model contract directly, add a narrow
  schema-only mirror type with `#[model_schema]`; do not hand-build provider
  JSON in runtime or provider code.

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
/debug      -> action menu -> readonly detail views
/app-status -> app picker -> app detail view
/status     -> readonly detail view
```

If the user's next step is to choose, browse, toggle, or inspect something, it
belongs in a bottom panel rather than in a visible slash subcommand.

## Session Activity Event Protocol

Session activity is a shared semantic event stream. It is not a shared rendered
cell model, not a tool-specific UI protocol, and not a generic text log.

In this section, `Event` means a session activity event: a typed fact that the
session wants clients to render in the activity feed, transcript, or history.
This is separate from the external-input `Event` stored in `EventStore`, but the
same design rule applies: the payload describes what happened, not how a client
should draw it.

The public Manager/Session/dashboard contract should expose session activity as
typed events. Tools, model output, runtime status changes, and other
client-visible session activity all enter the same semantic event stream. The
TUI, WebUI, transcript overlay, history views, and future clients each derive
their own local view models from that event stream.

The desired flow is:

```text
runtime / tools / model
  -> session activity Event
  -> TUI derives a local render view model
  -> WebUI derives AgentChatActivityItem or another React-local view model
  -> transcript derives raw/copyable text
```

Rules:

- Activity payloads describe the data itself, not how that data should be
  displayed. Do not store titles, preview lines, expanded/collapsed state,
  detail indentation, UI hints, markdown truncation, colors, icons, bullets, or
  layout markers in the shared activity contract.
- Do not force every activity into a generic payload. Preserve the natural
  structure of each domain event: terminal executions have commands, status,
  cwd, stdout/stderr, exit code, and timing; patches have files and diff lines;
  plans have steps; browser snapshots have URLs and captured content; model
  reasoning has source text.
- If an event's source data is text, keep it as source text. For example,
  thinking/reasoning should be `Thinking { content: String }`, not
  `title/body_lines/full_body/expanded`.
- Labels such as `Thinking`, `Ran`, `Updated Plan`, bullet markers, glyphs,
  folding affordances, preview limits, and markdown rendering choices belong to
  clients.
- Local interaction state belongs to clients. Expanded thinking, scroll
  offsets, selected rows, active detail panes, copied transcript selection, and
  similar state must not be stored in the shared activity event.
- Tool execution must not emit a tool-specific UI protocol. It should emit
  semantic activity events or domain tool observations; renderers then decide
  how to display them.
- TUI render cells are local view models derived from activity events. They must
  not be the Manager/Session/WebUI protocol, persistence format, or history
  source of truth.
- WebUI-local view models, such as `AgentChatActivityItem`, are derived from
  activity events. They must not be produced by parsing TUI cells or stored as
  the source of truth for history.
- Raw text fallback is acceptable only for unknown/debug events or genuinely
  unstructured output. A supported activity type should have a typed semantic
  payload.
- Do not add compatibility layers that normalize old tool-UI, TUI-cell, or
  WebUI item contracts back into shared activity data. Delete or replace old
  render-shaped contracts when touching those paths.

Examples:

```text
Thinking { content }
AssistantMessage { content }
UserInput { content, attachments }
TerminalExecution { command, cwd, status, stdout, stderr, exit_code, duration }
BrowserSnapshot { url, content, line_count, ref_count }
WebSearch { query, url, summary }
PlanUpdated { kind, explanation, steps }
PatchApplied { files }
PrimitiveActivated { primitive_id }
ReplySent { disposition, subject, message }
```

## WebUI Session Rendering

These rules apply to session conversation and activity rendering only: the
Agent session message list, live activity, transcript-like views, and slash
command surfaces inside the session. Other WebUI pages such as navigation,
login, settings, logs, global status, and sidebars are normal web product
surfaces and do not need to mirror the TUI.

The WebUI session surface should stay close to the TUI in information hierarchy,
density, naming, and runtime semantics, but it must not consume TUI rendered
structures as its data source. WebUI should derive web-native markup and
interaction from the shared session activity event payloads.

Rules:

- Render from structured semantic activity events in `DashboardState`. Do not
  parse rendered TUI strings, bullet lists, status prose, command output, or
  legacy cells back into web structure.
- If WebUI needs structure that does not exist yet, add it to the
  Manager/Session/dashboard activity event contract. Do not add frontend
  regexes or string split heuristics as the normal path.
- Slash command output and interaction inside the session follow the same rule:
  use web-native controls, lists, toggles, buttons, and detail surfaces backed
  by semantic data. Plain preformatted output is only acceptable for explicit
  debug/raw views or unknown fallback content.
- Keep visual parity with the TUI through spacing, ordering, labels, status
  semantics, and progressive disclosure derived from the same semantic events.
  Do not copy TUI implementation structures such as render cells, wrapped
  lines, or cached render output.
- The old per-kind TUI glyph icon system is deprecated for product UI. Do not
  revive glyphs such as app-specific symbols for Browser, Coding, Patch, or
  Primitive. The standard current TUI layout markers such as `•`, `›`, and
  detail indentation are layout markers, not a per-kind icon system; mirror
  them only when the current TUI renderer uses them.
- Avoid nested cards in the session message list. Use flat activity rows,
  sections, lists, tables, collapsibles, separators, and inline controls.

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
facts such as activity events, live activity events, runtime status, token usage,
skills, Telegram access requests, status text, and errors.

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
- visible live activity with spinner or progress affordances
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

### Manager Boot And Config Readiness

Manager startup must not depend on complete agent/runtime configuration.

The Manager is the product shell and public control plane. It must be able to
start, serve WebUI, authenticate requests, expose logs/status, and guide
configuration even when agent configuration is missing, incomplete, or damaged.
Configuration readiness controls whether Session workers and agent operations
are allowed; it does not decide whether the Manager can bind its port.

The only config value needed for Manager boot is the daemon port. Read it
through a lightweight boot reader, not full agent config validation:

```text
read_manager_boot_config()
  -> read daemon.port from config.toml if possible
  -> if current config is damaged, try config.toml.bak
  -> if both are unavailable or damaged, use default port 53825
```

Do not call full `load_config()` to decide whether the Manager can start. The
port belongs to Manager boot config; provider/model configuration belongs to
agent runtime readiness.

Config readiness distinguishes these cases:

```text
damaged       config.toml cannot be parsed or deserialized reliably
unconfigured no real agent config exists, including only-port config
incomplete   partial agent config exists but cannot run an agent
complete     config parses and validates as runnable agent config
```

Only `unconfigured`, `incomplete`, and `complete` are durable public readiness
states. `damaged` is a recovery path, not a long-term operating mode. Public
readiness responses should surface it as a recovery note and then report the
post-recovery state. On startup or readiness refresh:

1. Move damaged `config.toml` to a timestamped `.corrupt-*` file.
2. Try restoring `config.toml.bak`.
3. If the restored backup parses, classify readiness from it.
4. If the backup is missing or damaged, move the bad backup aside and write a
   setup-safe default config.

The setup-safe default config must not be `Config::default()` if that contains
fake providers, fake API keys, or fake model entries. It should contain only
Manager boot-safe values such as the default daemon port. Fake provider/model
placeholders must never make readiness look `complete`.

`unconfigured` means no real agent config exists. This includes no
`config.toml`, an only-port config, or the setup-safe default written during
recovery. WebUI and TUI route to initialization. Agent/session creation,
`/send`, runtime dashboard commands, and Session worker startup are disabled
with a clear config-not-ready error.

`incomplete` means the config parses and contains some agent configuration
intent, but cannot run the agent. Examples include provider without valid model
roles, model references to missing providers, missing `main_model` or
`efficient_model`, or empty required credential/base URL fields. WebUI routes
to settings/configuration completion, TUI routes to interactive config repair,
and agent operations remain disabled.

`complete` means provider/model/main/efficient references are valid and the
runtime can construct model providers. Only this state enables agent operations.

`config.toml.bak` is the latest successfully parsed config:

- every successful config parse updates `.bak` atomically
- every successful config write updates `.bak` atomically
- recovery preserves damaged files as `.corrupt-*`
- if both current config and backup are damaged, write setup-safe defaults and
  classify readiness as `unconfigured`

The Manager may serve these regardless of readiness:

- embedded WebUI
- auth/token endpoints
- `/health` and `/status`
- config readiness and setup endpoints
- logs
- settings/setup pages

These require `complete` readiness:

- creating sessions
- Session worker startup
- `/send`
- `/commands/run`
- runtime dashboard actions
- any operation that needs provider/model config

If config is deleted, damaged, or changed after Manager startup, readiness must
be recomputed. New agent operations must be rejected until readiness returns to
`complete`. A stricter implementation may stop or pause existing Session
workers when readiness degrades.

WebUI startup reads readiness before normal routing:

```text
damaged/recovered -> show recovery note, then route by final state
unconfigured      -> setup wizard
incomplete        -> settings/configuration completion
complete          -> normal app
```

TUI startup follows the same state:

```text
unconfigured -> initialization wizard
incomplete   -> interactive config repair
complete     -> normal session selector
```

WebUI must not duplicate TUI setup/probing logic. Extract shared Rust setup
logic, for example a `config_setup` module, to own setup-safe defaults,
provider/model input normalization, probing, readiness classification,
validation, atomic writes, and backup updates. TUI and WebUI are frontends over
that shared layer only.

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

## Static Checks And Autofix

Static checks should not turn the model into a manual formatter.

- When a configured tool can mechanically fix the reported issue, use that tool
  rather than editing the mechanical change by hand. Examples include
  `cargo fmt`, formatter commands, and safe project-specific autofix commands.
- For lint tools with autofix modes, prefer the autofix path only when the tool
  is already part of the project workflow and the resulting diff can be
  inspected.
- Manual edits are for semantic fixes, unsafe autofix output, unavailable
  autofix support, or cases where the tool reports a real design problem rather
  than a mechanical rewrite.
- After any automatic fix, inspect the diff and run the relevant verification
  again.

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
- `preturn_context`: memory recall, sensory state, plan, bound workflow state, and project instruction context

Therefore, when adding an agent-facing interface, first decide which layer it belongs to. Do not directly pile prompt instructions, routing catalogs, or durable task state into app-local state.

## Core Objects

### App

An `App` is a stateful capability domain. It owns a coherent set of tools,
state, lifecycle hooks, and UI status for one interactive surface or long-lived
runtime subsystem.

The current built-in apps are:

- `Browser`
- `Terminal`
- `Coding`

Something should be modeled as an `App` only if it satisfies all of these
conditions:

- Its tools and state belong to a stable domain such as browsing, terminal
  sessions, project-aware code operations, or a workspace extension surface.
- It owns local runtime state that persists across tool calls.
- Operations have temporal semantics, such as waiting for loading, continuing a
  process, handling a page/session, or reading state after it stabilizes.

An `App` is not a permission gate. The model must not be required to activate,
select, or switch to an app before calling that app's namespaced tools. App
tools are exposed by domain namespace, such as:

```text
browser__browser_open_page
browser__get_state
terminal__terminal_exec
terminal__get_state
coding__open_project
coding__get_state
```

Every `App` must expose two separate layers:

- `state`: current structured domain facts, returned by the app's generated
  `appid__get_state` tool and rendered in TUI/WebUI app-status surfaces
- `docs`: stable system-prompt documentation for operating the app's tools and
  understanding the app's capability boundary

Do not mix these layers.

- `state` is not an operating manual.
- `docs` are system prompt material only. They must not be returned
  by `appid__get_state`, app-status surfaces, preturn state, or any other
  runtime state channel.
- App docs are not a routing or focus metadata layer. Do not add app prompt
  frontmatter such as `description` or `when_to_use`; if the model needs stable
  capability guidance, write it in the app docs markdown itself.
- Project or workspace instructions such as `AGENTS.md` are instruction context,
  not app state.

In code, keep this separation visible through `App::render_state`, `docs()`,
and the generated `appid__get_state` surface. Do not put
self-optimizable task execution procedures into an app's supplemental
instruction layer. Reusable methods across tasks should be modeled as `Workflow`
SOP primitives, not as app-local explanatory text.

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
- runtime context: phase, available tool names, active surface, plan summary, compact context summary
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
- unread output and process lifecycle need structured state

Operational constraints:

- Operate only through the Terminal app tools, exposed with the `terminal__`
  namespace.
- Do not treat interactive full-screen programs as the normal path.
- Do not hand interactive login or authentication flows to the model.
- Sessions are explicitly addressed; there is no hidden selected session.

### Static Runtime File Tools

Plain file reading and plain file editing are static runtime tools, not an
`App` and not a `global::*` namespace. They do not represent an app-owned
interactive surface. They are exposed as ordinary runtime tools:

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
- `read_file` output lines use the same `line#hash|source line` format used by
  Coding reads. Repeated visible `read_file` or Coding source lines may be
  elided as `line#hash~` or `start_line#hash...end_line#hash~`; these anchors
  remain valid and mean the full source line was already visible in current
  context.
- Line hashes are stale-edit guards. They are not long-term identities and
  should stay short.
- `read_file` is the fallback for imports, top-level code, search misses,
  user-specified locations, and non-source/config/document files.
- Do not put arbitrary path/range read compatibility into `read_code`; that
  belongs here. `read_code` accepts only a path plus a line-hash anchor and an
  `around`/`full` mode.

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

- `edit_file` uses the same structured edit schema and `line#hash` anchors as
  `edit_code`.
- `edit_file` verifies line hashes before writing.
- The model-visible edit schema should be flat and must not use JSON Schema
  `oneOf`/`anyOf`. Expose edit `content` as a string field; implementation may
  accept legacy array content, but the schema should not advertise it.
- `edit_file` handles ordinary non-SCOPE files such as Markdown, TOML, YAML,
  JSON, shell scripts, and unsupported file types.
- `edit_file` does not run SCOPE propagation analysis and does not produce
  propagation review events.
- When a project scope is open and the target is a SCOPE-owned source file,
  `edit_file` must be rejected with an instruction to use `coding__edit_code`.

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

`Coding` is the interface for semantic code operations powered by SCOPE —
Semantic Code Operation & Propagation Engine (`scope-engine`).

It is an `App` because:

- project state (open project, LSP connections) persists across tool calls
- symbol lookups and propagation analysis require an open project context
- the model needs to see propagation status to decide whether to continue editing

**State rendering design:**

Coding must render its key state into `AppStateRender` so that:

1. **State is available on demand** — the model can call `coding__get_state` to
   read the current project, LSP status, and pending propagation events without
   relying on conversation history.
2. **Tool return values carry immediate feedback** — `search_code` returns
   path-scoped line-hash hits, `read_code` verifies a path plus line anchor and
   returns hash-anchored source lines, and `edit_code` returns propagation
   results so the model sees impact scope mid-turn.
3. **Notice is NOT for propagation review** — Coding uses `notice_reason()` only
   for background events like "LSP server crashed" or "project index ready".
   Propagation review is handled through tool return values and state rendering,
   not through notice-triggered turn interrupts.

Operational constraints:

- Coding tools (`coding__search_code`, `coding__read_code`,
  `coding__edit_code`, and review tools) are app-domain tools and must be
  called directly through the `coding__` namespace.
- Use `read_file` for explicit path/range reads. Do not make `read_code`
  support arbitrary path/range compatibility.
- Use `edit_file` for non-SCOPE files. When the current project scope is open,
  `edit_file` must reject SCOPE-owned source files and require
  `coding__edit_code`.
- Coding `render_state()` must include: project_root, open_languages,
  lsp_status, propagation_pending_count, and up to N recent propagation events.
- LSP process lifecycle (start, crash recovery, shutdown) belongs to Coding
  internals, not to tool return values.

### Coding Search / Read / Edit Protocol

The Coding tool protocol uses one source-location vocabulary:
`path + line#hash`. A `line#hash` anchor is meaningful only inside one file.
Do not introduce a second model-facing target identity or session-local target
registry for search/read flows.

Line-hash source records rendered by Coding and `read_file` have these model-visible forms:

```text
line#hash|source line
line#hash~
start_line#hash...end_line#hash~
```

For `search_code`, prefix the same record with the path:

```text
path|line#hash|source line
path|line#hash~
path|start_line#hash...end_line#hash~
```

The `~` forms mean the exact same `path + line#hash` source line, or every line
in the contiguous `start...end` span, was already shown earlier in the currently
visible model context. These omitted anchors remain valid for follow-up reads
and edits, but the model must re-read the range if it needs source text that is
not visible nearby. Do not use `~` for stale, unavailable, hidden, or
permission-denied content. A real empty source line is still rendered as
`line#hash|`, not `line#hash~`.

When assembling tool results, elide repeated code greedily and only when the
runtime can prove the earlier full source record is still visible in the current
assembled prompt. The visibility key is `path + line#hash`; do not match on raw
source text alone. `search_code` hits, `read_code` lines, and `read_file` lines
all add full source records to the same visible-line set. Collapse adjacent
omitted records only when they are in the same path and their line numbers are
consecutive.

`search_code` is the model-visible search primitive. It replaces separate
model-visible `grep` and `glob` tools while staying aligned with `rg`
semantics. Inputs should cover the useful `rg` shape: `query`, `path`, `mode`,
`case`, `word`, `line`, `include`, `exclude`, `types`, `type_not`, `hidden`,
`respect_ignore`, `follow`, and `limit`.

Search output is an array of hits:

```json
{
  "matches": [
    {
      "path": "src/foo.rs",
      "hit": "42#ab|    call_target();"
    }
  ]
}
```

Rules:

- Return one match object per matched line.
- `hit` must be exactly one coding source record: normally
  `line#hash|source line`, or `line#hash~` when that exact `path + line#hash`
  line was already shown earlier in the visible context.
- Return the actual matched line. If a match is inside a function, method, type,
  or other AST symbol, the search hit is still the matched line, not the
  enclosing declaration line.
- Do not split or repeat `line_number`, `hash`, `text`, `label`, `enclosing`,
  or other metadata already encoded by the line anchor and source line.
- `path + line#hash` is the target identity for follow-up reads. `line#hash`
  alone is file-local and not globally unique.
- Search may use ASTs internally for ranking, filtering, or presentation, but
  it must not replace the visible hit with an enclosing symbol target.

`read_code` reads a path-scoped line anchor:

```json
{
  "path": "src/foo.rs",
  "anchor": "42#ab",
  "mode": "full"
}
```

Rules:

- `read_code` accepts any syntactically valid `line#hash` anchor with a path. It
  must not require the anchor to have been produced by a prior `search_code`
  call.
- Before reading, verify that the current file line still matches the supplied
  hash. On mismatch, return a stale-anchor error and tell the model to search or
  read again.
- `mode` has exactly two values: `around` and `full`.
- `around` returns a fixed local window around the anchor, roughly a dozen lines
  above and below. It does not perform AST expansion and has no tunable context
  parameters.
- `full` automatically returns the enclosing AST symbol when the anchor is
  inside a recognizable symbol. If no enclosing symbol is recognizable, it
  falls back to `around`.
- Do not add manual `enclosing`, `selector`, `context_before`,
  `context_after`, path/range, or other compatibility fields to `read_code`.
- `read_code` output should be minimal: `{ "content": "..." }`.
- `content` lines use coding source records: normally `line#hash|source line`,
  or `line#hash~` / `start_line#hash...end_line#hash~` when those exact
  `path + line#hash` lines were already shown earlier in the visible context.
  Do not repeat `path`, `mode`, resolved range, enclosing symbol metadata, or
  other values the caller already supplied or that are implementation detail.
- Line hashes stay short. They are stale-edit guards, not long-term identities.

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

The model copies `path` from the search hit and copies `start`/`end` line
anchors from `read_code.content`. Existing replace/append/prepend semantics,
line hash verification, parse validation, and propagation analysis remain
unchanged. `edit_code` must not accept opaque target handles or search result
objects as edit targets.
Like `edit_file`, `edit_code` must expose a flat structured-edit schema without
JSON Schema `oneOf`/`anyOf`.

### App Tool Domains

All app tools are model-facing through explicit app namespaces. Runtime tool
spec construction should expose every installed app's valid tool specs, plus the
generated `appid__get_state` tool for that app.

Examples:

- Coding tools: `coding__open_project`, `coding__search_code`,
  `coding__read_code`, `coding__edit_code`, `coding__next_review`
- Terminal tools: `terminal__terminal_exec`,
  `terminal__terminal_write_stdin`, `terminal__terminal_terminate`
- Browser tools: `browser__browser_open_page`, `browser__browser_snapshot`,
  `browser__browser_click`
- State tools: `coding__get_state`, `terminal__get_state`,
  `browser__get_state`

Static runtime file tools such as `read_file` and `edit_file` are not owned by
any app. Their availability is governed by runtime policy and the SCOPE
boundary, not by app tool domains.

Do not reintroduce app composition as a tool-exposure mechanism. Cross-domain
work should simply call the correct namespaced tool directly.

### SCOPE Current Boundary and Static File Tool Boundary

SCOPE — Semantic Code Operation & Propagation Engine (`scope-engine`) provides
semantic code reading, searching, hash-anchored editing, and propagation review.
Do not document unimplemented refactoring features as expected model-facing
capabilities.

| Capability | SCOPE Status | Boundary |
|---|---|---|
| Target discovery | ✅ `search_code` | Content search returns path-scoped matched-line hits in `line#hash|source line` form. |
| Read code | ✅ `read_code` | Reads a path plus line-hash anchor in `around` or `full` mode and returns hash-anchored source lines. Explicit path/range reads belong to `read_file`. |
| Edit code | ✅ `edit_code` | Applies the same structured hash-anchored edits as `edit_file`, plus SCOPE parse validation and propagation review. |
| Propagation review | ✅ review tools | Edit impact is surfaced through propagation results and review events. |
| New source files | ⚠️ explicit supported creation paths | Use supported creation/edit paths; SCOPE has no template system. |
| Non-source/config files | Outside SCOPE | Use `read_file` and `edit_file` for `.toml`, `.yaml`, `.md`, `.json`, `.sh`, and other non-source files. |

**Static file edit boundary:**

When a project scope is open and `edit_file` targets a source-code file that
SCOPE owns (for example `.rs`, `.py`, `.go`, `.ts`, `.js`, `.java`, `.c`,
`.cpp`, `.rb`, `.php`), the runtime rejects the call and requires
`coding__edit_code` instead.

For non-source-code files (`.toml`, `.yaml`, `.md`, `.json`, `.sh`, etc.) or
unsupported cases outside SCOPE responsibility, `edit_file` is allowed.
Propagation review is then limited to what Coding can observe through its own
semantic operations and explicit review events; do not assume plain file edits
silently receive the same propagation analysis as `edit_code`.

## Third-Party App Package

Future third-party `App` extensions use a source-first design. Do not copy another product's plugin or connector structure.

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
    docs.md
```

Rules:

- `runtime/app.lua` is the only Lua entry point.
- `prompt/docs.md` is the app's only prompt document. It is plain markdown
  without required frontmatter. It may include stable capability boundaries and
  operation documentation for the app's tools.
- Third-party app packages do not carry self-optimizable workflow assets.

### `app.toml`

In v1, `app.toml` is intentionally minimal. It has one responsibility: specify the relative path to the Lua entry point.

Rules:

- It does not carry `id`.
- It does not carry permissions.
- It does not carry prompt docs or workflow metadata.
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

### App Domain Tools

App domain tools are exposed directly through namespaced tool names. The app id
is part of the tool name and is the operation scope.

Rules:

- Do not require a separate activation or selection tool before calling an app
  domain tool.
- Do not hide app operations behind global aliases when the app namespace makes
  ownership clearer.
- Every app should have a generated `appid__get_state` tool that returns its
  current structured state.
- State tools should be cheap, inspectable, and side-effect free except for
  harmless cache refresh needed to report current state.
- Operations should bind to explicit ids such as `page_id`, `session_id`,
  project root, path plus line anchor, or app-specific object id. Do not rely on hidden
  selected objects.

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
- Whether to bind a workflow is driven by task complexity and reusability, not by app domain state.

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
- enforcing tool availability policy and app namespace collisions
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
- choosing the right tool domain
- calling `appid__get_state` when current app state is needed
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
- Runtime history compaction is a `Compacted` turn stop reason. If compaction
  happens while claimed work is still in progress, release/reclaim the work and
  inject `AfterClaim Context` in the new turn instead of trying to restore it
  inside the old turn.
- Do not repeat it every turn unless compaction made reinjection necessary.
- Event input belongs here, not in a generic world snapshot.
- Workflow primitive routing catalog belongs here, not in per-turn execution state.
- It should enter runtime history as structured context so later tool calls and assistant messages can build on a stable prefix.

### Runtime Turn Stop Reasons

Keep runtime turn termination reasons minimal. A turn should stop only for one
of these reasons:

- `Finished`: the work is complete. This includes ordinary assistant final text
  when no external input is claimed, `finish_and_send` for claimed events, and
  `notice_resolved` for claimed app notices.
- `Error`: the runtime cannot safely continue the current turn because of a
  preflight failure, unrecoverable model/tool failure, fuse trip, or dead-loop
  protection.
- `Compacted`: runtime history was compacted and the next model request must be
  built from the compacted history.
- `Interrupt`: the active turn was interrupted by the user or client. This is
  the spelling to use. It covers TUI `Esc`, TUI `Ctrl-C`, WebUI
  `interrupt_runtime`, and process-level interrupt signals routed through the
  runtime interrupt path.

Do not introduce new turn stop reasons for ordinary context changes. If a tool
changes what the model needs to know next, return the new facts in the tool
result or insert a structured context-refresh message before the next model
request. Context refresh is not itself a turn stop reason.

### PreTurn Context

`PreTurn Context` is injected before each model turn, using the same broad trigger point that the old snapshot used.

It contains current execution state that may change after tools run:

- memory recall for the turn
- sensory state such as current time and machine status
- current plan state
- current project instruction context when the session has a project scope
- current bound primitive or temporary primitive graph execution context, including workflow id/origin when applicable and concise execution excerpt

Rules:

- Inject it before every model turn.
- Operations that change the next context view, such as
  `activate_composed_primitive`, `update_primitive_spec`, project instruction
  reloads, and workspace app dynamic tools, should continue the current turn by
  returning structured tool output or inserting a structured context-refresh
  message before the next model request. They should not end the turn unless
  they also satisfy `Finished`, `Error`, `Compacted`, or `Interrupt`.
- Treat runtime history compaction as a `Compacted` stop reason: compact, end
  the current turn, and build a fresh `PreTurn Context` for the next turn.
- It should enter runtime history as structured context, rather than being appended as a transient final user message outside history.
- Keep it concise and structurally compressible; do not persist full raw app
  screens, huge memory excerpts, or repeated low-value status dumps.
- Do not inject app state automatically. App state is read explicitly through
  `appid__get_state` and displayed in client app-status surfaces.

### Capability Docs

Do not mix capability manuals into state when a stable instruction layer is better.

Examples:

- event completion rules belong in system/tool contract or a small event contract block
- app docs are capability docs, not app state
- project instructions such as `AGENTS.md` belong to project instruction
  context, not Coding app state
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
- last active app surface when useful for UI or diagnostics
- final disposition or outcome
- completed event ids
- concise tool summary

This metadata should enter runtime history and be compressible. It is separate from sleep-only `PrimitiveRunRecord` evidence, which may be consumed by sleep and must not be the only source of daytime historical workflow attribution.

### Legacy Snapshot Mapping

When refactoring old snapshot parts, use this mapping:

- old `recall_memories` -> `PreTurn.Recall`
- old `sensory` -> `PreTurn.Environment`
- old `plan` -> `PreTurn.TaskState`
- old app surface dump -> explicit `appid__get_state` tool result or dashboard app-status output
- old app usage / how-to-use / docs -> `AppDocs`
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
- Storing transport cursors such as `selected_chat`, `selected_thread`, or
  `opened_message` in app state.
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

If the answer leans toward a stateful capability domain with namespaced tools,
model it as an `App`.

If the answer leans toward arrived fact and resolution, model it as an `Event`.

If the answer leans toward driving the next round of processing, it usually belongs to `PendingWork`, not `Event` or `App`.

## In Short

- `App` owns a namespaced tool domain, structured state, lifecycle hooks, and
  client-visible status.
- `Event` decides what happened, whether to respond, and how to complete it.
- `PendingWork` decides what should drive the next turn.
- `Workflow` supplies SOP primitives that runtime can compose for the current task and sleep can keep corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.
