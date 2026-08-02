# Lua Workflows

Lua workflows are first-class executable orchestration programs for isolated
agent workers. One workflow is one Lua 5.4 source file that declares its own
typed input and output, worker instructions, host-provided App tools,
workflow-local tools, and ordinary control flow.

A Workflow is deliberately distinct from the other runtime objects:

- **Workflow**: an executable, side-effectful Lua orchestration program that
  coordinates isolated worker turns and returns a typed result.
- **App**: a long-lived, stateful capability domain such as Browser, Terminal,
  or Coding. The host automatically provides a worker with allowed installed App
  operations under their normal names and schemas.
- **Event**: an external fact that the Session main agent must judge and
  resolve. Workers never receive a claimed event; their same-named
  `finish_and_send` returns typed output to the workflow runner rather than
  resolving or sending an event.
- **Skill**: reusable Markdown guidance for an agent. A skill may explain how
  to author or use a workflow, but it does not execute, schedule, or own a
  workflow run.

## Discovery and reload

The runtime scans user-authored workflow files in this global directory when a
session starts:

```text
~/.daat-locus/workflows/*.lua
```

The location follows `DAAT_LOCUS_HOME` when it is set, so the effective path is
`$DAAT_LOCUS_HOME/workflows`. The runtime creates the directory if it is
missing. All sessions load the same external workflow catalog from this
directory; workflow execution still uses the invoking session's workspace and
sandbox.

The filename stem is the workflow id and must be lower snake case. For example,
`research_brief.lua` exposes the id `research_brief`. A workflow has no
manifest, sidecar metadata, or predeclared job catalog.

Invalid files do not prevent valid workflows from loading. Their load errors
appear in the workflow picker. After adding or changing a file, use the
Dashboard's `/skills reload` action; it reloads both skills and workflows.

Each release also includes three built-in workflows in the source catalog:

- `goal` repeatedly implements a workspace goal and asks an independent verifier
  whether it is complete. Use it only for goals that require changing workspace
  or external state, such as implementing, repairing, configuring, or creating
  something. Do not use it for read-only investigation, research, codebase
  exploration, analysis, or explanation.
- `investigate` accepts one or more read-only targets, starts one isolated
  investigator for each target concurrently, then starts a separate synthesis
  worker. It returns the synthesized report, each target's findings, and source
  identifiers. Investigators and the synthesizer must not make changes.
- `search` performs open-ended evidence research. A planner creates independent
  search routes, separate searcher actors run those routes concurrently, and a
  verifier either sends the remaining evidence gap back to the planner or, once
  it is satisfied, hands the verified material to an analyzer for the final
  result. The workflow intentionally has no attempt or route-count limit;
  interruption, errors, and runtime budgets still apply.

Built-in workflows are compiled into the binary and loaded from memory; they are
never copied into `~/.daat-locus/workflows`. A file in that directory with the
same id explicitly overrides the built-in definition. Delete or rename that
file and reload to use the built-in again.

On the first reload after upgrading from the older installer, an untouched
legacy `goal.lua` or `search.lua` copy is moved to
`~/.daat-locus/workflows/legacy-builtin-workflows/` so it cannot accidentally
override the current built-in. Files that do not match those legacy copies are
left in place as explicit user overrides. The migration never overwrites an
existing backup; resolve that reported conflict manually before reloading.

## Starting a workflow

Interactive users open `/workflows` in the TUI or WebUI, choose a loaded file,
and complete the generated typed input form.

The Session main agent receives one typed tool per loaded workflow:

```text
workflow__research_brief(...)
```

That tool uses the workflow's declared input schema and returns its declared
result. There is intentionally no universal
`workflow__start({ workflow_id, arbitrary_json })` tool. The main agent stays
outside the workflow graph and can use the result in its normal turn.

Each invocation ends with one of these statuses:

- `completed`: the Lua script returned an output that matches its schema.
- `failed`: loading, Lua execution, a worker, a tool call, or output validation
  failed.
