import { ChevronRightIcon } from "lucide-react";
import { useState, type CSSProperties } from "react";
import { t } from "@/lib/i18n";
import { delegationState, useDelegationStatus, type Delegation } from "@/lib/delegations";
import { useActorName } from "@/hooks/use-actor-name";
import type { ActiveActor } from "@/hooks/use-active-actors";
import { GeneratedAvatar } from "@/components/generated-avatar";
import { actorAvatarSeed } from "@/lib/conversation-avatars";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

export function DelegationCards({ items, onActor }: { items: Delegation[]; onActor(id: string): void }) {
  return <ul className="dl-delegations" aria-label={t("delegation.tasks")}>{items.map(item => <li key={item.resultId}><DelegationCard item={item} onActor={onActor} /></li>)}</ul>;
}

function DelegationCard({ item, onActor }: { item: Delegation; onActor(id: string): void }) {
  const state = delegationState(useDelegationStatus(item.actorId, item.messageId));
  const name = useActorName(item.actorId, item.name || undefined) || item.actorId;
  const heading = item.title || (item.followUp ? t("delegation.follow_up") : t("delegation.task"));
  return <button type="button" className="dl-delegation" data-state={state} onClick={() => onActor(item.actorId)} aria-label={t("delegation.open", { name, task: heading })}>
    <GeneratedAvatar seed={actorAvatarSeed(item.actorId)} className="dl-delegation-avatar" />
    <span className="dl-delegation-text">
      <strong>{heading}</strong>
      <small><span>{name}</span><span className="dl-delegation-state">{state === "working" && <span className="dl-status-dot dl-status-working" aria-hidden="true" />}{t(`delegation.${state}`)}</span></small>
    </span>
    <ChevronRightIcon aria-hidden="true" />
  </button>;
}

export function ActiveWork({ actors, onActor }: { actors: ActiveActor[]; onActor(id: string): void }) {
  const [open, setOpen] = useState(false);
  if (!actors.length) return null;
  const label = t("delegation.working_now", { count: actors.length });
  return <Popover open={open} onOpenChange={setOpen}>
    <PopoverTrigger asChild>
      <button type="button" className="dl-active-work">
        <span className="dl-active-work-grid" aria-hidden="true">{Array.from({ length: 9 }, (_, index) => <i key={index} style={{ "--wave": index % 3 + Math.floor(index / 3) } as CSSProperties} />)}</span>
        <span>{label}</span>
      </button>
    </PopoverTrigger>
    <PopoverContent side="top" align="start" sideOffset={6} className="dl-active-work-menu" aria-label={label}>
      <ul>{actors.map(actor => <li key={actor.id}>
        <button type="button" onClick={() => { setOpen(false); onActor(actor.id); }} title={t("delegation.open_actor", { name: actor.name })}>
          <GeneratedAvatar seed={actorAvatarSeed(actor.id)} /><span className="dl-active-work-name">{actor.name}</span><span className="dl-status-dot dl-status-working" aria-hidden="true" />
        </button>
      </li>)}</ul>
    </PopoverContent>
  </Popover>;
}
