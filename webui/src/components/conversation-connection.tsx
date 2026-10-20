import { t } from "@/lib/i18n";
import type { ConversationConnectionMessage } from "@/lib/stage-api";

export function ConversationConnection({ message }: {
  message: ConversationConnectionMessage | null;
}) {
  return message && <p className="dl-conversation-connection" role="status" aria-live="polite" aria-atomic="true">{t(message)}</p>;
}
