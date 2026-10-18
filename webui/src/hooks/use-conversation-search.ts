import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { ConversationSearch } from "@/lib/conversation-search";

export function useConversationSearch(actor: string, query: string, enabled: boolean) {
  const search = useMemo(() => new ConversationSearch(actor), [actor]);
  const state = useSyncExternalStore(search.subscribe, search.getSnapshot, search.getSnapshot);
  const [retry, setRetry] = useState(0);
  const term = enabled ? query.trim() : "";
  useEffect(() => {
    if (!term) { void search.search(""); return; }
    const timer = setTimeout(() => { void search.search(term); }, 200);
    return () => { clearTimeout(timer); search.cancel(); };
  }, [search, term, retry]);
  const ready = state.query === term && !state.pending;
  return { ...state, ready, busy: Boolean(term) && !ready, more: (signal: AbortSignal) => search.more(signal), retry: () => setRetry(value => value + 1) };
}
