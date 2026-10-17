import { useEffect, useRef, useState } from "react";
import { ArrowLeftIcon, ArrowUpRightIcon, CheckIcon, CircleAlertIcon, ChevronRightIcon, RefreshCwIcon, SearchIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { CodexMark } from "@/components/import-conversation";
import { listCodexConversations, previewCodexImport, submitCodexImport, type CodexConversation, type ImportPreview } from "@/lib/conversation-import";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import { errorText } from "./controls";

export function ImportSettings({onViewCore}: {onViewCore(): void}) {
  const [source, setSource] = useState(false);
  const [items, setItems] = useState<CodexConversation[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState<"reading" | "sending" | null>(null);
  const [error, setError] = useState("");
  const [sent, setSent] = useState(false);
  const listController = useRef<AbortController | null>(null);
  const selectionController = useRef<AbortController | null>(null);
  useEffect(() => () => {listController.current?.abort();selectionController.current?.abort();}, []);
  async function load(after?: string, refresh = false) {
    listController.current?.abort();
    const current = new AbortController();listController.current=current;
    setLoading(true);setError("");
    if (!after) {selectionController.current?.abort();setPreview(null);setSelected(null);setSent(false);setBusy(null);setItems([]);setCursor(null);}
    try {const page=await listCodexConversations(query,after,refresh,current.signal);if(!current.signal.aborted){setItems(previous=>after?[...previous,...page.items]:page.items);setCursor(page.next_cursor);}}
    catch(cause){if(!current.signal.aborted)setError(errorText(cause));}
    finally{if(!current.signal.aborted)setLoading(false);}
  }
  useEffect(() => {
    if(!source)return;
    const timer=setTimeout(()=>void load(),180);
    return ()=>{clearTimeout(timer);listController.current?.abort();};
  },[source,query]);
  function search(value: string) {
    listController.current?.abort();selectionController.current?.abort();
    setQuery(value);setItems([]);setCursor(null);setSelected(null);setPreview(null);setSent(false);setBusy(null);setLoading(true);setError("");
  }
  async function choose(item: CodexConversation) {
    if(busy==="sending")return;
    selectionController.current?.abort();
    const current=new AbortController();selectionController.current=current;
    setSelected(item.id);setPreview(null);setSent(false);setBusy("reading");setError("");
    try {const result=await previewCodexImport(item.id,current.signal);if(!current.signal.aborted)setPreview(result);}
    catch(cause){if(!current.signal.aborted)setError(errorText(cause));}
    finally{if(!current.signal.aborted)setBusy(null);}
  }
  async function submit() {
    if(!preview||busy||sent)return;
    setBusy("sending");setError("");
    try {await submitCodexImport(preview.resource_id);setSent(true);}
    catch(cause){setError(errorText(cause));}
    finally{setBusy(null);}
  }
  return <div className="dl-import-settings">
    {!source ? <><p className="dl-import-intro">{t("imports.intro")}</p>
      <section aria-labelledby="dl-import-sources"><h2 id="dl-import-sources">{t("imports.sources")}</h2>
        <div className="dl-import-sources"><button className="dl-import-source dl-import-source-link" onClick={()=>{setSource(true);search("");}}><CodexMark/><strong>Codex</strong><ChevronRightIcon aria-hidden="true"/></button></div>
      </section></> : <section aria-label={t("imports.codex_conversations")}>
      <header className="dl-import-list-heading"><Button variant="ghost" size="icon-sm" disabled={busy==="sending"} aria-label={t("imports.back")} onClick={()=>{setSource(false);setQuery("");selectionController.current?.abort();setBusy(null);}}><ArrowLeftIcon/></Button><h2>Codex</h2></header>
      <div className="dl-import-list-controls"><label className="dl-import-search"><SearchIcon aria-hidden="true"/><input type="search" value={query} disabled={busy==="sending"} aria-label={t("imports.search")} placeholder={t("imports.search")} onChange={event=>search(event.target.value)}/></label><Button variant="ghost" size="icon" aria-label={t("imports.refresh")} disabled={loading||busy==="sending"} onClick={()=>void load(undefined,true)}><RefreshCwIcon/></Button></div>
      {items.length>0&&<ul className="dl-import-conversations">{items.map(item=><li key={item.id}><button aria-pressed={selected===item.id} disabled={busy==="sending"} onClick={()=>void choose(item)}><span><strong>{item.title}</strong><span>{new Date(item.updated_at_ms).toLocaleString(getCurrentWebUiLanguage(),{dateStyle:"medium",timeStyle:"short"})}{item.archived&&` · ${t("imports.archived")}`}</span></span>{selected===item.id?(busy==="reading"?<Spinner/>:preview?<CheckIcon/>:<CircleAlertIcon/>):<span className="dl-import-selection" aria-hidden="true"/>}</button></li>)}</ul>}
      {loading&&<div className="dl-import-list-status" role="status"><Spinner/>{t("imports.loading")}</div>}
      {!loading&&!items.length&&!error&&<p className="dl-import-empty">{t(query?"imports.no_results":"imports.empty")}</p>}
      {cursor&&!loading&&<Button variant="ghost" disabled={busy==="sending"} onClick={()=>void load(cursor)}>{t("imports.load_more")}</Button>}
      {error&&<div className="dl-import-error"><p role="alert" className="dl-operation-error">{error}</p><Button variant="ghost" size="sm" disabled={loading||busy==="sending"} onClick={()=>void load(undefined,true)}>{t("imports.refresh")}</Button></div>}
      {preview&&<footer className="dl-import-confirm"><div><strong>{preview.title}</strong><p>{t(sent?"imports.submitted":"imports.idle_note")}</p>{!sent&&<span>{t("imports.message_count",{count:preview.message_count})}</span>}</div>{sent?<Button onClick={onViewCore}>{t("imports.view_core")}<ArrowUpRightIcon/></Button>:<Button disabled={!!busy} onClick={()=>void submit()}>{busy==="sending"&&<Spinner/>}{t(busy==="sending"?"imports.sending":"imports.import")}</Button>}</footer>}
    </section>}
  </div>;
}
