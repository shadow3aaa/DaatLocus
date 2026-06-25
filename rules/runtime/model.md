# Runtime Model Rules

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
- `preturn_context`: memory recall, sensory state, plan, bound workflow state, and project instruction context

Therefore, when adding an agent-facing interface, first decide which layer it belongs to. Do not directly pile prompt instructions, routing catalogs, or durable task state into app-local state.
