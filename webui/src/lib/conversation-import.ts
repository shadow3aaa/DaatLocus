import { request, type Attachment, type MessageRecord } from "./actor-api";
import { t } from "./i18n";

import { CODEX_IMPORT_MEDIA_TYPE } from "./import-types";
export { CODEX_IMPORT_MEDIA_TYPE } from "./import-types";
export type ImportPreview = {resource_id: string; title: string; message_count: number};
export function importedConversation(message: Pick<MessageRecord, "attachments">): Attachment | undefined {
  return message.attachments.find(item => item.media_type === CODEX_IMPORT_MEDIA_TYPE);
}
export function messageDisplayText(message: Pick<MessageRecord, "content" | "attachments">) {
  const source = importedConversation(message);
  return source ? `${t("imports.bubble_title")} · ${source.name}` : message.content;
}
export type CodexConversation = {id: string; title: string; updated_at_ms: number; archived: boolean};
export const getImportCapabilities = (signal?: AbortSignal) => request<{codex: boolean}>("/v1/imports",{signal});
export const listCodexConversations = (search: string, after?: string, refresh = false, signal?: AbortSignal) =>
  request<{items: CodexConversation[]; next_cursor: string | null}>(`/v1/imports/codex?limit=24&search=${encodeURIComponent(search)}${after?`&after=${encodeURIComponent(after)}`:""}${refresh?"&refresh=true":""}`,{signal});
export const previewCodexImport = (source_id: string, signal?: AbortSignal) => request<ImportPreview>("/v1/imports/codex/preview",{method:"POST",body:{source_id},signal});
export function submitCodexImport(resource_id: string) {
  return request<MessageRecord>("/v1/imports/codex", {method: "POST", body: {resource_id}});
}
