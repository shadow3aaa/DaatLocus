import { ActorApiError, request, type FetchOptions, type Page } from "./actor-api";
import { t } from "./i18n";

export type PromptFile = { index: number; name: string; bytes: number; tokens: number; layer_id?: string };
export type PromptFiles = Page<PromptFile> & { revision: string; total_tokens: number; editable_layers?: boolean };
export type InstructionLayer = { id: string; name: string; bytes: number; tokens: number; revision: string; roles: PromptRole[] };
export type InstructionLayerDetail = InstructionLayer & { content: string };
export const formatPromptTokens = (value: number) => new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 }).format(value).replace("K", "k");
export type PromptRole = "core" | "others";
export const promptFiles = (role: PromptRole, after?: number, options?: FetchOptions) => request<PromptFiles>(
  `/v1/system-prompt?role=${role}&limit=100${after === undefined ? "" : `&after=${after}`}`, options);
export const promptContent = (role: PromptRole, index: number, revision: string, offset: number, options?: FetchOptions) => request<{ content: string; next_offset: number | null }>(
  `/v1/system-prompt/${index}?role=${role}&revision=${encodeURIComponent(revision)}&offset=${offset}`, options);

export async function allPromptFiles(role: PromptRole, options?: FetchOptions) {
  let result = await promptFiles(role, undefined, options);
  while (result.next_cursor !== null) {
    const page = await promptFiles(role, result.next_cursor, options);
    if (page.revision !== result.revision) throw new Error("changed");
    result = { ...page, items: [...result.items, ...page.items] };
  }
  return result;
}

export const instructionLayers = (search: string, after = 0, options?: FetchOptions) => request<Page<InstructionLayer>>(
  `/v1/instruction-layers?q=${encodeURIComponent(search)}&after=${after}&limit=50`, options);
/** Read remaining metadata only when revealing a new file or wrapping to the end. */
export async function allInstructionLayers(search: string, options?: FetchOptions) {
  let result = await instructionLayers(search, 0, options);
  while (result.next_cursor !== null) {
    const page = await instructionLayers(search, result.next_cursor, options);
    result = { ...page, items: [...result.items, ...page.items.filter(file => !result.items.some(old => old.id === file.id))] };
  }
  return result;
}
export const instructionLayer = (id: string, options?: FetchOptions) => request<InstructionLayerDetail>(`/v1/instruction-layers/${encodeURIComponent(id)}`, options);
export const saveInstructionLayer = (draft: { name: string; content: string }, current?: InstructionLayerDetail) => request<InstructionLayerDetail>(
  `/v1/instruction-layers${current ? `/${encodeURIComponent(current.id)}` : ""}`,
  { method: current ? "PATCH" : "POST", body: { ...draft, ...(current ? { revision: current.revision } : {}) } });
export const replacePromptLayers = (role: PromptRole, revision: string, layer_ids: string[]) => request<{ revision: string }>(
  `/v1/system-prompt/layers?role=${role}`, { method: "PUT", body: { revision, layer_ids } });

/** System sources retain code order; user files have no semantic ordering. */
export function displayPromptFiles(files: PromptFile[], inserted?: string) {
  const user = files.filter(file => file.layer_id).sort((a, b) => b.bytes - a.bytes || promptFileKey(a).localeCompare(promptFileKey(b)));
  return [...files.filter(file => !file.layer_id), ...user.filter(file => file.layer_id !== inserted), ...user.filter(file => file.layer_id === inserted)];
}
export const LIBRARY_VISIBLE_LAYERS = 4;
export function promptLayerWindow<T>(files: readonly T[], start: number, limit = LIBRARY_VISIBLE_LAYERS) {
  return Array.from({ length: Math.min(limit, files.length) }, (_, index) => files[((start + index) % files.length + files.length) % files.length]);
}
export const promptFileKey = (file: PromptFile) => file.layer_id ?? file.name;
export const layerError = (error: unknown) => t(error instanceof ActorApiError && error.status === 409 ? "context_composition.conflict" : "context_composition.save_failed");

export function layerColor(tokens: number, files: PromptFile[]) {
  const min = Math.min(...files.map(file => file.tokens)), max = Math.max(...files.map(file => file.tokens));
  const weight = Math.max(0, Math.min(1, (tokens - min) / (max - min || 1)));
  return `color-mix(in srgb, var(--primary) ${12 + weight * 50}%, var(--background))`;
}
