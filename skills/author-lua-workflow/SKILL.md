---
name: author-lua-workflow
description: Author, validate, and safely test a Lua workflow that orchestrates isolated Daat Locus agent workers.
---

# Author Lua Workflow

## When To Use

Use this skill when the task calls for a reusable, executable orchestration of
one or more isolated agent workers: a custom review pipeline, a research
handoff, a typed multi-step procedure, or a side-effectful automation that
needs Lua control flow.

Do not use a Workflow just because a task needs a browser, terminal, or coding
tool:

- Use an **App** for a stateful capability domain with its own persistent tools
  and state.
- Use an **Event** for an arrived external fact that the Session main agent must
  judge and resolve.
- Use a **Skill** for reusable Markdown guidance; it never executes or owns a
  worker run.
- Use a **Workflow** for a Lua program that creates isolated workers, chooses
  branches/retries/coordination, and returns a typed result.

## File and Loading Contract

1. Create exactly one file at `<execution_cwd>/workflows/<workflow_id>.lua`.
2. Derive `workflow_id` from the lower-snake-case filename stem. Use lowercase
   letters, digits, and single underscores; begin with a letter, never end with
   an underscore, and never use `__`.
3. Put all metadata in the Lua file. Do not add a manifest, sidecar schema,
   profile catalog, predeclared job list, or task vocabulary.
4. Reload through the Dashboard `/skills reload` action after editing. It
   reloads workflows as well as skills. Inspect `/workflows` for load errors and
   use its generated form for the first manual test.

The Session main agent receives `workflow__<workflow_id>` only after that
workflow loads. Do not invent a universal JSON launcher or make the main agent
manually route arbitrary payloads.

## Start With Portable Schemas

Define reusable Lua tables for the workflow input/output and each worker or
local-tool input/output. Every schema is checked at load time and every value
is checked again at invocation time.

```lua
local Input = {
  type = "object",
  properties = {
    query = { type = "string" },
    limit = { type = "integer" },
    note = { type = { "string", "null" } },
  },
  required = { "query", "limit", "note" },
  additionalProperties = false,
}
```

Schema rules:

- The root must be an object.
- Every object must have `properties`, `required`, and
  `additionalProperties = false`.
- `required` must exactly list all declared properties. Represent a
  model-visible optional value with a required nullable type, not a missing
  field.
- Use only simple scalar types, homogeneous arrays, objects, nullable unions,
  and string enums.
- Do not use defaults, maps/dynamic keys, tuples, `$ref`, schema-valued
  `additionalProperties`, composition/conditional keywords, or provider-only
  validation constraints.

Keep schemas small and precise: they become both the `/workflows` form and the
main-agent tool contract.

## Define Workers Deliberately

Declare a worker with a focused instruction and explicit boundaries:

```lua
local reviewer = workflow.agent({
  model = "efficient", -- or "main"
  input = Input,
  output = WorkerOutput,
  instruction = [[
Review the typed input. Use only the granted tools. Return exactly one JSON
object matching the output schema, without Markdown.
]],
  capabilities = {
    "browser__browser_open_page",
    "browser__browser_snapshot",
  },
  extra_tools = { "normalize_text" },
})
```

A worker receives only its instruction, typed input, declared model, and the
explicit tools above. It does **not** receive the Session main agent's Context,
conversation history, claimed event ids, event-completion authority, or
unrestricted runtime tools. It cannot call `finish_and_send`.

Select `"main"` only when the worker needs the main model's quality. It means
an isolated worker provider call, not reuse of the Session main agent or its
history. Prefer `"efficient"` for focused helper work when it is sufficient.

Grant capabilities by exact installed App tool name, and grant the minimum
needed. State/review tools (`get_state`, `next_review`), static runtime tools
(`read_file`, `edit_file`), and arbitrary main-agent tools are not worker
capabilities.

## Use Local Tools for Explicit Effects

Use `workflow.tool` for a typed, workflow-local operation and list it in the
worker's `extra_tools` only when that worker should call it.

```lua
workflow.tool({
  name = "normalize_text",
  input = {
    type = "object",
    properties = { text = { type = "string" } },
    required = { "text" },
    additionalProperties = false,
  },
  output = {
    type = "object",
    properties = { text = { type = "string" } },
    required = { "text" },
    additionalProperties = false,
  },
  run = function(input)
    return { text = input.text:gsub("%s+", " ") }
  end,
})
```

The local tool receives validated JSON-compatible input and must return an
output matching its schema. Use it to make deterministic transforms and
workflow-owned effects explicit rather than implicitly granting broad access.

## Keep Control Flow in Lua

Call `workflow.define` exactly once. Its `run` function owns ordinary Lua
conditions, loops, retries, branches, recursion, and worker coordination.

```lua
workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local first = workflow.await(reviewer:run(input))
    if first.needs_follow_up then
      return workflow.await(reviewer:run({
        query = input.query,
        limit = input.limit,
        note = first.follow_up,
      }))
    end
    return first
  end,
})
```

Use `worker:run(value)` with a colon, not `worker.run(value)`. It creates a
handle. Await each handle once with `workflow.await(handle)`, or create a list
of handles and use `workflow.await_all(handles)` to obtain outputs in matching
order. `ctx` is host-owned and currently exposes no stable public Session API.

Return only a JSON-compatible value that matches the workflow's declared
output schema. Do not use the worker output as an unvalidated final result.

## Handle Side Effects and Cancellation

Lua workflow code may use host-provided `io.open`, `io.popen`, and `os.execute`.
These calls remain constrained by `RuntimeSandboxPolicy`, workspace roots,
writable-root policy, and process sandboxing; they are not a way to bypass host
policy.

Treat file writes and shell commands as real side effects:

1. Make them idempotent when practical.
2. Persist a checkpoint explicitly if recovery matters.
3. Check results and branch explicitly in Lua.
4. Never rely on automatic replay after interruption.

An interrupted workflow is marked `interrupted`, not restarted from its first
line. Workers must return their declared JSON result; only the Session main
agent can eventually resolve a claimed external event.

## Authoring and Test Checklist

1. Write the schemas first and check all objects are fully required with
   `additionalProperties = false`.
2. Declare local tools, then workers with focused instructions and least
   privilege capabilities.
3. Call `workflow.define` once and keep orchestration control flow inside its
   Lua `run` function.
4. Reload with `/skills reload`; fix any `/workflows` load error before running.
5. Submit a smallest valid payload through the `/workflows` form and inspect
   the typed Workflow activity result.
6. Verify the main agent sees the generated `workflow__<workflow_id>` tool only
   after successful loading.
7. Add idempotence/checkpoints before enabling file or shell side effects.

For the full user and API reference, read `docs/workflows.md` or
`docs/workflows_zh-CN.md`.
