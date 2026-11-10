---
name: mcp-server-install
description: Install, configure or troubleshoot a standard MCP server as a device App.
---

# MCP server installation

1. Follow the server's official setup requirements for the target device, including dependencies, command or address, and authentication.
2. Inspect existing installations before adding another.
3. Prefer a server's official remote HTTP endpoint when available, especially on Android, where MCP stdio is unavailable. Install through App management. For HTTP Bearer authentication, enter the token privately in the Owner Apps page on any platform, or use `bearer_token_env` in `apps_manager__install` with only the name of an existing device environment variable. Never put a secret in chat, model tool arguments, a URL, the App name or command arguments. Do not assume OAuth is supported unless this client has a configured OAuth integration.
4. Let changes take effect at the next settled execution boundary, preserving active work.
5. Verify discovery and, when available, a relevant read-only operation.

## Troubleshooting

Check the actual executable, working directory and environment. Diagnose dependency, command resolution, connection, authentication and handshake failures from their recorded diagnostics.

Fix the dependency, executable path, environment or credentials identified by the diagnostics. Obtain approved OS privileges when the server requires them.

Check actual effects after a timeout before retrying.

## Timeouts and restarting

Inspect the installed server's current revision and timeout configuration. Connection timeouts bound individual network connections; startup timeouts cover process launch and initialization; request timeouts cover discovery and tool calls. Values are milliseconds. Startup and request deadlines may be null to wait until cancelled.

Adjust only the timeout for the failing phase, preserving the server address or command, environment and credentials. Configuration changes apply at the next settled execution boundary; they do not extend an in-flight request.

To restart, stop or settle active work, disable the installation, then enable it again and refresh its connection. Rediscover tools and verify a relevant read-only operation. Never replay a timed-out or cancelled effect without checking its actual outcome. Administrator connections require explicit reconnection under the intended OS authority.
