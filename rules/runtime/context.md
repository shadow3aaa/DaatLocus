# Runtime Context Rules

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
- app usage and `how_to_use` are capability docs, not app state
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
