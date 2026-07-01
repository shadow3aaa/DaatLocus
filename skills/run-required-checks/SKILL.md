---
name: run-required-checks
description: Run the smallest set of validation checks that gives meaningful confidence for the current change.
---

## When To Use
- Local changes, generated assets, or investigation results need validation before reporting, committing, or handing off.
- Project instructions or common conventions identify format, lint, build, or test commands.
- A failure must be classified as caused by the current work, pre-existing state, environment, or an external dependency.
- Downstream steps need concrete check results rather than assumptions.

## Preconditions
- The project path and intended change set are known.
- Relevant validation commands are known, inferable, or can be discovered from project files.
- Running checks is safe in the current environment and does not require interactive credentials.
- The user has not explicitly asked to skip validation.

## Workflow
1. Select the smallest set of checks that gives meaningful confidence for the current change.
2. Prefer project-prescribed format, lint, build, and targeted test commands before broad expensive suites.
3. Run checks non-interactively and wait for completion before interpreting results.
4. Capture each command, exit status, and the important success or failure details.
5. If a check fails, determine whether the failure is in scope to fix, pre-existing, environmental, or blocked.
6. Fix in-scope failures and rerun the relevant checks when practical.
7. Produce a validation artifact for reporting: commands run, outcomes, fixes made, and unresolved blockers.

## Done Criteria
- Appropriate validation commands were run, or a clear reason for not running them is recorded.
- Check results are known with enough detail to support a final report or commit decision.
- In-scope failures were fixed or explicitly documented as unresolved.
- Downstream steps can cite validation evidence without rerunning discovery.

## Recovery
- If a check is too slow or resource-heavy, run a narrower targeted check and state the limitation.
- If dependencies or network access are missing, record the environmental blocker and use available local checks.
- If a failure appears unrelated to the task, preserve evidence and avoid broad cleanup unless asked.
- If repeated retries do not change the result, stop and report the stable failure rather than looping.
