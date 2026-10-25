import type { MessageRecord } from "./actor-api";

/** Incoming messages appear in conversation immediately for every Actor. */
export function partitionConversationMessages(messages: MessageRecord[], _primary: boolean) {
  return { transcript: messages.filter(message => !isInternalNotification(message)) };
}

/**
 * The runtime resolves an appeal with an immutable machine envelope addressed to the
 * Actor that requested it. It drives the model but is never user-observable input:
 * it stays out of the conversation, history previews and search.
 */
export function isInternalNotification(message: Pick<MessageRecord, "source" | "content">): boolean {
  if (message.source.kind !== "actor") return false;
  const text = message.content.trim();
  if (!text.startsWith("{")) return false;
  let envelope: unknown;
  try { envelope = JSON.parse(text); } catch { return false; }
  if (!envelope || typeof envelope !== "object" || Array.isArray(envelope)) return false;
  const record = envelope as Record<string, unknown>;
  return record.type === "approval_appeal_decision" && (record.decision === "approved" || record.decision === "denied");
}
