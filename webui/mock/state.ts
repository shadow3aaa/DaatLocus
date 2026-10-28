/**
 * In-memory state for the WebUI mock daemon.
 *
 * Everything lives in one `store` singleton; `fixtures.ts#resetStore` re-seeds
 * it. Wire shapes are imported type-only from the real client contract in
 * `src/lib` so the mock cannot drift silently from what the WebUI expects.
 */
import type {
  ActorDetail,
  ActorEvent,
  ActorStatus,
  ActorSummary,
  ApprovalDecision,
  ApprovalRecord,
  Attachment,
  EventRecord,
  JsonValue,
  MessageRecord,
  MessageStatus,
  ModelConfig,
  ProviderConfig,
  SetupConfigRequest,
  ToolResult,
  ToolSummary,
} from "../src/lib/actor-api";
import type { DeviceSettings } from "../src/lib/stage-api";

export const STAGE_ID = "4f9c1a5e-8b2d-4e6f-9a3c-7d5e1f0b2a4c";
export const ACCOUNT_ID = "mock-account";
export const ACCOUNT_NAME = "林";
export const ROOT_ACTOR_ID = "core";
/** Listen port; env DAAT_LOCUS_MOCK_PORT overrides the 45826 default. */
export const MOCK_PORT = Number(process.env.DAAT_LOCUS_MOCK_PORT) || 45826;
/** Stream key of the catalog event stream (all actors). */
export const CATALOG_KEY = "__catalog__";
/** Per-stream replay ring cap, matching the SSE replay buffer budget. */
const EVENT_CAP = 1000;

export type ToolEntry = { seq: number; result: ToolResult };
export type AttachmentBlob = { attachment: Attachment; bytes: Uint8Array; media_type: string };

export type ActorStore = {
  summary: ActorSummary;
  detail: ActorDetail;
  /** Ascending by seq. */
  messages: MessageRecord[];
  /** Ascending by seq; trimmed to EVENT_CAP. */
  events: EventRecord[];
  tools: ToolEntry[];
  approvals: ApprovalRecord[];
  messageSeq: number;
  toolSeq: number;
  eventSeq: number;
};

/** Live-stream pushes: a stored event record, or a resync request after reset. */
export type StreamItem = EventRecord | { resync: true };
export type Listener = (item: StreamItem) => void;

export type Store = {
  actors: Map<string, ActorStore>;
  /** Stable actor insertion order for catalog paging. */
  order: string[];
  catalog: EventRecord[];
  catalogSeq: number;
  /** Approvals use one global seq so cross-actor cursors stay monotonic. */
  approvalSeq: number;
  attachments: Map<string, AttachmentBlob>;
  idempotency: Map<string, MessageRecord>;
};

export const store: Store = {
  actors: new Map(),
  order: [],
  catalog: [],
  catalogSeq: 0,
  approvalSeq: 0,
  attachments: new Map(),
  idempotency: new Map(),
};
export const workReset: {generation: import("../src/lib/stage-api").WorkGeneration | null} = {generation: null};

export const stageSettings: DeviceSettings = {
  label: "Mock Device",
  mode: "local",
  relay_origin: null,
  compute_priority: 100,
};

export function defaultConfig(): SetupConfigRequest {
  return {
    locale: null,
    providers: [
      {
        kind: "openai_compatible",
        name: "mock",
        base_url: `http://127.0.0.1:${MOCK_PORT}/v1/mock-llm`,
        api_key: null,
        credential_configured: true,
      },
    ] satisfies ProviderConfig[],
    models: [
      {
        name: "mock-large",
        provider_name: "mock",
        model_id: "mock-large-001",
        context_window_tokens: 200000,
        max_completion_tokens: 8192,
        supports_vision: false,
      },
      {
        name: "mock-mini",
        provider_name: "mock",
        model_id: "mock-mini-001",
        context_window_tokens: 128000,
        max_completion_tokens: 4096,
        supports_vision: true,
      },
    ] satisfies ModelConfig[],
    main_model: "mock-large",
    approval_model: null,
    daemon_port: MOCK_PORT,
  };
}

