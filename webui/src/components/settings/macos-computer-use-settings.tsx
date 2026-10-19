import { useEffect, useRef, useState } from "react";
import { t } from "@/lib/i18n";
import { configureComputerUse, getComputerUseStatus, requestComputerUsePermission, type ComputerUsePermissions } from "@/lib/macos-computer-use";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldTitle } from "@/components/ui/field";
import { SettingsGroup, SettingsRow } from "./structure";
import { errorText, StatusNote } from "./controls";

export function MacComputerUseSettings() {
  const [status, setStatus] = useState<ComputerUsePermissions | null>(null);
  const [dialog, setDialog] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const busy = useRef(false);
  const revision = useRef(0);
  const ready = !!status?.accessibility && !!status?.screen_recording;
  async function refresh() {
    if (busy.current) return;
    const current = revision.current;
    try {const actual = await getComputerUseStatus();if (mounted.current && !busy.current && current === revision.current) {setStatus(actual);setError(null);}}
    catch (cause) {if (mounted.current) setError(errorText(cause));}
  }
  useEffect(() => {mounted.current = true;void refresh();window.addEventListener("focus", refresh);return () => {mounted.current = false;window.removeEventListener("focus", refresh);};}, []);
  async function configure(enabled: boolean) {
    if (busy.current) return;
    busy.current = true;revision.current++;setPending(true);setError(null);
    try {const actual = await configureComputerUse(enabled);if (mounted.current) {setStatus(actual);setDialog(enabled && (!actual.accessibility || !actual.screen_recording));}}
    catch (cause) {if (mounted.current) setError(errorText(cause));}
    finally {busy.current = false;if (mounted.current) setPending(false);}
  }
  async function authorize(kind: "accessibility" | "screen_recording") {
    if (busy.current) return;
    busy.current = true;revision.current++;setPending(true);setError(null);
    try {await requestComputerUsePermission(kind);const actual = await getComputerUseStatus();if (mounted.current) setStatus(actual);}
    catch (cause) {if (mounted.current) setError(errorText(cause));}
    finally {busy.current = false;if (mounted.current) setPending(false);}
  }
  return <>
    {!dialog && <StatusNote text={error} error />}
    <SettingsGroup><SettingsRow id="dl-computer-use-enabled" label={t("gui_use.enable")} description={t("macos_computer_use.scope")}>
      {status?.enabled && !ready && <span role="status" className="dl-settings-caption">{t("macos_computer_use.permission_required")}</span>}
      {pending && <Spinner />}
      <Switch id="dl-computer-use-enabled" checked={status?.enabled ?? false} disabled={pending || !status} onCheckedChange={value => {void configure(value);}} />
    </SettingsRow></SettingsGroup>
    {!status && <Button variant="outline" onClick={() => {void refresh();}}>{t("common.retry")}</Button>}
    {status?.enabled && !ready && <Button variant="outline" disabled={pending} onClick={() => setDialog(true)}>{t("macos_computer_use.finish_authorization")}</Button>}
    <Dialog open={dialog} onOpenChange={setDialog}><DialogContent>
      <DialogHeader><DialogTitle>{t("gui_use.enable")}</DialogTitle><DialogDescription>{t("macos_computer_use.permission_description")}</DialogDescription></DialogHeader>
      <StatusNote text={error} error />
      <FieldGroup>{(["accessibility", "screen_recording"] as const).map(kind => <Field key={kind} orientation="horizontal" aria-labelledby={`dl-computer-use-${kind}`}>
        <FieldContent><FieldTitle id={`dl-computer-use-${kind}`}>{t(`macos_computer_use.${kind}`)}</FieldTitle><FieldDescription>{t(`macos_computer_use.${kind}_description`)}</FieldDescription></FieldContent>
        <Button variant="outline" className="shrink-0" disabled={pending || status?.[kind]} onClick={() => {void authorize(kind);}}>{status?.[kind] ? t("macos_computer_use.granted") : t("macos_computer_use.authorize")}</Button>
      </Field>)}</FieldGroup>
      <p className="dl-settings-caption">{t("macos_computer_use.restart_hint")}</p>
    </DialogContent></Dialog>
  </>;
}
