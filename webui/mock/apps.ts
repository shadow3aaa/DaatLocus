import type { AppDetail } from "../src/lib/apps-api";

export const mockApps: AppDetail[] = [];
let revision = 1;
export function resetApps(empty = false) {
  revision = 1;
  mockApps.splice(
    0,
    mockApps.length,
    ...(empty
      ? []
      : [
          {
            id: "mcp_00000000000000000000000000000001",
            name: "Linear",
            description: "查找问题、更新任务，跟进项目进展",
            revision: 1,
            enabled: true,
            source: {
              transport: "http" as const,
              url: "https://mcp.linear.app/mcp",
              credential_configured: true,
            },
            tool_count: 2,
            tools: [
              {
                name: "list_issues",
                description: "Search issues by project and status.",
              },
              { name: "update_issue", description: "Update a selected issue." },
            ],
          },
          {
            id: "mcp_00000000000000000000000000000002",
            name: "GitHub",
            description: "浏览仓库、查看拉取请求和问题",
            revision: 1,
            enabled: true,
            source: {
              transport: "http" as const,
              url: "https://api.githubcopilot.com/mcp",
              credential_configured: true,
            },
            tool_count: 1,
            tools: [
              {
                name: "search_repositories",
                description: "Search repositories by name or topic.",
              },
            ],
          },
          {
            id: "mcp_00000000000000000000000000000003",
            name: "Notion",
            description: "搜索笔记与文档",
            revision: 1,
            enabled: false,
            source: {
              transport: "http" as const,
              url: "https://mcp.notion.com/mcp",
              credential_configured: false,
            },
            tool_count: 1,
            tools: [{ name: "search", description: "Search documents." }],
          },
          {
            id: "mcp_00000000000000000000000000000004",
            name: "本地文件",
            description: "读取工作文件夹中的文件",
            revision: 1,
            enabled: true,
            source: { transport: "stdio" as const, command: "mcp-files" },
            tool_count: 1,
            tools: [
              {
                name: "read_file",
                description: "Read a file in the allowed workspace.",
              },
            ],
          },
        ]),
  );
}
resetApps();
export async function appsRoute(
  method: string,
  path: string,
  url: URL,
  req: Request,
): Promise<Response | null> {
  if (!path.startsWith("/v1/apps")) return null;
  const json = (value: unknown, status = 200) =>
    Response.json(value, { status });
  if (path === "/v1/apps" && method === "GET") {
    const after = url.searchParams.get("after") || "",
      search = (url.searchParams.get("search") || "").toLowerCase();
    const enabled = url.searchParams.get("enabled");
    const all = mockApps.filter(
      (app) =>
        app.id > after &&
        (enabled === null || app.enabled === (enabled === "true")) &&
        `${app.name} ${app.description}`.toLowerCase().includes(search),
    );
    const limit = Math.max(
      1,
      Math.min(64, Number(url.searchParams.get("limit")) || 24),
    );
    const items = all.slice(0, limit).map(({ tools: _, ...app }) => app);
    return json({
      items,
      next_cursor: all.length > limit ? items.at(-1)!.id : null,
      revision,
      stdio_supported: true,
    });
  }
  if (path === "/v1/apps" && method === "POST") {
    const body = await req.json();
    await new Promise((resolve) => setTimeout(resolve, 500));
    if (!body.name || !body.source || body.source.url?.includes("unavailable"))
      return json({ code: "unavailable", message: "Connection failed" }, 503);
    const source =
      body.source.transport === "http"
        ? {
            transport: "http" as const,
            url: body.source.url,
            credential_configured: Boolean(body.source.bearer_token),
          }
        : { transport: "stdio" as const, command: body.source.command };
    const app: AppDetail = {
      id: `mcp_${crypto.randomUUID().replaceAll("-", "")}`,
      name: body.name,
      description: body.description || "",
      revision: ++revision,
      enabled: true,
      source,
      tool_count: 1,
      tools: [{ name: "search", description: "Search available records." }],
    };
    mockApps.push(app);
    mockApps.sort((a, b) => a.id.localeCompare(b.id));
    return json(app);
  }
  const match = path.match(/^\/v1\/apps\/([^/]+)(\/state)?$/);
  if (!match) return null;
  const app = mockApps.find((item) => item.id === match[1]);
  if (!app) return json({ code: "not_found", message: "App not found" }, 404);
  if (method === "GET" && !match[2]) return json(app);
  if (method === "POST" && match[2]) {
    const body = await req.json();
    if (body.expected_revision !== app.revision)
      return json({ code: "conflict", message: "App changed" }, 409);
    if (typeof body.enabled !== "boolean")
      return json({ code: "invalid_request" }, 400);
    app.enabled = body.enabled;
    app.revision = ++revision;
    return json(app);
  }
  return null;
}
