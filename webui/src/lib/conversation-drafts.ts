import type { Attachment } from "./actor-api";
export type StoredDraft = { text: string; attachments: Attachment[]; pending?: { content: string; attachments: string[]; key: string } };
const prefix = "daat-locus.draft.v2:";
const lifetime = 12 * 60 * 60 * 1000;
function storage(): Storage | undefined { try { return typeof localStorage === "undefined" ? undefined : localStorage; } catch { return undefined; } }
const opaque = (id: unknown): id is string => typeof id === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(id);
export function loadDraft(key: string, actorId: string, store = storage(), now = Date.now()): StoredDraft | undefined {
  if (!store) return;
  const name = prefix + key;
  try {
    const record = JSON.parse(store.getItem(name) || "null");
    if (!record || record.version !== 2 || !Number.isFinite(record.expires_at) || record.expires_at <= now || record.actor_id !== actorId || record.subject_key !== key || typeof record.text !== "string" || record.text.length > 64 * 1024 || !Array.isArray(record.attachments) || record.attachments.length > 4) { store.removeItem(name); return; }
    const attachments: Attachment[] = record.attachments.filter((item: Attachment) => item && opaque(item.id) && typeof item.name === "string" && item.name.length <= 512 && typeof item.media_type === "string" && Number.isFinite(item.size_bytes)).map((item: Attachment) => ({ id: item.id, name: item.name, media_type: item.media_type, size_bytes: item.size_bytes, uri: `/v1/actors/${encodeURIComponent(actorId)}/resources/${encodeURIComponent(item.id)}` }));
    const input = record.pending;
    const pending = input && typeof input.content === "string" && input.content.length <= 64 * 1024 && Array.isArray(input.attachments) && input.attachments.length <= 4 && input.attachments.every(opaque) && typeof input.key === "string" && /^[!-~]{1,128}$/.test(input.key) ? { content: input.content, attachments: input.attachments, key: input.key } : undefined;
    return { text: record.text, attachments, pending };
  } catch { store.removeItem(name); }
}
export function saveDraft(key: string, actorId: string, draft: StoredDraft, store = storage(), now = Date.now()): void {
  if (!store) return;
  try {
    if (!draft.text && !draft.attachments.length && !draft.pending) { store.removeItem(prefix + key); return; }
    store.setItem(prefix + key, JSON.stringify({ version: 2, subject_key: key, actor_id: actorId, expires_at: now + lifetime, text: draft.text, attachments: draft.attachments.map(({ id, name, media_type, size_bytes }) => ({ id, name, media_type, size_bytes })), pending: draft.pending }));
  } catch { /* Storage failures leave the in-memory draft intact. */ }
}
export function clearStoredDrafts(store = storage()): void {
  if (!store) return;
  try { for (const key of Array.from({ length: store.length }, (_, index) => store.key(index))) if (key?.startsWith(prefix)) store.removeItem(key); } catch { /* Browser storage may be unavailable. */ }
}
