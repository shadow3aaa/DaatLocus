import { isInternalNotification } from "@/lib/conversation-messages";
import { importedConversation, messageDisplayText } from "@/lib/conversation-import";
import { ImportConversationBubble } from "@/components/import-conversation";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import type { Access, MessageRecord } from "@/lib/actor-api";
import { isUserDelivery, isUserSource } from "@/lib/work-timeline";
import { asText, asObject, domainToolName } from "@/lib/tool-presentation";
import { actorIdFromUrl } from "@/lib/conversation-links";
import type { useActorConversation } from "@/hooks/use-actor-conversation";
import { ActorReference, AttachmentList, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { WorkRound } from "@/components/work-round";
import { MessageOutcomes, messageOutcomeTools } from "@/components/message-outcomes";
import { cn } from "@/lib/utils";
import { conversationFailureMessage } from "@/lib/conversation-errors";
import { matchesConversationText } from "@/lib/conversation-search";
import { MessageContextMenu } from "@/components/message-context-menu";

type Chat = ReturnType<typeof useActorConversation>;
type Props = { message: MessageRecord; chat: Chat; access: Access; onActor(id: string, messageId?: string): void; searchQuery?: string };
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
export function PrimaryTurn({ message, chat, onActor, receiptMessageId, searchQuery }: Props & { receiptMessageId?: string }) {
  const user = isUserSource(message);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  const tasks = tools.filter(tool => !tool.error && ["actors__create", "actors__import_conversation", "actors__message"].includes(domainToolName(tool.name)) && tool.outcome && (domainToolName(tool.name) !== "actors__message" || asObject(tool.outcome).show_to_user !== false));
  const taskTarget = (tool: typeof tasks[number]) => { const info = asObject(tool.outcome); return actorIdFromUrl(asText(info.conversation_url), typeof location === "undefined" ? undefined : location.href) || asText(info.actor_id) || asText(info.id); };
  const delegated = new Set(tasks.filter(tool => domainToolName(tool.name) === "actors__message").map(taskTarget));
  const references = tasks.filter(tool => taskTarget(tool) && (["actors__message"].includes(domainToolName(tool.name)) || !delegated.has(taskTarget(tool))));
  if (!user && message.reply_to.kind !== "user" && !(message.reply_to.kind === "remote" && message.reply_to.user)) return null;
  const filtering = Boolean(searchQuery?.trim());
  const input = (user && (!filtering || matchesConversationText(messageDisplayText(message), searchQuery!)));
  const reply = isUserDelivery(message) && (!filtering || matchesConversationText(message.reply || "", searchQuery!));
  const outcomes = messageOutcomeTools(tools);
  const response = reply || (!filtering && (references.length > 0 || outcomes.artifacts.length > 0 || outcomes.changes.length > 0 || (user && ["failed", "cancelled", "interrupted"].includes(message.status))));
  if (!input && !response) return null;
  return <article className="dl-message-turn dl-primary-turn" data-first-side={input ? "user" : "assistant"} data-last-side={response ? "assistant" : "user"} id={`message-${message.actor_id}-${message.id}`}>
    {input && <div className="dl-primary-inputs">
    {user && <div className="dl-message dl-message-user"><MessageContextMenu content={messageDisplayText(message)}><div className="dl-primary-bubble-row"><div className="dl-bubble dl-bubble-user">{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{message.content && <p className="dl-user-text">{message.content}</p>}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</div><CopyMessage content={messageDisplayText(message)} /></div></MessageContextMenu>{(!receiptMessageId || receiptMessageId === message.id) && <MessageReceipt message={message} />}</div>}
    </div>}
    {response && <div className="dl-primary-response">
    {reply && <div className="dl-message dl-message-assistant"><MessageContextMenu content={message.reply || ""}><div className="dl-primary-bubble-row"><div className="dl-bubble dl-bubble-assistant"><Markdown content={message.reply || ""} onActor={onActor} /></div><CopyMessage content={message.reply || ""} /></div></MessageContextMenu></div>}
    {!filtering && references.map(tool => { const info = asObject(tool.outcome); const target = taskTarget(tool); return target ? <ActorReference key={tool.id} actorId={target} label={asText(info.name)} messageId={asText(info.message_id)} onOpen={onActor} /> : null; })}
    {!filtering && (outcomes.artifacts.length > 0 || outcomes.changes.length > 0) && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
    {!filtering && user && <OutcomeStatus message={message} />}
    </div>}
  </article>;
}
function MessageReceipt({ message }: { message: MessageRecord }) {
  return <small className="dl-message-receipt">{t("conversation_turns.delivered_time", {time:new Date(message.created_at_ms).toLocaleTimeString(getCurrentWebUiLanguage(), { hour: "numeric", minute: "2-digit" })})}</small>;
}
export function WorkTurn({ message, chat, onActor, searchQuery }: Props) {
  const user = isUserSource(message);
  const internal = isInternalNotification(message);
  const searchMode = searchQuery !== undefined;
  const inputMatches = !searchQuery || matchesConversationText(messageDisplayText(message), searchQuery);
  const replyMatches = !searchQuery || matchesConversationText(message.reply || "", searchQuery);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  return <article className="dl-message-turn dl-work-turn" id={`message-${message.actor_id}-${message.id}`}>
    {inputMatches && !internal && (message.content || message.attachments.length > 0) && <MessageContextMenu content={user ? messageDisplayText(message) : ""}><div className={cn("dl-work-input", user ? "dl-work-user-input" : "dl-work-delegated-input")}><div className={cn(user && "dl-work-prompt")}>{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{user ? <p className="dl-user-text">{message.content}</p> : <Markdown content={message.content} onActor={onActor} />}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</div>{user && message.content && <div className="dl-work-user-actions"><CopyMessage content={messageDisplayText(message)} /></div>}</div></MessageContextMenu>}
    {!searchMode && (internal || message.kind !== "delivery") && <WorkRound message={message} events={chat.events} tools={tools} onActor={onActor} />}
    {replyMatches && message.reply && <MessageContextMenu content={message.reply}><div className="dl-work-final"><Markdown content={message.reply} onActor={onActor} /><CopyMessage content={message.reply} /></div></MessageContextMenu>}
    {!searchMode && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
    {!searchMode && <OutcomeStatus message={message} />}
  </article>;
}
