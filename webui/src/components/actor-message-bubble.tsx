import { useEffect, useState, type ReactNode, type SyntheticEvent } from "react";
import { ArrowUpRightIcon } from "lucide-react";
import { getActor, getMessage, getToolResult, type MessageRecord } from "@/lib/actor-api";
import { actorIdFromUrl, localActorId } from "@/lib/conversation-links";
import { asObject, asText } from "@/lib/tool-presentation";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { Markdown } from "@/components/markdown";
import { CopyMessage } from "@/components/conversation-content";
import { MessageContextMenu } from "@/components/message-context-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { Button } from "@/components/ui/button";

export function communicationActor(message: MessageRecord, direction: "incoming" | "outgoing", messages: MessageRecord[] = []): string | null {
  const source = message.source;
  if (direction === "incoming") return source.kind === "actor" ? source.actor_id : source.kind === "remote_actor" ? `${source.stage_id}~${source.actor_id}` : null;
  const target = message.reply_to;
  if (target.kind === "actor") return target.actor_id;
  if (target.kind !== "remote" || target.user) return null;
  const seen = new Set<string>();
  let current: MessageRecord | undefined = message;
  while (current && !seen.has(current.id)) {
    seen.add(current.id);
    const origin = current.source;
    if (origin.kind === "remote_actor" && origin.stage_id === target.stage_id && origin.invocation_id === target.invocation_id) return `${origin.stage_id}~${origin.actor_id}`;
    current = messages.find(item => item.id === current?.in_reply_to);
  }
  return null;
}

function interactive(event: SyntheticEvent) {
  const target = event.target instanceof Element ? event.target.closest("a,button,input,textarea,select,pre,[role=button],[role=link]") : null;
  return Boolean(target && target !== event.currentTarget);
}
export function CommunicationBubble({ actorId, direction, children, onActor, className }: { actorId: string | null; direction: "incoming" | "outgoing"; children: ReactNode; onActor(id: string): void; className?: string }) {
  const id = actorId ? localActorId(actorId) : null;
  const [identity, setIdentity] = useState<{id: string; name: string} | null>(null);
  useEffect(() => {
    if (!id) return;
    const abort = new AbortController();
    void getActor(id, {signal:abort.signal}).then(actor => { if (!abort.signal.aborted) setIdentity({id,name:actor.config?.name || actor.name || id}); }).catch(() => {});
    return () => abort.abort();
  }, [id]);
  const caption = id ? t(direction === "incoming" ? "conversation_content.sent_by" : "conversation_content.sent_to", {name:identity?.id === id ? identity.name : id.split("~").at(-1)}) : undefined;
  return <div className={cn("dl-bubble", id && "dl-actor-message", className)} role={id ? "link" : undefined} tabIndex={id ? 0 : undefined} aria-label={caption}
    onClick={id ? event => { if (!interactive(event) && !window.getSelection()?.toString()) onActor(id); } : undefined}
    onKeyDown={id ? event => { if (!interactive(event) && ["Enter"," "].includes(event.key)) {event.preventDefault();onActor(id);} } : undefined}>
    {caption && <div className="dl-actor-message-caption"><span>{caption}</span><ArrowUpRightIcon aria-hidden="true" /></div>}
    {children}
  </div>;
}

export function ActorMessageBubble({ message, messages, direction, ...props }: { message: MessageRecord; messages: MessageRecord[]; direction: "incoming" | "outgoing"; children: ReactNode; className?: string; onActor(id: string): void }) {
  const direct = communicationActor(message, direction, messages);
  const [resolved, setResolved] = useState<{message: string; actor: string} | null>(null);
  useEffect(() => {
    if (direct || direction !== "outgoing" || message.reply_to.kind !== "remote" || message.reply_to.user) return;
    const abort = new AbortController();
    void (async () => {
      let current = message;
      const known = [...messages];
      const seen = new Set<string>();
      // Bounded recovery follows exact correlation, never guesses by device alone.
      for (let count = 0; count < 8 && current.in_reply_to && !seen.has(current.id); count++) {
        seen.add(current.id);
        current = known.find(item => item.id === current.in_reply_to) ?? await getMessage(message.actor_id, current.in_reply_to, {signal:abort.signal});
        known.push(current);
        const actor = communicationActor(message, direction, known);
        if (actor) { if (!abort.signal.aborted) setResolved({message:message.id,actor}); break; }
      }
    })().catch(() => {});
    return () => abort.abort();
  }, [direct, direction, message, messages]);
  return <CommunicationBubble {...props} direction={direction} actorId={direct || (resolved?.message === message.id ? resolved.actor : null)} />;
}

export function SentActorMessage({ actorId, resultId, onActor }: { actorId: string; resultId: string; onActor(id: string): void }) {
  const [message, setMessage] = useState<{content: string; actor: string} | null>(null);
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const abort = new AbortController();
    setMessage(null); setFailed(false);
    void getToolResult(actorId, resultId, {signal:abort.signal}).then(result => {
      const sent = asObject(asObject(result.payload).sent_message);
      const target = actorIdFromUrl(asText(sent.conversation_url), typeof location === "undefined" ? undefined : location.href);
      if (!target || typeof sent.content !== "string") throw new Error("Invalid communication");
      if (!abort.signal.aborted) setMessage({content:sent.content,actor:target});
    }).catch(() => { if (!abort.signal.aborted) setFailed(true); });
    return () => abort.abort();
  }, [actorId, resultId, retry]);
  if (!message) return failed ? <div className="dl-client-error" role="alert"><span>{t("conversation_content.could_not_load_details_try_again")}</span><Button variant="ghost" size="sm" onClick={() => setRetry(value => value+1)}>{t("app.retry")}</Button></div> : <Skeleton className="h-20 w-64 max-w-full rounded-2xl" />;
  return <MessageContextMenu content={message.content}><div data-result-id={resultId} className="dl-work-final dl-work-actor-reply"><CommunicationBubble actorId={message.actor} direction="outgoing" className="dl-bubble-actor" onActor={onActor}><Markdown content={message.content} onActor={onActor} /></CommunicationBubble><CopyMessage content={message.content} /></div></MessageContextMenu>;
}
