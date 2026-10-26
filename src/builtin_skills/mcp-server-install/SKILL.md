---
name: mcp-server-install
description: Add, enable or debug an external MCP server as a Daat Locus device App through the Apps page or the apps_manager tools, including stdio command resolution on Windows, environment variables, and registry troubleshooting.
---

# MCP Server Install

Daat Locus installs an external MCP server as one device-wide App instance. It
does not read a standard `mcpServers` document: the registry stores the server
definition and the tool catalog discovered at install time. Every enabled MCP
App is offered to every non-Core Actor on the device. Core keeps only the Actors
App and never receives MCP Apps, so add or inspect servers from another Actor or
from the Owner Apps page.

## Choose the entry point

Prefer the shortest path that can express what the server needs.

- Owner Apps page (`/apps` in the WebUI): name, transport, command or URL,
  arguments one per line, environment key/value rows, and a bearer token for an
  HTTP server that needs one.
- `apps_manager__install`, from any non-Core Actor: one call for command,
  arguments and `environment`. `list`, `inspect`, `enable`, `close` and
  `refresh` manage the result afterwards. An HTTP server that needs a bearer
  token cannot be installed this way; the tool always stores no token.

Do not edit the registry file by hand; see "Why not hand-edit" below.

## Payload

`apps_manager__install` takes one flat object. `description`, `url`, `command`
and `environment` may be null or empty, but every field must be present:

```json
{
  "name": "example-server",
  "description": "Optional summary",
  "transport": "stdio",
  "url": null,
  "command": "npx",
  "arguments": ["-y", "example-mcp-server@1.2.3"],
  "environment": ["EXAMPLE_API_TOKEN=value"]
}
```

`transport` is `http` or `stdio`. There is no SSE and no WebSocket transport,
and the two are mutually exclusive: a request that mixes their fields is refused
instead of partially applied.

- `http` needs `url`. HTTPS is required; plain HTTP is accepted only for
  `localhost`, `127.0.0.1` and `[::1]`. Redirects are refused, and the URL must
  carry no fragment.
- `stdio` needs `command`, and accepts `arguments` and `environment`. stdio is
  unavailable on Android.

Unknown fields are refused, so a misspelled key fails the call instead of being
ignored. Keep credentials in `environment`, never in `name` or `arguments`.

## stdio on Windows

- Resolution order for `command`: an absolute path is used as given; a relative
  path containing a separator resolves against the Actor's workspace; a bare
  name is searched on the `environment` `PATH`, otherwise the daemon's `PATH`,
  and tried as `.exe`, `.com`, `.cmd`, `.bat`. Write `npx`, not `npx.cmd`.
- `.cmd` and `.bat` targets are launched through `powershell -NoProfile
  -EncodedCommand`, and every argument is quoted as a PowerShell literal; an
  argument is never shell code. Do not add `cmd /c` or a shell option.
- The working directory is the Actor's workspace, not the daemon's directory.
- The child's stderr is discarded. A server that dies after launch surfaces only
  as a fixed message such as `MCP process handshake failed`. Reproduce with the
  same command line, arguments, working directory and environment outside Daat
  Locus to read the real error.

## Sandbox start failures

A stdio server starts inside the device sandbox, so its executable must be
runnable at that sandboxed process's integrity level. On Windows the sandbox
runs below the user's ordinary directories, which makes this the most common
start failure: a path that is completely correct still reports `MCP process
could not start inside the required sandbox` when the executable lives in a
higher-integrity directory, as a version-managed Node or tools directory
typically does.

Treat it as an environment problem, not a configuration one. Run the same
command in the sandboxed terminal; a refusal there confirms the cause. Then use
an executable location the sandbox may run, or invoke the installed App's tools
through `sudo__execute`, which reconnects that server outside the sandbox under
the elevated policy. Do not keep rewriting the command or the arguments while
the launch itself is being refused.

## Environment variables

The field is `environment`, a list of `KEY=value` strings. Each entry is split
on its first `=`, so a value may contain `=` and is passed through unchanged.
Blank entries are ignored and a repeated name is refused. The entries are added
to the inherited daemon environment rather than replacing it, so setting `PATH`
narrows the search path for a bare `command` to exactly that value and breaks a
server that relied on the daemon's own `PATH`.

Names must be valid variable names, non-empty, and must not start with
`DAAT_LOCUS`; values must not contain a NUL byte. A rejected list is reported as
`Invalid MCP environment`, and an entry with no `=` as `MCP environment entries
must be KEY=value`.

