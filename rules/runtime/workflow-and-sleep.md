# Workflow and Sleep Rules

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
