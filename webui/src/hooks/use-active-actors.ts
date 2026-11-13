import { useCallback, useEffect, useRef, useState } from "react";
import { ActorApiError, subscribeCatalogEvents, type EventRecord, type SessionSummary } from "@/lib/actor-api";
import { getSession, listRunningSessionActors } from "@/lib/session-api";
import { actorIdFromUrl, localActorId } from "@/lib/conversation-links";
import { delegationStatuses } from "@/lib/delegations";

export type ActiveActor = { id: string; name: string };
export type SessionWork = { session: SessionSummary | null; missing: boolean; actors: ActiveActor[] };

const remoteStage = (address: string) => address.includes("~") ? address.split("~")[0] : null;
const denied = (error: unknown) => error instanceof ActorApiError && [401, 403, 410].includes(error.status);

/**
 * The open session's summary and running workers. Its host's resumable catalog
 * stream refreshes workers and feeds task status facts, including remote hosts.
 */
export function useSessionWork(accountSession: string | null, address: string | null, onAccessRevoked: () => void) {
  const [work, setWork] = useState<SessionWork & {address: string | null}>({address: null, session: null, missing: false, actors: []});
  const refreshRef = useRef<() => void>(() => {});
  const revoked = useRef(onAccessRevoked);
  revoked.current = onAccessRevoked;

  useEffect(() => {
    setWork({address, session: null, missing: false, actors: []});
    if (!accountSession || !address) { refreshRef.current = () => {}; return; }
    const controller = new AbortController();
    const stage = remoteStage(address);
    const localId = stage ? address.slice(stage.length + 1) : address;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pending = false, again = false, streaming = false;
    const schedule = (delay = 400) => { clearTimeout(timer); timer = setTimeout(() => { void load(); }, delay); };
    async function stream(after: number) {
      streaming = true;
      let resync = false;
      try {
        await subscribeCatalogEvents({after, signal: controller.signal,
          onEvent: record => {
            delegationStatuses.observe(stage ? {...record, actor_id: localActorId(`${stage}~${record.actor_id}`)} : record);
            observe(record);
          },
          onAccessRevoked: () => revoked.current(),
          onResync: () => { resync = true; },
        }, stage ?? undefined);
      } catch (error) {
        if (controller.signal.aborted) return;
        if (denied(error)) { revoked.current(); return; }
        schedule(5000);
      } finally {
        streaming = false;
      }
      if (resync && !controller.signal.aborted) { delegationStatuses.refreshUnfinished(); schedule(0); }
    }
    async function load() {
      if (pending) { again = true; return; }
      pending = true;
      try {
        const page = await listRunningSessionActors(address!, {signal: controller.signal});
        const session = await getSession(address!, {signal: controller.signal});
        if (controller.signal.aborted) return;
        const actors = page.items.map(actor => ({
          id: localActorId(actorIdFromUrl(actor.conversation_url) ?? (stage ? `${stage}~${actor.id}` : actor.id)), name: actor.name,
        }));
        setWork({address, session, missing: false, actors});
        if (!streaming) void stream(page.event_cursor ?? 0);
      } catch (error) {
        if (controller.signal.aborted) return;
        if (denied(error)) { revoked.current(); return; }
        if (error instanceof ActorApiError && error.status === 404) { setWork({address, session: null, missing: true, actors: []}); return; }
        schedule(5000);
      } finally {
        pending = false;
        if (again && !controller.signal.aborted) { again = false; schedule(); }
      }
    }
    function observe(record: EventRecord) {
      const event = record.event;
      if (event.type === "session_changed" && event.session.id === localId) setWork(old => old.address === address ? {...old, session: event.session, missing: false} : old);
      if (event.type === "session_deleted" && event.session_id === localId) setWork({address, session: null, missing: true, actors: []});
      if ((event.type === "actor_changed" && event.actor.session_id === localId) || event.type === "actor_deleted" || (event.type === "session_changed" && event.session.id === localId) || (event.type === "session_deleted" && event.session_id === localId)) schedule();
    }
    refreshRef.current = () => { schedule(); delegationStatuses.refreshUnfinished(); };
    const resume = () => { if (document.visibilityState === "visible") refreshRef.current(); };
    document.addEventListener("visibilitychange", resume);
    void load();
    return () => {
      controller.abort(); clearTimeout(timer);
      refreshRef.current = () => {};
      document.removeEventListener("visibilitychange", resume);
    };
  }, [accountSession, address]);

  const refresh = useCallback(() => refreshRef.current(), []);
  /** Apply a summary returned by a session mutation before its catalog event arrives. */
  const update = useCallback((session: SessionSummary) => setWork(old => old.session && old.session.id === session.id ? {...old, session} : old), []);
  const current = work.address === address ? work : {session: null, missing: false, actors: []};
  return {...current, refresh, update};
}
