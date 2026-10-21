import { importedConversation, messageDisplayText } from "@/lib/conversation-import";
import { resourceSource } from "@/lib/transport";
import { t } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { CornerDownRightIcon, EllipsisIcon, FileIcon, ListEndIcon, PencilIcon, CopyIcon, Trash2Icon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { resourceUrl, type Access, type MessageRecord } from "@/lib/actor-api";
import type { useActorConversation } from "@/hooks/use-actor-conversation";
type Chat = ReturnType<typeof useActorConversation>;
function QueueMessage({ message, active, chat, access }: {message: MessageRecord; active?: MessageRecord; chat: Chat; access: Access}) {
  const [editing, setEditing] = useState(false), [text, setText] = useState(message.content), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const original = useRef(message);
  useEffect(() => { if (!editing) setText(message.content); }, [message.content, editing]);
  const pendingGuide = message.kind === "steering";
  const controls = access.kind === "owner" && ["user", "remote_user"].includes(message.source.kind);
  const importing = !!importedConversation(message);
  const canGuide = controls && active && !pendingGuide && !importing;
  const summary = messageDisplayText(message).trim() || message.attachments.map(item => item.name).join("、");
  function edit() { original.current = message; setText(message.content); setError(""); setEditing(true); }
  async function run(action: () => Promise<unknown>) {
    setBusy(true); setError("");
    try { await action(); setEditing(false); }
    catch (cause) { setError(cause instanceof Error ? cause.message : t("message_queue.action_failed_try_again")); }
    finally { setBusy(false); }
  }
  return <li className="dl-queue-item" aria-label={t("message_queue.queued_message_value0", {value0: summary})}><div className="dl-queue-row">
    <ListEndIcon className="dl-queue-symbol" aria-hidden="true" />
    {message.attachments.slice(0, 2).map(item => <a key={item.id} className="dl-queue-thumbnail" href={resourceUrl(message.actor_id, item.id)} target="_blank" rel="noreferrer" title={item.name}>{item.media_type.startsWith("image/") ? <img src={resourceSource(resourceUrl(message.actor_id, item.id))} alt={item.name} loading="lazy" /> : <FileIcon aria-label={item.name} />}</a>)}
    <span className="dl-queue-text" title={summary}>{summary}</span>
    {pendingGuide ? <span className="dl-queue-wait" role="status">{t("message_queue.applying_guidance")}</span> : controls && <div className="dl-queue-actions">
      <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="sm" disabled={busy || !canGuide} aria-label={t("message_queue.guide_task_value0", {value0: summary})} onClick={() => { if (active) void run(() => chat.steer(message, active.id)); }}><CornerDownRightIcon /><span>{t("message_queue.guide")}</span></Button></TooltipTrigger><TooltipContent>{active ? t("message_queue.add_this_message_to_the_current_task") : t("message_queue.no_task_is_running")}</TooltipContent></Tooltip>
      <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon-sm" disabled={busy} aria-label={t("message_queue.withdraw_queued_message_value0", {value0: summary})} onClick={() => void run(() => chat.amend(message))}><Trash2Icon /></Button></TooltipTrigger><TooltipContent>{t("message_queue.withdraw")}</TooltipContent></Tooltip>
      <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon-sm" disabled={busy} aria-label={t("message_queue.more_message_actions_value0", {value0: summary})}><EllipsisIcon /></Button></DropdownMenuTrigger><DropdownMenuContent align="end" side="top">
        {!importing && <DropdownMenuItem onSelect={edit}><PencilIcon />{t("message_queue.edit_message")}</DropdownMenuItem>}
        <DropdownMenuItem onSelect={() => void run(() => navigator.clipboard.writeText(messageDisplayText(message)))}><CopyIcon />{t("conversation_content.copy_message")}</DropdownMenuItem>
        <DropdownMenuSeparator /><DropdownMenuItem variant="destructive" onSelect={() => void run(() => chat.clearQueue())}><Trash2Icon />{t("message_queue.clear_queue")}</DropdownMenuItem>
      </DropdownMenuContent></DropdownMenu>
    </div>}
  </div>{editing && <div className="dl-queue-editor"><Textarea aria-label={t("message_queue.edit_queued_message")} value={text} onChange={event => setText(event.target.value)} disabled={busy} autoFocus /><div><Button variant="ghost" size="sm" disabled={busy} onClick={() => setEditing(false)}>{t("message_queue.cancel")}</Button><Button size="sm" disabled={busy || !text.trim() && !message.attachments.length} onClick={() => void run(() => chat.amend(original.current, text))}>{t("message_queue.save")}</Button></div></div>}
    {error && <p className="dl-operation-error" role="alert">{error}</p>}
  </li>;
}
export function MessageQueue({ messages, active, chat, access }: {messages: MessageRecord[]; active?: MessageRecord; chat: Chat; access: Access}) {
  const [error, setError] = useState("");
  if (!messages.length && chat.queueCursor == null) return null;
  return <section className="dl-message-queue" aria-label={t("message_queue.queued_messages")}><ul>{messages.map(message => <QueueMessage key={message.id} message={message} active={active} chat={chat} access={access} />)}</ul>
    {chat.queueCursor != null && <Button variant="ghost" size="sm" onClick={() => { void chat.loadQueue(chat.queueCursor ?? undefined).catch(() => setError(t("message_queue.could_not_load_the_queue"))); }}>{t("message_queue.show_more_queued_messages")}</Button>}
    {error && <p role="alert" className="dl-operation-error">{error}</p>}
  </section>;
}
