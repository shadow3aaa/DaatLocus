import { motionScrollBehavior } from "@/lib/appearance";
import { t } from "@/lib/i18n";
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ArrowDownIcon, RefreshCwIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Empty, EmptyHeader, EmptyTitle, EmptyDescription } from "@/components/ui/empty";
import { Composer, type ComposerModel } from "@/components/composer";
import { ConversationConnection } from "@/components/conversation-connection";
import { ConversationHistory } from "@/components/conversation-history";
import { PrimaryTranscript, WorkTranscript } from "@/components/conversation-turns";
import { partitionConversationMessages } from "@/lib/conversation-messages";
import { isPrimaryActor, isUserDelivery, isUserSource } from "@/lib/work-timeline";
import { useActorConversation } from "@/hooks/use-actor-conversation";
import { accessKey, actorModelReady, patchActorPreferences, type Access, type ActorDetail } from "@/lib/actor-api";
import { cn } from "@/lib/utils";
import type { ConversationConnectionMessage } from "@/lib/stage-api";
import { useConversationSearch } from "@/hooks/use-conversation-search";
import { matchingConversationMessages, matchesConversationText } from "@/lib/conversation-search";
import { FloatingSearchBar } from "@/components/floating-search";
import { LazyPage } from "@/components/lazy-page";

