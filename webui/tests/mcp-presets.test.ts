import { expect, test } from "bun:test";
import { mcpPresetDraft } from "../src/lib/mcp-presets";
import { installApp } from "../src/lib/apps-api";

test("hosted presets submit through the generic HTTP installer without a local runtime", async () => {
  for (const [id, endpoint, auth] of [
    ["github", "https://api.githubcopilot.com/mcp/", "token"],
    ["microsoft-learn", "https://learn.microsoft.com/api/mcp", "none"],
    ["context7", "https://mcp.context7.com/mcp", "token"],
  ]) {
    const draft = mcpPresetDraft(id)!;
    expect(draft.transport).toBe("http");
    expect(draft.url).toBe(endpoint);
    expect(draft.auth).toBe(auth);
    const requests: { path: string; body: unknown }[] = [];
    const previousFetch = globalThis.fetch;
    globalThis.fetch = (async (path: string, options: RequestInit) => {
      requests.push({ path, body: JSON.parse(options.body as string) });
      return Response.json({ id: "installed", name: draft.name });
    }) as typeof fetch;
    try {
      await installApp({
        name: draft.name,
        source: { transport: draft.transport, url: draft.url, bearer_token: draft.auth === "token" ? "synthetic-test-token" : null },
      });
    } finally {
      globalThis.fetch = previousFetch;
    }
    expect(requests).toEqual([{
      path: "/v1/apps",
      body: { name: draft.name, source: { transport: "http", url: endpoint, bearer_token: auth === "token" ? "synthetic-test-token" : null } },
    }]);
  }
});

test("choosing another preset replaces credentials and local command fields", () => {
  const previous = { ...mcpPresetDraft("github")!, token: "prior-server-secret", command: "local-server", argumentsText: "--secret", environment: [{ key: "TOKEN", value: "prior-server-secret" }] };
  const next = { ...previous, ...mcpPresetDraft("microsoft-learn")! };
  expect(next.auth).toBe("none");
  expect(next.token).toBe("");
  expect(next.command).toBe("");
  expect(next.argumentsText).toBe("");
  expect(next.environment).toEqual([]);
  expect(JSON.stringify(next)).not.toContain("prior-server-secret");
  expect(mcpPresetDraft("unknown-server")).toBeNull();
});
