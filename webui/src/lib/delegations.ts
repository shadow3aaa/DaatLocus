import { useEffect, useSyncExternalStore } from "react";
import { getMessage, listMessages, subscribeActorEvents, type EventRecord, type MessageStatus, type ToolSummary } from "./actor-api";
import { actorIdFromUrl, localActorId } from "./conversation-links";
import { asObject, asText, operationToolName } from "./tool-presentation";

/** A request Core sent to an Actor, identified by the recipient's own message. */
export type Delegation = { resultId: string; actorId: string; messageId: string; name: string; title: string; followUp: boolean };
export type DelegationState = "sent" | "waiting" | "working" | "done" | "failed" | "stopped" | "interrupted";

export function delegationsOf(tools: ToolSummary[]): Delegation[] {
  return tools.flatMap(tool => {
    const outcome = asObject(tool.outcome);
    const messageId = asText(outcome.message_id);
    if (tool.error || outcome.sent_message !== true || !messageId || !["actors__message", "actors__create"].includes(operationToolName(tool))) return [];
    const linked = actorIdFromUrl(asText(outcome.conversation_url));
    const actorId = localActorId(linked ?? (asText(outcome.actor_id) || asText(outcome.id)));
    if (!actorId) return [];
    return [{ resultId: tool.id, actorId, messageId, name: asText(outcome.name), title: asText(outcome.title), followUp: Boolean(asText(outcome.reply_to)) }];
  });
}

export function delegationState(status?: MessageStatus): DelegationState {
  switch (status) {
    case "queued": return "waiting";
    case "running": return "working";
    case "completed": return "done";
    case "failed": return "failed";
    case "cancelled": return "stopped";
    case "interrupted": return "interrupted";
    default: return "sent";
  }
}

const rank: Record<MessageStatus, number> = { queued: 0, running: 1, completed: 2, failed: 2, cancelled: 2, interrupted: 2 };
const finished = (status?: MessageStatus) => status !== undefined && rank[status] === 2;
const keyOf = (actorId: string, messageId: string) => `${actorId}\u0000${messageId}`;
type RemoteRequestStream = (actorId: string, signal: AbortSignal, onEvent: (record: EventRecord) => void) => Promise<void>;
const remoteRequestStream: RemoteRequestStream = async (actorId, signal, onEvent) => {
  while (!signal.aborted) {
    // Capture a bounded cursor before subscribing. The stream replays any event
    // committed after that query, avoiding a gap without replaying old history.
    const page = await listMessages(actorId, {limit: 1, signal});
    let resync = false, revoked = false;
    await subscribeActorEvents(actorId, {
      after: page.event_cursor ?? 0, signal, onEvent,
      onResync: () => { resync = true; },
      onAccessRevoked: () => { revoked = true; },
    });
    if (!resync || revoked) return;
  }
};

/**
 * Delegated request statuses for mounted task cards. Local updates arrive from
 * the catalog stream; remote ones are refetched only on conversation activity.
 */
export class DelegationStatuses {
  private states = new Map<string, MessageStatus>();
  private tracked = new Map<string, { actorId: string; messageId: string; count: number }>();
  private listeners = new Set<() => void>();
  private queue: { key: string; actorId: string; messageId: string }[] = [];
  private loading = new Set<string>();
  private active = 0;
  private epoch = 0;
  private remoteStreams = new Map<string, {controller: AbortController; count: number}>();
  constructor(
    private readonly read = (actorId: string, messageId: string) => getMessage(actorId, messageId).then(message => message.status),
    private readonly concurrency = 4,
    private readonly stream: RemoteRequestStream = remoteRequestStream,
  ) {}

  status(actorId: string, messageId: string) { return this.states.get(keyOf(actorId, messageId)); }
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };

  track(actorId: string, messageId: string): () => void {
    const key = keyOf(actorId, messageId);
    const entry = this.tracked.get(key);
    if (entry) entry.count++;
    else this.tracked.set(key, { actorId, messageId, count: 1 });
    if (actorId.includes("~")) this.trackRemote(actorId);
    if (!this.states.has(key)) this.load(key, actorId, messageId);
    return () => {
      const current = this.tracked.get(key);
      if (current && --current.count <= 0) {
        this.tracked.delete(key);
        if (actorId.includes("~")) this.untrackRemote(actorId);
      }
    };
  }

  /** Apply a status fact; terminal states never regress to earlier ones. */
  update(actorId: string, messageId: string, status: MessageStatus) {
    const key = keyOf(actorId, messageId), previous = this.states.get(key);
    if (previous === status || previous !== undefined && rank[previous] > rank[status]) return;
    this.states.set(key, status);
    for (const listener of this.listeners) listener();
  }

  observe(record: EventRecord) {
    if (record.event.type === "message_updated" && this.tracked.has(keyOf(record.actor_id, record.event.message_id)))
      this.update(record.actor_id, record.event.message_id, record.event.status);
  }

  refreshUnfinished() {
    for (const [key, entry] of this.tracked) if (!finished(this.states.get(key))) this.load(key, entry.actorId, entry.messageId);
  }

  reset() {
    this.epoch++;
    for (const stream of this.remoteStreams.values()) stream.controller.abort();
    this.remoteStreams.clear();
    this.states.clear();
    this.queue = [];
    this.loading.clear();
    for (const listener of this.listeners) listener();
  }

  private load(key: string, actorId: string, messageId: string) {
    if (this.loading.has(key)) return;
    this.loading.add(key);
    this.queue.push({ key, actorId, messageId });
    this.drain();
  }

  private trackRemote(actorId: string) {
    const current = this.remoteStreams.get(actorId);
    if (current) { current.count++; return; }
    const controller = new AbortController();
    this.remoteStreams.set(actorId, {controller, count: 1});
    void this.stream(actorId, controller.signal, record => {
      const event = record.event;
      if (event.type === "message_updated" && record.actor_id === actorId && this.tracked.has(keyOf(actorId, event.message_id)))
        this.update(actorId, event.message_id, event.status);
    }).catch(() => {});
  }

  private untrackRemote(actorId: string) {
    const current = this.remoteStreams.get(actorId);
    if (!current || --current.count > 0) return;
    this.remoteStreams.delete(actorId);
    current.controller.abort();
  }

  private drain() {
    while (this.active < this.concurrency && this.queue.length) {
      const { key, actorId, messageId } = this.queue.shift()!;
      const epoch = this.epoch;
      this.active++;
      void this.read(actorId, messageId).then(status => { if (epoch === this.epoch) this.update(actorId, messageId, status); }, () => {}).finally(() => {
        this.active--;
        if (epoch === this.epoch) this.loading.delete(key);
        this.drain();
      });
    }
  }
}

export const delegationStatuses = new DelegationStatuses();

export function useDelegationStatus(actorId: string, messageId: string): MessageStatus | undefined {
  useEffect(() => delegationStatuses.track(actorId, messageId), [actorId, messageId]);
  const read = () => delegationStatuses.status(actorId, messageId);
  return useSyncExternalStore(delegationStatuses.subscribe, read, read);
}
