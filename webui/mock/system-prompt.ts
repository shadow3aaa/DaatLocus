import { createHash } from "node:crypto";
import type { PromptRole } from "../src/lib/system-prompt";

// Independent fixtures; never read production prompts or private account files.
const sources = {
  core: [
    { name: "identity.md", content: "# Identity\n\nYou are the user's everyday assistant. Understand goals, coordinate work and bring back meaningful progress.\n\nAnswer directly when context is sufficient; clarify when more information is needed." },
    { name: "communication.md", content: "# Communication\n\nLead with the result in concise, natural language.\n\nInclude important decisions, blockers and next steps. Omit repetitive internal coordination." },
  ],
  others: [
    { name: "task.md", content: "# Task\n\nUnderstand the goal and constraints, complete the work and verify the result." },
    { name: "workflow.md", content: "# Working approach\n\n## Before starting\n\nRead relevant material and check the current state.\n\n## Execution and verification\n\nFollow dependency order and retain evidence and unfinished work." },
    { name: "report.md", content: "# Reporting\n\nDescribe completed work, verification results and unresolved issues." },
  ],
};
const examples = [
  { id: "concise", name: "concise.md", content: "# Concise writing\n\nLead with the conclusion, then provide the evidence needed to assess it.\n\n- Develop one main idea per paragraph.\n- Use concrete actions and familiar words.\n- Answer simple questions directly; give complex ones enough reasoning and detail.\n\nAvoid repetitive openings, lengthy summaries and empty praise." },
  { id: "verify", name: "verify-before-delivery.md", content: "# Verify before delivery\n\nVerify the outcome the user cares about before reporting completion.\n\n## Scope\n\nChoose checks proportional to the change, prioritizing failures and real interactions.\n\n## Report\n\nDistinguish verified facts, unchecked behavior and environment limits. A successful build alone does not prove usability." },
  { id: "language", name: "english-first.md", content: "# English first\n\nUse English by default. Preserve code, filenames and necessary technical terms.\n\nUse another language when the user requests it." },
  { id: "project", name: "project-conventions.md", content: "# Project conventions\n\nFollow existing design, naming and module boundaries.\n\nCheck existing code before adding abstractions. Avoid a framework for a single call site and keep the implementation simple to maintain." },
  { id: "design", name: "design-review.md", content: "# Design review\n\nReview the interface in its actual reading and interaction order.\n\n- Check alignment, spacing, typography and hierarchy.\n- Keep content readable and controls reachable on narrow screens.\n- Design empty, error and loading states too.\n\nRemove empty guidance and show the information needed for the current decision." },
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
