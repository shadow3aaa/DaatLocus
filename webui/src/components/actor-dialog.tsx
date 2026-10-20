import { t } from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeftIcon, SearchIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ConversationPane } from "@/components/conversation-pane";
import type { ComposerModel } from "@/components/composer";
import type { Access, ActorDetail } from "@/lib/actor-api";
import type { ConversationConnectionMessage } from "@/lib/stage-api";

export function ActorDialog({ connectionMessage, actorId, open, onClose, onActor, onBack, access, models, ready, onAccessRevoked, refreshVersion }: { connectionMessage?: ConversationConnectionMessage | null; actorId: string | null; open: boolean; onClose(): void; onActor(id: string): void; onBack?: () => void; access: Access; models: ComposerModel[]; ready: boolean; onAccessRevoked(): void; refreshVersion: number }) {
  const [actor, setActor] = useState<ActorDetail | null>(null); const [searching, setSearching] = useState(false);
  const searchingRef = useRef(searching);
  searchingRef.current = searching;
  useEffect(() => { setSearching(false); }, [actorId]);
  useEffect(() => {
    if (!open) return;
    const shortcut = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      if (!(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey || event.isComposing || event.defaultPrevented || !["f", "k"].includes(key)) return;
      event.preventDefault();
      if (event.repeat) return;
      if (key === "k") setSearching(value => !value);
      else {
        setSearching(true);
        const input = document.querySelector<HTMLInputElement>(".dl-actor-dialog .dl-floating-search input");
        input?.focus({preventScroll: true}); input?.select();
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [open]);
  const loaded = useCallback((value: ActorDetail) => setActor(value), []);
  const title = actor?.id === actorId ? actor.config?.name || actor.name || t("actor_dialog.actor_conversation") : t("actor_dialog.actor_conversation");
  return <Dialog open={open} onOpenChange={value => { if (!value) onClose(); }}><DialogContent className="dl-actor-dialog" showCloseButton aria-describedby={undefined} onEscapeKeyDown={event => { if (searchingRef.current) { event.preventDefault(); setSearching(false); } }}>
    <DialogHeader className="dl-actor-dialog-heading" data-window-drag-region><div className="dl-actor-dialog-title"><Button size="icon-sm" variant="ghost" className={onBack ? undefined : "dl-actor-mobile-back"} onClick={onBack || onClose} aria-label={t(onBack ? "actor_dialog.back_to_previous_actor" : "app.back_to_main_conversation")}><ArrowLeftIcon /></Button><div><DialogTitle>{title}</DialogTitle></div></div>
      {actorId && <div className="dl-actor-dialog-actions"><Button variant="ghost" size="icon-sm" aria-label={t("actor_dialog.search_this_conversation")} aria-keyshortcuts="Control+f Meta+f Control+k Meta+k" title={t("app.search_conversation_shortcut")} disabled={actor?.id !== actorId} onClick={() => setSearching(true)}><SearchIcon /></Button></div>}
    </DialogHeader>
    {actorId && <ConversationPane key={actorId} actorId={actorId} connectionMessage={connectionMessage} access={access} onActor={onActor} onAccessRevoked={onAccessRevoked} models={models} ready={ready} compact refreshVersion={refreshVersion} searchOpen={searching} onSearchOpenChange={setSearching} onActorLoaded={loaded} />}
  </DialogContent></Dialog>;
}