- `interrupted`: the runtime was interrupted cooperatively. Interrupted runs
  are not replayed automatically.

Workflow runs also appear as typed Workflow activity rows in the dashboard.

## Minimal workflow

The following file can be copied from
[`examples/workflows/research_brief.lua`](../examples/workflows/research_brief.lua)
to `~/.daat-locus/workflows/research_brief.lua`.

```lua
local Input = {
  type = "object",
  properties = {
    topic = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "topic", "sources" },
  additionalProperties = false,
}

local ResearchOutput = {
  type = "object",
  properties = {
    summary = { type = "string" },
  },
  required = { "summary" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    summary = { type = "string" },
    source_count = { type = "integer" },
  },
  required = { "summary", "source_count" },
  additionalProperties = false,
}

workflow.tool({
  name = "count_sources",
  input = {
    type = "object",
    properties = {
      sources = { type = "array", items = { type = "string" } },
    },
    required = { "sources" },
    additionalProperties = false,
  },
  output = {
    type = "object",
    properties = { count = { type = "integer" } },
    required = { "count" },
    additionalProperties = false,
  },
  run = function(input)
    return { count = #input.sources }
  end,
})

local researcher = workflow.agent({
  role = "research",
  model = "efficient",
  input = Input,
  output = ResearchOutput,
  instruction = [[
Write a concise research brief from the supplied topic and source URLs.
Use the count_sources tool when it is useful. Return only the required JSON
object; do not add Markdown.
]],
  extra_tools = { "count_sources" },
})

workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local research = workflow.await(researcher:run(input))
    return {
      summary = research.summary,
      source_count = #input.sources,
    }
  end,
})
```

Call the worker factory with `worker:run(input)`, using Lua's colon form. The
colon supplies the factory as the first argument. `ctx` is a host-owned
placeholder; it does not expose the Session main agent, conversation history,
or a stable public API.

## Lua surface and control flow

A workflow file must call `workflow.define(...)` exactly once and provide a
`run` function. Inside that script, normal Lua functions, local state,
conditions, loops, recursion, result-driven retries, and branches are
available. The execution graph is created as the script creates and awaits
worker handles; it is not a static DAG.

The supported workflow API is intentionally small:

```lua
workflow.define({ input = InputSchema, output = OutputSchema,
  run = function(input, ctx) ... end })
workflow.agent({ role, model, input, output, instruction, extra_tools })
workflow.await(handle[, transition])
workflow.await_all(handles[, transition])
workflow.tool({ name, input, output, run })
-- every returned actor also supports actor:reset()
```

`role` is required and must be a non-empty string. It names the worker attempt
in the workflow inspector; it does not select a hidden host profile. The
inspector keeps repeated executions as separate attempts, even when they use
the same role.

