# TUI Dashboard Rules

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
