import { ChevronRightIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { delegationState, useDelegationStatus, type Delegation } from "@/lib/delegations";
import { useActorName } from "@/hooks/use-actor-name";
import type { ActiveActor } from "@/hooks/use-active-actors";
import { GeneratedAvatar } from "@/components/generated-avatar";
import { actorAvatarSeed } from "@/lib/conversation-avatars";

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
  if (!actors.length) return null;
  return <div className="dl-active-work" role="group" aria-label={t("delegation.working_now", { count: actors.length })}>
    <span className="dl-active-work-label"><span className="dl-status-dot dl-status-working" aria-hidden="true" />{t("delegation.working_now", { count: actors.length })}</span>
    {actors.map(actor => <button key={actor.id} type="button" className="dl-active-work-actor" onClick={() => onActor(actor.id)} title={t("delegation.open_actor", { name: actor.name })}>
      <GeneratedAvatar seed={actorAvatarSeed(actor.id)} /><span>{actor.name}</span>
    </button>)}
  </div>;
}
