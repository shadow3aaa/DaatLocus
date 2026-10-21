import { t } from "@/lib/i18n";
import type { MessageRecord } from "@/lib/actor-api";
import { AttachmentList } from "@/components/conversation-content";

export function GuidanceMessage({ message }: { message: MessageRecord }) {
  return <div id={`message-${message.actor_id}-${message.id}`} className="dl-work-user-input dl-guidance-message">
    <small className="dl-guidance-label">{t(message.source.kind === "actor" && message.source.actor_id === "core" ? "conversation_turns.guidance_from_core_applied" : "conversation_turns.guidance_applied")}</small>
    <div className="dl-work-prompt">
      {message.content && <p className="dl-user-text">{message.content}</p>}
      {message.attachments.length > 0 && <AttachmentList items={message.attachments} />}
    </div>
  </div>;
}
