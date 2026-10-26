import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const source = (name: string) => readFileSync(resolve(import.meta.dirname, "../../src/prompts", name), "utf8");
/** Static instruction files only: deliberately excludes messages and tools. */
export function systemPromptRoute(url: URL): Response | null {
  const match = /^\/v1\/system-prompt(?:\/(\d+))?$/.exec(url.pathname);
  if (!match) return null;
  const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
  const role = url.searchParams.get("role") || "core";
  if (!["core", "others"].includes(role)) return json({ code: "invalid_request" }, 422);
  const files = [
    ...(role === "core" ? [{ name: "core.md", content: source("core.md").replaceAll("{{account_name}}", "林舟") }] : []),
    { name: "actors.md", content: source("actors.md") },
    { name: "os/windows-patch.md", content: source("os/windows-patch.md") },
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
