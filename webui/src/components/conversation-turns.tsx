import { importedConversation, messageDisplayText } from "@/lib/conversation-import";
import { ImportConversationBubble } from "@/components/import-conversation";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import type { Access, MessageRecord } from "@/lib/actor-api";
import { isUserDelivery, isUserSource } from "@/lib/work-timeline";
import { asText, asObject, domainToolName } from "@/lib/tool-presentation";
import { actorIdFromUrl } from "@/lib/conversation-links";
import type { useActorConversation } from "@/hooks/use-actor-conversation";
import { ActorReference, Approval, AttachmentList, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { WorkRound } from "@/components/work-round";
import { MessageOutcomes, messageOutcomeTools } from "@/components/message-outcomes";
import { cn } from "@/lib/utils";
import { conversationFailureMessage } from "@/lib/conversation-errors";
import { matchesConversationText } from "@/lib/conversation-search";

type Chat = ReturnType<typeof useActorConversation>;
type Props = { message: MessageRecord; chat: Chat; access: Access; onActor(id: string, messageId?: string): void; searchQuery?: string };
function Guidance({ message, chat, primary, receiptMessageId, searchQuery }: {message:MessageRecord;chat:Chat;primary?:boolean;receiptMessageId?:string;searchQuery?:string}) {
  return <>{chat.messages.filter(item=>item.kind==="steering" && item.status==="completed" && item.in_reply_to===message.id && (!searchQuery || matchesConversationText(messageDisplayText(item), searchQuery))).map(item=><div key={item.id} id={`message-${item.actor_id}-${item.id}`} className={primary?"dl-message dl-message-user dl-guidance-message":"dl-work-user-input dl-guidance-message"}><small className="dl-guidance-label">{t("conversation_turns.guidance_for_the_current_task")}</small><div className={primary?"dl-bubble dl-bubble-user":"dl-work-prompt"}>{item.content&&<p className="dl-user-text">{item.content}</p>}{item.attachments.length>0&&<AttachmentList items={item.attachments}/>}</div>{primary && receiptMessageId === item.id && <MessageReceipt message={item} />}</div>)}</>;
}
function Decisions({ message, chat, access }: Omit<Props, "onActor">) {
  return <>{chat.approvals.filter(record => record.message_id === message.id && record.mode === "ask" && record.decision === "pending").map(record => <Approval key={record.id} record={record} canManage={access.kind === "owner"} onResolved={() => { void chat.refresh(); }} />)}</>;
}
function OutcomeStatus({ message }: { message: MessageRecord }) {
  if (message.status === "failed") return <p role="status" className="dl-operation-error">{conversationFailureMessage(message.error)}</p>;
  if (message.status === "cancelled" || message.status === "interrupted") return <p role="status" className="dl-operation-note">{message.started_at_ms == null ? t("conversation_turns.queued_message_withdrawn") : t("conversation_turns.task_stopped_completed_actions_are_retained")}</p>;
  return null;
}
export function PrimaryTranscript({ messages, working, receiptMessageId, ...props }: Omit<Props, "message"> & { messages: MessageRecord[]; working: boolean; receiptMessageId?: string }) {
  return <>
    {messages.map(message => <PrimaryTurn key={message.id} message={message} receiptMessageId={receiptMessageId} {...props} />)}
    {working && props.searchQuery === undefined && <div className="dl-primary-turn" data-first-side="assistant" data-last-side="assistant">
      <div className="dl-primary-typing" role="status" aria-label={t("conversation_turns.working")}><i /><i /><i /></div>
    </div>}
  </>;
}
export function PrimaryTurn({ message, chat, access, onActor, receiptMessageId, searchQuery }: Props & { receiptMessageId?: string }) {
  const user = isUserSource(message);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  const tasks = tools.filter(tool => !tool.error && ["actors__create", "actors__import_conversation", "actors__send"].includes(domainToolName(tool.name)) && tool.outcome);
  const taskTarget = (tool: typeof tasks[number]) => { const info = asObject(tool.outcome); return actorIdFromUrl(asText(info.conversation_url), typeof location === "undefined" ? undefined : location.href) || asText(info.actor_id) || asText(info.id); };
  const delegated = new Set(tasks.filter(tool => domainToolName(tool.name) === "actors__send").map(taskTarget));
  const references = tasks.filter(tool => taskTarget(tool) && (domainToolName(tool.name) === "actors__send" || !delegated.has(taskTarget(tool))));
  if (!user && message.reply_to.kind !== "user" && !(message.reply_to.kind === "remote" && message.reply_to.user)) return null;
  const guidance = chat.messages.some(item => item.kind === "steering" && item.status === "completed" && item.in_reply_to === message.id);
  const filtering = Boolean(searchQuery?.trim());
  const input = (user && (!filtering || matchesConversationText(messageDisplayText(message), searchQuery!))) || (!filtering && guidance);
  const reply = isUserDelivery(message) && (!filtering || matchesConversationText(message.reply || "", searchQuery!));
  const outcomes = messageOutcomeTools(tools);
  const response = reply || (!filtering && (references.length > 0 || outcomes.artifacts.length > 0 || outcomes.changes.length > 0 || chat.approvals.some(record => record.message_id === message.id && record.mode === "ask" && record.decision === "pending") || (user && ["failed", "cancelled", "interrupted"].includes(message.status))));
  if (!input && !response) return null;
  return <article className="dl-message-turn dl-primary-turn" data-first-side={input ? "user" : "assistant"} data-last-side={response ? "assistant" : "user"} id={`message-${message.actor_id}-${message.id}`}>
    {input && <div className="dl-primary-inputs">
    {user && <div className="dl-message dl-message-user"><div className="dl-primary-bubble-row"><div className="dl-bubble dl-bubble-user">{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{message.content && <p className="dl-user-text">{message.content}</p>}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</div><CopyMessage content={messageDisplayText(message)} /></div>{(!receiptMessageId || receiptMessageId === message.id) && <MessageReceipt message={message} />}</div>}
    <Guidance message={message} chat={chat} primary receiptMessageId={receiptMessageId} searchQuery={searchQuery} />
    </div>}
    {response && <div className="dl-primary-response">
    {reply && <div className="dl-message dl-message-assistant"><div className="dl-primary-bubble-row"><div className="dl-bubble dl-bubble-assistant"><Markdown content={message.reply || ""} onActor={onActor} /></div><CopyMessage content={message.reply || ""} /></div></div>}
    {!filtering && references.map(tool => { const info = asObject(tool.outcome); const target = taskTarget(tool); return target ? <ActorReference key={tool.id} actorId={target} label={asText(info.name)} messageId={asText(info.message_id)} onOpen={onActor} /> : null; })}
    {!filtering && (outcomes.artifacts.length > 0 || outcomes.changes.length > 0) && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
    {!filtering && <Decisions message={message} chat={chat} access={access} />}
    {!filtering && user && <OutcomeStatus message={message} />}
    </div>}
  </article>;
}
function MessageReceipt({ message }: { message: MessageRecord }) {
  return <small className="dl-message-receipt">{t("conversation_turns.delivered_time", {time:new Date(message.created_at_ms).toLocaleTimeString(getCurrentWebUiLanguage(), { hour: "numeric", minute: "2-digit" })})}</small>;
}
export function WorkTurn({ message, chat, access, onActor, searchQuery }: Props) {
  const user = isUserSource(message);
  const searchMode = searchQuery !== undefined;
  const inputMatches = !searchQuery || matchesConversationText(messageDisplayText(message), searchQuery);
  const replyMatches = !searchQuery || matchesConversationText(message.reply || "", searchQuery);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  return <article className="dl-message-turn dl-work-turn" id={`message-${message.actor_id}-${message.id}`}>
    {inputMatches && (message.content || message.attachments.length > 0) && <div className={cn("dl-work-input", user ? "dl-work-user-input" : "dl-work-delegated-input")}>{!user && message.source.kind === "actor" && <ActorReference actorId={message.source.actor_id} onOpen={onActor} detail={["result", "progress"].includes(message.kind) ? t("conversation_turns.collaboration_reply") : t("conversation_turns.delegate_task")} />}<div className={cn(user && "dl-work-prompt")}>{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{user ? <p className="dl-user-text">{message.content}</p> : <Markdown content={message.content} onActor={onActor} />}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</div>{user && message.content && <div className="dl-work-user-actions"><CopyMessage content={messageDisplayText(message)} /></div>}</div>}
    <Guidance message={message} chat={chat} searchQuery={searchQuery} />
    {!searchMode && message.kind !== "delivery" && <WorkRound message={message} events={chat.events} tools={tools} onActor={onActor} />}
    {!searchMode && <Decisions message={message} chat={chat} access={access} />}
    {replyMatches && message.reply && <div className="dl-work-final"><Markdown content={message.reply} onActor={onActor} /><CopyMessage content={message.reply} /></div>}
    {!searchMode && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
    {!searchMode && <OutcomeStatus message={message} />}
  </article>;
}
