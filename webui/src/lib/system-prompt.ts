import { request, type FetchOptions, type Page } from "./actor-api";

export type PromptFile = { index: number; name: string; bytes: number; tokens: number };
export type PromptFiles = Page<PromptFile> & { revision: string; total_tokens: number };
export const formatPromptTokens = (value: number) => new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 }).format(value).replace("K", "k");
export type PromptRole = "core" | "others";
export const promptFiles = (role: PromptRole, after?: number, options?: FetchOptions) => request<PromptFiles>(
  `/v1/system-prompt?role=${role}&limit=100${after === undefined ? "" : `&after=${after}`}`, options);
export const promptContent = (role: PromptRole, index: number, revision: string, offset: number, options?: FetchOptions) => request<{ content: string; next_offset: number | null }>(
  `/v1/system-prompt/${index}?role=${role}&revision=${encodeURIComponent(revision)}&offset=${offset}`, options);

export function layerColor(tokens: number, files: PromptFile[]) {
  const min = Math.min(...files.map(file => file.tokens)), max = Math.max(...files.map(file => file.tokens));
  const weight = (tokens - min) / (max - min || 1);
  return `color-mix(in srgb, var(--primary) ${12 + weight * 50}%, var(--background))`;
}
