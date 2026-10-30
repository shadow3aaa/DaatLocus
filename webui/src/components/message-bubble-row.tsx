import { forwardRef, type ComponentPropsWithoutRef } from "react";
import { GeneratedAvatar } from "@/components/generated-avatar";
import { cn } from "@/lib/utils";
import { useActorName } from "@/hooks/use-actor-name";
import { t } from "@/lib/i18n";

/** Avatars stay outside the bubble's theme and its send animation. */
export const MessageBubbleRow = forwardRef<HTMLDivElement, Omit<ComponentPropsWithoutRef<"div">, "name"> & { seed: string; actorId?: string; name?: string; own?: boolean }>(function MessageBubbleRow({ seed, actorId, name, own = false, className, children, ...props }, ref) {
  const sender = useActorName(actorId, name) || t("app.account");
  return <div {...props} ref={ref} className={cn("dl-message-bubble-row", own && "dl-message-bubble-row-own", className)}>
    <GeneratedAvatar seed={seed} className="dl-message-avatar" />
    <div className="dl-message-body"><small className="dl-message-sender">{sender}</small><div className="dl-message-content">{children}</div></div>
  </div>;
});
