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
  SessionSummary,
  SetupConfigRequest,
  ToolResult,
  ToolSummary,
} from "../src/lib/actor-api";
import type { DeviceSettings } from "../src/lib/stage-api";

export const STAGE_ID = "4f9c1a5e-8b2d-4e6f-9a3c-7d5e1f0b2a4c";
export const ACCOUNT_ID = "mock-account";
export const ACCOUNT_NAME = "Lin";
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

/** Owner-facing metadata of a session, keyed by its session Core actor id. */
export type SessionMeta = {
  title: string | null;
  archived: boolean;
  created_at_ms: number;
  updated_at_ms: number;
  last_activity_at_ms: number;
};

export type Store = {
  actors: Map<string, ActorStore>;
  sessions: Map<string, SessionMeta>;
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
  sessions: new Map(),
  order: [],
  catalog: [],
  catalogSeq: 0,
  approvalSeq: 0,
  attachments: new Map(),
  idempotency: new Map(),
};
export const workReset: {generation: import("../src/lib/stage-api").WorkGeneration | null} = {generation: null};

export const stageSettings: DeviceSettings = {
  label: "Workstation",
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
        capabilities: "Complex reasoning, architecture and coordination; preferred main model.",
        context_window_tokens: 200000,
        max_completion_tokens: 8192,
        supports_vision: false,
      },
      {
        name: "mock-mini",
        provider_name: "mock",
        model_id: "mock-mini-001",
        capabilities: "Low-cost research and organization; suitable for short decisions and automatic reviews.",
        context_window_tokens: 128000,
        max_completion_tokens: 4096,
        supports_vision: true,
      },
      {
        name: "mock-motion",
        provider_name: "mock",
        model_id: "mock-motion-001",
        capabilities: "Strong at animation, motion and visual implementation; prefer for complex visual work at a higher cost.",
        context_window_tokens: 200000,
        max_completion_tokens: 8192,
        supports_vision: true,
      },
    ] satisfies ModelConfig[],
    main_model: "mock-large",
    approval_model: "mock-mini",
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
  title?: string | null;
};

/** A short name taken from the task, unique within the session like the backend's name check. */
export function taskActorName(session: string, task: string): string {
  const words = task.replace(/[^\p{L}\p{N}\s-]/gu, " ").trim().split(/\s+/).filter(Boolean).slice(0, 3).join(" ");
  const base = words ? words[0].toUpperCase() + words.slice(1) : "Task";
  const used = new Set(sessionWorkers(session).map(actor => actor.summary.name.trim().toLowerCase()));
  let name = base;
  for (let index = 2; used.has(name.toLowerCase()); index++) name = `${base} ${index}`;
  return name;
}

export function requestOrigin(actor: ActorStore): Pick<MessageRecord, "source" | "reply_to"> {
  const core = actor.summary.session_id;
  return actor.summary.role === "session"
    ? {source:{kind:"user"},reply_to:{kind:"user"}}
    : {source:{kind:"actor",actor_id:core},reply_to:{kind:"actor",actor_id:core,request_id:store.actors.get(core)?.messages.at(-1)?.id ?? "preview-request"}};
}

export function createActor(seed: ActorSeed): ActorStore {
  const ts = now();
  const session_id = seed.parent_id ? store.actors.get(seed.parent_id)?.summary.session_id ?? seed.parent_id : seed.id;
  const role = seed.parent_id ? "worker" : "session";
  const apps = role === "session" ? ["actors","admin_actors","super_skills"] : ["actors","apps_manager","files","web","browser_use","artifacts",...(process.env.DAAT_LOCUS_MOCK_PHONE === "1" ? ["phone_use"] : ["terminal","sudo",...(process.env.DAAT_LOCUS_MOCK_MACOS === "1" ? ["computer_use"] : [])])];
  const summary: ActorSummary = {
    id: seed.id,
    parent_id: seed.parent_id,
    session_id,
    role,
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
    session_id,
    role,
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
  if (role === "session") store.sessions.set(seed.id, { title: seed.title ?? null, archived: false, created_at_ms: ts, updated_at_ms: ts, last_activity_at_ms: ts });
  return actor;
}

/** Workers of one session in creation order. */
export function sessionWorkers(session: string): ActorStore[] {
  return store.order.map(id => store.actors.get(id)!).filter(actor => actor.summary.session_id === session && actor.summary.role === "worker");
}

const busy = (actor: ActorStore) => actor.summary.status === "running" || actor.messages.some(message => message.status === "queued" || message.status === "running");

export function sessionSummary(id: string): SessionSummary | null {
  const meta = store.sessions.get(id), core = store.actors.get(id);
  if (!meta || !core) return null;
  const workers = sessionWorkers(id);
  const working = workers.filter(actor => actor.summary.status === "running").length;
  return {
    id,
    stage_id: STAGE_ID,
    title: meta.title,
    status: meta.archived ? "archived" : busy(core) || workers.some(busy) ? "running" : "idle",
    working_actors: meta.archived ? 0 : working,
    model: core.detail.config?.model ?? null,
    created_at_ms: meta.created_at_ms,
    updated_at_ms: meta.updated_at_ms,
    last_activity_at_ms: meta.last_activity_at_ms,
    conversation_url: `/actors/${id}`,
  };
}

/** Publish the current session summary on the catalog stream. */
export function emitSessionChanged(id: string, fields: Partial<SessionMeta> = {}): SessionSummary | null {
  const meta = store.sessions.get(id);
  if (!meta) return null;
  Object.assign(meta, fields);
  const session = sessionSummary(id);
  if (session) emitEvent(id, { type: "session_changed", session });
  return session;
}

export type NewMessage = {
  kind: MessageRecord["kind"];
  source: MessageRecord["source"];
  reply_to: MessageRecord["reply_to"];
  in_reply_to?: string | null;
  content: string;
  status: MessageStatus;
  attachments?: Attachment[];
  skills?: MessageRecord["skills"];
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
    skills: input.skills,
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
  const meta = store.sessions.get(actor.summary.session_id);
  if (meta) {
    meta.last_activity_at_ms = ts;
    if (meta.title === null && actor.summary.role === "session" && input.source.kind === "user") meta.title = [...(input.content.trim().split("\n")[0] ?? "")].slice(0, 80).join("") || null;
  }
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
  "session_changed",
  "session_deleted",
  "actor_changed",
  "actor_deleted",
  "approval_requested",
  "approval_resolved",
  "message_updated",
  "message_added",
]);
/** Actor facts that can change a session's status, working count or activity time. */
const SESSION_FACTS = new Set(["actor_changed", "message_added", "message_updated"]);

export function emitEvent(actorId: string, event: ActorEvent): EventRecord | null {
  const actor = store.actors.get(actorId);
  const ts = now();
  let record: EventRecord | null = null;
  if (actor && !event.type.startsWith("session_")) {
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
  if (actor && SESSION_FACTS.has(event.type)) {
    const session = actor.summary.session_id;
    if (event.type !== "actor_changed") { const meta = store.sessions.get(session); if (meta) meta.last_activity_at_ms = ts; }
    emitSessionChanged(session);
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
