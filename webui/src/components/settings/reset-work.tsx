import { useEffect, useState } from "react";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { getResetStatus, resetAccountWork, type ResetStatus } from "@/lib/stage-api";
import { clearConversationDrafts } from "@/hooks/use-actor-conversation";
import { SettingsGroup, SettingsRow, SettingsSection } from "./structure";
import { errorText, StatusNote } from "./controls";

export function ResetWork() {
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<ResetStatus | null>(null);
  const [confirmation, setConfirmation] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setInfo(null); setError(null); setConfirmation("");
    void getResetStatus({signal: controller.signal}).then(value => {
      if (!controller.signal.aborted) setInfo(value);
    }).catch(cause => { if (!controller.signal.aborted) setError(errorText(cause)); });
    return () => controller.abort();
  }, [open, retry]);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!info || pending || info.resetting || confirmation !== info.name) return;
    setPending(true); setError(null);
    try {
      await resetAccountWork(info);
      clearConversationDrafts();
      window.location.assign("/");
    } catch (cause) {
      setError(errorText(cause));
      // Refresh the generation after an ambiguous response or a concurrent reset.
      try { setInfo(await getResetStatus()); } catch { /* Keep the confirmation and original failure. */ }
      setPending(false);
    }
  }
  return <SettingsSection title={t("reset_work.section")}><SettingsGroup>
    <SettingsRow label={t("reset_work.title")} description={t("reset_work.preserved")}>
      <Button variant="destructive" onClick={() => setOpen(true)}>{t("reset_work.action")}</Button>
    </SettingsRow>
    <Dialog open={open} onOpenChange={next => { if (!pending) setOpen(next); }}>
      <DialogContent showCloseButton={!pending}>
        <form onSubmit={event => void submit(event)} className="flex flex-col gap-4">
          <DialogHeader><DialogTitle>{t("reset_work.title")}</DialogTitle><DialogDescription>{t("reset_work.warning")}</DialogDescription></DialogHeader>
          <p>{t("reset_work.preserved")}</p>
          <p>{t("reset_work.offline_devices")}</p>
          {!info && !error && <Spinner />}
          <StatusNote text={error} error />
          {error && !info && <Button type="button" variant="outline" onClick={() => setRetry(value => value + 1)}>{t("app.retry")}</Button>}
          {info && <Field><FieldLabel htmlFor="dl-reset-confirmation">{t("reset_work.confirm_name", {name: info.name})}</FieldLabel><Input id="dl-reset-confirmation" value={confirmation} disabled={pending || info.resetting} autoComplete="off" spellCheck={false} onChange={event => setConfirmation(event.target.value)} /></Field>}
          <DialogFooter><Button type="button" variant="outline" disabled={pending} onClick={() => setOpen(false)}>{t("reset_work.cancel")}</Button><Button type="submit" variant="destructive" disabled={pending || !info || info.resetting || confirmation !== info.name}>{pending && <Spinner data-icon="inline-start" />}{pending ? t("reset_work.resetting") : t("reset_work.confirm")}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  </SettingsGroup></SettingsSection>;
}
