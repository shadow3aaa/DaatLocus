---
name: mcp-server-install
description: Install, configure or troubleshoot a standard MCP server as a device App.
---

# MCP server installation

1. Follow the server's official setup requirements for the target device, including dependencies, command or address, and authentication.
2. Inspect existing installations before adding another.
3. Install through App management. Store credentials in environment variables or dedicated credential fields; HTTP bearer credentials require the Apps page.
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
