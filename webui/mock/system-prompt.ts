import { createHash } from "node:crypto";
import type { PromptRole } from "../src/lib/system-prompt";

// Independent fixtures; never read production prompts or private account files.
const sources = {
  core: [
    { name: "identity.md", content: "# 身份\n\n你是用户的日常助手。理解目标，协调工作，并把值得用户关注的进展带回来。\n\n上下文充分时直接回答；需要更多信息时先澄清。" },
    { name: "communication.md", content: "# 沟通\n\n先说明结果，使用自然、简洁的语言。\n\n保留重要的决定、阻碍和下一步，省略重复的内部协调。" },
  ],
  others: [
    { name: "task.md", content: "# 任务\n\n理解任务目标和约束，完成工作并验证结果。" },
    { name: "workflow.md", content: "# 工作方式\n\n## 开始之前\n\n阅读相关材料，确认当前状态。\n\n## 执行与验证\n\n按依赖顺序推进任务，记录证据和未完成事项。" },
    { name: "report.md", content: "# 汇报\n\n说明实际完成的工作、验证结果和仍未解决的问题。" },
  ],
};
const examples = [
  { id: "concise", name: "简洁表达.md", content: "# 简洁表达\n\n先给出结论，再补充判断所需的依据。\n\n- 一段只表达一个主要观点。\n- 使用具体的动作和熟悉的词语。\n- 简单问题直接回答，复杂问题给出足够的推理和细节。\n\n避免重复开场、冗长总结和没有实际信息的赞美。" },
  { id: "verify", name: "交付前验证.md", content: "# 交付前验证\n\n在报告完成之前，验证用户实际关心的结果。\n\n## 验证范围\n\n选择与改动相称的检查，优先覆盖失败路径和真实交互。\n\n## 汇报\n\n区分已经验证的事实、尚未验证的部分和环境限制。不要把一次编译成功当成产品可用。" },
  { id: "language", name: "中文优先.md", content: "# 中文优先\n\n默认使用简体中文与用户沟通。保留代码、文件名和必要的专业名词。\n\n用户要求其他语言时，使用用户指定的语言。" },
  { id: "project", name: "项目约定.md", content: "# 项目约定\n\n沿用已有的设计、命名和模块边界。\n\n新增抽象前先检查现有代码，避免为单个调用点增加框架。保持实现简单、容易维护。" },
  { id: "design", name: "设计审阅.md", content: "# 设计审阅\n\n从真实的阅读和操作顺序检查界面。\n\n- 检查对齐、留白、字号和信息层级。\n- 窄屏优先保证内容可读、按钮可触达。\n- 空状态、错误状态与加载状态同样需要设计。\n\n删除无意义的说明，只展示用户当前决策需要的信息。" },
];
let layers = examples.map(file => ({ ...file }));
let enabled: Record<PromptRole, string[]>;
let sequence = 1;
const listeners = new Set<() => void>();
function changed() { sequence++; for (const listener of listeners) listener(); }
export function resetInstructionLayers() {
  layers = examples.map(file => ({ ...file }));
  enabled = { core: ["concise"], others: ["verify"] };
  changed();
}
resetInstructionLayers();
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
const error = (status: number, code: string) => json({ code }, status);
const hash = (value: unknown) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const bytes = (text: string) => new TextEncoder().encode(text).length;
const tokens = (text: string) => Math.ceil(bytes(text) / 4);
const layerMeta = (layer: typeof layers[number]) => ({ id: layer.id, name: layer.name, bytes: bytes(layer.content), tokens: tokens(layer.content), revision: hash(layer), roles: (["core", "others"] as const).filter(role => enabled[role].includes(layer.id)) });
function assembly(role: PromptRole) {
  const custom = layers.filter(layer => enabled[role].includes(layer.id))
    .sort((a, b) => bytes(b.content) - bytes(a.content) || a.id.localeCompare(b.id))
    .map(layer => ({ name: layer.name, content: layer.content, layer_id: layer.id }));
  const files = [...sources[role], ...custom];
  return { files, revision: hash(files) };
}
const page = <T>(url: URL, items: T[]) => {
  const limit = Number(url.searchParams.get("limit") || 50), start = Number(url.searchParams.get("after") || 0);
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100 || !Number.isSafeInteger(start) || start < 0) return error(422, "invalid_request");
  return { items: items.slice(start, start + limit), next_cursor: start + limit < items.length ? start + limit : null };
};

