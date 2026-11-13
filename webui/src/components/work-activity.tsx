import { t } from "@/lib/i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { EventRecord, MessageRecord } from "@/lib/actor-api";
import { listMessageActivity } from "@/lib/actor-api";
import { activityRecords, activityTools, extendActivityHistory, mergeActivity, workActivityTimeline, type ActivityRange } from "@/lib/work-timeline";
import type { ToolRef } from "@/hooks/use-actor-conversation";
import { Activities, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { SentActorMessage } from "@/components/actor-message-bubble";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { LazyPage } from "@/components/lazy-page";
import { MessageBubbleRow } from "@/components/message-bubble-row";
import { actorAvatarSeed } from "@/lib/conversation-avatars";

function useActivityHistory(message: MessageRecord, events: EventRecord[]) {
  const [history, setHistory] = useState<EventRecord[]>([]);
  const [cursor, setCursor] = useState<number | null | undefined>(undefined);
  const through = useRef(0);
  const paging=useRef(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const lifetime = useRef<AbortController | null>(null);
  useEffect(() => {
    const abort = new AbortController();
    lifetime.current = abort;
    return () => abort.abort();
  }, [message.actor_id, message.id, retry]);
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
  const more = useCallback(async (caller: AbortSignal) => {
    const signal = lifetime.current?.signal;
    if (caller.aborted || !signal || signal.aborted || loading || cursor == null || paging.current) return;
    paging.current=true;
    try {
      const page = await listMessageActivity(message.actor_id, message.id, { after: cursor, limit: 100, signal });
      if (signal.aborted) return;
      setHistory(old => mergeActivity(old, page.items, message.actor_id, message.id));
      through.current = page.next_cursor ?? Math.max(through.current, page.event_cursor ?? 0, ...page.items.map(record => record.seq));
      setCursor(page.next_cursor);
    } catch (error) {
      if (!signal.aborted) setError(t("work_activity.could_not_load_activity"));
      throw error;
    } finally {paging.current=false;}
  }, [loading, cursor, message.actor_id, message.id]);
  const reload = useCallback(() => { setError(""); setCursor(undefined); setRetry(value => value + 1); }, []);
  return useMemo(() => ({history,cursor,loading,error,more,reload}), [history,cursor,loading,error,more,reload]);
}

export type ActivityHistory = ReturnType<typeof useActivityHistory>;

export function WorkActivityHistory({message,events,onChange}: {message:MessageRecord;events:EventRecord[];onChange(id:string,history:ActivityHistory):void}) {
  const history = useActivityHistory(message,events);
  useEffect(() => onChange(message.id,history), [message.id,history,onChange]);
  return null;
}

export function WorkActivity({ message, avatarActorId = message.actor_id, avatarActorName, range, history: data, tools, onActor }: { message: MessageRecord; avatarActorId?: string; avatarActorName?: string; range: ActivityRange; history?: ActivityHistory; tools: ToolRef[]; onActor(id: string): void }) {
  const { live } = range;
  const history = data?.history;
  const cursor = data?.cursor;
  const records = useMemo(() => activityRecords(history ?? [], message, range), [history, message, range.start, range.end]);
  const complete = cursor === null || Boolean(history?.some(record => range.end !== null && (record.created_at_ms > range.end || record.created_at_ms === range.end && range.end !== message.finished_at_ms)));
  const segments = useMemo(() => {
    const source = message.final_generation == null && cursor === null ? {...message,final_generation:Math.max(0,...(history ?? []).flatMap(record => "generation" in record.event ? [record.event.generation] : []))} : message;
    return workActivityTimeline(records, source, cursor === null, live && complete, history);
  }, [records, message, history, cursor, complete, live]);
  const activity = useMemo(() => activityTools(history ?? [], message, tools, cursor === null), [history, message, tools, cursor]);
  if (message.status === "queued") return null;
  if (!data) return <Skeleton className="h-14 w-full" />;
  return <>
    {data.loading && !segments.length && <Skeleton className="h-14 w-full" />}
    {segments.map(segment => segment.kind === "message" ? <SentActorMessage key={segment.seq} actorId={message.actor_id} avatarActorId={avatarActorId} avatarActorName={avatarActorName} resultId={segment.resultId} onActor={onActor} /> : segment.kind === "text" ?
      <div data-message-id={message.id} data-generation={segment.generation} className="dl-work-final dl-work-actor-reply" key={segment.seq}><MessageBubbleRow seed={actorAvatarSeed(avatarActorId)} actorId={avatarActorId} name={avatarActorName} own><div className="dl-bubble dl-bubble-actor"><Markdown content={segment.content} onActor={onActor} /></div><CopyMessage content={segment.content} /></MessageBubbleRow></div> :
      <Activities key={segment.seq} actorId={message.actor_id} tools={activity.filter(tool => segment.callIds.includes(tool.call_id))} thinking={segment.thinking} onActor={onActor} />)}
    {!complete && cursor != null && <LazyPage cursor={cursor} onLoad={data.more} errorMessage={t("work_activity.could_not_load_more_activity")} />}
    {data.error && <div className="dl-client-error" role="alert"><span>{data.error}</span><Button variant="ghost" size="sm" disabled={data.loading} onClick={data.reload}>{t("app.retry")}</Button></div>}
  </>;
}
