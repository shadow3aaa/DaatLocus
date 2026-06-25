# Tool Design Rules

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