/** In-memory implementation of the Owner instruction API for standalone previews. */
export async function systemPromptRoute(req: Request, url: URL): Promise<Response | null> {
  if (!url.pathname.startsWith("/v1/system-prompt") && !url.pathname.startsWith("/v1/instruction-layers")) return null;
  if (url.pathname === "/v1/system-prompt/event-stream" && req.method === "GET") {
    let cursor = Number(req.headers.get("last-event-id") || 0), cleanup = () => {};
    const encoder = new TextEncoder();
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        const send = () => { if (sequence > cursor) { cursor = sequence; controller.enqueue(encoder.encode(`id: ${cursor}\nevent: cursor\ndata: {}\n\n`)); } };
        const timer = setInterval(() => controller.enqueue(encoder.encode(": keepalive\n\n")), 5000);
        const abort = () => { cleanup(); controller.close(); };
        cleanup = () => { clearInterval(timer); listeners.delete(send); req.signal.removeEventListener("abort", abort); };
        listeners.add(send); req.signal.addEventListener("abort", abort, { once: true }); send();
      },
      cancel() { cleanup(); },
    });
    return new Response(body, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-store" } });
  }
  if (url.pathname === "/v1/instruction-layers" && req.method === "GET") {
    const search = (url.searchParams.get("q") || "").toLowerCase();
    const result = page(url, layers.filter(layer => `${layer.name}\n${layer.content}`.toLowerCase().includes(search)).sort((a, b) => bytes(b.content) - bytes(a.content) || a.id.localeCompare(b.id)).map(layerMeta));
    return result instanceof Response ? result : json(result);
  }
  const layerMatch = /^\/v1\/instruction-layers\/([^/]+)$/.exec(url.pathname);
  const layer = layerMatch ? layers.find(layer => layer.id === decodeURIComponent(layerMatch[1])) : undefined;
  if (layerMatch && !layer) return error(404, "not_found");
  if (layer && req.method === "GET") return json({ ...layerMeta(layer), content: layer.content });
  if ((url.pathname === "/v1/instruction-layers" && req.method === "POST") || (layer && req.method === "PATCH")) {
    const body = await req.json().catch(() => null);
    if (!body || typeof body.name !== "string" || typeof body.content !== "string" || !body.name.trim() || !body.content.trim() || /[/\\]/.test(body.name)) return error(422, "invalid_request");
    if (layer && body.revision !== layerMeta(layer).revision) return error(409, "conflict");
    const name = body.name.trim().endsWith(".md") ? body.name.trim() : `${body.name.trim()}.md`;
    if (layers.some(other => other.id !== layer?.id && other.name === name)) return error(409, "conflict");
    const saved = layer ?? { id: crypto.randomUUID(), name, content: body.content };
    Object.assign(saved, { name, content: body.content });
    if (!layer) layers.push(saved);
    changed();
    return json({ ...layerMeta(saved), content: saved.content }, layer ? 200 : 201);
  }
  const role = url.searchParams.get("role") || "core";
  if (role !== "core" && role !== "others") return error(422, "invalid_request");
  const { files, revision } = assembly(role);
  if (url.pathname === "/v1/system-prompt/layers" && req.method === "PUT") {
    const body = await req.json().catch(() => null);
    if (!body || !Array.isArray(body.layer_ids) || body.layer_ids.some((id: unknown) => typeof id !== "string" || !layers.some(layer => layer.id === id)) || new Set(body.layer_ids).size !== body.layer_ids.length) return error(422, "invalid_request");
    if (body.revision !== revision) return error(409, "conflict");
    enabled[role] = [...body.layer_ids];
    changed();
    return json({ revision: assembly(role).revision });
  }
  if (req.method !== "GET") return error(405, "method_not_allowed");
  if (url.pathname === "/v1/system-prompt") {
    const metadata = files.map((file, index) => ({ index, name: file.name, bytes: bytes(file.content), tokens: tokens(file.content), ...("layer_id" in file ? { layer_id: file.layer_id } : {}) }));
    const result = page(url, metadata);
    return result instanceof Response ? result : json({ ...result, revision, total_tokens: tokens(files.map(file => file.content).join("\n\n")), editable_layers: true });
  }
  const match = /^\/v1\/system-prompt\/(\d+)$/.exec(url.pathname);
  if (!match) return error(404, "not_found");
  if (url.searchParams.get("revision") !== revision) return error(409, "conflict");
  const file = files[Number(match[1])];
  if (!file) return error(404, "not_found");
  const offset = Number(url.searchParams.get("offset") || 0), data = new TextEncoder().encode(file.content);
  if (!Number.isSafeInteger(offset) || offset < 0 || offset > data.length || (offset < data.length && (data[offset] & 0xc0) === 0x80)) return error(422, "invalid_request");
  let end = Math.min(data.length, offset + 16 * 1024);
  while (end < data.length && (data[end] & 0xc0) === 0x80) end--;
  return json({ content: new TextDecoder().decode(data.slice(offset, end)), next_offset: end < data.length ? end : null });
}
