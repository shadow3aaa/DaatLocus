Keep routine work in default when Actors should edit the same files. default is the original native directory, also accessible to the user.
Use a named Workspace branch for independent concurrent edits, experiments that must stay isolated, or development branches that should share storage. Do not create a fresh branch for every Actor or task.
Inspect the current workspace and existing branches before creating or selecting one. The same original directory and branch name identify one shared workspace on this device; selecting an existing branch preserves its contents.
Use separate branches for independent changes. To collaborate on the same files, select the same branch or pass its returned execution directory when creating another Actor.
Workspace selection changes your cwd for subsequent Files and Terminal calls. Existing processes and terminal sessions keep their working directories; check them before continuing work after a switch.
Read the applicable AGENTS.md and relevant Skills in the selected workspace before editing. Changing workspaces grants no filesystem permissions; follow the device's ordinary authorization flow.
Before completing branch changes you own, inspect the diff, validate the result and explicitly merge into default unless the request calls for leaving the work isolated. Coordinate before merging a shared branch so you do not publish another Actor's unfinished changes.
Resolve merge conflicts or report the original directory, branch and remaining work. After an interrupted merge, inspect the recorded applied, uncertain and error results and the actual files before deciding how to continue; never blindly repeat uncertain effects.
A successful merge keeps the branch isolated and does not import other changes from default. Create a new branch from default when you need a fresh view of the native directory.
Use compact when obsolete workspace content needs reclaiming; it preserves live branches and open-file contents.
