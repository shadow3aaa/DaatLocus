import { t } from "@/lib/i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import type { EventRecord, MessageRecord } from "@/lib/actor-api";
import { listMessageActivity } from "@/lib/actor-api";
import { activityTools, elapsedLabel, extendActivityHistory, mergeActivity, workActivityTimeline, type WorkRoundSpan } from "@/lib/work-timeline";
import type { ToolRef } from "@/hooks/use-actor-conversation";
import { Activities } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { LazyPage } from "@/components/lazy-page";
import { cn } from "@/lib/utils";

export function WorkRound({ message, span, events, tools, onActor }: { message: MessageRecord; span: WorkRoundSpan; events: EventRecord[]; tools: ToolRef[]; onActor(id: string): void }) {
  const { running } = span;
  const [open, setOpen] = useState(running);
  const [history, setHistory] = useState<EventRecord[]>([]);
  const [cursor, setCursor] = useState<number | null | undefined>(undefined);
  const through = useRef(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const [now, setNow] = useState(Date.now());
  useEffect(() => { setOpen(running); }, [running]);
  useEffect(() => { if (!running) return; setNow(Date.now()); const clock = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(clock); }, [running]);
  useEffect(() => {
    if (!open || cursor !== undefined) return;
    const abort = new AbortController(); setLoading(true); setError("");
    void listMessageActivity(message.actor_id, message.id, { limit: 100, signal: abort.signal }).then(page => {
      if (abort.signal.aborted) return;
      setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id));
      through.current = page.next_cursor ?? Math.max(page.event_cursor ?? 0, ...page.items.map(record => record.seq));
      setCursor(page.next_cursor);
    }).catch(() => { if (!abort.signal.aborted) setError(t("work_round.could_not_load_activity")); }).finally(() => { if (!abort.signal.aborted) setLoading(false); });
    return () => abort.abort();
  }, [open, cursor, message.actor_id, message.id, retry]);
  useEffect(() => {
    if (!open || cursor !== null) return;
    const next = extendActivityHistory(history, events, message.actor_id, message.id, through.current);
    if (next.gap) setCursor(through.current);
    else {
      through.current = next.through;
      if (next.records !== history) setHistory(next.records);
    }
  }, [open, cursor, history, events, message.actor_id, message.id]);
  const records = useMemo(() => history.filter(record => record.created_at_ms >= span.start && (span.end === null || record.created_at_ms < span.end)), [history, span.start, span.end]);
  const complete = cursor === null || history.some(record => span.end !== null && record.created_at_ms >= span.end);
  const segments = useMemo(() => {
    // Inferring the final generation needs the whole input, not just this span.
    const source = message.final_generation == null && cursor === null ? {...message,final_generation:Math.max(0,...history.flatMap(record => "generation" in record.event ? [record.event.generation] : []))} : message;
    return workActivityTimeline(records, source, cursor === null, running && complete);
  }, [records, message, history, cursor, complete, running]);
  const activity = useMemo(() => activityTools(records, message, tools, complete), [records, message, tools, complete]);
  const elapsed = elapsedLabel(message.started_at_ms == null ? null : span.start, span.end, now);
  async function more(signal: AbortSignal) {
    if (loading || cursor == null) return;
    const page = await listMessageActivity(message.actor_id, message.id, { after: cursor, limit: 100, signal });
    if (signal.aborted) return;
    setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id));
    through.current = page.next_cursor ?? Math.max(through.current, page.event_cursor ?? 0, ...page.items.map(record => record.seq));
    setCursor(page.next_cursor);
  }
  if (message.status === "queued") return null;
  return <section className="dl-work-round"><button className="dl-round-heading" type="button" aria-expanded={open} onClick={() => setOpen(!open)}><span>{running ? elapsed ? t("work_round.working_value0", {value0: elapsed}) : t("conversation_turns.working") : elapsed ? t("work_round.elapsed_value0", {value0: elapsed}) : message.status === "cancelled" ? t("conversation_content.stopped") : t("work_round.activity")}</span><ChevronRightIcon className={cn(open && "dl-round-open")} aria-hidden="true" /></button>
    {open && <div className="dl-round-content">{loading && !segments.length && <Skeleton className="h-14 w-full" />}{segments.map(segment => segment.kind === "text" ? <div className="dl-work-progress" key={segment.seq}><Markdown content={segment.content} onActor={onActor} /></div> : <Activities key={segment.seq} actorId={message.actor_id} tools={activity.filter(tool => segment.callIds.includes(tool.call_id))} thinking={segment.thinking} onActor={onActor} />)}{!running && !loading && complete && !segments.length && !error && <p className="dl-round-empty">{t("work_round.no_intermediate_activity_recorded")}</p>}{!complete && cursor != null && <LazyPage cursor={cursor} onLoad={more} errorMessage={t("work_round.could_not_load_more_activity")} />}{error && <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" disabled={loading} onClick={() => { setRetry(value => value + 1); }}>{t("app.retry")}</Button></div>}</div>}
  </section>;
}
