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

`Workflow` is the self-optimizable SOP layer. Persisted workflow specs are SOP primitives, not composite task templates, not innate model capability, and not app-local supplemental instruction.

A persisted `WorkflowSpec` answers these questions:

- What stable procedure can be reused as a primitive?
- What inputs, artifacts, capabilities, or preconditions does the primitive need?
- What outputs, artifacts, completion evidence, or handoff does it produce?
- How should this primitive recover from failures or blockers?
- What boundary prevents it from absorbing neighboring work?

The runtime may temporarily compose several primitives into an execution graph for one task. That graph is runtime state, similar to a structured plan with explicit artifact handoff between steps. It must not be written back as a new persisted workflow just because it succeeded once.

`Workflow` must be split into three layers:

- `WorkflowSpec`: a persisted SOP primitive asset exposed to the agent through a concise name, capability summary, and thin input/output contract
- `WorkflowBinding` / runtime composition: which primitive or temporary primitive graph the current task is using; runtime state only
- `WorkflowRunRecord`: evidence automatically accumulated after daytime execution for sleep; the current implementation writes it directly at the work-completion boundary instead of generating it later by replaying sleep

Rules:

- All persisted workflow specs are primitives. Do not add a `kind` field just to distinguish primitive versus composite workflows; composite workflows should not be persisted in the workflow library.
- `WorkflowSpec` must not carry runtime selection state or transient state such as "active".
- `WorkflowBinding` only means the current task is using a primitive or a temporary composition. It must not write back to the workflow itself.
- `WorkflowRunRecord` is recorded by code. The model must not manually write a daytime outcome log.
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

- fixing workspace SOP primitive workflow specs based only on workspace `WorkflowRunRecord`
- producing primitive workflow patches and workflow merges

Builtin workflows belong to the base capability layer:

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

### Coding (Planned)

`Coding` is the interface for semantic code operations powered by scope-engine.

It is an `App` because:

- project state (open project, LSP connections) persists across tool calls
- symbol lookups and propagation analysis require a focused project context
- the model needs to see propagation status to decide whether to continue editing

**State rendering design:**

Coding app must render its key state into `AppStateRender` so that:

1. **Turn re-entry sees critical state immediately** — after context compression or turn interruption, the model can read the current project, LSP status, and pending propagation events from `<preturn_context>` or `<afterclaim_context>` without relying on conversation history.
2. **Tool return values carry immediate feedback** — `edit_code`, `search_code`, and `read_code` return `PropagationResult` lists in their tool result so the model sees impact scope mid-turn.
3. **Notice is NOT for propagation review** — Coding app uses `notice_reason()` only for background events like "LSP server crashed" or "project index ready". Propagation review is handled through tool return values and state rendering, not through notice-triggered turn interrupts.

Operational constraints:

- Coding tools (`read_code`, `edit_code`, `search_code`, `find_references`) must go through the Coding app, requiring `focus_app("coding")` first.
- `apply_patch` remains a Terminal app tool for raw file edits; Coding app handles semantic (selector-based) operations.
- Coding app `render_state()` must include: project_root, open_languages, lsp_status, propagation_pending_count, and up to N recent propagation events.
- LSP process lifecycle (start, crash recovery, shutdown) belongs to Coding app internals, not to tool return values.

### App Composition

An app may declare that it *contains* other apps, making their tools available when the composing app is focused.

When `Coding` is focused, the tool scope includes:

- Coding's own tools: `read_code`, `edit_code`, `search_code`, `find_references`
- Terminal's tools: `terminal_exec`, `terminal_write_stdin`, `terminal_terminate`, `apply_patch`
- Browser's tools: **not** available unless the model explicitly focuses Browser

Implementation: each `App` can optionally expose `fn composed_apps() -> Vec<AppId>`. The runtime tool-scope check traverses this list so that focused-app restriction plus composition gives the correct tool availability.

Rationale:

- "I am coding" inherently includes "I need to run commands and edit raw files."
- Forcing `focus_app("terminal")` back-and-forth would be an unnecessary interruption.
- Composition preserves the attention model: `focus_app("coding")` means "I am in coding mode," and all tools needed for that mode are available.

### SCOPE Capability Gap and Propagation Bridge

