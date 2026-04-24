# Daat Locus Agent Guidelines

This document defines the agent-facing boundaries that match the current Daat Locus implementation.

The goal is not to write abstract slogans. The goal is to give future changes to `app`, `events`, `runtime_tools`, `snapshot`, `telegram_transport`, `workflow`, `memory`, `sleep`, and related modules a set of design constraints that match how the code actually works.

## Project Reality

- Daat Locus is a long-running, tool-driven agent.
- Its main loop is not a chat app where a user sends one message and the model sends one answer. It is a runtime where world state enters a snapshot, the model decides what to do, and tools mutate the world.
- External input enters the current turn primarily through `Event`, `PendingWork`, background app notices, and automatic memory recall.
- Plain assistant text is normally an explanation or intermediate record inside the runtime. It is not automatically sent to Telegram or any other external system.
- Real-world changes must happen through explicit tool calls.

## Non-Negotiables

- Telegram is not an `App`; it is a transport and event source.
- Normal event completion must not be plain text only. It must explicitly call `finish_and_send`.
- Browser and Terminal are `App`s because they represent interactive surfaces that require focus and continued operation.
- `App` and `Event` are parallel concepts. Do not collapse one into the other.
- Let the model make semantic judgments. Do not make the model perform mechanical enumeration, lookup, deduplication, or freshness checks that code can already perform.

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
3. Current world snapshot `world_snapshot`
4. Model text output or tool calls
5. Tool results written back into history
6. Another tool cycle when needed

The current snapshot covers at least:

- sensory: time and machine state
- plan: the current step-by-step plan
- workflows: the currently bound workflow and candidate workflow summaries
- events: pending events
- apps: the current foreground app and app-structured state
- memories: automatically recalled long-term memories

Therefore, when adding an agent-facing interface, first decide which layer it belongs to. Do not directly pile it into some app state.

## Core Objects

### App

An `App` is an interactive surface that only makes sense to operate after it has been focused.

The current implementation has only two built-in apps:

- `Browser`
- `Terminal`

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

Do not put self-optimizable task execution procedures into an app's supplemental instruction layer. Reusable methods across tasks should be modeled as `Workflow`, not as app-local explanatory text.

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

`Workflow` is a reusable execution specification for a class of tasks. It is not an innate model capability and not app-local supplemental instruction.

It answers these questions:

- When is this class of task worth handling through a stable reusable process?
- In what order does that process usually proceed?
- What counts as completion?
- How should it recover from failures or blockers?

`Workflow` must be split into three layers:

- `WorkflowSpec`: the workflow itself; an agent-facing specification asset
- `WorkflowBinding`: whether the current task is bound to a workflow; runtime state only
- `WorkflowRunRecord`: evidence automatically accumulated after daytime execution for sleep; the current implementation writes it directly at the work-completion boundary instead of generating it later by replaying sleep

Rules:

- `WorkflowSpec` must not carry runtime selection state or transient state such as "active".
- `WorkflowBinding` only means the current task is using a workflow. It must not write back to the workflow itself.
- `WorkflowRunRecord` is recorded by code. The model must not manually write a daytime outcome log.
- The main workflow evolution actions are `patch` and `merge`.
- v1 does not introduce `deprecate`.
- v1 does not require semantic search. Candidate workflows are shown directly in the snapshot.
- Workflow evolution must depend only on workflow-bound execution evidence. It must not depend on error demos, failure patterns, or prompt evaluation artifacts.

Do not use workflow as:

- a long-term mirror of the plan
- an implicit runtime state slot
- a set of auto-generated default templates for blind model use
- a performance ledger that the model has to maintain manually

### Memory

`Memory` has two parts:

- runtime conversation: the current thread context
- hindsight queue: long-term memory items waiting to be retained or already retained

Memory serves thread continuity and long-term experience accumulation. It does not serve mechanical state synchronization.

### Sleep / Self-Improvement

Daat Locus has an explicit self-improvement loop:

- runtime trace
- sleep
- turn compile
- compiled prompt additions

This means runtime design is not disposable. Any agent-facing interface that systematically induces bad behavior will pollute traces and workflow run evidence, and then affect later compilation.

Agent-facing interfaces should therefore be stable, explicit, and reviewable. Do not rely on vague conventions.

Sleep internals must be separated into two independent pipelines:

- `Prompt Improvement Pipeline`
- `Workflow Improvement Pipeline`

They may run in parallel during the same sleep cycle, but neither pipeline may depend on the other as input.

The `Prompt Improvement Pipeline` is responsible for:

- fixing system prompt and behavior constraints based only on runtime traces
- directly producing prompt patches and compile artifacts
- keeping failure patterns, bootstrap demos, stress cases, and similar objects as internal trace analysis artifacts only; they must not become an independent evidence layer

The `Workflow Improvement Pipeline` is responsible for:

- fixing workspace workflow specs based only on workspace `WorkflowRunRecord`
- producing workflow patches and workflow merges

Builtin workflows belong to the base capability layer:

- They are compiled from repository `workflows/*.md` by `build.rs`.
- They are read-only, not writable, and cannot be patched or merged by sleep.
- They may be selected and bound by the agent, but they are not self-optimization targets.

Explicitly forbidden:

- driving workflow patches directly from runtime reviews, error demos, or failure patterns
- using workflow merge or patch results as evidence for prompt compile

Keep these two object classes separate:

