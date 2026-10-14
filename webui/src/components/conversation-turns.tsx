import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import type { Access, MessageRecord } from "@/lib/actor-api";
import { isUserDelivery, isUserSource } from "@/lib/work-timeline";
import { asText, asObject, domainToolName } from "@/lib/tool-presentation";
import { actorIdFromUrl } from "@/lib/conversation-links";
import type { useActorConversation } from "@/hooks/use-actor-conversation";
import { ActorReference, Approval, AttachmentList, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { WorkRound } from "@/components/work-round";
import { MessageOutcomes } from "@/components/message-outcomes";
import { cn } from "@/lib/utils";
import { conversationFailureMessage } from "@/lib/conversation-errors";

type Chat = ReturnType<typeof useActorConversation>;
type Props = { message: MessageRecord; chat: Chat; access: Access; onActor(id: string, messageId?: string): void };
function Guidance({ message, chat, primary }: {message:MessageRecord;chat:Chat;primary?:boolean}) {
  return <>{chat.messages.filter(item=>item.kind==="steering" && item.status==="completed" && item.in_reply_to===message.id).map(item=><div key={item.id} id={`message-${item.actor_id}-${item.id}`} className={primary?"dl-message dl-message-user dl-guidance-message":"dl-work-user-input dl-guidance-message"}><small className="dl-guidance-label">{t("conversation_turns.guidance_for_the_current_task")}</small><div className={primary?"dl-bubble dl-bubble-user":"dl-work-prompt"}>{item.content&&<p className="dl-user-text">{item.content}</p>}{item.attachments.length>0&&<AttachmentList items={item.attachments}/>}</div></div>)}</>;
}
function Decisions({ message, chat, access }: Omit<Props, "onActor">) {
  return <>{chat.approvals.filter(record => record.message_id === message.id && record.decision === "pending").map(record => <Approval key={record.id} record={record} canManage={access.kind === "owner"} onResolved={() => { void chat.refresh(); }} />)}</>;
}
function OutcomeStatus({ message }: { message: MessageRecord }) {
  if (message.status === "failed") return <p role="status" className="dl-operation-error">{conversationFailureMessage(message.error)}</p>;
  if (message.status === "cancelled" || message.status === "interrupted") return <p role="status" className="dl-operation-note">{message.started_at_ms == null ? t("conversation_turns.queued_message_withdrawn") : t("conversation_turns.task_stopped_completed_actions_are_retained")}</p>;
  return null;
}
export function PrimaryTurn({ message, chat, access, onActor, typing }: Props & { typing?: boolean }) {
  const user = isUserSource(message);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  const tasks = tools.filter(tool => !tool.error && ["actors__create", "actors__send"].includes(domainToolName(tool.name)) && tool.outcome);
  const taskTarget = (tool: typeof tasks[number]) => { const info = asObject(tool.outcome); return actorIdFromUrl(asText(info.conversation_url), typeof location === "undefined" ? undefined : location.origin) || asText(info.actor_id) || asText(info.id); };
  const delegated = new Set(tasks.filter(tool => domainToolName(tool.name) === "actors__send").map(taskTarget));
  const references = tasks.filter(tool => domainToolName(tool.name) !== "actors__create" || !delegated.has(taskTarget(tool)));
  if (!user && message.reply_to.kind !== "user" && !(message.reply_to.kind === "remote" && message.reply_to.user)) return null;
  return <article className="dl-message-turn dl-primary-turn" id={`message-${message.actor_id}-${message.id}`}>
    {user && <div className="dl-message dl-message-user"><div className="dl-bubble dl-bubble-user">{message.content && <p className="dl-user-text">{message.content}</p>}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</div><small className="dl-message-receipt">{t("conversation_turns.delivered_time", {time:new Date(message.created_at_ms).toLocaleTimeString(getCurrentWebUiLanguage(), { hour: "numeric", minute: "2-digit" })})}</small><CopyMessage content={message.content} /></div>}
    <Guidance message={message} chat={chat} primary />
    {isUserDelivery(message) && <div className="dl-message dl-message-assistant"><div className="dl-bubble dl-bubble-assistant"><Markdown content={message.reply || ""} onActor={onActor} /></div><CopyMessage content={message.reply || ""} /></div>}
    {references.map(tool => { const info = asObject(tool.outcome); const target = taskTarget(tool); return target ? <ActorReference key={tool.id} actorId={target} label={asText(info.name)} messageId={asText(info.message_id)} onOpen={onActor} /> : null; })}
    <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />
    <Decisions message={message} chat={chat} access={access} />
    {user && <OutcomeStatus message={message} />}
    {(typing ?? (message.status === "running")) && <div className="dl-primary-typing" role="status" aria-label={t("conversation_turns.working")}><i /><i /><i /></div>}
  </article>;
}
export function WorkTurn({ message, chat, access, onActor }: Props) {
  const user = isUserSource(message);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  return <article className="dl-message-turn dl-work-turn" id={`message-${message.actor_id}-${message.id}`}>
    {(message.content || message.attachments.length > 0) && <div className={cn("dl-work-input", user ? "dl-work-user-input" : "dl-work-delegated-input")}>{!user && message.source.kind === "actor" && <ActorReference actorId={message.source.actor_id} onOpen={onActor} detail={["result", "progress"].includes(message.kind) ? t("conversation_turns.collaboration_reply") : t("conversation_turns.delegate_task")} />}<div className={cn(user && "dl-work-prompt")}>{user ? <p className="dl-user-text">{message.content}</p> : <Markdown content={message.content} onActor={onActor} />}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</div></div>}
    <Guidance message={message} chat={chat} />
    {message.kind !== "delivery" && !chat.messages.some(item => item.in_reply_to === message.id && item.id !== message.id) && <WorkRound message={message} events={chat.events} tools={tools} nextToolCursor={chat.toolNext[message.id]} onMoreTools={() => chat.loadMoreTools(message.id)} onActor={onActor} />}
    <Decisions message={message} chat={chat} access={access} />
    {message.reply && <div className="dl-work-final"><Markdown content={message.reply} onActor={onActor} /><CopyMessage content={message.reply} /></div>}
    <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />
    <OutcomeStatus message={message} />
  </article>;
}
