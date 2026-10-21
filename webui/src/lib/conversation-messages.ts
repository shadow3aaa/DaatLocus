import type { MessageRecord } from "./actor-api";

/** Core keeps its IM flow; work conversations hold all pending inputs by the composer. */
export function partitionConversationMessages(messages: MessageRecord[], primary: boolean) {
  if (primary) return { queue: [] as MessageRecord[], transcript: messages };
  const queue: MessageRecord[] = [], transcript: MessageRecord[] = [];
  for (const message of messages) {
    (message.status === "queued" && message.kind !== "delivery" ? queue : transcript).push(message);
  }
  return { queue, transcript };
}
