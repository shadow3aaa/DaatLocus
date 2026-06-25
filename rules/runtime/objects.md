# Runtime Object Rules

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

Every `App` must expose three separate layers:

- `state`: current structured domain facts, returned by the app's generated
  `appid__get_state` tool and rendered in TUI/WebUI app-status surfaces
- `usage`: what the app is for and when it is worth using
- `how_to_use`: how to operate the app's tools correctly

Do not mix these layers.

- `state` is not an operating manual.
- `usage` is not a full tutorial.
- `how_to_use` is not world state.
- Project or workspace instructions such as `AGENTS.md` are instruction context,
  not app state.

In code, keep this separation visible through `App::render_state`, `usage()`,
`how_to_use()`, and the generated `appid__get_state` surface. Do not put
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

### Memory

`Memory` has two parts:

- runtime conversation: the current thread context
- hindsight queue: long-term memory items waiting to be retained or already retained

Memory serves thread continuity and long-term experience accumulation. It does not serve mechanical state synchronization.
