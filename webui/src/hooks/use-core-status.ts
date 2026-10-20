import { useEffect, useState } from "react";
import { ActorApiError } from "@/lib/actor-api";
import { getStageStatus, type CoreStatus } from "@/lib/stage-api";
import { clearConversationDrafts } from "./use-actor-conversation";

/** Only the small readiness fact is polled; conversation data stays on its cursor stream. */
export function useCoreStatus(session: string | null, onAccessRevoked: () => void) {
  const [value, setValue] = useState<{session: string; core: CoreStatus | null} | null>(null);
  useEffect(() => {
    if (!session) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let request: AbortController | undefined;
    let generation: string | undefined;
    async function refresh() {
      clearTimeout(timer);
      request?.abort();
      const current = new AbortController();
      request = current;
      try {
        const status = await getStageStatus({signal: AbortSignal.any([controller.signal, current.signal, AbortSignal.timeout(5000)])});
        if (controller.signal.aborted || current.signal.aborted) return;
        const next = JSON.stringify(status.work_generation ?? null);
        if (generation !== undefined && generation !== next) {
          clearConversationDrafts();
          window.location.assign("/");
          return;
        }
        generation = next;
        if (!controller.signal.aborted) setValue({session: session!, core: status.core});
      } catch (error) {
        if (controller.signal.aborted || current.signal.aborted) return;
        setValue({session: session!, core: null});
        if (error instanceof ActorApiError && [401,403,410].includes(error.status)) { onAccessRevoked(); return; }
      }
      if (!controller.signal.aborted && !current.signal.aborted) timer = setTimeout(() => { void refresh(); }, 2000);
    }
    function resume() { if (document.visibilityState === "visible") void refresh(); }
    document.addEventListener("visibilitychange", resume);
    window.addEventListener("pageshow", resume);
    void refresh();
    return () => {
      controller.abort(); clearTimeout(timer);
      document.removeEventListener("visibilitychange", resume);
      window.removeEventListener("pageshow", resume);
    };
  }, [session, onAccessRevoked]);
  return value?.session === session ? value?.core : undefined;
}
