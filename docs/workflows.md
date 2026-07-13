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
  resolve. Workers never receive a claimed event or `finish_and_send`.
- **Skill**: reusable Markdown guidance for an agent. A skill may explain how
  to author or use a workflow, but it does not execute, schedule, or own a
  workflow run.

## Discovery and reload

The runtime scans this directory when a session starts:

```text
<execution_cwd>/workflows/*.lua
```

It creates the `workflows` directory if it is missing. `execution_cwd` is the
current session workspace, so use the workspace for the session you want to
run the workflow in rather than assuming it is the repository checkout.

The filename stem is the workflow id and must be lower snake case. For example,
`research_brief.lua` exposes the id `research_brief`. A workflow has no
manifest, sidecar metadata, or predeclared job catalog.

Invalid files do not prevent valid workflows from loading. Their load errors
appear in the workflow picker. After adding or changing a file, use the
Dashboard's `/skills reload` action; it reloads both skills and workflows.

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
to `<execution_cwd>/workflows/research_brief.lua`.

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
workflow.agent({ model, input, output, instruction, extra_tools })
workflow.await(handle)
workflow.await_all(handles)
workflow.tool({ name, input, output, run })
```

`workflow.agent(...)` returns a worker factory. `factory:run(value)` creates a
handle, `workflow.await(handle)` returns the worker's typed output, and
`workflow.await_all({ handle_a, handle_b })` returns outputs in handle order.
Create handles before waiting when the script needs to coordinate a batch.
A handle can be awaited only once.

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

- its `instruction` and typed input;
- its declared `model` (`"main"` or `"efficient"`);
- host-provided allowed App operations; and
- local tools named in `extra_tools`.

The host supplies installed App operation names, descriptions, and schemas
automatically. Workflow authors do not list or maintain App tool names. State
and review helpers such as `get_state` and `next_review`, static runtime tools
such as `read_file` and `edit_file`, `finish_and_send`, and arbitrary main-agent
tools remain unavailable to workers.

It does **not** inherit the Session main agent's Context, claimed event id,
event-completion authority, conversation history, or unrestricted tool list.
Workers must finish by returning exactly one JSON object matching their output
schema, with no Markdown. The host currently limits a worker to 16 model/tool
rounds.

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

1. Choose a lower-snake-case filename under the target session's `workflows`
   directory.
2. Declare portable input and output schemas, then call `workflow.define` once.
3. Give every worker a focused instruction, typed input/output, an explicit
   model, and only the workflow-local tools it needs.
4. Use `worker:run(...)`, `await`, and `await_all` for the control flow that
   belongs in Lua; keep Session event completion outside the workflow.
5. Treat file and shell operations as real side effects under the sandbox;
   implement idempotence or checkpoints yourself.
6. Reload through `/skills reload`, resolve any load errors in `/workflows`, and
   test with the generated form before relying on the dynamic
   `workflow__<id>` main-agent tool.

For authoring guidance intended for agents, see the built-in
[`author-lua-workflow` skill](../skills/author-lua-workflow/SKILL.md).
