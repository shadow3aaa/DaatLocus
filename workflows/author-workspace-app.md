---
id: author-workspace-app
---

## When To Use
- A third-party app needs to be created under `~/daat-locus-workspace/apps/<app-name>/`.
- An app needs its minimal runnable package structure, prompt docs, and Lua runtime entrypoint.
- The task goal is to author an app that the runtime can recognize, load, and render, not only to fix a tiny local bug.

## Preconditions
- The app id, target capability, and main interaction surface are clear.
- The app is confirmed to be a workspace app, not a builtin Rust app.
- The app source directory under the runtime workspace is writable.
- The minimal package structure is understood: at least `app.toml`, `runtime/app.lua`, `prompt/usage.md`, and `prompt/how_to_use.md`.

## Workflow
1. Clarify the app goal, boundaries, inputs, outputs, and whether it truly should be modeled as an `App`.
2. Check whether `~/daat-locus-workspace/apps/<app-name>/` already exists and decide whether to create it or complete an existing package.
3. Create the minimal package structure and first ensure `app.toml` points to `runtime/app.lua`.
4. Implement the minimal runnable loop in `runtime/app.lua`, covering at least the state/render path and the tool call or notice/poll path actually needed by the task.
5. Write `prompt/usage.md` to explain what the app is and when it is worth focusing.
6. Write `prompt/how_to_use.md` to explain how to operate the app after focus, without mixing workflow content into app prompts.
7. Try `focus_app` to verify the app can be loaded, recognized, and rendered correctly; fix package structure or schema issues as needed.
8. When feasible, run one minimal recheck to ensure render, tool input/output, and reload behavior are not obviously broken.

## Done Criteria
- The app directory structure is complete and all minimum required files exist.
- The runtime can recognize this workspace app and produce reasonable app state and prompt information.
- The app has at least one runnable path for its core capability, not only static placeholder files.
- `usage.md` and `how_to_use.md` clearly define app semantics and operation without mixing in workflow specification content.

## Recovery
- If the app boundary is unclear, shrink to the smallest runnable capability before adding more features.
- If the Lua entrypoint becomes too large or unstable, first build a minimal state/render loop, then add tools and notices incrementally.
- If schema or load validation fails, fix package structure and entrypoint paths before changing too much logic at once.
- If the task is actually a local fix to an existing app, switch to a narrower repair workflow instead of continuing the full authoring flow.
