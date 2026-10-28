import type { ContactFocus } from "@/lib/contacts";
import { isInternalNotification } from "@/lib/conversation-messages";
import { importedConversation, messageDisplayText } from "@/lib/conversation-import";
import { ImportConversationBubble } from "@/components/import-conversation";
import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import type { Access, MessageRecord } from "@/lib/actor-api";
import { isUserDelivery, isUserSource, workTranscriptParts, type WorkTranscriptPart } from "@/lib/work-timeline";
import { forwardRef, Fragment } from "react";
import { AnimatePresence, motion, MotionConfig, useReducedMotion } from "motion/react";
import { readPreferences } from "@/lib/preferences";
import { SentBubble, type SendOrigin } from "@/components/sent-bubble";
import type { useActorConversation } from "@/hooks/use-actor-conversation";
import { AttachmentList, CopyMessage } from "@/components/conversation-content";
import { Markdown } from "@/components/markdown";
import { ActorMessageBubble } from "@/components/actor-message-bubble";
import { WorkActivity } from "@/components/work-activity";
import { MessageOutcomes, messageOutcomeTools } from "@/components/message-outcomes";
import { cn } from "@/lib/utils";
import { conversationFailureMessage } from "@/lib/conversation-errors";
import { matchesConversationText } from "@/lib/conversation-search";
import { MessageContextMenu } from "@/components/message-context-menu";

