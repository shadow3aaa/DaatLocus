import { DownloadIcon, TerminalIcon } from "lucide-react";
import type { MessageRecord } from "@/lib/actor-api";
import { importedConversation } from "@/lib/conversation-import";
import { t } from "@/lib/i18n";
import { Spinner } from "@/components/ui/spinner";

export function CodexMark() {
  return <span className="dl-codex-mark" aria-hidden="true"><TerminalIcon /></span>;
}
export function ImportConversationBubble({message}: {message: MessageRecord}) {
  const source = importedConversation(message);
  if (!source) return null;
  const pending = message.status === "running" || message.status === "queued";
  return <div className="dl-import-bubble">
    <CodexMark /><div><strong>{t("imports.bubble_title")}</strong><p>{source.name}</p>
      <span>{t(`imports.status_${message.status}`)}</span></div>
    {pending ? <Spinner className="dl-import-bubble-status" /> : <DownloadIcon className="dl-import-bubble-status" aria-hidden="true" />}
  </div>;
}
