import { beforeEach, expect, test } from "bun:test";
import { systemPromptRoute, resetInstructionLayers } from "../mock/system-prompt";
import { allInstructionLayers, displayPromptFiles, promptLayerWindow, subscribePromptChanges, type InstructionLayerDetail, type PromptFiles } from "../src/lib/system-prompt";

beforeEach(resetInstructionLayers);
const call = async (path: string, method = "GET", body?: unknown) => {
  const url = new URL(path, "http://127.0.0.1");
  return (await systemPromptRoute(new Request(url, { method, ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { "Content-Type": "application/json" } }) }), url))!;
};
const composition = async (role = "core") => (await (await call(`/v1/system-prompt?role=${role}`)).json()) as PromptFiles;

test("live invalidation reloads metadata after another client's edit without polling", async () => {
  const original = globalThis.fetch, controller = new AbortController();
  const cursors: number[] = [], paths: string[] = [];
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input), "http://127.0.0.1"); paths.push(url.pathname);
    return (await systemPromptRoute(new Request(url, init), url))!;
  }) as typeof fetch;
  try {
    await subscribePromptChanges({ signal: controller.signal, onCursor: async cursor => {
      cursors.push(cursor);
      if (cursors.length === 1) await call("/v1/instruction-layers", "POST", { name: "from-another-client", content: "Live instructions" });
      else controller.abort();
    } });
    expect(cursors).toHaveLength(2);
    expect(cursors[1]).toBeGreaterThan(cursors[0]);
    expect(paths).toEqual(["/v1/system-prompt/event-stream"]);
    const result = await allInstructionLayers("");
    expect(result.items.some(file => file.name === "from-another-client.md")).toBe(true);
  } finally { controller.abort(); globalThis.fetch = original; }
});

test("role membership is unordered and cannot disable built-in sources; removing a layer retains its library file", async () => {
  const initial = await composition();
  const created = await call("/v1/instruction-layers", "POST", { name: "team", content: "# Team\n\nKeep original user files." });
  expect(created.status).toBe(201);
  const layer = await created.json() as InstructionLayerDetail;
  expect((await call("/v1/system-prompt/layers?role=core", "PUT", { revision: initial.revision, layer_ids: [layer.id, "concise"] })).status).toBe(200);
  const updated = await composition();
  expect(updated.items.map(file => file.name)).toEqual(["identity.md", "communication.md", "简洁表达.md", "team.md"]);
  const content = await (await call(`/v1/system-prompt/3?revision=${updated.revision}`)).json();
  expect(content.content).toBe("# Team\n\nKeep original user files.");
  expect((await composition("others")).items.some(file => file.layer_id === layer.id)).toBe(false);
  await call("/v1/system-prompt/layers?role=core", "PUT", { revision: updated.revision, layer_ids: ["concise", layer.id] });
  expect((await composition()).revision).toBe(updated.revision);
  const removed = await call("/v1/system-prompt/layers?role=core", "PUT", { revision: updated.revision, layer_ids: [] });
  expect(removed.status).toBe(200);
  expect((await composition()).items.map(file => file.name)).toEqual(["identity.md", "communication.md"]);
  expect((await call(`/v1/instruction-layers/${layer.id}`)).status).toBe(200);
});

