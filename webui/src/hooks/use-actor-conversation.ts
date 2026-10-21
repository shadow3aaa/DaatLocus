import { t } from "@/lib/i18n";
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import * as api from "@/lib/actor-api";
import { clearStoredDrafts, loadDraft, saveDraft } from "@/lib/conversation-drafts";
export type GenerationDraft = {
  generation: number;
  assistant: string;
  reasoning: string;
};
export type ToolRef = api.ToolSummary & {
  status: "running" | "completed" | "unknown";
};
export type ConversationState = {
  actorId: string | null;
  actor: api.ActorDetail | null;
  messages: api.MessageRecord[];
  events: api.EventRecord[];
  generations: Record<string, GenerationDraft>;
  tools: ToolRef[];
  toolNext: Record<string, number | null>;
  approvals: api.ApprovalRecord[];
  cursor: number;
  nextCursor: number | null;
  queueCursor: number | null;
  deleted: boolean;
};
export type ConversationAction = {
  type: "reset";
  actorId: string | null;
} | {
  type: "actor";
  actor: api.ActorDetail;
} | {
  type: "page";
  page: api.Page<api.MessageRecord>;
  initial?: boolean;
} | {
  type: "queue";
  page: api.Page<api.MessageRecord> & { active?: api.MessageRecord | null };
} | {
  type: "message";
  message: api.MessageRecord;
} | {
  type: "event";
  record: api.EventRecord;
} | {
  type: "tools";
  actorId: string;
  messageId: string;
  nextCursor: number | null;
  tools: api.ToolSummary[];
} | {
  type: "approval";
  approval: api.ApprovalRecord;
} | {
  type: "cursor";
  cursor: number;
};
export function initialConversation(actorId: string | null): ConversationState { return { actorId, actor: null, messages: [], events: [], generations: {}, tools: [], toolNext: {}, approvals: [], cursor: 0, nextCursor: null, queueCursor: null, deleted: false }; }
const terminal = (status: api.MessageStatus) => !["queued", "running"].includes(status);
function mergeMessages(current: api.MessageRecord[], incoming: api.MessageRecord[]): api.MessageRecord[] {
  const values = new Map(current.map(message => [message.id, message]));
  for (const message of incoming) {
    const previous = values.get(message.id);
    if (previous?.status === "running" && message.status === "queued" && message.updated_at_ms === previous.updated_at_ms) continue;
    if (!previous || message.updated_at_ms > previous.updated_at_ms ||
      (message.updated_at_ms === previous.updated_at_ms && (!terminal(previous.status) || terminal(message.status))))
      values.set(message.id, message);
  }
  return [...values.values()].sort((a, b) => a.seq - b.seq);
}
function mergeTools(current: ToolRef[], incoming: ToolRef[]): ToolRef[] {
  const values = new Map(current.map(tool => [`${tool.message_id}:${tool.call_id}`, tool]));
  for (const tool of incoming)
    values.set(`${tool.message_id}:${tool.call_id}`, tool);
  return [...values.values()].sort((a, b) => a.seq - b.seq);
}
export function conversationReducer(state: ConversationState, action: ConversationAction): ConversationState {
  switch (action.type) {
    case "reset": return initialConversation(action.actorId);
    case "actor": return action.actor.id === state.actorId ? { ...state, actor: action.actor } : state;
    case "cursor": return action.cursor > state.cursor ? { ...state, cursor: action.cursor } : state;
    case "page": return { ...state, messages: mergeMessages(state.messages, action.page.items.filter(message => message.actor_id === state.actorId)), nextCursor: action.page.next_cursor,
      cursor: action.initial ? Math.max(state.cursor, action.page.event_cursor ?? 0) : state.cursor };
    case "message": return action.message.actor_id === state.actorId ? { ...state, messages: mergeMessages(state.messages, [action.message]) } : state;
    case "queue": return { ...state, messages: mergeMessages(state.messages, [...action.page.items, ...(action.page.active ? [action.page.active] : [])].filter(message => message.actor_id === state.actorId)), queueCursor: action.page.next_cursor };
    case "tools": return action.actorId === state.actorId ? { ...state, tools: mergeTools(state.tools, action.tools.map(tool => ({ ...tool, status: "completed" }))), toolNext: { ...state.toolNext, [action.messageId]: action.nextCursor } } : state;
    case "approval": {
      if (action.approval.actor_id !== state.actorId)
        return state;
      const previous = state.approvals.find(item => item.id === action.approval.id);
      if (previous && previous.decision !== "pending" && action.approval.decision === "pending")
        return state;
      return { ...state, approvals: [...state.approvals.filter(item => item.id !== action.approval.id), action.approval].sort((a, b) => a.seq - b.seq) };
    }
    case "event": {
      const record = action.record;
      if (record.actor_id !== state.actorId || record.seq <= state.cursor)
        return state;
      let next = { ...state, cursor: record.seq, events: [...state.events, record].slice(-300) };
      const event = record.event;
      if (event.type === "actor_deleted")
        return { ...next, deleted: true, actor: null };
      if (event.type === "actor_changed" && next.actor && event.actor.updated_at_ms >= next.actor.updated_at_ms)
        next = { ...next, actor: { ...next.actor, name: event.actor.name, status: event.actor.status, updated_at_ms: event.actor.updated_at_ms, conversation_url: event.actor.conversation_url } };
      if (event.type === "message_updated") {
        next = { ...next, messages: next.messages.map(message => message.id === event.message_id && !terminal(message.status) ? { ...message, status: event.status } : message) };
        if (terminal(event.status)) {
          const generations = { ...next.generations };
          delete generations[event.message_id];
          next = { ...next, generations };
        }
      }
      if (event.type === "generation_started" || event.type === "generation_delta") {
        if (next.messages.some(message => message.id === event.message_id && terminal(message.status)))
          return next;
        const previous = next.generations[event.message_id];
        if (!previous || event.generation > previous.generation)
          next = { ...next, generations: { ...next.generations, [event.message_id]: { generation: event.generation, assistant: "", reasoning: "" } } };
        const current = next.generations[event.message_id];
        if (event.type === "generation_delta" && current && event.generation === current.generation && ["assistant", "reasoning"].includes(event.channel)) {
          const channel = event.channel as "assistant" | "reasoning";
          next = { ...next, generations: { ...next.generations, [event.message_id]: { ...current, [channel]: current[channel] + event.text } } };
        }
      }
      if (event.type === "tool_started")
        next = { ...next, tools: mergeTools(next.tools, [{ seq: record.seq, id: `live:${event.call_id}`, message_id: event.message_id, call_id: event.call_id, name: event.name, summary: "", error: null, status: "running" }]) };
      if (event.type === "tool_finished")
        next = { ...next, tools: mergeTools(next.tools, [{ seq: record.seq, id: event.result_id, message_id: event.message_id, call_id: event.call_id, name: event.name, summary: event.summary, error: event.error, status: "completed", started_at_ms: event.started_at_ms, finished_at_ms: event.finished_at_ms, outcome: event.outcome }]) };
      return next;
    }
  }
}
export type PendingSubmission = {
  content: string;
  attachments: string[];
  key: string;
};
export function submissionFor(previous: PendingSubmission | undefined, content: string, attachments: string[], createKey = () => crypto.randomUUID()): PendingSubmission {
  if (previous && previous.content === content && JSON.stringify(previous.attachments) === JSON.stringify(attachments))
    return previous;
  return { content, attachments: [...attachments], key: createKey() };
}
type AttachmentDraft = {
  attachments: api.Attachment[];
  uploadQueue?: { pending: number; tail: Promise<void> };
};
type DraftEntry = AttachmentDraft & {
  text: string;
  pending?: PendingSubmission;
};
/** One draft serializes uploads across mounted views. */
export async function queueAttachmentUploads(entry: AttachmentDraft, files: FileList | File[], options: {
  signal: AbortSignal;
  isCurrent: () => boolean;
  upload: (file: File, signal: AbortSignal) => Promise<api.Attachment>;
  onChanged: () => void;
}): Promise<void> {
  if (!Array.isArray(files) && !(typeof FileList !== "undefined" && files instanceof FileList))
    throw new Error("Attachments must be a file array or FileList");
  const selected = Array.from(files);
  if (!selected.length) return;
  const queue = entry.uploadQueue ??= { pending: 0, tail: Promise.resolve() };
  const ensureCurrent = () => {
    if (options.signal.aborted) throw options.signal.reason ?? new DOMException("Aborted", "AbortError");
    if (!options.isCurrent()) throw new DOMException("Conversation changed", "AbortError");
  };
  ensureCurrent();
  queue.pending += 1;
  options.onChanged();
  const task = queue.tail.then(async () => {
    for (const file of selected) {
      ensureCurrent();
      const attachment = await options.upload(file, options.signal);
      ensureCurrent();
      entry.attachments.push(attachment);
      options.onChanged();
    }
  });
  const finished = task.finally(() => {
    queue.pending -= 1;
    options.onChanged();
  });
  queue.tail = finished.catch(() => {});
  return finished;
}
const sessionDrafts = new Map<string, DraftEntry>();
const draftListeners = new Map<string, Set<() => void>>();
function notifyDraft(key: string): void { const entry = sessionDrafts.get(key); if (entry) saveDraft(key, key.slice(key.lastIndexOf(":") + 1), entry); for (const notify of draftListeners.get(key) ?? [])
  notify(); }
