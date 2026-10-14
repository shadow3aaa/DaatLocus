import type { ReactNode } from "react";
import { Spinner } from "@/components/ui/spinner";
import { t } from "@/lib/i18n";
import type { ConversationConnectionMessage } from "@/lib/stage-api";

export function ConversationConnection({ message, children }: {
  message: ConversationConnectionMessage | null;
  children: ReactNode;
}) {
  const waiting = Boolean(message);
  return <>
    <div className="dl-conversation-content" inert={waiting || undefined} aria-hidden={waiting || undefined}>
      {children}
    </div>
    {message && <div className="dl-conversation-connection" role="status" aria-live="polite" aria-atomic="true">
      <Spinner className="size-9" role="presentation" aria-label={undefined} aria-hidden="true" strokeWidth={1.75} />
      <p>{t(message)}</p>
    </div>}
  </>;
}
