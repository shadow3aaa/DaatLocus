import type { Access, MessageRecord } from "./actor-api";
import { localActorId } from "./conversation-links";

export const accountAvatarSeed = (accountId?: string) => `account:${accountId || "local"}`;

/** Bare Actor references in a remote conversation belong to that device. */
export function conversationActorId(actorId: string, conversationId?: string): string {
  const qualified = !actorId.includes("~") && conversationId?.includes("~")
    ? `${conversationId.split("~")[0]}~${actorId}` : actorId;
  return localActorId(qualified);
}

export const actorAvatarSeed = (actorId: string, conversationId?: string) => `actor:${conversationActorId(actorId, conversationId)}`;

export function messageAvatar(message: MessageRecord, side: "input" | "reply", access: Access, conversationId = message.actor_id) {
  const source = message.source;
  const actorId = side === "reply" ? conversationActorId(message.actor_id, conversationId)
    : source.kind === "actor" ? conversationActorId(source.actor_id, conversationId)
    : source.kind === "remote_actor" ? conversationActorId(`${source.stage_id}~${source.actor_id}`) : null;
  return actorId ? { actorId, seed: actorAvatarSeed(actorId) }
    : { seed: accountAvatarSeed(access.account?.account_id), name: access.account?.name };
}