export const configHolder: { current: SetupConfigRequest } = { current: defaultConfig() };

const listeners = new Map<string, Set<Listener>>();

export function addListener(key: string, listener: Listener): () => void {
  let set = listeners.get(key);
  if (!set) {
    set = new Set();
    listeners.set(key, set);
  }
  set.add(listener);
  return () => {
    set?.delete(listener);
  };
}

function pushItem(key: string, item: StreamItem): void {
  for (const listener of listeners.get(key) ?? []) listener(item);
}

/** Tell every live SSE stream to resync (used after a mock reset). */
export function broadcastResync(): void {
  for (const key of [...listeners.keys()]) pushItem(key, { resync: true });
}

export const now = (): number => Date.now();
export const newId = (): string => crypto.randomUUID();

export function getActor(id: string): ActorStore | undefined {
  return store.actors.get(id);
}

export type ActorSeed = {
  id: string;
  name: string;
  parent_id: string | null;
  apps: string[];
  model: string | null;
  workspace: string;
  instructions: string;
};

export function createActor(seed: ActorSeed): ActorStore {
  const ts = now();
  const apps = seed.id === ROOT_ACTOR_ID ? ["actors"] : ["actors","apps_manager","files","terminal","web","browser_use","artifacts"];
  const summary: ActorSummary = {
    id: seed.id,
    parent_id: seed.parent_id,
    conversation_url: `/actors/${seed.id}`,
    name: seed.name,
    status: "idle",
    apps: [...apps],
    created_at_ms: ts,
    updated_at_ms: ts,
  };
  const detail: ActorDetail = {
    remote: false,
    id: seed.id,
    parent_id: seed.parent_id,
    conversation_url: `/actors/${seed.id}`,
    name: seed.name,
    config: {
      name: seed.name,
      workspace: seed.workspace,
      apps: [...apps],
      model: seed.model,
      instructions: seed.instructions,

    },
    status: "idle",
    created_at_ms: ts,
    updated_at_ms: ts,
    access: { view: true, converse: true, manage: true },
  };
  const actor: ActorStore = {
    summary,
    detail,
    messages: [],
    events: [],
    tools: [],
    approvals: [],
    messageSeq: 0,
    toolSeq: 0,
    eventSeq: 0,
  };
  store.actors.set(seed.id, actor);
  store.order.push(seed.id);
  return actor;
}

export type NewMessage = {
  kind: MessageRecord["kind"];
  source: MessageRecord["source"];
  reply_to: MessageRecord["reply_to"];
  in_reply_to?: string | null;
  content: string;
  status: MessageStatus;
  attachments?: Attachment[];
};

export function addMessage(actor: ActorStore, input: NewMessage): MessageRecord {
  const ts = now();
  const record: MessageRecord = {
    seq: ++actor.messageSeq,
    id: newId(),
    actor_id: actor.summary.id,
    source: input.source,
    reply_to: input.reply_to,
    kind: input.kind,
    in_reply_to: input.in_reply_to ?? null,
    content: input.content,
    attachments: input.attachments ?? [],
    status: input.status,
    cancel_requested: false,
    started_at_ms: input.status === "running" ? ts : null,
    finished_at_ms: null,
    final_generation: null,
    reply: null,
    error: null,
    created_at_ms: ts,
    updated_at_ms: ts,
  };
  actor.messages.push(record);
  return record;
}

export function touchMessage(record: MessageRecord, fields: Partial<MessageRecord>): void {
  Object.assign(record, fields, { updated_at_ms: now() });
}

/**
 * The owner catalog stream only forwards these event types
 * (src/service/stream.rs); per-actor noise like generation_delta stays on the
 * actor stream.
 */
const CATALOG_EVENTS = new Set([
  "actor_changed",
  "actor_deleted",
  "approval_requested",
  "approval_resolved",
  "message_updated",
  "message_added",
]);

