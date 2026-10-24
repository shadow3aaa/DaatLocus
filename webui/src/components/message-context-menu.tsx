import { useState, useSyncExternalStore, type ReactElement, type SyntheticEvent } from "react";
import { CopyIcon } from "lucide-react";
import { ContextMenu, ContextMenuContent, ContextMenuGroup, ContextMenuItem, ContextMenuTrigger } from "@/components/ui/context-menu";
import { isAndroid } from "@/lib/mobile";
import { t } from "@/lib/i18n";

function touchActions() {
  return typeof window !== "undefined" && (isAndroid() || window.matchMedia("(hover: none)").matches);
}
function subscribeTouchActions(changed: () => void) {
  const query = window.matchMedia("(hover: none)");
  query.addEventListener("change", changed);
  return () => query.removeEventListener("change", changed);
}
function preserveInteractiveTarget(event: SyntheticEvent) {
  if (event.target instanceof Element && event.target.closest("a, button, input, textarea, select, pre, [role=button], [role=link], [role=region]")) event.stopPropagation();
}

export function MessageContextMenu({ content, children }: { content: string; children: ReactElement }) {
  const touch = useSyncExternalStore(subscribeTouchActions, touchActions, () => false);
  const [open, setOpen] = useState(false);
  const [failed, setFailed] = useState(false);
  async function copy() {
    try { await navigator.clipboard.writeText(content); setOpen(false); }
    catch { setFailed(true); }
  }
  return <ContextMenu open={touch && open} onOpenChange={value => {setOpen(value);if(value)setFailed(false);}}>
    <ContextMenuTrigger asChild disabled={!touch || !content} data-touch-actions={touch && Boolean(content) || undefined}
      onPointerDownCapture={touch ? preserveInteractiveTarget : undefined} onContextMenuCapture={touch ? preserveInteractiveTarget : undefined}>
      {children}
    </ContextMenuTrigger>
    <ContextMenuContent collisionPadding={12}>
      <ContextMenuGroup><ContextMenuItem onSelect={event => {event.preventDefault();void copy();}}><CopyIcon />{t("conversation_content.copy_message")}</ContextMenuItem></ContextMenuGroup>
      {failed && <p className="dl-operation-error" role="alert">{t("conversation_content.copy_failed")}</p>}
    </ContextMenuContent>
  </ContextMenu>;
}
