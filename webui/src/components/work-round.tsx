import { t } from "@/lib/i18n";
import { useEffect, useMemo, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import type { EventRecord, MessageRecord } from "@/lib/actor-api";
import { listMessageActivity } from "@/lib/actor-api";
import { activityTools, elapsedLabel, isLiveReasoning, mergeActivity, workTimeline } from "@/lib/work-timeline";
import type { ToolRef } from "@/hooks/use-actor-conversation";
import { Activities, Thinking } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

export function WorkRound({ message, events, tools, nextToolCursor, onMoreTools, onActor }: { message: MessageRecord; events: EventRecord[]; tools: ToolRef[]; nextToolCursor?: number | null; onMoreTools(): Promise<unknown>; onActor(id: string): void }) {
  const running = message.status === "running";
  const [open, setOpen] = useState(running);
  const [history, setHistory] = useState<EventRecord[]>([]);
  const [cursor, setCursor] = useState<number | null | undefined>(undefined);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const [now, setNow] = useState(Date.now());
  useEffect(() => { setOpen(running); }, [running]);
  useEffect(() => { if (!running) return; const clock = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(clock); }, [running]);
  useEffect(() => {
    if (!open || cursor !== undefined) return;
    const abort = new AbortController(); setLoading(true); setError("");
    void listMessageActivity(message.actor_id, message.id, { limit: 100, signal: abort.signal }).then(page => {
      if (abort.signal.aborted) return;
      setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id)); setCursor(page.next_cursor);
    }).catch(() => { if (!abort.signal.aborted) setError(t("work_round.could_not_load_activity")); }).finally(() => { if (!abort.signal.aborted) setLoading(false); });
    return () => abort.abort();
  }, [open, cursor, message.actor_id, message.id, retry]);
  // Do not join an incomplete historical prefix directly to recent live deltas.
  const records = useMemo(() => mergeActivity(history, cursor != null ? [] : events, message.actor_id, message.id), [history, events, cursor, message.actor_id, message.id]);
  const segments = useMemo(() => workTimeline(records, message, cursor === null), [records, message, cursor]);
  const activity = useMemo(() => activityTools(records, message, tools, cursor === null), [records, message, tools, cursor]);
  const elapsed = elapsedLabel(message.started_at_ms, message.finished_at_ms, now);
  const used = new Set(segments.flatMap(segment => segment.kind === "tools" ? segment.callIds : []));
  const remaining = cursor != null ? [] : activity.filter(tool => !used.has(tool.call_id));
  async function more() {
    if (loading || cursor == null) return;
    setLoading(true); setError("");
    try { const page = await listMessageActivity(message.actor_id, message.id, { after: cursor, limit: 100 }); setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id)); setCursor(page.next_cursor); }
    catch { setError(t("work_round.could_not_load_more_activity")); } finally { setLoading(false); }
  }
  if (message.status === "queued") return null;
  return <section className="dl-work-round"><button className="dl-round-heading" type="button" aria-expanded={open} onClick={() => setOpen(!open)}><span>{running ? elapsed ? t("work_round.working_value0", {value0: elapsed}) : t("conversation_turns.working") : elapsed ? t("work_round.elapsed_value0", {value0: elapsed}) : message.status === "cancelled" ? t("conversation_content.stopped") : t("work_round.activity")}</span><ChevronRightIcon className={cn(open && "dl-round-open")} aria-hidden="true" /></button>
    {open && <div className="dl-round-content">{loading && !segments.length && <Skeleton className="h-14 w-full" />}{segments.map((segment, index) => segment.kind === "text" ? segment.channel === "reasoning" ? <Thinking key={segment.seq} content={segment.content} live={isLiveReasoning(segment, index, segments, running)} /> : <div className="dl-work-progress" key={segment.seq}><Markdown content={segment.content} onActor={onActor} /></div> : <Activities key={segment.seq} actorId={message.actor_id} tools={activity.filter(tool => segment.callIds.includes(tool.call_id))} onActor={onActor} />)}{remaining.length > 0 && <Activities actorId={message.actor_id} tools={remaining} onActor={onActor} nextCursor={nextToolCursor} onLoadMore={onMoreTools} />}{!running && !loading && !segments.length && !remaining.length && !error && <p className="dl-round-empty">{t("work_round.no_intermediate_activity_recorded")}</p>}{cursor != null && <Button variant="ghost" size="sm" disabled={loading} onClick={() => { void more(); }}>{t("work_round.more_activity")}</Button>}{cursor === null && !remaining.length && nextToolCursor != null && <Button variant="ghost" size="sm" onClick={() => { void onMoreTools(); }}>{t("conversation_content.more_actions")}</Button>}{error && <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" disabled={loading} onClick={() => { if (cursor == null) setRetry(value => value + 1); else void more(); }}>{t("app.retry")}</Button></div>}</div>}
  </section>;
}