SCOPE (scope-engine) provides semantic code operations, but its modification capability is **not complete**:

| Capability | SCOPE Status | Gap |
|---|---|---|
| Symbol location | ✅ tree-sitter `find_containing_symbol` | — |
| Read code | ✅ `read_code` (selector-based) | — |
| Search code | ✅ `search_code` (ripgrep + symbol) | — |
| Edit code | ⚠️ `edit_code` (SCOPE Diff) | Selector-based Add/Delete/Update; no semantic refactoring |
| Rename | ❌ | Not implemented |
| Extract/inline | ❌ | Not implemented |
| File-level structure | ❌ | Cannot add imports, move files |
| New files | ⚠️ | `edit_code` can create files but no template support |
| Config files | ❌ | SCOPE does not understand .toml/.yaml/.json config |

**Propagation bridge for `apply_patch`:**

When Coding is focused and `apply_patch` is used on a source-code file (by extension: `.rs`, `.py`, `.go`, `.ts`, `.js`, `.java`, `.c`, `.cpp`, `.rb`, `.php`):

1. The patch executes normally as a raw file edit.
2. **Then** Coding app automatically runs tree-sitter symbol analysis on the modified file to find affected symbols.
3. The propagation results are appended to the tool result in the same format as `edit_code` returns.
4. For non-source-code files (`.toml`, `.yaml`, `.md`, `.json`, `.sh`, etc.), `apply_patch` works normally with no propagation analysis.

This ensures that even when the model bypasses semantic editing, propagation tracking does not go silent.

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
- Builtin workflows live in repository root `workflows/*.md` and are compiled into the program by `build.rs`.
- Evolvable workspace workflows live in `~/daat-locus-workspace/workflows/*.md`.
- Each workflow is one Markdown file, and the filename is the workflow id.
- Workflows use a frontmatter plus Markdown body schema.
- A workflow file should describe one reusable SOP primitive, not a composite task class.
- Composite task execution is a temporary runtime graph assembled from primitives; it is not a workflow asset to save by default.
- `prompt/*.md` is for app descriptions; `workflows/*.md` is for self-optimizable execution processes. Do not mix them.
- Builtin workflows do not fall into a writable runtime directory and are not touched by optimization pipelines.

### Reload Strategy

Third-party apps should not be fully reparsed on every turn.

Recommended strategy:

- Perform one full scan of `~/daat-locus-workspace/apps` at startup.
- Scan and watch the workflow directory `~/daat-locus-workspace/workflows` separately.
- Use `notify` at runtime to watch supported directory changes.
- Map file events to the affected `<app_id_snake_case>`.
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
- If the host later truly needs to persist app runtime state, use the protected runtime state system, for example `~/.daat-locus/state/apps/<app_id_snake_case>/`.
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

Workflow's responsibility is to expose a reusable SOP primitive library for task-time composition, not to carry dynamic world state.

Current rules:

- The workflow primitive routing catalog appears directly in `afterclaim_context` as a full `primitive_ids` vocabulary plus `relevant_primitives` details for the top task-relevant primitives.
- `primitive_ids` should include every loaded primitive ID so runtime composition can see the available vocabulary; `relevant_primitives` entries should emphasize primitive name, capability, inputs, outputs, and constraints. `when_to_use` is supporting metadata, not the main interface.
- The currently bound workflow or temporary primitive graph is exposed to the model in fuller form.
- v1 only needs `create_workflow` and `activate_workflow`, or an equivalent bind tool.
- `create_workflow` may only create workspace workflows. It must not overwrite builtin workflows.
- Do not make the model perform workflow semantic search or browse an expanded lexicographic workflow dump before continuing. The full ID vocabulary and relevant primitive details should be displayed directly in `afterclaim_context`.
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
- Preserve turn boundaries for operations that change the next context view, such as `activate_workflow`, `focus_app`, `put_away_app`, `update_workflow`, and workspace app dynamic tools that return a turn boundary.
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

This metadata should enter runtime history and be compressible. It is separate from sleep-only `WorkflowRunRecord` evidence, which may be consumed by sleep and must not be the only source of daytime historical workflow attribution.

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
- `Workflow` supplies SOP primitives that runtime can compose for the current task and sleep can keep corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.