export function clearConversationDrafts(): void { sessionDrafts.clear(); clearStoredDrafts(); for (const key of draftListeners.keys())
  notifyDraft(key); }
const errorText = (error: unknown) => error instanceof Error ? error.message : "Request failed";
export type ConversationOptions = {
  enabled?: boolean;
  onAccessRevoked?: () => void;
  sessionKey?: string;
  includeActivity?: boolean;
};
export function sameAccessSession(expected: string | undefined, actual: api.Access): boolean {
  return !expected || expected === api.accessKey(actual);
}
export function useActorConversation(actorId: string | null, options: ConversationOptions = {}) {
  const enabled = options.enabled ?? true;
  const draftKey = `${options.sessionKey ?? "session"}:${actorId ?? ""}`;
  const [state, dispatch] = useReducer(conversationReducer, initialConversation(actorId));
  const stateRef = useRef(state);
  stateRef.current = state;
  const epoch = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const loadingTools = useRef(new Set<string>());
  const onAccessRevoked = useRef(options.onAccessRevoked);
  onAccessRevoked.current = options.onAccessRevoked;
  const [draft, setDraftState] = useState("");
  const [attachments, setAttachments] = useState<api.Attachment[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [sending, setSending] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [streamStatus, setStreamStatus] = useState<api.StreamStatus>("closed");
  const [toolDetails, setToolDetails] = useState<Record<string, api.ToolResult>>({});
  const [reload, setReload] = useState(0);
  const draftEntry = useCallback(() => { let entry = sessionDrafts.get(draftKey); if (!entry) {
    entry = actorId ? loadDraft(draftKey, actorId) || { text: "", attachments: [] } : { text: "", attachments: [] };
    sessionDrafts.set(draftKey, entry);
  } return entry; }, [draftKey, actorId]);
  const persistDraft = useCallback(() => { if (actorId) saveDraft(draftKey, actorId, draftEntry()); }, [draftKey, actorId, draftEntry]);
  const setDraft = useCallback((text: string) => { draftEntry().text = text; persistDraft(); setDraftState(text); notifyDraft(draftKey); }, [draftEntry, draftKey, persistDraft]);
  const valid = useCallback((generation: number) => generation === epoch.current, []);
  const accessFailure = useCallback(async (generation: number, signal: AbortSignal, failure?: unknown) => {
    try {
      const access = await api.getAccess({ signal });
      if (!valid(generation)) return;
      dispatch({ type: "reset", actorId });
      setError(failure ? errorText(failure) : t("errors.forbidden"));
      if (!sameAccessSession(options.sessionKey, access)) onAccessRevoked.current?.();
    } catch (failure) {
      if (valid(generation) && !signal.aborted) {
        if (failure instanceof api.ActorApiError && [401, 403, 410].includes(failure.status)) {
          dispatch({ type: "reset", actorId: null }); onAccessRevoked.current?.();
        } else setError(errorText(failure));
      }
    }
  }, [actorId, options.sessionKey, valid]);
  const loadTools = useCallback(async (messageId: string, before?: number) => {
    const inFlight = loadingTools.current;
    if (!actorId || stateRef.current.actorId !== actorId || inFlight.has(messageId))
      return;
    const generation = epoch.current;
    inFlight.add(messageId);
    try {
      const page = await api.listToolSummaries(actorId, messageId, { before, signal: controller.current?.signal });
      if (valid(generation))
        dispatch({ type: "tools", actorId, messageId, nextCursor: page.next_cursor, tools: page.items });
      return page;
    } finally { inFlight.delete(messageId); }
  }, [actorId, valid]);
  const loadMoreTools = useCallback(async (messageId: string) => {
    const before = stateRef.current.toolNext[messageId];
    if (before === undefined || before === null) return;
    return loadTools(messageId, before);
  }, [loadTools]);
  useEffect(() => {
    const sync = () => { const entry = draftEntry(); setDraftState(entry.text); setAttachments([...entry.attachments]); setUploading(Boolean(entry.uploadQueue?.pending)); };
    let listeners = draftListeners.get(draftKey);
    if (!listeners) {
      listeners = new Set();
      draftListeners.set(draftKey, listeners);
    }
    listeners.add(sync);
    return () => { listeners.delete(sync); if (!listeners.size)
      draftListeners.delete(draftKey); };
  }, [draftKey, draftEntry]);
  useEffect(() => {
    const generation = ++epoch.current;
    const abort = new AbortController();
    controller.current = abort;
    loadingTools.current = new Set();
    dispatch({ type: "reset", actorId });
    setToolDetails({});
    setError(null);
    setSending(false);
    setUploading(false);
    setLoadingOlder(false);
    const entry = draftEntry();
    setDraftState(entry.text);
    setAttachments([...entry.attachments]);
    setUploading(Boolean(entry.uploadQueue?.pending));
    if (!actorId || !enabled) {
      setLoading(false);
      setStreamStatus("closed");
      return () => abort.abort();
    }
    setLoading(true);
    async function initialize() {
      try {
        // Capture the history cursor first; Actor changes during subsequent
        // reads are replayed by the stream and deduplicated by event ID.
        const page = await api.listMessages(actorId!, { signal: abort.signal });
        const actor = await api.getActor(actorId!, { signal: abort.signal });
        if (!valid(generation))
          return;
        dispatch({ type: "page", page, initial: true });
        dispatch({ type: "actor", actor });
        if (entry.attachments.length) {
          const original = [...entry.attachments];
          void Promise.all(original.map(async item => ({ item, valid: await api.validateAttachment(actorId!, item, { signal: abort.signal }) }))).then(checked => {
            if (!valid(generation) || abort.signal.aborted) return;
            const expired = new Set(checked.filter(item => !item.valid).map(({ item }) => item.id));
            if (expired.size) { entry.attachments = entry.attachments.filter(item => !expired.has(item.id)); delete entry.pending; notifyDraft(draftKey); setError(t("use_actor_conversation.some_attachments_are_unavailable_your_draft_is_retained_attach_t")); }
          }).catch(() => {});
        }
        setLoading(false);
        // Ancillary history loads must not prevent the live subscription.
        void (async () => {
          const approvalPage = await api.listApprovals(actorId!, { limit: 30, signal: abort.signal });
          if (!valid(generation))
            return;
          for (const approval of approvalPage.items)
            dispatch({ type: "approval", approval });
          // Small references only. Details remain unloaded until explicitly opened.
          for (let start = 0; start < page.items.length; start += 4)
            await Promise.all(page.items.slice(start, start + 4).map(async (message) => {
              const refs = await api.listToolSummaries(actorId!, message.id, { limit: 30, signal: abort.signal });
              if (valid(generation))
                dispatch({ type: "tools", actorId: actorId!, messageId: message.id, nextCursor: refs.next_cursor, tools: refs.items });
            }));
        })().catch(failure => { if (valid(generation) && !abort.signal.aborted)
          setError(errorText(failure)); });
        await api.subscribeActorEvents(actorId!, { after: page.event_cursor ?? 0, signal: abort.signal,
          onStatus: status => { if (valid(generation))
            setStreamStatus(status); },
          onAccessRevoked: () => { if (valid(generation)) void accessFailure(generation, abort.signal); },
          onResync: () => { if (valid(generation)) setReload(value => value + 1); },
          onCursor: cursor => { if (valid(generation))
            dispatch({ type: "cursor", cursor }); },
          onEvent: async (record) => {
            if (!valid(generation))
              return;
            const event = record.event;
            if (event.type === "conversation_restored") {
              setReload(value => value + 1);
              return;
            }
            if (event.type === "actor_changed") {
              const actor = await api.getActor(actorId!, { signal: abort.signal });
              if (!valid(generation)) return;
              dispatch({ type: "actor", actor });
            }
            if (event.type === "message_added" || event.type === "message_updated") {
              const message = await api.getMessage(actorId!, event.message_id, { signal: abort.signal });
              if (!valid(generation))
                return;
              dispatch({ type: "message", message });
            }
            if (event.type === "approval_requested" || event.type === "approval_resolved") {
              const approval = await api.getApproval(event.approval_id, actorId!, { signal: abort.signal });
              if (!valid(generation))
                return;
              dispatch({ type: "approval", approval });
            }
            if (options.includeActivity !== false || !["generation_started", "generation_delta", "tool_started"].includes(event.type)) dispatch({ type: "event", record });
          },
        });
      }
      catch (failure) {
        if (valid(generation) && !abort.signal.aborted) {
          setError(errorText(failure));
          setLoading(false);
          if (failure instanceof api.ActorApiError && [401, 403, 410].includes(failure.status))
            void accessFailure(generation, abort.signal, failure);
        }
      }
    }
    void initialize();
    return () => { abort.abort(); if (controller.current === abort)
      controller.current = null; };
  }, [actorId, enabled, draftEntry, valid, reload, accessFailure, options.includeActivity, draftKey]);
  const loadOlder = useCallback(async () => {
    const before = stateRef.current.nextCursor;
    if (!actorId || before === null || loadingOlder)
      return;
    const generation = epoch.current;
    setLoadingOlder(true);
    try {
      const page = await api.listMessages(actorId, { before, signal: controller.current?.signal });
      if (!valid(generation))
        return;
      dispatch({ type: "page", page });
      for (let start = 0; start < page.items.length; start += 4)
        await Promise.all(page.items.slice(start, start + 4).map(message => loadTools(message.id)));
    }
    catch (failure) {
      if (valid(generation))
        setError(errorText(failure));
    }
    finally {
      if (valid(generation))
        setLoadingOlder(false);
    }
  }, [actorId, loadingOlder, valid, loadTools]);
  const refreshActor = useCallback(async () => {
    if (!actorId)
      return;
    const generation = epoch.current;
    const actor = await api.getActor(actorId, { signal: controller.current?.signal });
    if (valid(generation))
      dispatch({ type: "actor", actor });
    return actor;
  }, [actorId, valid]);
  const refresh = useCallback(async () => { setReload(value => value + 1); }, []);
  const send = useCallback(async (content?: string, attachmentIds?: string[]) => {
    if (!actorId || sending || !enabled || stateRef.current.actorId !== actorId)
      return;
    const generation = epoch.current;
    const entry = draftEntry();
    const submission = submissionFor(entry.pending, content ?? entry.text, attachmentIds ?? entry.attachments.map(item => item.id));
    if (!submission.content.trim() && submission.attachments.length === 0)
      return;
    entry.pending = submission;
    persistDraft();
    setSending(true);
    setError(null);
    try {
      const message = await api.sendMessage(actorId, { content: submission.content, attachments: submission.attachments }, submission.key, { signal: controller.current?.signal });
      if (entry.pending?.key === submission.key)
        delete entry.pending;
      if (entry.text === submission.content && JSON.stringify(entry.attachments.map(item => item.id)) === JSON.stringify(submission.attachments)) {
        entry.text = "";
        entry.attachments = [];
      }
      notifyDraft(draftKey);
      if (valid(generation)) {
        dispatch({ type: "message", message });
        setDraftState(entry.text);
        setAttachments([...entry.attachments]);
      }
      return message;
    }
    catch (failure) {
      if (valid(generation))
        setError(errorText(failure));
      throw failure;
    }
    finally {
      if (valid(generation))
        setSending(false);
    }
  }, [actorId, sending, draftEntry, draftKey, enabled, valid, persistDraft]);
  const cancel = useCallback(async (messageId: string) => {
    if (!actorId)
      return;
    const generation = epoch.current;
    try {
      await api.cancelMessage(actorId, messageId);
      const message = await api.getMessage(actorId, messageId);
      if (valid(generation))
        dispatch({ type: "message", message });
    }
    catch (failure) {
      if (valid(generation))
        setError(errorText(failure));
      throw failure;
    }
  }, [actorId, valid]);
  const addAttachments = useCallback(async (files: FileList | File[]) => {
    if (!actorId || !enabled || stateRef.current.actorId !== actorId)
      return;
    const generation = epoch.current;
    const signal = controller.current?.signal;
    if (!signal || signal.aborted) return;
    const entry = draftEntry();
    try {
      await queueAttachmentUploads(entry, files, {
        signal,
        isCurrent: () => valid(generation),
        upload: (file, uploadSignal) => api.uploadAttachment(actorId, file, { signal: uploadSignal }),
        onChanged: () => notifyDraft(draftKey),
      });
    }
    catch (failure) {
      if (valid(generation) && !signal.aborted)
        setError(errorText(failure));
      throw failure;
    }
  }, [actorId, enabled, draftEntry, draftKey, valid]);
  const removeAttachment = useCallback((id: string) => { const entry = draftEntry(); entry.attachments = entry.attachments.filter(item => item.id !== id); setAttachments([...entry.attachments]); notifyDraft(draftKey); }, [draftEntry, draftKey]);
  const amend = useCallback(async (message: api.MessageRecord, content?: string) => {
    if (!actorId) return;
    const generation = epoch.current;
    const updated = content === undefined ? await api.withdrawQueuedMessage(actorId, message) : await api.editQueuedMessage(actorId, message, content);
    if (valid(generation)) dispatch({ type: "message", message: updated });
    return updated;
  }, [actorId, valid]);
  const steer = useCallback(async (message: api.MessageRecord, target: string) => {
    if (!actorId) return;
    const generation = epoch.current;
    const updated = await api.steerQueuedMessage(actorId, message, target);
    if (valid(generation)) dispatch({ type: "message", message: updated });
    return updated;
  }, [actorId, valid]);
  const loadQueue = useCallback(async (after?: number) => {
    if (!actorId) return;
    const generation = epoch.current;
    const page = await api.listQueuedMessages(actorId, { after, signal: controller.current?.signal });
    if (valid(generation)) dispatch({ type: "queue", page });
  }, [actorId, valid]);
  useEffect(() => {
    let live = true;
    if (enabled && actorId && !loading) void loadQueue().catch(failure => { if (live) setError(errorText(failure)); });
    return () => { live = false; };
  }, [actorId, enabled, loading, loadQueue, reload]);
  const clearQueue = useCallback(async () => { if (!actorId) return; const result = await api.clearQueuedMessages(actorId); await refresh(); return result; }, [actorId, refresh]);
  const loadToolDetails = useCallback(async (resultId: string) => {
    if (!actorId)
      return;
    if (toolDetails[resultId])
      return toolDetails[resultId];
    const generation = epoch.current;
    const detail = await api.getToolResult(actorId, resultId, { signal: controller.current?.signal });
    if (valid(generation))
      setToolDetails(previous => ({ ...previous, [resultId]: detail }));
    return detail;
  }, [actorId, toolDetails, valid]);
  const live = useMemo(() => new Map(Object.entries(state.generations)), [state.generations]);
  const current = state.actorId === actorId && enabled;
  return { ...(current ? state : initialConversation(actorId)), live: current ? live : new Map<string, GenerationDraft>(),
    draft: current ? draft : draftEntry().text, setDraft, attachments: current ? attachments : draftEntry().attachments, uploading, addAttachments, removeAttachment,
    loading: loading || Boolean(actorId && enabled && !current), loadingOlder, hasMore: current && state.nextCursor !== null, more: current && state.nextCursor !== null, loadOlder, loadMore: loadOlder,
    sending, send, cancel, amend, steer, loadQueue, clearQueue, error, streamStatus, connected: streamStatus === "connected", refresh, refreshActor,
    loadTools, loadMoreTools, loadToolDetails, fetchResult: loadToolDetails, toolDetails: current ? toolDetails : {} };
}
