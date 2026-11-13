import type { ActorEvent, SessionSummary } from "./actor-api";

/** One device's page sequence of sessions; `stage` is null for the device serving this WebUI. */
export type SessionSource = {
  stage: string | null;
  label: string | null;
  items: SessionSummary[];
  next: string | null;
  failed: boolean;
};
export type SessionEntry = { address: string; session: SessionSummary; source: SessionSource };

const textOrder = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
/** Match the host's SQLite order, including sessions with identical activity times. */
export const compareSessions = (a: SessionSummary, b: SessionSummary) => b.last_activity_at_ms - a.last_activity_at_ms || textOrder(b.id, a.id);
const pending = (source: SessionSource) => !source.failed && source.next !== null;
const entryOf = (source: SessionSource, session: SessionSummary): SessionEntry => ({ address: source.stage ? `${source.stage}~${session.id}` : session.id, session, source });
const entryOrder = (a: SessionEntry, b: SessionEntry) => compareSessions(a.session, b.session) || textOrder(a.address, b.address);
const oldest = (source: SessionSource) => source.items.at(-1);

/**
 * Each source pages independently in descending activity order. A merged row is
 * only final once every source with more pages has reached its full sort key.
 * Equal timestamps must not let unloaded IDs reorder already-visible rows.
 */
export function sessionBoundary(sources: SessionSource[]): SessionEntry | null {
  let boundary: SessionEntry | null = null;
  for (const source of sources) if (pending(source)) {
    const session = oldest(source);
    if (!session) continue;
    const entry = entryOf(source, session);
    if (!boundary || entryOrder(entry, boundary) < 0) boundary = entry;
  }
  return boundary;
}

/** The source whose next page extends the visible boundary. */
export function nextSessionSource(sources: SessionSource[]): SessionSource | null {
  let found: SessionSource | null = null;
  for (const source of sources) if (pending(source)) {
    const session = oldest(source);
    if (!session) return source;
    const current = found && oldest(found);
    if (!found || !current || entryOrder(entryOf(source, session), entryOf(found, current)) < 0) found = source;
  }
  return found;
}

export function visibleSessions(sources: SessionSource[]): SessionEntry[] {
  if (sources.some(source => pending(source) && !source.items.length)) return [];
  const boundary = sessionBoundary(sources);
  const entries: SessionEntry[] = [];
  for (const source of sources) for (const session of source.items) {
    const entry = entryOf(source, session);
    if (boundary === null || entryOrder(entry, boundary) <= 0) entries.push(entry);
  }
  return entries.sort(entryOrder);
}

/** Insert or replace a session within the range this source has already loaded. */
export function upsertSession(source: SessionSource, session: SessionSummary, archived: boolean): SessionSource {
  const items = source.items.filter(item => item.id !== session.id);
  const belongs = (session.status === "archived") === archived;
  const last = oldest(source);
  const loaded = source.next === null || (last !== undefined && compareSessions(session, last) <= 0);
  if (belongs && loaded) items.push(session);
  return { ...source, items: items.sort(compareSessions) };
}

export function applySessionEvent(source: SessionSource, event: ActorEvent, archived: boolean): SessionSource {
  if (event.type === "session_changed") return upsertSession(source, event.session, archived);
  if (event.type === "session_deleted" && source.items.some(item => item.id === event.session_id))
    return { ...source, items: source.items.filter(item => item.id !== event.session_id) };
  return source;
}

export const sessionCursorKey = (sources: SessionSource[]) => sources.map(source => `${source.stage ?? ""}:${source.next ?? ""}`).join("|");
