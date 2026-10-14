import { t } from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDownIcon, MessageCircleIcon, ShieldCheckIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { ActorApiError, getActor, listApprovals, type ApprovalRecord } from "@/lib/actor-api";

type WaitingApproval = Pick<ApprovalRecord, "id" | "actor_id" | "seq">;
const reference = ({ id, actor_id, seq }: ApprovalRecord): WaitingApproval => ({ id, actor_id, seq });

/** Polling interval for the awaiting-approval query (no standing SSE stream). */
const POLL_INTERVAL_MS = 20_000;

function isAccessRevoked(cause: unknown): boolean {
  return cause instanceof ActorApiError && (cause.status === 401 || cause.status === 403 || cause.status === 410);
}

function WaitingActor({ actorId, count, onActor }: { actorId: string; count: number; onActor(id: string): void }) {
  const [name, setName] = useState(t("owner_approval_notice.actors_awaiting_approval"));
  useEffect(() => {
    const abort = new AbortController();
    void getActor(actorId, { signal: abort.signal }).then(actor => setName(actor.config?.name || actor.name || "Actor")).catch(() => {});
    return () => abort.abort();
  }, [actorId]);
  return <DropdownMenuItem onSelect={() => onActor(actorId)}><MessageCircleIcon data-icon="inline-start" /><span className="dl-menu-choice"><span>{name}</span><small>{count} {t("owner_approval_notice.actions_awaiting_approval")}</small></span></DropdownMenuItem>;
}

/** Other Actors may need an Owner decision even while the current conversation is idle. */
export function OwnerApprovalNotice({ actorId, onActor, onAccessRevoked }: { actorId: string; onActor(id: string): void; onAccessRevoked(): void }) {
  const [records, setRecords] = useState<WaitingApproval[]>([]);
  const [before, setBefore] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const [reload, setReload] = useState(0);
  const controller = useRef<AbortController | null>(null);
  const revision = useRef(0);
  const revoked = useRef(onAccessRevoked);
  revoked.current = onAccessRevoked;

  /** Fetch the first page of awaiting approvals; resolved actors drop out naturally via refetch. */
  const refresh = useCallback(async () => {
    const abort = controller.current;
    if (!abort) return;
    const current = revision.current;
    try {
      const page = await listApprovals(undefined, { awaiting_owner: true, limit: 30, signal: abort.signal });
      if (abort.signal.aborted || current !== revision.current) return;
      setRecords(page.items.map(reference)); setBefore(page.next_cursor); setError(false);
    } catch (cause) {
      if (abort.signal.aborted || current !== revision.current) return;
      if (isAccessRevoked(cause)) { revoked.current(); return; }
      setError(true);
    }
  }, []);

  useEffect(() => {
    const abort = new AbortController();
    controller.current = abort;
    revision.current++;
    setRecords([]); setBefore(null); setError(false); setLoading(false);
    void refresh();
    const timer = setInterval(() => { void refresh(); }, POLL_INTERVAL_MS);
    return () => { clearInterval(timer); abort.abort(); controller.current = null; revision.current++; };
  }, [reload, refresh]);

  async function more() {
    if (before === null || loading) return;
    const abort = controller.current;
    const current = revision.current;
    setLoading(true);
    try {
      const page = await listApprovals(undefined, { awaiting_owner: true, before, limit: 30, signal: abort?.signal });
      if (abort?.signal.aborted || current !== revision.current) return;
      setRecords(old => [...old, ...page.items.filter(item => !old.some(existing => existing.id === item.id)).map(reference)]);
      setBefore(page.next_cursor);
    } catch (cause) { if (!abort?.signal.aborted && current === revision.current) { if (isAccessRevoked(cause)) revoked.current(); else setError(true); } }
    finally { if (!abort?.signal.aborted) setLoading(false); }
  }

  const actors = new Map<string, number>();
  for (const record of records) if (record.actor_id !== actorId) actors.set(record.actor_id, (actors.get(record.actor_id) || 0) + 1);
  if (error) return <Button variant="ghost" size="sm" className="dl-approval-notice" onClick={() => setReload(old => old + 1)}>{t("owner_approval_notice.could_not_load_approvals_click_to_retry")}</Button>;
  if (!actors.size && before === null) return null;
  return <DropdownMenu onOpenChange={open => { if (open) void refresh(); }}><DropdownMenuTrigger asChild><Button variant="outline" size="sm" className="dl-approval-notice"><ShieldCheckIcon data-icon="inline-start" />{actors.size ? t("owner_approval_notice.value0_conversations_awaiting_approval", {value0: actors.size}) : t("owner_approval_notice.view_conversations_awaiting_approval")}<ChevronDownIcon data-icon="inline-end" /></Button></DropdownMenuTrigger>
    <DropdownMenuContent side="top" align="start" className="dl-approval-notice-menu"><DropdownMenuLabel>{t("owner_approval_notice.open_conversation_to_review")}</DropdownMenuLabel><DropdownMenuGroup>{Array.from(actors, ([id, count]) => <WaitingActor key={id} actorId={id} count={count} onActor={onActor} />)}{before !== null && <DropdownMenuItem disabled={loading} onSelect={event => { event.preventDefault(); void more(); }}>{loading ? t("conversation_content.loading") : t("owner_approval_notice.more_conversations_awaiting_approval")}</DropdownMenuItem>}</DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu>;
}
