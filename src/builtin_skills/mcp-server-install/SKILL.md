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