test("editing a reused file changes every referencing assembly and rejects stale writes", async () => {
  const core = await composition(), others = await composition("others");
  await call("/v1/system-prompt/layers?role=others", "PUT", { revision: others.revision, layer_ids: ["concise"] });
  const shared = await (await call("/v1/instruction-layers/concise")).json() as InstructionLayerDetail;
  expect(shared.roles).toEqual(["core", "others"]);
  expect((await call("/v1/instruction-layers/concise", "PATCH", { revision: shared.revision, name: shared.name, content: "# Updated\n\nNew instruction." })).status).toBe(200);
  expect((await composition()).revision).not.toBe(core.revision);
  expect((await call(`/v1/system-prompt/0?revision=${core.revision}`)).status).toBe(409);
  expect((await call("/v1/instruction-layers/concise", "PATCH", { revision: shared.revision, name: shared.name, content: "Lost update" })).status).toBe(409);
  expect((await call("/v1/system-prompt/layers?role=core", "PUT", { revision: core.revision, layer_ids: [] })).status).toBe(409);
  const result = await composition("others"), index = result.items.find(file => file.layer_id === "concise")!.index;
  expect((await (await call(`/v1/system-prompt/${index}?role=others&revision=${result.revision}`)).json()).content).toBe("# Updated\n\nNew instruction.");
});

test("unknown, duplicate and built-in IDs cannot modify membership; library queries remain bounded", async () => {
  const initial = await composition();
  for (const layer_ids of [["missing"], ["identity.md"], ["concise", "concise"]])
    expect((await call("/v1/system-prompt/layers?role=core", "PUT", { revision: initial.revision, layer_ids })).status).toBe(422);
  expect((await composition()).revision).toBe(initial.revision);
  expect((await call("/v1/instruction-layers", "POST", { name: "../outside.md", content: "No path input" })).status).toBe(422);
  const first = await (await call("/v1/instruction-layers?limit=2")).json();
  const second = await (await call(`/v1/instruction-layers?limit=2&after=${first.next_cursor}`)).json();
  expect(first.items).toHaveLength(2); expect(second.items).toHaveLength(2);
  expect(second.items[0].id).not.toBe(first.items[0].id);
  expect(first.items[0].content).toBeUndefined();
});

test("display keeps built-ins above size-sorted user layers and introduces a newly added layer at the bottom", () => {
  const file = (name: string, bytes: number, layer_id?: string) => ({ name, bytes, layer_id, index: 0, tokens: bytes });
  const input = [file("small", 2, "small"), file("system-one", 1), file("large", 50, "large"), file("system-two", 10), file("medium", 8, "medium")];
  expect(displayPromptFiles(input).map(file => file.name)).toEqual(["system-one", "system-two", "large", "medium", "small"]);
  expect(displayPromptFiles(input, "large").map(file => file.name)).toEqual(["system-one", "system-two", "medium", "small", "large"]);
});

test("the library window stays bounded while cycling in either direction", () => {
  const files = ["a", "b", "c", "d", "e"];
  expect(promptLayerWindow(files, 4)).toEqual(["e", "a", "b", "c"]);
  expect(promptLayerWindow(files, 5)).toEqual(["a", "b", "c", "d"]);
  expect(promptLayerWindow(files, -1)).toEqual(["e", "a", "b", "c"]);
  expect(promptLayerWindow(["one"], 10)).toEqual(["one"]);
  expect(promptLayerWindow([], 2)).toEqual([]);
  expect(promptLayerWindow(files, 2, files.length)).toEqual(["c", "d", "e", "a", "b"]);
});

test("revealing a new file or wrapping backwards can reach files beyond the first metadata page", async () => {
  for (let index = 0; index < 52; index++) await call("/v1/instruction-layers", "POST", { name: `file-${index}`, content: `Instruction ${index}` });
  const original = globalThis.fetch;
  const requests: string[] = [];
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = new URL(String(input), "http://127.0.0.1"); requests.push(url.search);
    return (await systemPromptRoute(new Request(url), url))!;
  }) as typeof fetch;
  try {
    const result = await allInstructionLayers("");
    expect(result.items).toHaveLength(57);
    expect(result.items.some(file => file.name === "file-51.md")).toBe(true);
    expect(new Set(result.items.map(file => file.id)).size).toBe(57);
    expect(result.next_cursor).toBeNull();
    expect(requests).toHaveLength(2);
    expect(requests.every(query => query.includes("limit=50"))).toBe(true);
  } finally { globalThis.fetch = original; }
});
