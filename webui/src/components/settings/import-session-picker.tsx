import { useEffect, useState } from "react";
import type { SessionSummary } from "@/lib/actor-api";
import { listSessions } from "@/lib/session-api";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import { LazyPage } from "@/components/lazy-page";
import { sessionTitle } from "@/components/session-actions";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { errorText } from "./controls";

/** Active sessions on this device, where the staged import resource lives. */
export function ImportSessionPicker({open, pending, error, onOpenChange, onChoose}: {
  open: boolean; pending: boolean; error: string; onOpenChange(open: boolean): void; onChoose(session: string): void;
}) {
  const [items, setItems] = useState<SessionSummary[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState("");
  async function load(after?: string, signal?: AbortSignal) {
    const page = await listSessions({after, limit: 30, signal});
    if (signal?.aborted) return;
    setItems(previous => after ? [...previous, ...page.items.filter(item => !previous.some(old => old.id === item.id))] : page.items);
    setCursor(page.next_cursor);
  }
  useEffect(() => {
    if (!open) return;
    const current = new AbortController();
    setItems([]); setCursor(null); setLoadError(""); setLoading(true);
    load(undefined, current.signal)
      .catch(cause => { if (!current.signal.aborted) setLoadError(errorText(cause)); })
      .finally(() => { if (!current.signal.aborted) setLoading(false); });
    return () => current.abort();
  }, [open]);
  return <Dialog open={open} onOpenChange={next => { if (!pending) onOpenChange(next); }}>
    <DialogContent className="dl-session-dialog dl-import-session-picker" aria-describedby={undefined} showCloseButton={!pending}>
      <DialogTitle>{t("imports.add_to_session")}</DialogTitle>
      {items.length > 0 && <div className="dl-import-session-scroll"><ul>{items.map(session => <li key={session.id}>
        <button type="button" disabled={pending} onClick={() => onChoose(session.id)}>
          <strong>{sessionTitle(session)}</strong>
          <span>{new Date(session.last_activity_at_ms).toLocaleString(getCurrentWebUiLanguage(), {dateStyle: "medium", timeStyle: "short"})}</span>
        </button>
      </li>)}</ul>
        {cursor && <LazyPage cursor={cursor} enabled={!pending} onLoad={signal => load(cursor, signal)} errorMessage={t("lazy_page.load_failed")} />}
      </div>}
      {loading && <div className="dl-import-list-status" role="status"><Spinner />{t("imports.loading_sessions")}</div>}
      {!loading && !loadError && !items.length && <p className="dl-import-empty">{t("imports.no_sessions")}</p>}
      {(loadError || error) && <Alert variant="destructive"><AlertDescription>{loadError || error}</AlertDescription></Alert>}
      {pending && <div className="dl-import-list-status" role="status"><Spinner />{t("imports.sending")}</div>}
    </DialogContent>
  </Dialog>;
}
