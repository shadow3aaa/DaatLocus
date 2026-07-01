---
name: author-workspace-app
description: Create or complete a third-party workspace app package under ~/daat-locus-workspace/apps/.
---

## When To Use
- A third-party app needs to be created under `~/daat-locus-workspace/apps/<app_id_snake_case>/`.
- An app needs its minimal runnable package structure, prompt docs, and Lua runtime entrypoint.
- The task goal is to author an app that the runtime can recognize, load, and render, not only to fix a tiny local bug.

## Preconditions
- The app id, target capability, and main interaction surface are clear.
- The app is confirmed to be a workspace app, not a builtin Rust app.
- The app source directory under the runtime workspace is writable.
- The minimal package structure is understood: at least `app.toml`, `runtime/app.lua`, and `prompt/docs.md`.

## Workflow
1. Clarify the app goal, boundaries, inputs, outputs, and whether it truly should be modeled as an `App`.
2. Check whether `~/daat-locus-workspace/apps/<app_id_snake_case>/` already exists and decide whether to create it or complete an existing package.
3. Create the minimal package structure and first ensure `app.toml` points to `runtime/app.lua`.
4. Implement the minimal runnable loop in `runtime/app.lua`, covering at least the state/render path and the tool call or notice/poll path actually needed by the task.
5. Write `prompt/docs.md` as plain markdown that explains the app capability boundary and how to operate the app's namespaced tools, without mixing workflow content into app prompts.
6. Verify the app can be loaded, recognized, and rendered through the workspace app registry or generated `appid__get_state` surface; fix package structure or schema issues as needed.
7. When feasible, run one minimal recheck to ensure render, tool input/output, and reload behavior are not obviously broken.

## Done Criteria
- The app directory structure is complete and all minimum required files exist.
- The runtime can recognize this workspace app and produce reasonable app state and prompt information.
- The app has at least one runnable path for its core capability, not only static placeholder files.
- `docs.md` clearly defines app semantics and operation without mixing in SOP content.

## Recovery
- If the app boundary is unclear, shrink to the smallest runnable capability before adding more features.
- If the Lua entrypoint becomes too large or unstable, first build a minimal state/render loop, then add tools and notices incrementally.
- If schema or load validation fails, fix package structure and entrypoint paths before changing too much logic at once.
- If the task is actually a local fix to an existing app, switch to a narrower repair workflow instead of continuing the full authoring flow.
