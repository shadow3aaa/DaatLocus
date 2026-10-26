import { createHash } from "node:crypto";

/** Independent display examples; production inspection reads the assembled prompt. */
export function systemPromptRoute(url: URL): Response | null {
  const match = /^\/v1\/system-prompt(?:\/(\d+))?$/.exec(url.pathname);
  if (!match) return null;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const role = url.searchParams.get("role") || "core";
  if (!["core", "others"].includes(role)) return json({ code: "invalid_request" }, 422);
  const files = role === "core" ? [
    { name: "identity.md", content: "# 身份\n\n这是一份独立的界面预览示例。\n\n帮助用户明确目标，并给出简洁、自然的回复。" },
    { name: "communication.md", content: "# 沟通方式\n\n- 先说明结果。\n- 只保留对当前任务有帮助的信息。\n- 遇到不确定的事实时，明确说明。" },
  ] : [
    { name: "task.md", content: "# 任务\n\n这是一份独立的界面预览示例。\n\n理解任务目标和约束，完成工作并验证结果。" },
    { name: "workflow.md", content: "# 工作方式\n\n## 开始之前\n\n阅读相关材料，确认当前状态。\n\n## 执行与验证\n\n按依赖顺序推进任务，记录证据和未完成事项。" },
    { name: "report.md", content: "# 汇报\n\n说明实际完成的工作、验证结果和仍未解决的问题。" },
  ];
  const encoder = new TextEncoder(), bytes = (text: string) => encoder.encode(text).length;
  const revision = createHash("sha256").update(JSON.stringify(files)).digest("hex");
  if (match[1] !== undefined) {
    if (url.searchParams.get("revision") !== revision) return json({ code: "conflict" }, 409);
    const file = files[Number(match[1])];
    if (!file) return json({ code: "not_found" }, 404);
    const offset = Number(url.searchParams.get("offset") || 0), data = encoder.encode(file.content);
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > data.length || (offset < data.length && (data[offset] & 0xc0) === 0x80)) return json({ code: "invalid_request" }, 422);
    let end = Math.min(data.length, offset + 16 * 1024);
    while (end < data.length && (data[end] & 0xc0) === 0x80) end--;
    return json({ content: new TextDecoder().decode(data.slice(offset, end)), next_offset: end < data.length ? end : null });
  }
  const limit = Number(url.searchParams.get("limit") || 50), after = Number(url.searchParams.get("after") || 0);
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100 || !Number.isSafeInteger(after) || after < 0) return json({ code: "invalid_request" }, 422);
  return json({ revision, total_tokens: Math.ceil(bytes(files.map(file => file.content).join("\n\n")) / 4), items: files.map((file, index) => ({ index, name: file.name, bytes: bytes(file.content), tokens: Math.ceil(bytes(file.content) / 4) })).slice(after, after + limit), next_cursor: after + limit < files.length ? after + limit : null });
}
