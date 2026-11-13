import { query, request, sessionPath, type ActorSummary, type FetchOptions, type Page, type SessionSummary } from "./actor-api";

export type SessionPage = Page<SessionSummary, string>;
export type SessionPatch = { title?: string | null; model?: string | null };

const stageSessions = (stage?: string) => stage ? `/v1/stages/${encodeURIComponent(stage)}/sessions` : "/v1/sessions";

/** The WebUI address of a session: local ids stay bare, remote ids carry their device. */
export const sessionAddress = (session: SessionSummary, remoteStage?: string) => remoteStage ? `${remoteStage}~${session.id}` : session.id;

export const listSessions = (options: FetchOptions & { archived?: boolean; after?: string; limit?: number; stage?: string } = {}) =>
  request<SessionPage>(query(stageSessions(options.stage), { archived: options.archived ?? false, after: options.after, limit: options.limit ?? 50 }), options);
export const createSession = (input: SessionPatch, idempotencyKey: string, options?: FetchOptions) =>
  request<SessionSummary>("/v1/sessions", { ...options, method: "POST", body: input, headers: { "Idempotency-Key": idempotencyKey } });
export const getSession = (id: string, options?: FetchOptions) => request<SessionSummary>(sessionPath(id), options);
export const patchSession = (id: string, patch: SessionPatch, options?: FetchOptions) =>
  request<SessionSummary>(sessionPath(id), { ...options, method: "PATCH", body: patch });
export const archiveSession = (id: string, options?: FetchOptions) => request<SessionSummary>(`${sessionPath(id)}/archive`, { ...options, method: "POST" });
export const unarchiveSession = (id: string, options?: FetchOptions) => request<SessionSummary>(`${sessionPath(id)}/unarchive`, { ...options, method: "POST" });
export const deleteSession = (id: string, options?: FetchOptions) => request<void>(sessionPath(id), { ...options, method: "DELETE", empty: true });
export const listSessionActors = (id: string, options: FetchOptions & { running?: boolean; after?: string; limit?: number } = {}) =>
  request<Page<ActorSummary, string>>(query(`${sessionPath(id)}/actors`, { status: options.running ? "running" : undefined, after: options.after, limit: options.limit ?? 50 }), options);

/** Read bounded worker pages, retaining the first cursor so subsequent changes can be replayed. */
export async function listRunningSessionActors(id: string, options?: FetchOptions): Promise<Page<ActorSummary, string>> {
  const first = await listSessionActors(id, {...options, running: true});
  const items = new Map(first.items.map(actor => [actor.id, actor]));
  let after = first.next_cursor;
  while (after !== null) {
    const page = await listSessionActors(id, {...options, running: true, after});
    for (const actor of page.items) items.set(actor.id, actor);
    after = page.next_cursor;
  }
  return {...first, items: [...items.values()], next_cursor: null};
}