## Make it take effect

Changes made through the Apps page or `apps_manager` are written atomically and
scheduled. The Actor picks them up at the start of the next model loop after the
current tool group settles: no daemon restart, no Actor restart, and the
conversation context is preserved. `enable` and `close` take an
`expected_revision` from `list` or `inspect`; disabling blocks new calls and
lets started calls finish.

Changes made by editing the registry file need a daemon restart, because nothing
watches it.

## Tool names and schemas

A server tool named `read` appears to the model as `t` plus the first ten hex
characters of the SHA-256 of that name, and its description is prefixed with the
App name and the original tool name. Read the mapping and the original JSON
Schema with `apps_manager__inspect`, passing `id` and `tool`.

A portable schema is exposed directly. Any other schema is wrapped in a
portable `{"arguments_json":"{…}"}` object, and Daat Locus validates the decoded
object against the original schema before calling the server. Server-side tool
changes are rediscovered at the next boundary; a changed definition cannot be
invoked through the previous mapping.

## Sandbox, review and failure

stdio servers run under the Actor's device sandbox and record native denials;
HTTP uses a dedicated client with no browser cookies. Only `sudo__execute` and
`sudo__execute_as_admin` carry automatic review: an ordinary MCP tool call is
not reviewed separately, it inherits the sandbox and nothing more. Do not expect
a review prompt, and do not treat a server's own descriptions or read-only hints
as authorization. Reach outside the sandbox by expanding it explicitly; that
restarts the native session under the new policy and never replays an effect.
Remote read-only authority cannot install, enable, close or invoke.

Timeouts are 30 s to start the process, 20 s for the handshake, 20 s for tool
discovery and 120 s per call. A cold `npx` still downloading the package can
exceed the start and handshake limits, so install the package first when a
first launch fails that way. Nothing is retried automatically, and a timeout is
reported as uncertain effects: inspect the real state before calling again.

## Troubleshoot

Read `<root>/logs/daat-locus.log`, where `<root>` is `$DAAT_LOCUS_HOME` or
`~/.daat-locus`; `RUST_LOG` sets the filter. Then read `apps_manager__list` for
the revision and the enabled state, and `apps_manager__inspect` for the
discovered tools.

- `MCP executable was not found`: the command did not resolve; check the `PATH`
  that this installation actually passes.
- `MCP process could not start inside the required sandbox`: the sandbox refused
  the launch, usually a path or integrity-level problem rather than a bad command.
- `MCP process handshake failed`, `MCP handshake timed out`: the process started
  but did not speak MCP; stderr is hidden, so run it manually.
- `MCP connection failed; check the address and authentication`: HTTP reachability,
  TLS or bearer token.
- `MCP requires HTTPS except on loopback`: non-HTTPS URL on a non-local host.
- `App changed; inspect it again`, `App changed or was closed; refresh Apps`: the
  revision moved; re-read it before changing or calling.
- A daemon that no longer starts after a manual edit means the registry failed to
  parse; restore the file before starting again.

## Why not hand-edit

The registry lives at `<root>/actors/apps.json`, which is the `actors` subdirectory
and not the per-Actor `actors/apps` directory. Its state is
`{"revision": n, "installations": {"mcp_<32 lowercase hex>": {…}}}`, with both
top-level fields required. Each installation also carries the discovered `tools`
array, and a tool entry needs the exact `local_name` mapping described above.

Hand-writing it is therefore impractical as well as unsupported: without a
catalog the App exposes no tools, and no session is opened to discover them
because there is nothing to call. Install through an entry point above and let
Daat Locus fill the catalog.

## Example: a stdio server that needs environment

REA is an npm MCP server that reads its analysis settings from the environment,
so its App needs `environment` entries:

```json
{
  "name": "rea",
  "description": "Reverse engineering analysis",
  "transport": "stdio",
  "url": null,
  "command": "npx",
  "arguments": ["-y", "rea-agents@6.8.0", "mcp"],
  "environment": [
    "GHIDRA_INSTALL_DIR=C:\\tools\\ghidra",
    "REA_ANALYSIS_PROVIDER=ghidra",
    "REA_JADX_MCP_JAR=C:\\tools\\jadx-headless-mcp.jar",
    "REA_WABT_BIN_DIRECTORY=C:\\tools\\wabt\\bin"
  ]
}
```

Use the values that server's own installation produced; the variable names and
version constraints are part of that server's contract, not this App schema.
Then call `apps_manager__list` to confirm the entry, `inspect` its tools, and
invoke one to prove the server answers.
