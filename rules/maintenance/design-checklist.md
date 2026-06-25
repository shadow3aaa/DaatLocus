# Design Checklist

## Design Checklist

Before adding an agent-facing interface, ask:

1. Is this an interactive surface or a structured fact that has already arrived?
2. Would a human describe it as "go operate that interface" or as "something happened; decide how to handle it"?
3. Does code already have the facts the model needs?
4. Does the action bind to a concrete object and freshness guard?
5. Will this interface induce mechanical enumeration by the model?
6. Is it compatible with the trace, workflow-run-record, and sleep evaluation loop?

If the answer leans toward a stateful capability domain with namespaced tools,
model it as an `App`.

If the answer leans toward arrived fact and resolution, model it as an `Event`.

If the answer leans toward driving the next round of processing, it usually belongs to `PendingWork`, not `Event` or `App`.

## In Short

- `App` owns a namespaced tool domain, structured state, lifecycle hooks, and
  client-visible status.
- `Event` decides what happened, whether to respond, and how to complete it.
- `PendingWork` decides what should drive the next turn.
- `Workflow` supplies SOP primitives that runtime can compose for the current task and sleep can keep corrected.
- `Plan` decides how the current task continues.
- `Memory` provides thread continuity and long-term experience.
- `Sleep` improves behavior from runtime mistakes.

When modifying these boundaries, use the code's real runtime behavior as the source of truth. Do not merge distinct concepts just for superficial uniformity.