type Chat = ReturnType<typeof useActorConversation>;
type Props = { focus?:ContactFocus|null; message: MessageRecord; chat: Chat; access: Access; onActor(id: string, messageId?: string): void; searchQuery?: string };
function OutcomeStatus({ message }: { message: MessageRecord }) {
  if (message.status === "failed") return <p role="status" className="dl-operation-error">{conversationFailureMessage(message.error)}</p>;
  if (message.status === "cancelled" || message.status === "interrupted") return <p role="status" className="dl-operation-note">{message.started_at_ms == null ? t("conversation_turns.queued_message_withdrawn") : t("conversation_turns.task_stopped_completed_actions_are_retained")}</p>;
  return null;
}
export function PrimaryTranscript({ messages, working, receiptMessageId, sendOrigin, ...props }: Omit<Props, "message"> & { messages: MessageRecord[]; working: boolean; receiptMessageId?: string; sendOrigin?: SendOrigin }) {
  const systemReduced = useReducedMotion();
  const preference = readPreferences().appearance.motion;
  const reduced = preference === "reduce" || preference === "system" && systemReduced;
  return <MotionConfig reducedMotion={reduced ? "always" : "never"} transition={{duration:reduced ? 0 : .28,ease:[.22,1,.36,1],layout:{type:"tween",duration:reduced ? 0 : .28,ease:[.22,1,.36,1]}}}><AnimatePresence initial={false} mode="popLayout">
    {messages.map(message => <PrimaryTurn key={message.id} message={message} receiptMessageId={receiptMessageId} sendOrigin={!reduced && sendOrigin?.messageId === message.id ? sendOrigin : undefined} {...props} />)}
    {working && props.searchQuery === undefined && <motion.div key="typing" layout="position" {...bubbleMotion} className="dl-primary-turn" data-first-side="assistant" data-last-side="assistant">
      <div className="dl-primary-typing" role="status" aria-label={t("conversation_turns.working")}><i /><i /><i /></div>
    </motion.div>}
  </AnimatePresence></MotionConfig>;
}
const bubbleMotion = {initial:{opacity:0,y:8,scale:.98},animate:{opacity:1,y:0,scale:1},exit:{opacity:0,y:-4,scale:.98}};
const disclosureMotion = {initial:{height:0,opacity:0},animate:{height:"auto",opacity:1},exit:{height:0,opacity:0}};
export const PrimaryTurn = forwardRef<HTMLElement, Props & { receiptMessageId?: string; sendOrigin?: SendOrigin }>(function PrimaryTurn({ message, chat, onActor, receiptMessageId, searchQuery, sendOrigin }, ref) {
  const user = isUserSource(message);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  if (!user && message.reply_to.kind !== "user" && !(message.reply_to.kind === "remote" && message.reply_to.user)) return null;
  const filtering = Boolean(searchQuery?.trim());
  const input = (user && (!filtering || matchesConversationText(messageDisplayText(message), searchQuery!)));
  const reply = isUserDelivery(message) && (!filtering || matchesConversationText(message.reply || "", searchQuery!));
  const outcomes = messageOutcomeTools(tools);
  const response = reply || (!filtering && (outcomes.artifacts.length > 0 || outcomes.changes.length > 0 || (user && ["failed", "cancelled", "interrupted"].includes(message.status))));
  if (!input && !response) return null;
  return <motion.article ref={ref} layout="position" {...bubbleMotion} initial={sendOrigin ? false : bubbleMotion.initial} className="dl-message-turn dl-primary-turn" data-first-side={input ? "user" : "assistant"} data-last-side={response ? "assistant" : "user"} id={`message-${message.actor_id}-${message.id}`}>
    <AnimatePresence initial={false}>
    {input && <motion.div key="input" {...disclosureMotion} className="dl-primary-inputs">
    {user && <div className="dl-message dl-message-user" data-message-side="input"><MessageContextMenu content={messageDisplayText(message)}><div className="dl-primary-bubble-row"><SentBubble origin={sendOrigin}>{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{message.content && <p className="dl-user-text">{message.content}</p>}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</SentBubble><CopyMessage content={messageDisplayText(message)} /></div></MessageContextMenu><AnimatePresence initial={false}>{(!receiptMessageId || receiptMessageId === message.id) && <motion.div key="receipt" {...disclosureMotion} className="dl-receipt-presence"><MessageReceipt message={message} /></motion.div>}</AnimatePresence></div>}
    </motion.div>}
    {response && <motion.div key="response" {...disclosureMotion} className="dl-primary-response">
    {reply && <div className="dl-message dl-message-assistant" data-message-side="reply"><MessageContextMenu content={message.reply || ""}><div className="dl-primary-bubble-row"><div className="dl-bubble dl-bubble-assistant"><Markdown content={message.reply || ""} onActor={onActor} /></div><CopyMessage content={message.reply || ""} /></div></MessageContextMenu></div>}
    {!filtering && (outcomes.artifacts.length > 0 || outcomes.changes.length > 0) && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
    {!filtering && user && <OutcomeStatus message={message} />}
    </motion.div>}
    </AnimatePresence>
  </motion.article>;
});
function MessageReceipt({ message }: { message: MessageRecord }) {
  return <small className="dl-message-receipt">{t("conversation_turns.delivered_time", {time:new Date(message.created_at_ms).toLocaleTimeString(getCurrentWebUiLanguage(), { hour: "numeric", minute: "2-digit" })})}</small>;
}
export function WorkTranscript({ messages, ...props }: Omit<Props, "message"> & { messages: MessageRecord[] }) {
  const parts = workTranscriptParts(messages, props.chat.messages, props.chat.events).filter(part => {
    const message = part.message;
    if (part.kind === "input") return !isInternalNotification(message) && Boolean(message.content || message.attachments.length) && (!props.searchQuery || matchesConversationText(messageDisplayText(message), props.searchQuery));
    if (part.kind === "activity") return props.searchQuery === undefined;
    if (props.searchQuery !== undefined) return Boolean(message.reply) && matchesConversationText(message.reply!, props.searchQuery);
    const outcomes = messageOutcomeTools(props.chat.tools.filter(tool => tool.message_id === message.id));
    return Boolean(message.reply || outcomes.artifacts.length || outcomes.changes.length || ["failed", "cancelled", "interrupted"].includes(message.status));
  });
  const turns: { message: MessageRecord; parts: WorkTranscriptPart[] }[] = [];
  for (const part of parts) {
    const previous = turns.at(-1);
    if (previous?.message.id === part.message.id) previous.parts.push(part);
    else turns.push({message:part.message,parts:[part]});
  }
  const anchored = new Set<string>();
  return <>{turns.map(turn => {
    const first = !anchored.has(turn.message.id);
    anchored.add(turn.message.id);
    const key = `${turn.message.id}:${turn.parts[0].kind}:${turn.parts[0].kind === "activity" ? turn.parts[0].range.start : ""}`;
    return <WorkTurn key={key} {...props} {...turn} anchor={first} />;
  })}</>;
}
function WorkTurn({ focus, message, parts, anchor, chat, onActor, searchQuery }: Props & { parts: WorkTranscriptPart[]; anchor: boolean }) {
  const user = isUserSource(message);
  const actorReply = message.reply_to.kind === "actor" || message.reply_to.kind === "remote" && !message.reply_to.user;
  const internal = isInternalNotification(message);
  const searchMode = searchQuery !== undefined;
  const inputMatches = !searchQuery || matchesConversationText(messageDisplayText(message), searchQuery);
  const replyMatches = !searchQuery || matchesConversationText(message.reply || "", searchQuery);
  const tools = chat.tools.filter(tool => tool.message_id === message.id);
  return <article className="dl-message-turn dl-work-turn" id={anchor ? `message-${message.actor_id}-${message.id}` : undefined}>
    {parts.map(part => <Fragment key={`${part.kind}:${part.kind === "activity" ? part.range.start : ""}`}>
    {part.kind === "input" && inputMatches && !internal && (message.content || message.attachments.length > 0) && <MessageContextMenu content={messageDisplayText(message)}><div data-message-id={message.id} data-message-side="input" className={cn("dl-work-input", user ? "dl-work-user-input" : "dl-work-delegated-input")}><ActorMessageBubble message={message} direction="incoming" messages={chat.messages} onActor={onActor} className={cn("dl-work-prompt", user ? "dl-bubble-user" : "dl-bubble-peer")}>{importedConversation(message) ? <ImportConversationBubble message={message} /> : <>{user ? <p className="dl-user-text">{message.content}</p> : <Markdown content={message.content} onActor={onActor} />}{message.attachments.length > 0 && <AttachmentList items={message.attachments} />}</>}</ActorMessageBubble>{message.content && <div className="dl-work-user-actions"><CopyMessage content={messageDisplayText(message)} /></div>}</div></MessageContextMenu>}
    {part.kind === "activity" && !searchMode && <WorkActivity focus={focus?.message_id===message.id?focus:null} message={message} range={part.range} events={chat.events} tools={tools} onActor={onActor} />}
    {part.kind === "outcome" && <>
      {replyMatches && message.reply && <MessageContextMenu content={message.reply}><div data-message-id={message.id} data-message-side="reply" className={cn("dl-work-final", actorReply && "dl-work-actor-reply")}><ActorMessageBubble message={message} direction="outgoing" messages={chat.messages} onActor={onActor} className={actorReply ? "dl-bubble-actor" : "dl-bubble-assistant"}><Markdown content={message.reply} onActor={onActor} /></ActorMessageBubble><CopyMessage content={message.reply} /></div></MessageContextMenu>}
      {!searchMode && <MessageOutcomes actorId={message.actor_id} tools={tools} onActor={onActor} />}
      {!searchMode && <OutcomeStatus message={message} />}
    </>}
    </Fragment>)}
  </article>;
}
