import { useEffect, useState } from "react";
import { ActorApiError } from "@/lib/actor-api";
import { getStageStatus, type CoreStatus } from "@/lib/stage-api";

/** Only the small readiness fact is polled; conversation data stays on its cursor stream. */
export function useCoreStatus(session: string | null, onAccessRevoked: () => void) {
  const [value, setValue] = useState<{session: string; core: CoreStatus | null} | null>(null);
  useEffect(() => {
    if (!session) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function refresh() {
      try {
        const status = await getStageStatus({signal: AbortSignal.any([controller.signal, AbortSignal.timeout(5000)])});
        if (!controller.signal.aborted) setValue({session: session!, core: status.core});
      } catch (error) {
        if (controller.signal.aborted) return;
        setValue(null);
        if (error instanceof ActorApiError && [401,403,410].includes(error.status)) { onAccessRevoked(); return; }
      }
      if (!controller.signal.aborted) timer = setTimeout(() => { void refresh(); }, 2000);
    }
    void refresh();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [session, onAccessRevoked]);
  return value?.session === session ? value?.core : undefined;
}
