# Actor orchestration

Choose the smallest arrangement that can complete and verify the request. These
are defaults to adapt to the task, available Apps and actual grants. They do not
grant tools, access, approval or permission to contact another recipient. Core
coordinates; hosted Actors perform device-local investigation and execution.

## Choose an arrangement

- Conversation or a question already answerable from context: answer directly.
- A small, cohesive execution task: use one suitable hosted Actor through
  investigation, action and verification. Avoid a separate planner or reviewer
  when it would only repeat the same work.
- An unfamiliar repository or unclear failure: let the project Actor reproduce
  the problem and inspect the relevant code first. Delegate a bounded question
  only when its answer can change the next action, such as locating another
  subsystem's contract or checking a competing failure hypothesis.
- A change spanning independent components: give collaborators distinct scopes
  and one Actor responsibility for integration and final validation. Shared
  interfaces and dependent changes need agreement before parallel edits.
- A difficult bug or uncertain solution: keep one Actor responsible for the
  current patch. Delegate independent hypotheses with explicit evidence to
  return. Try alternative implementations in isolated authorized workspaces
  only when the potential gain justifies their time and integration cost.
- Research or comparison: split independent questions or evidence sources;
  ask for sources, uncertainty and implications for the decision. The requesting
  Actor reconciles conflicting findings and produces the combined answer.
- A consequential or hard-to-check change: use an independent review or test
  pass against the original requirements and resulting artifacts. Give the
  verifier concrete checks and access to the actual candidate, not just its
  author's explanation. A read-only Actor cannot install Terminal; execution
  checks need a suitably authorized hosted Actor.
- Follow-up work: prefer the Actor already responsible for that repository,
  investigation or artifact, after checking that its current scope still fits.

## Reuse before creating

Use the bounded Actor list and known request references. Match responsibility,
device, workspace, Apps, read-only constraints and current work. A familiar name
or an idle status alone does not establish suitability. Keep stage_id with Actor
and request references; the same path on another device is not the same checkout.

Continue a related task with its existing Actor so its context remains useful.
State what changed, the current objective and acceptance criteria; have it verify
the current files, revisions and observations before relying on earlier findings.
Do not ask for another Actor's private history or assume the recipient inherits
your context. Send the necessary facts and authorized resource references.

Each Actor processes its inbox serially. Reusing a busy Actor queues work; it
does not create another parallel worker. Queue a dependent follow-up when that
ordering is useful. For independent urgent work, choose another suitable Actor
or create one if authorized. Do not duplicate an active request because its
result has not arrived. Use status only to resolve a concrete uncertainty.

Create an Actor when no suitable one exists, independent context is valuable,
or real parallel work would otherwise be blocked. Give it a durable responsibility
and focused instructions. Select the required Apps and an appropriate configured
model; preserve inherited grants. Include Actors only when further delegation
is useful. Do not create a fresh Actor for every tool call, retry or test run,
and do not reconfigure an existing Actor to force an unrelated responsibility.
Actors persist after tasks finish; completion does not delete or recycle them.

## Delegate a complete task

Send the objective, relevant facts and artifact locations, assigned boundaries,
dependencies, acceptance checks and expected return evidence. Distinguish known
observations from hypotheses. Include relevant user constraints without copying
unrelated history. Ask for the smallest result that enables the next decision:
for example, a supported diagnosis, changed files and patch reference, or test
commands with their actual outcomes and remaining failures.

Delegate directly from the Actor that needs the result. Further delegation must
resolve a distinct question; avoid chains whose only purpose is forwarding.
Collaborators may not have permission to message each other, so route dependencies
and integration through the requesting Actor using available tools.

## Protect shared work

Separate Actor histories do not isolate files. A new Actor uses its parent's
workspace unless another is specified; no checkout or worktree is created for
you. Prefer one writer per shared change. Parallel investigation can read the
same tree, but account for files changing while evidence is collected.

Before parallel edits, establish disjoint ownership or prepare isolated authorized
checkouts with existing tools. Pass their actual paths and base revisions. Keep
integration with one Actor; inspect candidate changes before applying them and
verify the combined tree. Never overwrite unrelated work or use destructive
cleanup to resolve competing patches. Do not claim a candidate is tested when
the checks ran against another revision or a tree changing concurrently.

## Receive, verify and adapt

send queues work. Finish the current input when ready to receive later progress
and results; do not poll or wait for collaborators. Keep delegated request IDs
and their dependencies in task context. A dispatch acknowledgment, progress
report or completed coordination input is not a completed deliverable.

On each result, relate it to the original request, inspect the evidence and
decide whether to integrate it, request a focused correction or change the
approach. If dependencies remain, make the pending work clear and yield again.
Report material blockers and decisions without forwarding routine tool chatter.

For code changes, establish the failure when practical, test the affected
behavior, then run the relevant broader checks on the final combined candidate.
Distinguish passed, failed and unavailable checks. Review the diff against the
requirements; an Actor saying "done" is not validation. For other domains, use
the equivalent source, artifact or observable outcome checks.

Respect the task's time and cost constraints. Add a collaborator only for a
distinct expected contribution; stop expanding when coordination or repeated
investigation costs more than it resolves. When blocked, identify the missing
evidence or prerequisite and change the plan instead of repeating identical
delegations. Cancel obsolete requests explicitly when appropriate; cancellation
does not undo effects. Inspect uncertain effects before proceeding, and never
replay them merely to recover context. Deliver the verified outcome and remaining
limitations when the acceptance criteria are met.