- Prompt compile changes how the model should think and decide.
- Workflow evolution changes the normal process for a class of tasks.

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

## Third-Party App Package

Future third-party `App` extensions use a source-first design. Do not copy Codex plugin or connector structure.

### Directory Placement

- Third-party app source directories are fixed under the runtime workspace: `~/daat-locus-workspace/apps/<app-name>/`.
- The current runtime workspace is resolved by `resolve_runtime_workspace_dir()`, which defaults to `~/daat-locus-workspace`.
- `app_id` is exactly the folder name `<app-name>`.
- `~/.daat-locus` is a protected runtime directory and must not store third-party app source code.
- This design exists because `~/.daat-locus` is treated as a protected runtime path inside the sandbox, while the workspace is the default editable area.

### Package Layout

Minimal directory structure:

```text
~/daat-locus-workspace/apps/<app-name>/
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

Self-optimizable workflows do not belong to the app package. They are runtime-level assets.

Rules:

- Workflows are not attached to any app by default.
- Builtin workflows live in repository root `workflows/*.md` and are compiled into the program by `build.rs`.
- Evolvable workspace workflows live in `~/daat-locus-workspace/workflows/*.md`.
- Each workflow is one Markdown file, and the filename is the workflow id.
- Workflows use a frontmatter plus Markdown body schema.
- A workflow is a reusable execution specification across tasks, not app-local instruction.
- `prompt/*.md` is for app descriptions; `workflows/*.md` is for self-optimizable execution processes. Do not mix them.
- Builtin workflows do not fall into a writable runtime directory and are not touched by optimization pipelines.

### Reload Strategy

Third-party apps should not be fully reparsed on every turn.

Recommended strategy:

- Perform one full scan of `~/daat-locus-workspace/apps` at startup.
- Scan and watch the workflow directory `~/daat-locus-workspace/workflows` separately.
- Use `notify` at runtime to watch supported directory changes.
- Map file events to the affected `<app-name>`.
- Mark only that app as dirty and reload it incrementally.
- When workflow files change, mark only the affected workflow as dirty and reload it incrementally.
- If the watcher fails or directory state becomes untrusted, fall back to one full rescan.

Do not make full parsing the normal path.

### State and Cache

v1 does not define a dedicated third-party app cache directory.

Current conclusions:

- Define only the source directory: `~/daat-locus-workspace/apps`.
- Define workflow source separately as `~/daat-locus-workspace/workflows`.
- Do not define `cache/apps`.
- Do not define `cache/workflows`.
- If the host later truly needs to persist app runtime state, use the protected runtime state system, for example `~/.daat-locus/state/apps/<app-name>/`.
- If workflow telemetry later needs host persistence, use the protected runtime state system, for example `~/.daat-locus/state/workflows/`.

Third-party apps and workflow specs are agent-editable assets, but they are not runtime state owned directly by the agent.

## Current Event Semantics

### Telegram

In the current code, Telegram is:

- input side: `TelegramTransport` polls the Bot API and registers incoming events
- state side: `TelegramTransportState` maintains known chats and the outbox
- send side: completing an event enqueues a message into the outbox, and the transport delivers it asynchronously

Telegram is not an `App`.

Reasons:

- When a new message arrives, code already knows enough structured facts.
- Normal handling is "judge and respond", not "first navigate to a chat UI and explore".
- Standard actions should bind `event_id` and explicit `chat_id`, not rely on hidden cursors.

The standard path for an approved Telegram message:

1. The transport receives the message.
2. It creates a `TelegramIncomingEvent`.
3. It registers the event in `EventStore`.
4. It enqueues `PendingWork::Event`.
5. The runtime claims the event.
6. The model judges and calls tools.
7. It ends the event with `finish_and_send`.
8. The transport delivers from the outbox and updates event state.

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

Workflow's responsibility is to provide reusable execution specifications across tasks, not to carry dynamic world state.

Current rules:

- The workflow list appears directly in the snapshot as summaries.
- The currently bound workflow is exposed to the model in fuller form.
- v1 only needs `create_workflow` and `activate_workflow`, or an equivalent bind tool.
- `create_workflow` may only create workspace workflows. It must not overwrite builtin workflows.
- Do not introduce `select_workflow` semantic search. Candidate workflows should be displayed directly in the snapshot.
- Do not introduce explicit `log_workflow_outcome`. Daytime evidence should be written automatically by code into `WorkflowRunRecord`.
- Whether to bind a workflow is driven by task complexity and reusability, not by `focus_app`.

## What Code Should Do

Code is responsible for:

- polling and receiving Telegram updates
- deduplicating events
- persisting state
- loading builtin workflows and workspace workflow specs
- writing `WorkflowRunRecord` directly at the work-completion boundary
- claiming, releasing, and requeueing pending work
- maintaining the outbox
- loading snapshots
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

## Snapshot Rules

Snapshots should provide enough information for judgment. They should not force the model into mechanical exploration.

Allowed snapshot contents:

- current foreground app
- structured app state
- currently bound workflow
- workflow summaries
- event summaries
- plan
- memory excerpts
- machine state

Disallowed snapshot contents:

- hidden multistep choreographies
- long-term selected cursors
- locating information that should be provided explicitly as tool parameters
- long, uncompressed, low-value raw logs

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
- Writing the current workflow binding back into the workflow spec itself.
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
- `Workflow` decides what reusable process should advance a class of tasks and how sleep should keep it corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.
