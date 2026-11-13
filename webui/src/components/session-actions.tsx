import { useState, type ReactNode } from "react";
import { ArchiveIcon, ArchiveRestoreIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { t } from "@/lib/i18n";
import type { SessionSummary } from "@/lib/actor-api";
import { archiveSession, deleteSession, patchSession, unarchiveSession } from "@/lib/session-api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

export const sessionTitle = (session: Pick<SessionSummary, "title"> | null | undefined) => session?.title || t("sessions.untitled");
const errorText = (error: unknown) => error instanceof Error && error.message ? error.message : t("sessions.action_failed");

/** Owner session management shared by the session list and the open session's header. */
export function SessionActions({address, session, trigger, onChanged, onDeleted}: {
  address: string; session: SessionSummary; trigger: ReactNode;
  onChanged(session: SessionSummary): void; onDeleted(): void;
}) {
  const [dialog, setDialog] = useState<"rename" | "delete" | null>(null);
  const [title, setTitle] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const archived = session.status === "archived";
  async function run(action: () => Promise<void>) {
    setPending(true); setError(null);
    try { await action(); setDialog(null); } catch (failure) { setError(errorText(failure)); } finally { setPending(false); }
  }
  const toggleArchive = () => run(async () => onChanged(await (archived ? unarchiveSession : archiveSession)(address)));
  const close = (open: boolean) => { if (!open && !pending) { setDialog(null); setError(null); } };
  return <>
    <DropdownMenu>
      <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuGroup>
          <DropdownMenuItem disabled={pending} onSelect={() => { setTitle(session.title ?? ""); setError(null); setDialog("rename"); }}><PencilIcon data-icon="inline-start" />{t("sessions.rename")}</DropdownMenuItem>
          <DropdownMenuItem disabled={pending} onSelect={() => { void toggleArchive(); }}>{archived ? <ArchiveRestoreIcon data-icon="inline-start" /> : <ArchiveIcon data-icon="inline-start" />}{t(archived ? "sessions.unarchive" : "sessions.archive")}</DropdownMenuItem>
        </DropdownMenuGroup>
        {archived && <><DropdownMenuSeparator /><DropdownMenuGroup><DropdownMenuItem variant="destructive" disabled={pending} onSelect={() => { setError(null); setDialog("delete"); }}><Trash2Icon data-icon="inline-start" />{t("sessions.delete")}</DropdownMenuItem></DropdownMenuGroup></>}
      </DropdownMenuContent>
    </DropdownMenu>
    {error && !dialog && <div className="dl-action-error" role="alert"><span>{error}</span><Button variant="ghost" size="sm" onClick={() => setError(null)}>{t("common.close")}</Button></div>}
    <Dialog open={dialog === "rename"} onOpenChange={close}><DialogContent className="dl-session-dialog" aria-describedby={undefined} showCloseButton={!pending}>
      <DialogTitle>{t("sessions.rename")}</DialogTitle>
      <form onSubmit={event => { event.preventDefault(); event.stopPropagation(); if (!pending) void run(async () => onChanged(await patchSession(address, {title: title.trim() || null}))); }}>
        <FieldGroup><Field><FieldLabel htmlFor="dl-session-title">{t("sessions.name")}</FieldLabel><Input id="dl-session-title" value={title} onChange={event => setTitle(event.target.value)} disabled={pending} autoComplete="off" autoFocus /></Field></FieldGroup>
        {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
        <DialogFooter><Button type="button" variant="outline" disabled={pending} onClick={() => close(false)}>{t("common.cancel")}</Button><Button type="submit" disabled={pending}>{t("app.save")}</Button></DialogFooter>
      </form>
    </DialogContent></Dialog>
    <Dialog open={dialog === "delete"} onOpenChange={close}><DialogContent className="dl-session-dialog" showCloseButton={!pending}>
      <DialogTitle>{t("sessions.delete_title")}</DialogTitle>
      <DialogDescription>{t("sessions.delete_description", {title: sessionTitle(session)})}</DialogDescription>
      {error && <Alert variant="destructive"><AlertDescription>{error}</AlertDescription></Alert>}
      <DialogFooter><Button type="button" variant="outline" disabled={pending} onClick={() => close(false)}>{t("common.cancel")}</Button><Button type="button" variant="destructive" disabled={pending} onClick={() => { void run(async () => { await deleteSession(address); onDeleted(); }); }}>{t("sessions.delete")}</Button></DialogFooter>
    </DialogContent></Dialog>
  </>;
}
