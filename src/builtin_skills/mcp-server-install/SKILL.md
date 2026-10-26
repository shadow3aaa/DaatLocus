---
name: mcp-server-install
description: Add, enable or debug an external MCP server as a Daat Locus device App through the Apps page or the apps HTTP API, including stdio command resolution, environment variables, and registry troubleshooting.
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
  arguments one per line, and environment key/value rows. This is the graphical
  path for stdio servers that need environment variables.
- `POST /v1/apps`: body `{name, description, source}`. This is the only
  programmatic path that accepts `environment`. It requires the Owner's session
  cookie and a trusted Origin; an `Authorization` header is rejected, `name` is
  limited to 100 characters and `description` to 500.
- `apps_manager__install`: fastest inside an Actor, but it cannot set
  environment variables — it rejects an `environment` field instead of ignoring
  it, and stores the source with an empty one. Use it only for servers that need
  no environment. `apps_manager__list`, `inspect` and `refresh` only read.

Do not edit the registry file by hand; see "Why not hand-edit" below.

## Payload

```json
{
  "name": "example-server",
  "description": "Optional summary",
  "source": {
    "transport": "stdio",
    "command": "npx",
    "arguments": ["-y", "example-mcp-server@1.2.3"],
    "environment": { "EXAMPLE_API_TOKEN": "value" }
  }
}
```

A source is one of two transports; there is no SSE and no WebSocket transport.

- `{"transport":"http","url":"https://host/path","bearer_token":"secret"|null}`.
  HTTPS is required; plain HTTP is accepted only for `localhost`, `127.0.0.1` and
  `[::1]`. Redirects are refused, and the URL must carry no fragment.
- `{"transport":"stdio","command":"name-or-path","arguments":["..."],"environment":{"KEY":"value"}}`.
  stdio is unavailable on Android.

Both variants reject unknown fields, so a misspelled key fails the request. Keep
credentials in `environment` or `bearer_token`, never in `name`, `arguments` or
a URL. Public reads omit the token, the environment, the arguments and URL query
strings.

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

The field is `environment`, a flat string-to-string map, not `env`. It is added
to the inherited daemon environment rather than replacing it. Setting `PATH`
therefore narrows the search path for a bare `command` to exactly that value,
which breaks a server that relied on the daemon's own `PATH`.

Keys must be valid variable names, non-empty, and must not start with
`DAAT_LOCUS`; values must not contain a NUL byte. A rejected map is reported as
`Invalid MCP environment`.

## Make it take effect

Changes made through the Apps page, `POST /v1/apps` or `apps_manager` are
written atomically and scheduled. The Actor picks them up at the start of the
next model loop after the current tool group settles: no daemon restart, no
Actor restart, and the conversation context is preserved. `enable` and `close`
take an `expected_revision` from `list` or `inspect`; disabling blocks new calls
and lets started calls finish.

Changes made by editing the registry file need a daemon restart, because nothing
watches it.

## Tool names and schemas

A server tool named `read` appears to the model as `t` plus the first ten hex
characters of the SHA-256 of that name, and its description is prefixed with the
App name and the original tool name. Read the mapping and the original JSON
Schema with `apps_manager__inspect`, passing `id` and `tool`, or with
`GET /v1/apps/{id}?tool=…`.

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
`~/.daat-locus`; `RUST_LOG` sets the filter. Then query `GET /v1/apps` for
`revision`, `enabled`, `tool_count` and `stdio_supported`, and
`GET /v1/apps/{id}` for the tool list.

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
so `apps_manager__install` cannot add it. Install it from the Owner Apps page or
with `POST /v1/apps`:

```json
{
  "name": "rea",
  "description": "Reverse engineering analysis",
  "source": {
    "transport": "stdio",
    "command": "npx",
    "arguments": ["-y", "rea-agents@6.8.0", "mcp"],
    "environment": {
      "GHIDRA_INSTALL_DIR": "C:\\tools\\ghidra",
      "REA_ANALYSIS_PROVIDER": "ghidra",
      "REA_JADX_MCP_JAR": "C:\\tools\\jadx-headless-mcp.jar",
      "REA_WABT_BIN_DIRECTORY": "C:\\tools\\wabt\\bin"
    }
  }
}
```

Use the values that server's own installation produced; the environment names
and version constraints are part of that server's contract, not this App schema.
Then confirm the result with `GET /v1/apps`, inspect the discovered tools, and
invoke one to prove the server answers.