export function emitEvent(actorId: string, event: ActorEvent): EventRecord | null {
  const actor = store.actors.get(actorId);
  const ts = now();
  let record: EventRecord | null = null;
  if (actor) {
    record = { seq: ++actor.eventSeq, actor_id: actorId, created_at_ms: ts, event };
    actor.events.push(record);
    if (actor.events.length > EVENT_CAP) actor.events.splice(0, actor.events.length - EVENT_CAP);
    pushItem(actorId, record);
  }
  if (CATALOG_EVENTS.has(event.type)) {
    const catalogRecord: EventRecord = { seq: ++store.catalogSeq, actor_id: actorId, created_at_ms: ts, event };
    store.catalog.push(catalogRecord);
    if (store.catalog.length > EVENT_CAP) store.catalog.splice(0, store.catalog.length - EVENT_CAP);
    pushItem(CATALOG_KEY, catalogRecord);
  }
  return record;
}

export function addTool(actor: ActorStore, result: ToolResult): ToolEntry {
  const entry: ToolEntry = { seq: ++actor.toolSeq, result };
  actor.tools.push(entry);
  return entry;
}

export function toolSummary(entry: ToolEntry): ToolSummary {
  const result = entry.result;
  return {
    seq: entry.seq,
    id: result.id,
    message_id: result.message_id,
    call_id: result.call_id,
    name: result.name,
    summary: result.summary,
    error: result.error,
    outcome: result.outcome ?? null,
  };
}

export function addApproval(
  actor: ActorStore,
  input: {
    message_id: string;
    call: { id: string; name: string; arguments: JsonValue };
    requested: { readable_roots: string[]; writable_roots: string[] };
  },
): ApprovalRecord {
  const record: ApprovalRecord = {
    seq: ++store.approvalSeq,
    id: newId(),
    message_id: input.message_id,
    call: input.call,
    requested: input.requested,
    decision: "pending",
    reason: "",
    created_at_ms: now(),
    resolved_at_ms: null,
  };
  actor.approvals.push(record);
  return record;
}

/**
 * Resolve a pending approval, emit `approval_resolved` on both streams and
 * retain the immutable resolved record for audit history.
 */
export function resolveApprovalRecord(
  approvalId: string,
  decision: ApprovalDecision,
  reason: string,
): ApprovalRecord | null {
  for (const actor of store.actors.values()) {
    const record = actor.approvals.find(item => item.id === approvalId);
    if (!record) continue;
    if (record.decision === "pending") {
      record.decision = decision;
      record.reason = reason;
      record.resolved_at_ms = now();
      emitEvent(actor.summary.id, {
        type: "approval_resolved",
        approval_id: record.id,
        message_id: record.message_id,
        decision,
        reason,
      });
    }
    return record;
  }
  return null;
}

export function findApproval(approvalId: string): ApprovalRecord | null {
  for (const actor of store.actors.values()) {
    const record = actor.approvals.find(item => item.id === approvalId);
    if (record) return record;
  }
  return null;
}

export function setActorStatus(actor: ActorStore, status: ActorStatus): void {
  if (actor.summary.status === status) return;
  const ts = now();
  actor.summary.status = status;
  actor.summary.updated_at_ms = ts;
  actor.detail.status = status;
  actor.detail.updated_at_ms = ts;
  emitEvent(actor.summary.id, { type: "actor_changed", actor: { ...actor.summary } });
}

/** Events of one stream (actor id or CATALOG_KEY) with seq > after, ascending. */
export function streamEvents(key: string, after: number): EventRecord[] {
  if (key === CATALOG_KEY) return store.catalog.filter(record => record.seq > after);
  const actor = store.actors.get(key);
  return actor ? actor.events.filter(record => record.seq > after) : [];
}

/** Current max event seq of one stream (actor id or CATALOG_KEY). */
export function maxSeq(key: string): number {
  if (key === CATALOG_KEY) return store.catalogSeq;
  return store.actors.get(key)?.eventSeq ?? 0;
}
