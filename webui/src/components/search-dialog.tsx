import { messageDisplayText } from "@/lib/conversation-import";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { ArrowUpRightIcon, SearchIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { searchMessages, type MessageRecord } from "@/lib/actor-api";
import { searchPreview } from "@/lib/search-preview";

function excerpt(value: string, query: string) { const position = value.toLocaleLowerCase().indexOf(query.toLocaleLowerCase()); const start = Math.max(0, position - 40); return `${start ? "…" : ""}${value.slice(start, start + 200)}${value.length > start + 200 ? "…" : ""}`; }
function highlight(value: string, query: string) { const index = value.toLocaleLowerCase().indexOf(query.toLocaleLowerCase()); return index < 0 ? value : <>{value.slice(0, index)}<mark>{value.slice(index, index + query.length)}</mark>{value.slice(index + query.length)}</>; }

export function SearchDialog({ open, onOpenChange, actorId, onSelect }: { open: boolean; onOpenChange(open: boolean): void; actorId: string; onSelect(message: MessageRecord): void }) {
  const [query, setQuery] = useState(""); const [items, setItems] = useState<MessageRecord[]>([]); const [more, setMore] = useState<number | null>(null); const [loading, setLoading] = useState(false); const [error, setError] = useState("");
  const epoch = useRef(0);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!open) { epoch.current++; return; }
    const generation = ++epoch.current; const controller = new AbortController();
    setItems([]); setMore(null); setError("");
    if (!query.trim()) { setLoading(false); return; }
    setLoading(true);
    const timer = setTimeout(() => { void searchMessages(actorId, query.trim(), { signal: controller.signal }).then(page => { if (generation === epoch.current) { setItems(page.items); setMore(page.next_cursor); } }).catch(failure => { if (failure?.name !== "AbortError" && generation === epoch.current) setError(t("search_dialog.search_failed_try_again")); }).finally(() => { if (generation === epoch.current) setLoading(false); }); }, 250);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [query, actorId, open, retry]);
  async function loadMore() { if (more === null || loading) return; const generation = epoch.current; setLoading(true); try { const page = await searchMessages(actorId, query.trim(), { before: more }); if (generation === epoch.current) { setItems(old => [...old, ...page.items]); setMore(page.next_cursor); } } catch { if (generation === epoch.current) setError(t("search_dialog.could_not_load_earlier_results")); } finally { if (generation === epoch.current) setLoading(false); } }
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="dl-search-dialog" aria-describedby={undefined}><DialogHeader><DialogTitle>{t("app.search_conversation")}</DialogTitle></DialogHeader>
    <InputGroup className="dl-search-input"><InputGroupAddon><SearchIcon aria-hidden="true" /></InputGroupAddon><InputGroupInput aria-label={t("app.search_conversation")} autoFocus value={query} onChange={event => setQuery(event.target.value)} placeholder={t("search_dialog.search")} /></InputGroup>
    <div className="dl-search-results">
      {loading && !items.length && <div className="flex flex-col gap-3"><Skeleton className="h-20 w-full rounded-xl" /><Skeleton className="h-20 w-full rounded-xl" /></div>}
      {!loading && !items.length && !error && <Empty><EmptyHeader><EmptyTitle>{query.trim() ? t("search_dialog.no_results") : t("search_dialog.search_this_conversation")}</EmptyTitle>{query.trim() && <EmptyDescription>{t("search_dialog.try_another_search")}</EmptyDescription>}</EmptyHeader></Empty>}
      {items.map(message => { const source = messageDisplayText(message) + "\n" + (message.reply || ""); const plain = searchPreview(source); const value = plain.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()) ? plain : source; return <button className="dl-search-result" key={message.id} onClick={() => { onSelect(message); onOpenChange(false); }}><span><small>{new Date(message.created_at_ms).toLocaleDateString(getCurrentWebUiLanguage())} · {message.source.kind === "user" ? t("search_dialog.messages_and_replies") : t("search_dialog.collaboration_messages")}</small><p>{highlight(excerpt(value, query.trim()), query.trim())}</p></span><ArrowUpRightIcon aria-hidden="true" /></button>; })}
      {more !== null && <Button variant="ghost" size="sm" disabled={loading} onClick={() => { void loadMore(); }}>{t("search_dialog.more_results")}</Button>}
      {error && <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" onClick={() => { setError(""); if (items.length && more !== null) void loadMore(); else setRetry(value => value + 1); }}>{t("app.retry")}</Button></div>}
    </div>
  </DialogContent></Dialog>;
}
