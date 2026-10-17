import { t } from "@/lib/i18n";
import { useCallback, useEffect, useState } from "react";
import { ArrowLeftIcon, SearchIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ConversationPane } from "@/components/conversation-pane";
import { SearchDialog } from "@/components/search-dialog";
import type { ComposerModel } from "@/components/composer";
import type { Access, ActorDetail } from "@/lib/actor-api";
import type { ConversationConnectionMessage } from "@/lib/stage-api";

export function ActorDialog({ connectionMessage, actorId, open, onClose, onActor, onBack, access, models, ready, onAccessRevoked, refreshVersion, initialTarget }: { connectionMessage?: ConversationConnectionMessage | null; actorId: string | null; open: boolean; onClose(): void; onActor(id: string, messageId?: string): void; onBack?: () => void; access: Access; models: ComposerModel[]; ready: boolean; onAccessRevoked(): void; refreshVersion: number; initialTarget?: {id:string; revision:number} | null }) {
  const [actor, setActor] = useState<ActorDetail | null>(null); const [searching, setSearching] = useState(false); const [targetMessage, setTargetMessage] = useState<{ id: string; revision: number } | null>(null);
  useEffect(() => { setTargetMessage(initialTarget || null); setSearching(false); }, [actorId, initialTarget]);
  useEffect(() => { if (!open) return; const key = (event: KeyboardEvent) => { if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") { event.preventDefault(); setSearching(true); } }; window.addEventListener("keydown",key); return () => window.removeEventListener("keydown",key); }, [open]);
  const loaded = useCallback((value: ActorDetail) => setActor(value), []);
  const title = actor?.id === actorId ? actor.config?.name || actor.name || t("actor_dialog.actor_conversation") : t("actor_dialog.actor_conversation");
  return <Dialog open={open} onOpenChange={value => { if (!value) onClose(); }}><DialogContent className="dl-actor-dialog" showCloseButton aria-describedby={undefined}>
    <DialogHeader className="dl-actor-dialog-heading"><div className="dl-actor-dialog-title"><Button size="icon-sm" variant="ghost" className={onBack ? undefined : "dl-actor-mobile-back"} onClick={onBack || onClose} aria-label={t(onBack ? "actor_dialog.back_to_previous_actor" : "app.back_to_main_conversation")}><ArrowLeftIcon /></Button><div><DialogTitle>{title}</DialogTitle></div></div>
      {actorId && <div className="dl-actor-dialog-actions"><Button variant="ghost" size="icon-sm" aria-label={t("actor_dialog.search_this_conversation")} disabled={actor?.id !== actorId} onClick={() => setSearching(true)}><SearchIcon /></Button></div>}
    </DialogHeader>
    {actorId && <ConversationPane key={actorId} actorId={actorId} connectionMessage={connectionMessage} access={access} onActor={onActor} onAccessRevoked={onAccessRevoked} models={models} ready={ready} compact refreshVersion={refreshVersion} targetMessage={targetMessage} onActorLoaded={loaded} />}
    {actorId && <SearchDialog open={searching} onOpenChange={setSearching} actorId={actorId} onSelect={message => setTargetMessage(old => ({ id: message.id, revision: (old?.revision || 0) + 1 }))} />}
  </DialogContent></Dialog>;
}
