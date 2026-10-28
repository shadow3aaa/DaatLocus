import { t } from "@/lib/i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import type { EventRecord, MessageRecord } from "@/lib/actor-api";
import { listMessageActivity } from "@/lib/actor-api";
import { activityTools, extendActivityHistory, mergeActivity, workActivityTimeline, type ActivityRange } from "@/lib/work-timeline";
import type { ToolRef } from "@/hooks/use-actor-conversation";
import { Activities, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { SentActorMessage } from "@/components/actor-message-bubble";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { LazyPage } from "@/components/lazy-page";

export function WorkActivity({ message, range, events, tools, onActor }: { message: MessageRecord; range: ActivityRange; events: EventRecord[]; tools: ToolRef[]; onActor(id: string): void }) {
  const { live } = range;
  const [history, setHistory] = useState<EventRecord[]>([]);
  const [cursor, setCursor] = useState<number | null | undefined>(undefined);
  const through = useRef(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (cursor !== undefined) return;
    const abort = new AbortController(); setLoading(true); setError("");
    void listMessageActivity(message.actor_id, message.id, { limit: 100, signal: abort.signal }).then(page => {
      if (abort.signal.aborted) return;
      setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id));
      through.current = page.next_cursor ?? Math.max(page.event_cursor ?? 0, ...page.items.map(record => record.seq));
      setCursor(page.next_cursor);
    }).catch(() => { if (!abort.signal.aborted) setError(t("work_activity.could_not_load_activity")); }).finally(() => { if (!abort.signal.aborted) setLoading(false); });
    return () => abort.abort();
  }, [cursor, message.actor_id, message.id, retry]);
  useEffect(() => {
    if (cursor !== null) return;
    const next = extendActivityHistory(history, events, message.actor_id, message.id, through.current);
    if (next.gap) setCursor(through.current);
    else {
      through.current = next.through;
      if (next.records !== history) setHistory(next.records);
    }
  }, [cursor, history, events, message.actor_id, message.id]);
  const records = useMemo(() => history.filter(record => record.created_at_ms >= range.start && (range.end === null || record.created_at_ms < range.end)), [history, range.start, range.end]);
  const complete = cursor === null || history.some(record => range.end !== null && record.created_at_ms >= range.end);
  const segments = useMemo(() => {
    // Inferring the final generation needs the whole input, not just this activity range.
    const source = message.final_generation == null && cursor === null ? {...message,final_generation:Math.max(0,...history.flatMap(record => "generation" in record.event ? [record.event.generation] : []))} : message;
    return workActivityTimeline(records, source, cursor === null, live && complete, history);
  }, [records, message, history, cursor, complete, live]);
  const activity = useMemo(() => activityTools(records, message, tools, complete), [records, message, tools, complete]);
  async function more(signal: AbortSignal) {
    if (loading || cursor == null) return;
    const page = await listMessageActivity(message.actor_id, message.id, { after: cursor, limit: 100, signal });
    if (signal.aborted) return;
    setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id));
    through.current = page.next_cursor ?? Math.max(through.current, page.event_cursor ?? 0, ...page.items.map(record => record.seq));
    setCursor(page.next_cursor);
  }
  if (message.status === "queued") return null;
  return <>
    {loading && !segments.length && <Skeleton className="h-14 w-full" />}
    {segments.map(segment => segment.kind === "message" ? <SentActorMessage key={segment.seq} actorId={message.actor_id} resultId={segment.resultId} onActor={onActor} /> : segment.kind === "text" ?
      <div className="dl-work-final dl-work-actor-reply" key={segment.seq}><div className="dl-bubble dl-bubble-actor"><Markdown content={segment.content} onActor={onActor} /></div><CopyMessage content={segment.content} /></div> :
      <Activities key={segment.seq} actorId={message.actor_id} tools={activity.filter(tool => segment.callIds.includes(tool.call_id))} thinking={segment.thinking} onActor={onActor} />)}
    {!complete && cursor != null && <LazyPage cursor={cursor} onLoad={more} errorMessage={t("work_activity.could_not_load_more_activity")} />}
    {error && <div className="dl-client-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" disabled={loading} onClick={() => { setRetry(value => value + 1); }}>{t("app.retry")}</Button></div>}
  </>;
}
