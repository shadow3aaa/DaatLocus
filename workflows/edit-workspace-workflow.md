---
id: edit-workspace-workflow
---

## When To Use
- The user asks to change, clean up, replace, tighten, relax, or otherwise maintain an existing workflow.
- The user asks to modify the workflow for a past, existing, or previously discussed task class.
- The requested change concerns the reusable workflow behind that task class, even if the user does not explicitly say "workflow".
- The user gives a follow-up instruction that refers to the previously discussed or recently used task process rather than only the current one-off result.
- The task is about editing a workflow specification, not executing the target workflow itself.

## Preconditions
- The user intent names, strongly implies, or is contextually attached to a target workflow.
- The target workflow corresponds to a past, existing, or previously discussed task class.
- The target workflow is a workspace workflow, not a read-only builtin workflow.
- The edit is intended to change reusable future behavior, not only to complete the current one-off task.

## Workflow
1. Identify the target workflow from `afterclaim_context` workflow routing summaries, user wording, recent conversation context, the currently discussed workflow, and workflow ids.
2. Activate this meta workflow as the current workflow; do not activate the target workflow being edited.
3. Call `read_workflow` for the target workflow before deciding edits.
4. Map the user's intent to the correct spec sections: `When To Use`, `Preconditions`, `Workflow`, `Done Criteria`, and `Recovery`.
5. Produce a complete replacement spec, preserving still-valid existing items and removing duplicates, contradictions, stale task-specific details, and sleep-patch bloat.
6. Call `update_workflow` with all sections of the cleaned replacement spec.
7. Review the tool result and send a concise final reply describing which workflow changed and what behavior changed.

## Done Criteria
- The target workflow was identified by id.
- The complete current target workflow was read before editing.
- The edit changed only a workspace workflow.
- The final workflow remains reusable for a task class rather than recording a one-off execution log.
- The replacement spec is concise, internally consistent, and free of obvious duplicate rules.
- The user receives a concise summary of the workflow change.

## Recovery
- If the target workflow is ambiguous after using conversation context and workflow summaries, ask the user to identify the task class or workflow.
- If the target workflow is builtin, explain that builtin workflows are read-only and offer to create or edit a workspace workflow instead.
- If the requested change conflicts with existing workflow behavior, replace the conflicting rule instead of appending a contradictory rule.
- If a requested edit would make the workflow too narrow for reuse, ask for confirmation or keep the reusable boundary explicit.
