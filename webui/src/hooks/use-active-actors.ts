import { useCallback, useEffect, useRef, useState } from "react";
import { ActorApiError, subscribeCatalogEvents } from "@/lib/actor-api";
import { listContacts } from "@/lib/contacts";
import { actorIdFromUrl, localActorId } from "@/lib/conversation-links";
import { delegationStatuses } from "@/lib/delegations";

export type ActiveActor = { id: string; name: string };

/**
 * Actors working anywhere on the account, read through the bounded contacts
 * query. One catalog stream refreshes it and feeds local task status facts.
 */
export function useActiveActors(session: string | null, primaryActor: string, onAccessRevoked: () => void) {
  const [actors, setActors] = useState<ActiveActor[]>([]);
  const refreshRef = useRef<() => void>(() => {});
  useEffect(() => {
    setActors([]);
    if (!session) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pending = false, again = false, streaming = false;
    const schedule = (delay = 400) => { clearTimeout(timer); timer = setTimeout(() => { void load(); }, delay); };
    const denied = (error: unknown) => error instanceof ActorApiError && [401, 403, 410].includes(error.status);
    async function load() {
      if (pending) { again = true; return; }
      pending = true;
      try {
        const page = await listContacts("", { sort: "recent", signal: controller.signal });
        if (controller.signal.aborted) return;
        setActors(page.items.filter(item => item.actor.status === "running").map(item => ({
          id: localActorId(actorIdFromUrl(item.actor.conversation_url) ?? item.actor.id), name: item.actor.name,
        })).filter(actor => actor.id !== primaryActor && !actor.id.endsWith("~core")));
        if (!streaming) void stream(page.event_cursor ?? 0);
      } catch (error) {
        if (controller.signal.aborted) return;
        if (denied(error)) { onAccessRevoked(); return; }
        schedule(5000);
      } finally {
        pending = false;
        if (again && !controller.signal.aborted) { again = false; schedule(); }
      }
    }
    async function stream(after: number) {
      streaming = true;
      let resync = false;
      await subscribeCatalogEvents({ after, signal: controller.signal,
        onEvent: record => {
          delegationStatuses.observe(record);
          if (["actor_changed", "actor_deleted", "conversation_restored"].includes(record.event.type)) schedule();
        },
        onAccessRevoked,
        onResync: () => { resync = true; },
      }).catch(() => {});
      streaming = false;
      if (resync && !controller.signal.aborted) { delegationStatuses.refreshUnfinished(); void load(); }
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
  }, [session, primaryActor, onAccessRevoked]);
  // Remote devices have no local catalog events; conversation activity triggers a reread.
  const refresh = useCallback(() => refreshRef.current(), []);
  return { actors, refresh };
}
