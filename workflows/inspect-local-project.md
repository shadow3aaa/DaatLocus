---
id: inspect-local-project
---

## When To Use
- A local project must be understood before editing, debugging, testing, or explaining it.
- The relevant files, build system, project conventions, or validation commands are not yet known.
- A later primitive needs a project-context artifact rather than raw file listings.
- The task involves navigating source, configuration, documentation, or tests in a local repository or workspace.

## Preconditions
- The project path is known and readable.
- Local inspection tools are available through the focused app or terminal surface.
- The task goal is specific enough to guide what parts of the project should be inspected.
- Repository or workspace instructions, if present, can be read and followed.

## Workflow
1. Identify the project root and read any applicable agent or contributor instructions.
2. Inspect the top-level layout and manifest files only as far as needed to understand the project type.
3. Locate source, configuration, documentation, and tests relevant to the user's goal.
4. Read the smallest useful slices of code or documents instead of dumping large files indiscriminately.
5. Identify likely validation commands, generated-file boundaries, and style constraints.
6. Produce a concise project-context artifact: relevant files, key symbols or modules, constraints, and suggested next checks.

## Done Criteria
- The relevant project area and files are known.
- Applicable instructions and editing constraints have been considered.
- The likely build/test/format commands are identified or the reason they are unknown is recorded.
- The next primitive can act without repeating broad project discovery.

## Recovery
- If the project is too large or ambiguous, narrow inspection around the user's explicit goal or ask a focused clarification.
- If tools cannot parse the project semantically, fall back to targeted text search and file reads.
- If dependency installation or network access would be required for deeper inspection, record the blocker and continue with available local evidence.
- If generated or vendored directories dominate search results, exclude them and inspect source-owned paths first.