export function ConversationPane({ connectionMessage, actorId, access, onActor, onAccessRevoked, models = [], ready = true, compact = false, refreshVersion = 0, searchOpen = false, onSearchOpenChange, onActorLoaded, onConfigureModels }: {
  connectionMessage?: ConversationConnectionMessage | null;
  actorId: string; access: Access; onActor(id: string): void; onAccessRevoked(): void; models?: ComposerModel[]; ready?: boolean;
  compact?: boolean; refreshVersion?: number; searchOpen?: boolean; onSearchOpenChange?(open: boolean): void; onActorLoaded?(actor: ActorDetail): void;
  onConfigureModels?(): void;
}) {
  const primaryView = isPrimaryActor(actorId);
  const openActor = (id: string) => onActor(actorId.includes("~") && !id.includes("~") ? `${actorId.split("~")[0]}~${id}` : id);
  const chat = useActorConversation(actorId, { onAccessRevoked, sessionKey: accessKey(access), includeActivity: !primaryView });
  const [searchQuery, setSearchQuery] = useState("");
  const search = useConversationSearch(actorId, searchQuery, searchOpen);
  const filtering = searchOpen && Boolean(searchQuery.trim());
  const resultsId = useId();
  const normalView = useRef<{top: number; follow: boolean; anchor?: {id: string; top: number}}>({top: 0, follow: true});
  const previousSearch = useRef({open: false, query: ""});
  const pane = useRef<HTMLElement>(null);
  const footer = useRef<HTMLDivElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const olderAnchor = useRef<{node: HTMLElement; top: number} | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [changing, setChanging] = useState(false);
  const [localError, setLocalError] = useState("");
  const [preparingFiles, setPreparingFiles] = useState(false);
  const filePreparation = useRef({ actorId, busy: false });
  const actor = chat.actor;
  ready = actorModelReady(actor, ready);
  const canManage = access.kind === "owner" && (!actor?.remote || actor.access?.manage === true);
  const waitingMessage = connectionMessage || (chat.streamStatus === "reconnecting" ? "conversation_pane.reconnecting" : null);
  // Execution readiness gates submission. An interrupted observation stream
  // does not stop the HTTP endpoint from accepting a new input.
  const connecting = Boolean(connectionMessage);
  const archived = actor?.status === "archived";
  const canConverse = !archived && actor?.access?.converse !== false;
  const name = actor?.config?.name || actor?.name || (actorId === "core" ? "Daat Locus" : "Actor");
  const workingMessages = chat.messages.filter(message => message.status === "running" || message.status === "queued");
  const active = [...workingMessages].reverse().find(message => message.status === "running");
  const sourceMessages = useMemo(() => filtering ? matchingConversationMessages(search.ready && !search.failed ? search.items : [], chat.messages, searchQuery) : chat.messages, [filtering, search.ready, search.failed, search.items, chat.messages, searchQuery]);
  const visibleMessages = useMemo(() => partitionConversationMessages(sourceMessages, primaryView).transcript.filter(message => !(message.status === "cancelled" && message.started_at_ms == null && ["user", "remote_user"].includes(message.source.kind)) && (!filtering || !primaryView || (isUserSource(message) && matchesConversationText(message.content, searchQuery)) || (isUserDelivery(message) && matchesConversationText(message.reply || "", searchQuery)))), [sourceMessages, primaryView, filtering, searchOpen, searchQuery]);
  const presentationChat = filtering ? {...chat, messages: sourceMessages} : chat;
  const receiptMessageId = primaryView ? [...chat.messages].reverse().find(message => isUserSource(message) && !(message.status === "cancelled" && message.started_at_ms == null))?.id : undefined;
  useEffect(() => { follow.current = true; olderAnchor.current = null; setAtBottom(true); setLocalError(""); filePreparation.current = { actorId, busy: false }; setPreparingFiles(false); }, [actorId]);
  useEffect(() => { if (actor) onActorLoaded?.(actor); }, [actor, onActorLoaded]);
  useEffect(() => {
    if (!refreshVersion) return;
    let live = true;
    void chat.refreshActor().catch(error => { if (live) setLocalError(error instanceof Error ? error.message : String(error)); });
    return () => { live = false; };
  }, [refreshVersion, chat.refreshActor]);
  useEffect(() => {
    const area = scroll.current;
    if (!area) return;
    let touchY: number | null = null;
    const wheel = (event: WheelEvent) => {
      // The modal's document scroll lock assumes a positive scrollTop and
      // mistakes our bottom origin for the top. Let this viewport handle its
      // native scrolling; overscroll-behavior still contains it inside the pane.
      event.stopPropagation();
      if (event.deltaY < 0) follow.current = false;
    };
    const touchStart = (event: TouchEvent) => { touchY = event.touches.length === 1 ? event.touches[0].clientY : null; };
    const touchMove = (event: TouchEvent) => {
      if (event.touches.length !== 1 || touchY === null) return;
      event.stopPropagation();
      const next = event.touches[0].clientY;
      if (next > touchY) follow.current = false;
      touchY = next;
    };
    const keyDown = (event: KeyboardEvent) => {
      if (["ArrowUp", "PageUp", "Home"].includes(event.key) && !(event.target as HTMLElement).closest("input, textarea, select, [contenteditable]")) follow.current = false;
    };
    area.addEventListener("wheel", wheel, {passive: true});
    area.addEventListener("touchstart", touchStart, {passive: true});
    area.addEventListener("touchmove", touchMove, {passive: true});
    area.addEventListener("keydown", keyDown);
    return () => {
      area.removeEventListener("wheel", wheel);
      area.removeEventListener("touchstart", touchStart);
      area.removeEventListener("touchmove", touchMove);
      area.removeEventListener("keydown", keyDown);
    };
  }, [actorId]);
  useLayoutEffect(() => {
    const section = pane.current;
    const controls = footer.current;
    if (!section || !controls) return;
    const header = compact ? section.closest(".dl-actor-dialog")?.querySelector<HTMLElement>(".dl-actor-dialog-heading") : null;
    const updateInsets = () => {
      // Layout heights stay correct during the dialog's scale animation.
      section.style.setProperty("--dl-footer-space", `${controls.offsetHeight}px`);
      if (header) section.style.setProperty("--dl-header-space", `${header.offsetHeight}px`);
      // Growing drafts and attachment controls change the reserved space,
      // while the message viewport itself continues behind those controls.
      if (follow.current && scroll.current && scroll.current.scrollTop !== 0) scroll.current.scrollTop = 0;
      if (scroll.current) {
        const area = scroll.current;
        setAtBottom(area.scrollTop > -110);
      }
    };
    updateInsets();
    const observer = new ResizeObserver(updateInsets);
    observer.observe(controls);
    observer.observe(section);
    // Markdown, images and expanded activity can grow after the message commit.
    const list = scroll.current?.firstElementChild;
    if (list) observer.observe(list);
    if (header) observer.observe(header);
    return () => observer.disconnect();
  }, [actorId, compact, waitingMessage]);
  function visibleAnchor() {
    const area = scroll.current;
    return area && Array.from(area.querySelectorAll<HTMLElement>(".dl-message-turn")).find(item => item.getBoundingClientRect().bottom > area.getBoundingClientRect().top);
  }
  function rememberView() {
    const area = scroll.current; if (!area) return;
    const anchor = visibleAnchor();
    normalView.current = {top: area.scrollTop, follow: follow.current, anchor: anchor ? {id: anchor.id, top: anchor.getBoundingClientRect().top} : undefined};
  }
  useLayoutEffect(() => {
    const area = scroll.current; if (!area) return;
    const previous = previousSearch.current;
    previousSearch.current = {open: searchOpen, query: searchQuery};
    if (previous.open && !searchOpen) {
      const saved = normalView.current;
      follow.current = saved.follow;
      area.scrollTop = saved.follow ? 0 : saved.top;
      const anchor = saved.anchor && area.querySelector<HTMLElement>(`#${CSS.escape(saved.anchor.id)}`);
      if (anchor && saved.anchor && !saved.follow) area.scrollTop += anchor.getBoundingClientRect().top - saved.anchor.top;
      setAtBottom(area.scrollTop > -110);
      setSearchQuery("");
      return;
    }
    if (searchOpen && (!previous.open || previous.query !== searchQuery)) {
      follow.current = false;
      area.scrollTop = 0;
      setAtBottom(true);
    } else if (olderAnchor.current) {
      const anchor = olderAnchor.current;
      area.scrollTop += anchor.node.getBoundingClientRect().top - anchor.top;
      olderAnchor.current = null;
    } else if (!searchOpen && follow.current && area.scrollTop !== 0) area.scrollTop = 0;
    if (!searchOpen) rememberView();
  }, [visibleMessages, chat.generations, chat.tools, chat.approvals, chat.loading, searchOpen, searchQuery]);
  useLayoutEffect(() => {
    if (!searchOpen || motionScrollBehavior() === "auto") return;
    const list = scroll.current?.firstElementChild;
    const animation = list?.animate([{opacity: .4, transform: "translateY(5px)"}, {opacity: 1, transform: "translateY(0)"}], {duration: 180, easing: "cubic-bezier(.2,.8,.2,1)"});
    return () => animation?.cancel();
  }, [visibleMessages, searchOpen]);
  async function configure(next: { model: string | null }) {
    if (!actor?.config || active) return;
    setChanging(true); setLocalError("");
    try { await patchActorPreferences(actorId, { model: next.model }); await chat.refreshActor(); }
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
  function bottom() {
    follow.current = true;
    olderAnchor.current = null;
    setAtBottom(true);
    // Smooth scrolling emits intermediate positions that look like leaving the
    // bottom and can disable following before the new message is laid out.
    scroll.current?.scrollTo({ top: 0, behavior: "instant" });
  }
  function submit() {
    if (connecting) return;
    // Resume following before send() publishes the message or clears the draft.
    // Do not jump again after the request: the user may have scrolled meanwhile.
    bottom();
    if (searchOpen) {
      normalView.current = {top: 0, follow: true};
      onSearchOpenChange?.(false);
    }
    void chat.send().catch(() => {});
  }
  async function earlier() {
    const node = visibleAnchor();
    olderAnchor.current = node ? {node, top: node.getBoundingClientRect().top} : null;
    try { await chat.loadOlder(); }
    catch { olderAnchor.current = null; setLocalError(t("conversation_pane.could_not_load_earlier_messages")); }
  }
  async function moreMatches(signal: AbortSignal) {
    const node = visibleAnchor();
    olderAnchor.current = node ? {node, top: node.getBoundingClientRect().top} : null;
    try { await search.more(signal); } catch (error) { olderAnchor.current = null; throw error; }
  }
  const errors = localError || chat.error;
  return <section ref={pane} className={cn("dl-conversation", primaryView ? "dl-primary-conversation" : "dl-work-conversation", compact && "dl-conversation-compact")} aria-label={t("conversation_pane.value0_conversation", {value0: name})} data-search-open={searchOpen} onKeyDownCapture={event => { if (searchOpen && event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onSearchOpenChange?.(false); } }}>
    <FloatingSearchBar open={searchOpen} query={searchQuery} onQueryChange={setSearchQuery} onClose={() => onSearchOpenChange?.(false)} busy={search.busy} controls={resultsId} />
      {!compact && !primaryView && <header className="dl-conversation-heading"><span>{name}</span><span className={cn("dl-status-dot", active && "dl-status-working")} /><small>{chat.streamStatus === "reconnecting" ? t("conversation_pane.reconnecting") : active ? t("conversation_pane.working") : t("conversation_pane.ready")}</small></header>}
      <div className="dl-conversation-scroll" ref={scroll} onScroll={() => { const area = scroll.current; if (!area) return; const near = area.scrollTop > -110; if (!searchOpen) follow.current = area.scrollTop >= 0; setAtBottom(near); if (!searchOpen) rememberView(); }}>
        <div className="dl-message-list" id={resultsId} aria-busy={filtering && search.busy} aria-live={searchOpen ? "off" : "polite"} aria-relevant="additions">
          {!filtering && !chat.loading && actor && !chat.messages.length && !errors && <Empty className="dl-conversation-empty"><EmptyHeader>
            {primaryView && <img src="/logo-mark.svg" alt="" className="dl-empty-logo dl-brand-mark" />}
            <EmptyTitle>{!connecting && !ready ? t("conversation_pane.set_up_model") : primaryView ? "Daat Locus" : name}</EmptyTitle>
            {!connecting && ready && <EmptyDescription>{t("conversation_pane.start_conversation")}</EmptyDescription>}
          </EmptyHeader>{!connecting && !ready && onConfigureModels && <Button onClick={onConfigureModels}>{t("providers_settings.add_provider")}</Button>}</Empty>}
          {chat.loading && <div className="dl-loading-messages"><Skeleton className="h-24 w-4/5 rounded-3xl" /><Skeleton className="ml-auto h-14 w-3/5 rounded-3xl" /><Skeleton className="h-32 w-4/5 rounded-3xl" /></div>}
          {!filtering && chat.hasMore && <Button variant="ghost" size="sm" className="dl-load-history" disabled={chat.loadingOlder} onClick={() => { void earlier(); }}>{t("conversation_pane.load_earlier_messages")}</Button>}
          {filtering && search.ready && !search.failed && search.next !== null && <LazyPage key={searchQuery.trim()} cursor={search.next} onLoad={moreMatches} errorMessage={t("search_dialog.could_not_load_earlier_results")} />}
          {filtering && search.ready && search.failed && <div className="dl-client-error" role="alert"><span>{t("search_dialog.search_failed_try_again")}</span><Button variant="ghost" size="sm" onClick={search.retry}>{t("app.retry")}</Button></div>}
          {filtering && search.ready && !search.failed && !visibleMessages.length && <Empty className="dl-filter-empty"><EmptyHeader><EmptyTitle>{t("search_dialog.no_results")}</EmptyTitle></EmptyHeader></Empty>}
          {primaryView ? <PrimaryTranscript messages={visibleMessages} working={Boolean(active)} chat={presentationChat} searchQuery={searchOpen ? searchQuery : undefined} access={access} onActor={openActor} receiptMessageId={receiptMessageId} /> : <WorkTranscript messages={visibleMessages} chat={presentationChat} searchQuery={searchOpen ? searchQuery : undefined} access={access} onActor={openActor} />}
          <div className="dl-conversation-bottom" />
        </div>
      </div>
      {!primaryView && !searchOpen && <ConversationHistory messages={visibleMessages} scroll={scroll} onNavigate={() => {follow.current = false;}} />}
      <div ref={footer} className="dl-conversation-footer">
        <ConversationConnection message={waitingMessage} />
        {!connecting && !ready && chat.messages.length > 0 && onConfigureModels && <div className="dl-model-required"><span>{t("conversation_pane.model_required")}</span><Button variant="outline" size="sm" onClick={onConfigureModels}>{t("app.settings")}</Button></div>}
        {!atBottom && <Button className="dl-jump-bottom" variant="outline" size="icon" aria-label={t("conversation_pane.jump_to_latest")} onClick={bottom}><ArrowDownIcon /></Button>}
        {errors && <div className="dl-client-error" role="alert"><span>{errors}</span><Button size="sm" variant="ghost" onClick={() => { setLocalError(""); void chat.refresh(); }}><RefreshCwIcon data-icon="inline-start" />{t("app.retry")}</Button></div>}
        <Composer key={actorId} value={chat.draft} onValueChange={chat.setDraft} onSubmit={submit} attachments={chat.attachments} onFiles={selected => { void files(selected); }} onRemoveAttachment={chat.removeAttachment} uploading={chat.uploading || preparingFiles} busy={chat.sending || changing} working={Boolean(active)} onStop={() => { if (active) void chat.cancel(active.id).catch(() => {}); }} submissionDisabled={connecting} disabled={!canConverse || !actor || !ready || chat.deleted || chat.loading} canManage={canManage} models={models} modelName={actor?.config?.model || ""} onModelChange={model => { void configure({ model: model || null }); }} placeholder={archived ? t("conversation_pane.actor_archived") : !ready && !connecting ? t("conversation_pane.add_a_model_to_send_messages") : t("composer.message")} compact={compact} />
      </div>
  </section>;
}
