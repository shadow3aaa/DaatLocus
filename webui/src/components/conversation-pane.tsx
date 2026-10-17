import { motionScrollBehavior } from "@/lib/appearance";
import { t } from "@/lib/i18n";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowDownIcon, RefreshCwIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Empty, EmptyHeader, EmptyTitle, EmptyDescription } from "@/components/ui/empty";
import { Composer, type ComposerModel } from "@/components/composer";
import { ConversationConnection } from "@/components/conversation-connection";
import { ConversationHistory } from "@/components/conversation-history";
import { PrimaryTurn, WorkTurn } from "@/components/conversation-turns";
import { MessageQueue, queuedUserInput } from "@/components/message-queue";
import { isPrimaryActor } from "@/lib/work-timeline";
import { OwnerApprovalNotice } from "@/components/owner-approval-notice";
import { useActorConversation } from "@/hooks/use-actor-conversation";
import { accessKey, actorModelReady, patchActorPreferences, type Access, type ApprovalMode, type ActorDetail } from "@/lib/actor-api";
import { cn } from "@/lib/utils";
import type { ConversationConnectionMessage } from "@/lib/stage-api";
import { connectionBlocks } from "@/lib/stage-api";

export function ConversationPane({ connectionMessage, actorId, access, onActor, onAccessRevoked, models = [], ready = true, compact = false, refreshVersion = 0, targetMessage, onActorLoaded, onConfigureModels }: {
  connectionMessage?: ConversationConnectionMessage | null;
  actorId: string; access: Access; onActor(id: string, messageId?: string): void; onAccessRevoked(): void; models?: ComposerModel[]; ready?: boolean;
  compact?: boolean; refreshVersion?: number; targetMessage?: { id: string; revision: number } | null; onActorLoaded?(actor: ActorDetail): void;
  onConfigureModels?(): void;
}) {
  const primaryView = isPrimaryActor(actorId);
  const openActor = (id: string, messageId?: string) => onActor(actorId.includes("~") && !id.includes("~") ? `${actorId.split("~")[0]}~${id}` : id, messageId);
  const chat = useActorConversation(actorId, { onAccessRevoked, sessionKey: accessKey(access), includeActivity: !primaryView });
  const pane = useRef<HTMLElement>(null);
  const footer = useRef<HTMLDivElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const initialized = useRef(false);
  const olderHeight = useRef<number | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [changing, setChanging] = useState(false);
  const [localError, setLocalError] = useState("");
  const [preparingFiles, setPreparingFiles] = useState(false);
  const filePreparation = useRef({ actorId, busy: false });
  const actor = chat.actor;
  ready = actorModelReady(actor, ready);
  const canManage = access.kind === "owner" && (!actor?.remote || actor.access?.manage === true);
  const waitingMessage = connectionMessage || (chat.streamStatus === "reconnecting" ? "conversation_pane.reconnecting" : null);
  const connecting = connectionBlocks(waitingMessage);
  const canConverse = actor?.access?.converse !== false;
  const name = actor?.config?.name || actor?.name || (actorId === "core" ? "Daat Locus" : "Actor");
  const workingMessages = chat.messages.filter(message => message.status === "running" || message.status === "queued");
  const active = workingMessages.find(message => message.status === "running");
  const mode = actor?.config?.permissions.approval_mode || "auto";
  const queue = primaryView ? [] : chat.messages.filter(queuedUserInput);
  const visibleMessages = chat.messages.filter(message => (primaryView || !queuedUserInput(message)) && !(message.status === "cancelled" && message.started_at_ms == null && ["user", "remote_user"].includes(message.source.kind)) && !(message.kind === "steering" && message.status === "completed" && chat.messages.some(parent => parent.id === message.in_reply_to)));
  const typingId = primaryView ? [...visibleMessages].reverse().find(message => message.status === "running")?.id : undefined;
  useEffect(() => { initialized.current = false; follow.current = true; setAtBottom(true); setLocalError(""); filePreparation.current = { actorId, busy: false }; setPreparingFiles(false); }, [actorId]);
  useEffect(() => { if (actor) onActorLoaded?.(actor); }, [actor, onActorLoaded]);
  useEffect(() => { if (refreshVersion) void chat.refresh(); }, [refreshVersion]);
  useLayoutEffect(() => {
    const section = pane.current;
    const controls = footer.current;
    if (!section || !controls) return;
    const header = compact ? section.closest(".dl-actor-dialog")?.querySelector<HTMLElement>(".dl-actor-dialog-heading") : null;
    const updateInsets = () => {
      // Layout heights stay correct during the dialog's scale animation.
      section.style.setProperty("--dl-footer-space", `${controls.offsetHeight}px`);
      if (header) section.style.setProperty("--dl-header-space", `${header.offsetHeight}px`);
      // Growing drafts and attachment/queue controls change the reserved space,
      // while the message viewport itself continues behind those controls.
      if (follow.current && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight;
      if (scroll.current) {
        const area = scroll.current;
        setAtBottom(area.scrollHeight - area.scrollTop - area.clientHeight < 110);
      }
    };
    updateInsets();
    const observer = new ResizeObserver(updateInsets);
    observer.observe(controls);
    observer.observe(section);
    if (header) observer.observe(header);
    return () => observer.disconnect();
  }, [actorId, compact, waitingMessage]);
  useLayoutEffect(() => {
    const area = scroll.current; if (!area) return;
    if (olderHeight.current !== null) { area.scrollTop += area.scrollHeight - olderHeight.current; olderHeight.current = null; return; }
    if (!chat.loading && !initialized.current) {
      initialized.current = true;
      if (targetMessage) follow.current = false;
      else area.scrollTop = area.scrollHeight;
    }
    else if (follow.current) area.scrollTop = area.scrollHeight;
  }, [chat.messages, chat.generations, chat.tools, chat.approvals, chat.loading]);
  useEffect(() => {
    if (!targetMessage || chat.loading) return;
    follow.current = false;
    let live = true;
    void chat.jump(targetMessage.id).then(() => {
      if (!live) return;
      requestAnimationFrame(() => {
        if (!live) return;
        const area = scroll.current;
        const node = area?.querySelector<HTMLElement>(`#${CSS.escape(`message-${actorId}-${targetMessage.id}`)}`);
        // Loading history can emit a bottom scroll event while jump() is pending.
        // Reassert the navigation intent before later activity updates arrive.
        follow.current = false;
        node?.scrollIntoView({ behavior: "instant", block: "center" });
        if (area) setAtBottom(area.scrollHeight - area.scrollTop - area.clientHeight < 110);
        node?.classList.add("dl-message-highlight");
        setTimeout(() => node?.classList.remove("dl-message-highlight"), 2500);
      });
    }).catch(() => { if (live) setLocalError(t("conversation_pane.could_not_open_this_record")); });
    return () => { live = false; };
  }, [targetMessage, actorId, chat.loading]);
  async function configure(next: { model?: string | null; mode?: ApprovalMode }) {
    if (!actor?.config || active) return;
    setChanging(true); setLocalError("");
    try { await patchActorPreferences(actorId, { ...(next.model !== undefined ? { model: next.model } : {}), ...(next.mode ? { approval_mode: next.mode } : {}) }); await chat.refreshActor(); }
    catch { setLocalError(t("conversation_pane.settings_were_not_saved_try_again")); } finally { setChanging(false); }
  }
  async function files(selected: File[]) {
    const preparation = filePreparation.current;
    if (preparation.actorId !== actorId || preparation.busy) return;
    preparation.busy = true; setPreparingFiles(true);
    setLocalError("");
    try {
      const folderFiles = selected.filter(file => file.webkitRelativePath);
      if (folderFiles.length) {
        const { zipFolder } = await import("@/lib/folder-attachment");
        const archive = await zipFolder(folderFiles);
        if (filePreparation.current !== preparation) return;
        await chat.addAttachments([archive]);
      } else await chat.addAttachments(selected);
    } catch (error) { if (filePreparation.current === preparation) setLocalError(error instanceof Error ? error.message : t("conversation_pane.attachment_upload_failed_try_again")); }
    finally { preparation.busy = false; if (filePreparation.current === preparation) setPreparingFiles(false); }
  }
  function bottom() { follow.current = true; setAtBottom(true); scroll.current?.scrollTo({ top: scroll.current.scrollHeight, behavior: motionScrollBehavior() }); }
  const errors = localError || chat.error;
  return <section ref={pane} className={cn("dl-conversation", primaryView ? "dl-primary-conversation" : "dl-work-conversation", compact && "dl-conversation-compact")} aria-label={t("conversation_pane.value0_conversation", {value0: name})}>
    <ConversationConnection message={waitingMessage}>
      {!compact && !primaryView && <header className="dl-conversation-heading"><span>{name}</span><span className={cn("dl-status-dot", active && "dl-status-working")} /><small>{chat.streamStatus === "reconnecting" ? t("conversation_pane.reconnecting") : active ? t("conversation_pane.working") : t("conversation_pane.ready")}</small></header>}
      <div className="dl-conversation-scroll" ref={scroll} onScroll={() => { const area = scroll.current; if (!area) return; const near = area.scrollHeight - area.scrollTop - area.clientHeight < 110; follow.current = near; setAtBottom(near); }}>
        <div className="dl-message-list" aria-live="polite" aria-relevant="additions">
          {!chat.loading && actor && !chat.messages.length && !errors && <Empty className="dl-conversation-empty"><EmptyHeader>
            {primaryView && <img src="/logo-mark.svg" alt="" className="dl-empty-logo dl-brand-mark" />}
            <EmptyTitle>{!connecting && !ready ? t("conversation_pane.set_up_model") : primaryView ? "Daat Locus" : name}</EmptyTitle>
            {!connecting && ready && <EmptyDescription>{t("conversation_pane.start_conversation")}</EmptyDescription>}
          </EmptyHeader>{!connecting && !ready && onConfigureModels && <Button onClick={onConfigureModels}>{t("providers_settings.add_provider")}</Button>}</Empty>}
          {chat.loading && <div className="dl-loading-messages"><Skeleton className="h-24 w-4/5 rounded-3xl" /><Skeleton className="ml-auto h-14 w-3/5 rounded-3xl" /><Skeleton className="h-32 w-4/5 rounded-3xl" /></div>}
          {chat.hasMore && <Button variant="ghost" size="sm" className="dl-load-history" disabled={chat.loadingOlder} onClick={() => { olderHeight.current = scroll.current?.scrollHeight ?? null; void chat.loadOlder().catch(() => setLocalError(t("conversation_pane.could_not_load_earlier_messages"))); }}>{t("conversation_pane.load_earlier_messages")}</Button>}
          {visibleMessages.map(message => primaryView ? <PrimaryTurn key={message.id} message={message} chat={chat} access={access} onActor={openActor} typing={message.id === typingId} /> : <WorkTurn key={message.id} message={message} chat={chat} access={access} onActor={openActor} />)}
          <div className="dl-conversation-bottom" />
        </div>
      </div>
      {!primaryView && <ConversationHistory messages={visibleMessages} scroll={scroll} onNavigate={() => {follow.current = false;}} />}
      <div ref={footer} className="dl-conversation-footer">
        {!connecting && !ready && chat.messages.length > 0 && onConfigureModels && <div className="dl-model-required"><span>{t("conversation_pane.model_required")}</span><Button variant="outline" size="sm" onClick={onConfigureModels}>{t("app.settings")}</Button></div>}
        {!compact && canManage && <OwnerApprovalNotice actorId={actorId} onActor={onActor} onAccessRevoked={onAccessRevoked} />}
        {!atBottom && <Button className="dl-jump-bottom" variant="outline" size="icon" aria-label={t("conversation_pane.jump_to_latest")} onClick={bottom}><ArrowDownIcon /></Button>}
        {errors && <div className="dl-client-error" role="alert"><span>{errors}</span><Button size="sm" variant="ghost" onClick={() => { setLocalError(""); void chat.refresh(); }}><RefreshCwIcon data-icon="inline-start" />{t("app.retry")}</Button></div>}
        <MessageQueue messages={queue} active={active} chat={chat} access={access} />
        <Composer presentation={primaryView ? "primary" : "work"} key={actorId} value={chat.draft} onValueChange={chat.setDraft} onSubmit={() => { if (connecting) return; void chat.send().then(bottom).catch(() => {}); }} attachments={chat.attachments} onFiles={selected => { void files(selected); }} onRemoveAttachment={chat.removeAttachment} uploading={chat.uploading || preparingFiles} busy={chat.sending || changing} working={Boolean(active)} onStop={() => { if (active) void chat.cancel(active.id).catch(() => {}); }} submissionDisabled={connecting} disabled={!canConverse || !actor || !ready || chat.deleted || chat.loading} canManage={canManage} mode={mode} onModeChange={next => { void configure({ mode: next }); }} models={models} modelName={actor?.config?.model || ""} onModelChange={model => { void configure({ model: model || null }); }} placeholder={!ready && !connecting ? t("conversation_pane.add_a_model_to_send_messages") : !primaryView ? t("conversation_pane.message_value0", {value0: name}) : t("composer.message")} compact={compact} />
      </div>
    </ConversationConnection>
  </section>;
}
