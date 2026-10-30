import { useEffect, useState } from "react";
import { getActor } from "@/lib/actor-api";

/** Known conversation names avoid a lookup; peer names use their scoped Actor URL. */
export function useActorName(id?: string | null, knownName?: string) {
  const [identity, setIdentity] = useState<{ id: string; name: string } | null>(null);
  useEffect(() => {
    if (!id || knownName) return;
    const abort = new AbortController();
    void getActor(id, { signal: abort.signal }).then(actor => {
      if (!abort.signal.aborted) setIdentity({ id, name: actor.config?.name || actor.name || id.split("~").at(-1)! });
    }).catch(() => {});
    return () => abort.abort();
  }, [id, knownName]);
  return knownName || (identity?.id === id ? identity?.name : id?.split("~").at(-1));
}