`workflow.agent(...)` returns one isolated worker actor for the current
workflow invocation. `actor:run(value)` creates a handle, and subsequent
sequential runs of the same actor retain its worker-local conversation history,
isolated App instances, plan, and other worker runtime state. They do **not**
reset after a completed run. `workflow.await(handle[, transition])` returns the
worker's typed output, and `workflow.await_all({ handle_a, handle_b }
[, transition])` runs handles concurrently and returns outputs in handle order.
If any worker fails or the workflow is interrupted, the whole group stops and
the error or interruption is propagated. Each actor created by a separate
`workflow.agent(...)` call, including one with the same role, owns separate
runtime and App instances; actors from separate workflow invocations are also
isolated. Only explicitly shared external resources and workflow-local tool
effects are shared.

Call `actor:reset()` only when a fresh worker-local conversation, App instances,
plan, and runtime state are required. Reset is explicit, returns no worker
output, and yields through the workflow runner like `await`; do not reset an
actor while one of its handles is running. `transition` is optional and defaults
to `"await"`; use `"verify"`, `"revision"`, or `"retry"` when the next worker
has that relationship to the prior awaited group. Create handles before waiting
when the script needs to coordinate a batch. A handle can be awaited only once.

## Portable schemas

All schemas declared by `workflow.define`, `workflow.agent`, and
`workflow.tool` are validated when the workflow loads. Inputs passed to the
workflow, workers, local tools, and final outputs are validated again at run
time.

Use the runtime's portable model-facing schema dialect:

- The root schema must be an object.
- Every object needs `properties`, `required`, and
  `additionalProperties = false`.
- `required` must list every key in `properties`. Model-visible optional values
  are required nullable fields, for example `type = { "string", "null" }`.
- Use only `string`, `integer`, `number`, `boolean`, `object`, homogeneous
  `array`, and nullable unions. String enums are supported for finite choices.
- Do not use defaults, maps with dynamic keys, tuples, schema-valued
  `additionalProperties`, `$ref`, composition (`oneOf`, `allOf`, `anyOf`),
  conditional keywords, or unsupported validation constraints.

Declare the narrowest useful schemas. They are both the interactive form and
the main-agent tool contract.

## Workers and tools

Each worker is an isolated agent turn. It receives only:

- its required non-empty `role`, instruction, and typed input;
- its declared `model` (`"main"` or `"efficient"`);
- host-provided runtime and App tools; and
- local tools named in `extra_tools`.

The host automatically supplies `read_file`, `edit_file`, `update_plan`, and,
when the selected model supports vision, `view_image`. `update_plan` affects
only the worker-local plan. It also supplies installed App operation names,
descriptions, and schemas through isolated App instances, including generated
`appid__get_state` tools and `coding__next_review`, so workflow authors do not
list or maintain App tool names.

Workers do not receive workflow entry tools (`workflow__<id>`) or the main-agent
version of `finish_and_send`. Instead, a Worker has a same-named completion tool
whose input is the declared worker output schema; it returns the typed result to
the workflow runner and cannot resolve or send a user event. Arbitrary
main-agent-only tools also remain unavailable.

It does **not** inherit the Session main agent's Context, claimed event id,
event-completion authority, conversation history, or unrestricted tool list.
Workers must finish by calling their completion tool with exactly one JSON object
matching their output schema, with no Markdown. A worker may continue through
model/tool rounds until it completes, fails, or is interrupted; there is no
fixed worker turn-count limit.

`workflow.tool(...)` creates a named local tool. Its `run` function receives a
validated JSON-compatible value and must return a value matching its declared
output schema. Add its name to a worker's `extra_tools` list before that worker
can call it. Local tools are useful for deterministic transforms, workflow
state access, and host-controlled effects that the workflow intentionally
models as a tool.

## Files, shell commands, and interruption

Workflows are side-effectful by design. They can use the provided Lua
`io.open`, `io.popen`, and `os.execute` operations. Reads, writes, and child
processes remain subject to the Session's `RuntimeSandboxPolicy`, workspace,
writable roots, and process sandbox. Lua's standard library is otherwise kept
small; do not assume unrestricted `os`, package loading, or access to the
Session Context.

Because a script may write files or run commands, the runtime never
automatically resumes or replays an interrupted invocation. Design external
effects to be idempotent when practical, persist checkpoints explicitly when
needed, and make recovery an explicit branch in your own Lua code.

## Authoring checklist

1. Choose a lower-snake-case filename under `~/.daat-locus/workflows`.
2. Declare portable input and output schemas, then call `workflow.define` once.
3. Give every worker a required non-empty role, focused instruction, typed input/output, an explicit
   model, and only the workflow-local tools it needs.
4. Use `worker:run(...)`, `await`, and `await_all` for the control flow that
   belongs in Lua. Label verification, revision, and retry handoffs with the
   optional transition argument; keep Session event completion outside the workflow.
5. Treat file and shell operations as real side effects under the sandbox;
   implement idempotence or checkpoints yourself.
6. Reload through `/skills reload`, resolve any load errors in `/workflows`, and
   test with the generated form before relying on the dynamic
   `workflow__<id>` main-agent tool.

For authoring guidance intended for agents, see the built-in
[`author-lua-workflow` skill](../skills/author-lua-workflow/SKILL.md).
