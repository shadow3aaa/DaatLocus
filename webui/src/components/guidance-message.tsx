import { t } from "@/lib/i18n";
import type { MessageRecord } from "@/lib/actor-api";
import { AttachmentList } from "@/components/conversation-content";

export function GuidanceMessage({ message }: { message: MessageRecord }) {
  return <div id={`message-${message.actor_id}-${message.id}`} className="dl-work-user-input dl-guidance-message">
    <small className="dl-guidance-label">{t("conversation_turns.guidance_for_the_current_task")}</small>
    <div className="dl-work-prompt">
      {message.content && <p className="dl-user-text">{message.content}</p>}
      {message.attachments.length > 0 && <AttachmentList items={message.attachments} />}
    </div>
  </div>;
}
