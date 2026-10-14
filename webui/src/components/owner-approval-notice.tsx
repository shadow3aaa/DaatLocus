import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { ChevronDownIcon, MessageCircleIcon, ShieldCheckIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { ActorApiError, getActor, getApproval, listApprovals, subscribeCatalogEvents, type ApprovalRecord } from "@/lib/actor-api";

type WaitingApproval = Pick<ApprovalRecord, "id" | "actor_id" | "seq">;
const reference = ({ id, actor_id, seq }: ApprovalRecord): WaitingApproval => ({ id, actor_id, seq });
const isAccessRevoked = (cause: unknown) => cause instanceof ActorApiError && [401, 403, 410].includes(cause.status);

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

  useEffect(() => {
    const abort = new AbortController();
    controller.current = abort;
    revision.current++;
    setRecords([]); setBefore(null); setError(false); setLoading(false);
    void (async () => {
      const page = await listApprovals(undefined, { awaiting_owner: true, limit: 30, signal: abort.signal });
      if (abort.signal.aborted) return;
      setRecords(page.items.map(reference)); setBefore(page.next_cursor);
      await subscribeCatalogEvents({ after: page.event_cursor || 0, signal: abort.signal,
        onAccessRevoked: () => { if (!abort.signal.aborted) revoked.current(); },
        onResync: () => { if (!abort.signal.aborted) setReload(old => old + 1); },
        onEvent: async ({ event }) => {
          if (abort.signal.aborted) return;
          if (event.type === "actor_deleted") {
            revision.current++;
            setRecords(old => old.filter(record => record.actor_id !== event.actor_id));
          } else if (event.type === "approval_resolved") {
            revision.current++;
            setRecords(old => old.filter(record => record.id !== event.approval_id));
          } else if (event.type === "approval_requested") {
            let record: ApprovalRecord;
            try { record = await getApproval(event.approval_id, undefined, { signal: abort.signal }); }
            catch (cause) { if (cause instanceof ActorApiError && cause.status === 404) return; throw cause; }
            if (abort.signal.aborted) return;
            revision.current++;
            if (record.mode === "ask" && record.decision === "pending")
              setRecords(old => [reference(record), ...old.filter(item => item.id !== record.id)]);
          }
        },
      });
    })().catch(cause => { if (!abort.signal.aborted) { if (isAccessRevoked(cause)) revoked.current(); else setError(true); } });
    return () => { abort.abort(); controller.current = null; revision.current++; };
  }, [reload]);

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
  return <DropdownMenu><DropdownMenuTrigger asChild><Button variant="outline" size="sm" className="dl-approval-notice"><ShieldCheckIcon data-icon="inline-start" />{actors.size ? t("owner_approval_notice.value0_conversations_awaiting_approval", {value0: actors.size}) : t("owner_approval_notice.view_conversations_awaiting_approval")}<ChevronDownIcon data-icon="inline-end" /></Button></DropdownMenuTrigger>
    <DropdownMenuContent side="top" align="start" className="dl-approval-notice-menu"><DropdownMenuLabel>{t("owner_approval_notice.open_conversation_to_review")}</DropdownMenuLabel><DropdownMenuGroup>{Array.from(actors, ([id, count]) => <WaitingActor key={id} actorId={id} count={count} onActor={onActor} />)}{before !== null && <DropdownMenuItem disabled={loading} onSelect={event => { event.preventDefault(); void more(); }}>{loading ? t("conversation_content.loading") : t("owner_approval_notice.more_conversations_awaiting_approval")}</DropdownMenuItem>}</DropdownMenuGroup></DropdownMenuContent>
  </DropdownMenu>;
}
