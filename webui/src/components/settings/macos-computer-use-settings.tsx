import { useEffect, useRef, useState } from "react";
import { ChevronDownIcon } from "lucide-react";
import { Input } from "@/components/ui/input";
import { CollapsibleContent } from "@/components/ui/collapsible";
import { t } from "@/lib/i18n";
import { configureComputerUse, configureComputerUseDisplay, validMacDisplay, getComputerUseStatus, requestComputerUsePermission, type ComputerUsePermissions } from "@/lib/macos-computer-use";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldTitle } from "@/components/ui/field";
import { SettingsGroup, SettingsRow } from "./structure";
import { errorText, Picker, StatusNote } from "./controls";

export function MacComputerUseSettings() {
  const [status, setStatus] = useState<ComputerUsePermissions | null>(null);
  const [dialog, setDialog] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const busy = useRef(false);
  const revision = useRef(0);
  const authorized = !!status?.accessibility && !!status?.screen_recording;
  const ready = authorized && !!status?.display_ready;
  const [advanced, setAdvanced] = useState(false);
  const [draft, setDraft] = useState({width: "", height: "", scale: "1"});
  const dirty = useRef(false);
  const [invalidDisplay, setInvalidDisplay] = useState(false);
  const [displayError, setDisplayError] = useState<string | null>(null);
  function accept(actual: ComputerUsePermissions) {
    setStatus(actual);
    if (!dirty.current) setDraft({width:String(actual.display.width),height:String(actual.display.height),scale:String(actual.display.scale)});
  }
  async function applyDisplay() {
    if (busy.current || !dirty.current) return;
    const candidate = {width:Number(draft.width),height:Number(draft.height),scale:Number(draft.scale)};
    if (!validMacDisplay(candidate)) {setInvalidDisplay(true);setDisplayError(t("macos_computer_use.display_invalid"));return;}
    busy.current = true;revision.current++;setPending(true);setInvalidDisplay(false);setDisplayError(null);
    try {const actual = await configureComputerUseDisplay(candidate);if(mounted.current){dirty.current=false;accept(actual);}}
    catch(cause) {
      if(mounted.current)setDisplayError(errorText(cause));
      try {const actual = await getComputerUseStatus();if (mounted.current) accept(actual);} catch {}
    }
    finally {busy.current=false;if(mounted.current)setPending(false);}
  }
  async function refresh() {
    if (busy.current) return;
    const current = revision.current;
    try {const actual = await getComputerUseStatus();if (mounted.current && !busy.current && current === revision.current) {accept(actual);setError(null);}}
    catch (cause) {if (mounted.current) setError(errorText(cause));}
  }
  useEffect(() => {mounted.current = true;void refresh();window.addEventListener("focus", refresh);return () => {mounted.current = false;window.removeEventListener("focus", refresh);};}, []);
  async function configure(enabled: boolean) {
    if (busy.current) return;
    busy.current = true;revision.current++;setPending(true);setError(null);
    try {const actual = await configureComputerUse(enabled);if (mounted.current) {accept(actual);setDialog(enabled && (!actual.accessibility || !actual.screen_recording));}}
    catch (cause) {
      if (mounted.current) setError(errorText(cause));
      try {const actual = await getComputerUseStatus();if (mounted.current) accept(actual);} catch {}
    }
    finally {busy.current = false;if (mounted.current) setPending(false);}
  }
  async function authorize(kind: "accessibility" | "screen_recording") {
    if (busy.current) return;
    busy.current = true;revision.current++;setPending(true);setError(null);
    try {
      await requestComputerUsePermission(kind);
      let actual = await getComputerUseStatus();
      if (actual.enabled && actual.accessibility && actual.screen_recording && !actual.display_ready) actual = await configureComputerUse(true);
      if (mounted.current) {accept(actual);if (actual.display_ready) setDialog(false);}
    }
    catch (cause) {
      if (mounted.current) setError(errorText(cause));
      try {const actual = await getComputerUseStatus();if (mounted.current) accept(actual);} catch {}
    }
    finally {busy.current = false;if (mounted.current) setPending(false);}
  }
  return <>
    {!dialog && <StatusNote text={error} error />}
    <SettingsGroup><SettingsRow id="dl-computer-use-enabled" label={t("gui_use.enable")} description={t("gui_use.scope")}>
      {status?.enabled && !ready && <span role="status" className="dl-settings-caption">{t(!status.display_supported ? "macos_computer_use.display_unsupported" : authorized ? "computer_use_settings.unavailable" : "macos_computer_use.permission_required")}</span>}
      {pending && <Spinner />}
      <Switch id="dl-computer-use-enabled" checked={status?.enabled ?? false} disabled={pending || !status} onCheckedChange={value => {void configure(value);}} />
    </SettingsRow></SettingsGroup>
    {!status && <Button variant="outline" onClick={() => {void refresh();}}>{t("app.retry")}</Button>}
    {status?.enabled && !authorized && <Button variant="outline" disabled={pending} onClick={() => setDialog(true)}>{t("macos_computer_use.finish_authorization")}</Button>}
    {status?.enabled && authorized && !ready && <Button variant="outline" disabled={pending || !status.display_supported} onClick={() => {void configure(true);}}>{t("app.retry")}</Button>}
    <section aria-busy={pending}>
      <Button variant="ghost" className="dl-settings-disclosure" aria-expanded={advanced} aria-controls="dl-macos-display-advanced" onClick={() => setAdvanced(!advanced)}>{t("gui_use.advanced")}<ChevronDownIcon data-icon="inline-end" /></Button>
      <div id="dl-macos-display-advanced"><CollapsibleContent open={advanced} className="dl-settings-advanced">
        <form noValidate className="flex flex-col gap-4" onSubmit={event => {event.preventDefault();void applyDisplay();}}>
          <p className="dl-settings-caption">{t("computer_use_settings.display_effect")}</p>
          <SettingsGroup>{(["width","height"] as const).map(key => <SettingsRow key={key} id={`dl-macos-display-${key}`} label={t(`phone_use.${key}`)} invalid={invalidDisplay}>
            <Input id={`dl-macos-display-${key}`} type="number" inputMode="numeric" min={256} max={4096} step={1} aria-invalid={invalidDisplay || undefined} value={draft[key]} disabled={pending || !status}
              onChange={event => {dirty.current=true;setDraft(previous => ({...previous,[key]:event.target.value}));setDisplayError(null);setInvalidDisplay(false);}} />
          </SettingsRow>)}
          <Picker id="dl-macos-display-scale" label={t("macos_computer_use.scale")} value={draft.scale} disabled={pending || !status} options={[{value:"1",label:"1×"},{value:"2",label:"2×"}]} onChange={value => {dirty.current=true;setDraft(previous => ({...previous,scale:value}));setDisplayError(null);setInvalidDisplay(false);}} />
          </SettingsGroup>
          <StatusNote text={displayError} error />
          <Button type="submit" className="self-end" disabled={pending || !status || !dirty.current}>{pending && <Spinner data-icon="inline-start" />}{t("gui_use.apply")}</Button>
        </form>
      </CollapsibleContent></div>
    </section>
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
