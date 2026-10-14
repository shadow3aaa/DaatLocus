import { t } from "@/lib/i18n";
import type { EventRecord, MessageRecord } from "./actor-api";
export type WorkSegment = { kind: "text"; seq: number; generation: number; channel: "assistant" | "reasoning"; content: string } | { kind: "tools"; seq: number; callIds: string[] };
export function mergeActivity(current: EventRecord[], incoming: EventRecord[], actorId: string, messageId: string): EventRecord[] {
  const records = new Map<number, EventRecord>();
  for (const record of [...current, ...incoming]) if (record.actor_id === actorId && "message_id" in record.event && record.event.message_id === messageId) records.set(record.seq, record);
  return Array.from(records.values()).sort((a,b) => a.seq-b.seq);
}
export function workTimeline(events: EventRecord[], message: MessageRecord, complete = true): WorkSegment[] {
  const items = mergeActivity([], events, message.actor_id, message.id);
  const lastGeneration = Math.max(0, ...items.flatMap(({event}) => "generation" in event ? [event.generation] : []));
  const output: WorkSegment[] = [];
  const seenCalls = new Set<string>();
  for (const record of items) {
    const event = record.event;
    if (event.type === "generation_delta" && (event.channel === "assistant" || event.channel === "reasoning")) {
      if (message.status === "completed" && message.reply && event.channel === "assistant" && event.generation === (message.final_generation ?? (complete ? lastGeneration : -1))) continue;
      const previous = output.at(-1);
      if (previous?.kind === "text" && previous.generation === event.generation && previous.channel === event.channel) previous.content += event.text;
      else output.push({ kind: "text", seq: record.seq, generation: event.generation, channel: event.channel, content: event.text });
    } else if (event.type === "tool_started" || event.type === "tool_finished") {
      if (seenCalls.has(event.call_id)) continue;
      seenCalls.add(event.call_id);
      const previous = output.at(-1);
      if (previous?.kind === "tools") previous.callIds.push(event.call_id);
      else output.push({ kind: "tools", seq: record.seq, callIds: [event.call_id] });
    }
  }
  return output;
}
export function elapsedLabel(start?: number | null, end?: number | null, now = Date.now()): string | null {
  if (start == null) return null;
  const seconds = Math.max(0, Math.floor(((end ?? now) - start) / 1000));
  if (seconds < 60) return t("work_timeline.value0_s", {value0: seconds});
  return t("work_timeline.value0_m_value1", {value0: Math.floor(seconds/60), value1: seconds%60 ? ` ${t("work_timeline.value0_s", {value0: seconds%60})}` : ""});
}
let primaryActor = "root";
export function configurePrimaryActor(actorId: string): void { primaryActor = actorId; }
export const isPrimaryActor = (actorId: string) => actorId === primaryActor || (primaryActor === "core" && actorId.endsWith("~core"));
export const isUserSource = (message: MessageRecord) => message.source.kind === "user" || message.source.kind === "remote_user";
export function isUserDelivery(message: MessageRecord) { return (message.reply_to.kind === "user" || message.reply_to.kind === "remote" && message.reply_to.user) && message.status === "completed" && Boolean(message.reply); }
