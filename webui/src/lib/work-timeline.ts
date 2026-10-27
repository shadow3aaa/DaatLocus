import { t } from "@/lib/i18n";
import type { ToolRef } from "@/hooks/use-actor-conversation";
import type { EventRecord, MessageRecord } from "./actor-api";
export type WorkSegment = { kind: "text"; seq: number; generation: number; channel: "assistant" | "reasoning"; content: string } | { kind: "tools"; seq: number; callIds: string[] };
export function mergeActivity(current: EventRecord[], incoming: EventRecord[], actorId: string, messageId: string): EventRecord[] {
  const records = new Map<number, EventRecord>();
  for (const record of [...current, ...incoming]) if (record.actor_id === actorId && "message_id" in record.event && record.event.message_id === messageId) records.set(record.seq, record);
  return Array.from(records.values()).sort((a,b) => a.seq-b.seq);
}
// An expanded round retains its consumed prefix independently of the bounded
// conversation stream. A disconnected live tail must be backfilled before it
// can be joined; tool summaries cannot establish chronology for the gap.
export function extendActivityHistory(history: EventRecord[], live: EventRecord[], actorId: string, messageId: string, through: number) {
  const window = live.filter(record => record.actor_id === actorId);
  const latest = window.at(-1)?.seq ?? 0;
  if (latest <= through) return {records: history, through, gap: false};
  if (window[0].seq > through) return {records: history, through, gap: true};
  return {records: mergeActivity(history, window, actorId, messageId), through: latest, gap: false};
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
// Tool-start facts also restore unfinished work after reconnect; summary pages
// intentionally contain completed observations only.
export function activityTools(events: EventRecord[], message: MessageRecord, summaries: ToolRef[], complete = true): ToolRef[] {
  const tools = new Map<string, ToolRef>();
  for (const record of mergeActivity([], events, message.actor_id, message.id)) {
    const event = record.event;
    if (event.type === "tool_started") tools.set(event.call_id, { seq: record.seq, id: `live:${event.call_id}`, message_id: event.message_id, call_id: event.call_id, name: event.name, summary: "", error: !complete || ["queued", "running"].includes(message.status) ? null : t("conversation_content.interrupted"), status: !complete ? "unknown" : ["queued", "running"].includes(message.status) ? "running" : "completed", started_at_ms: record.created_at_ms });
    if (event.type === "tool_finished") tools.set(event.call_id, { seq: record.seq, id: event.result_id, message_id: event.message_id, call_id: event.call_id, name: event.name, summary: event.summary, error: event.error, status: "completed", started_at_ms: event.started_at_ms, finished_at_ms: event.finished_at_ms, outcome: event.outcome });
  }
  for (const tool of summaries) {
    const existing = tools.get(tool.call_id);
    if (!existing || existing.status !== "completed" || tool.status === "completed") tools.set(tool.call_id, tool);
  }
  return [...tools.values()];
}
export function isLiveReasoning(segment: WorkSegment, index: number, segments: WorkSegment[], running: boolean): boolean {
  return running && index === segments.length - 1 && segment.kind === "text" && segment.channel === "reasoning";
}
export type WorkActivitySegment =
  | Extract<WorkSegment, { kind: "text" }>
  | { kind: "activity"; seq: number; callIds: string[]; thinking: string | null };

// Reasoning updates the current activity's headline; only actual assistant prose
// separates groups. Historical reasoning never becomes another disclosure body.
export function workActivityTimeline(events: EventRecord[], message: MessageRecord, complete = true, live = true): WorkActivitySegment[] {
  const segments = workTimeline(events, message, complete);
  const output: WorkActivitySegment[] = [];
  for (const [index, segment] of segments.entries()) {
    if (segment.kind === "text" && segment.channel === "assistant") {
      output.push(segment);
      continue;
    }
    let group = output.at(-1);
    if (group?.kind !== "activity") {
      group = { kind: "activity", seq: segment.seq, callIds: [], thinking: null };
      output.push(group);
    }
    if (segment.kind === "tools") group.callIds.push(...segment.callIds);
    else if (isLiveReasoning(segment, index, segments, live && message.status === "running")) {
      let thinking = segment.content;
      for (let earlier = index - 1; earlier >= 0; earlier--) {
        const previous = segments[earlier];
        if (previous.kind !== "text" || previous.generation !== segment.generation) break;
        if (previous.channel === "reasoning") thinking = previous.content + thinking;
      }
      group.thinking = thinking.split(/\n\s*\n/).filter(part => part.trim()).at(-1)?.replace(/\s+/g, " ").trim() || null;
    }
  }
  const visible: WorkActivitySegment[] = [];
  for (const segment of output) {
    if (segment.kind === "activity" && !segment.callIds.length && !segment.thinking) continue;
    const previous = visible.at(-1);
    // Progress flushes can interleave assistant and reasoning chunks. Hidden
    // reasoning is not a paragraph boundary; preserve the original Markdown.
    // Tool groups and distinct generations still separate prose.
    if (segment.kind === "text" && previous?.kind === "text" && previous.generation === segment.generation && previous.channel === segment.channel) previous.content += segment.content;
    else visible.push(segment);
  }
  return visible;
}
export function elapsedLabel(start?: number | null, end?: number | null, now = Date.now()): string | null {
  if (start == null) return null;
  const seconds = Math.max(0, Math.floor(((end ?? now) - start) / 1000));
  if (seconds < 60) return t("work_timeline.value0_s", {value0: seconds});
  return t("work_timeline.value0_m_value1", {value0: Math.floor(seconds/60), value1: seconds%60 ? ` ${t("work_timeline.value0_s", {value0: seconds%60})}` : ""});
}
function workFinishedAt(message: MessageRecord, events: EventRecord[]) {
  const finished = events.filter(record => record.actor_id === message.actor_id && record.event.type === "message_updated" && record.event.message_id === message.id && !["running", "queued"].includes(record.event.status)).at(-1);
  return message.finished_at_ms ?? finished?.created_at_ms ?? message.updated_at_ms;
}
export type WorkRoundSpan = { start: number; end: number | null; running: boolean };
export type WorkTranscriptPart = { message: MessageRecord } & ({ kind: "input" | "outcome" } | { kind: "activity"; span: WorkRoundSpan });

/** Split serial execution around nested inputs, rather than placing resumed work at its original input. */
export function workTranscriptParts(messages: MessageRecord[], allMessages: MessageRecord[], events: EventRecord[] = []): WorkTranscriptPart[] {
  const actor = messages[0]?.actor_id;
  const known = [...new Map([...allMessages, ...messages].filter(message => message.actor_id === actor).map(message => [message.id, message])).values()];
  const lifetimes = known.filter(message => message.kind !== "delivery" && message.status !== "queued" && !(message.started_at_ms == null && ["cancelled", "interrupted"].includes(message.status))).map(message => ({
    message, start: message.started_at_ms ?? message.created_at_ms,
    end: message.status === "running" && message.finished_at_ms == null ? Infinity : workFinishedAt(message, events),
  }));
  const boundaries = [...new Set(lifetimes.flatMap(item => [item.start, item.end]).filter(Number.isFinite))].sort((a,b) => a-b);
  const spans: { message: MessageRecord; span: WorkRoundSpan }[] = [];
  for (const [index, start] of boundaries.entries()) {
    const owner = lifetimes.filter(item => item.start <= start && item.end > start).sort((a,b) => b.message.seq-a.message.seq)[0];
    if (!owner) continue;
    const end = boundaries[index+1] ?? null;
    const previous = spans.at(-1);
    if (previous?.message.id === owner.message.id && previous.span.end === start) {
      previous.span.end = end;
      previous.span.running = end === null;
    } else spans.push({message: owner.message, span: {start, end, running: end === null}});
  }
  // Instant terminal records still retain their disclosure and outcome.
  for (const item of lifetimes) if (item.end <= item.start) spans.push({message:item.message,span:{start:item.start,end:item.start,running:false}});
  const visible = new Set(messages.map(message => message.id));
  const parts: { at: number; order: number; part: WorkTranscriptPart }[] = [];
  for (const message of messages) {
    parts.push({at:Math.min(message.created_at_ms, message.started_at_ms ?? message.created_at_ms),order:0,part:{kind:"input",message}});
    const last = spans.filter(item => item.message.id === message.id).at(-1);
    parts.push({at:last?.span.running ? Infinity : message.finished_at_ms ?? last?.span.end ?? message.updated_at_ms,order:last?.span.running ? 4 : 2,part:{kind:"outcome",message}});
  }
  for (const {message,span} of spans) if (visible.has(message.id)) parts.push({at:span.running ? Infinity : span.start,order:span.end===span.start ? 1 : 3,part:{kind:"activity",message,span}});
  return parts.sort((a,b) => a.at-b.at || a.order-b.order || a.part.message.seq-b.part.message.seq).map(item => item.part);
}
let primaryActor = "core";
export function configurePrimaryActor(actorId: string): void { primaryActor = actorId; }
export const isPrimaryActor = (actorId: string) => actorId === primaryActor || (primaryActor === "core" && actorId.endsWith("~core"));
export const isUserSource = (message: MessageRecord) => message.source.kind === "user" || message.source.kind === "remote_user";
export function isUserDelivery(message: MessageRecord) { return (message.reply_to.kind === "user" || message.reply_to.kind === "remote" && message.reply_to.user) && message.status === "completed" && Boolean(message.reply); }
