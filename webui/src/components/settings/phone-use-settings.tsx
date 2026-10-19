import { useEffect, useRef, useState } from "react";
import { t } from "@/lib/i18n";
import { configurePhoneUse, configurePhoneUseDisplay, getPhoneUseStatus, type PhoneUseMethod, type PhoneUseStatus } from "@/lib/phone-use";
import { validPhoneUseDisplay, type PhoneUseDisplay } from "@/lib/phone-use-display";
import { ChevronDownIcon } from "lucide-react";
import { Input } from "@/components/ui/input";
import { CollapsibleContent } from "@/components/ui/collapsible";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { errorText, StatusNote } from "./controls";
import { SettingsGroup, SettingsRow } from "./structure";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

export function PhoneUseSettings() {
  const [status, setStatus] = useState<PhoneUseStatus | null>(null);
  const [method, setMethod] = useState<PhoneUseMethod>("shizuku");
  const [enabled, setEnabled] = useState(false);
  const [dialog, setDialog] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const busy = useRef(false);
  const [advanced, setAdvanced] = useState(false);
  const [displayDraft, setDisplayDraft] = useState({width: "", height: "", density: ""});
  const draft = useRef(displayDraft);
  const dirty = useRef(false);
  const [displayDirty, setDisplayDirty] = useState(false);
  const revision = useRef(0);
  const [savingDisplay, setSavingDisplay] = useState(false);
  const [invalidDisplay, setInvalidDisplay] = useState<Partial<Record<keyof PhoneUseDisplay, boolean>>>({});
  const [displayError, setDisplayError] = useState<string | null>(null);
  function acceptDisplay(display: PhoneUseDisplay) {
    if (dirty.current) return;
    draft.current = {width: String(display.width), height: String(display.height), density: String(display.density)};
    setDisplayDraft(draft.current);
  }
  function editDisplay(key: keyof PhoneUseDisplay, value: string) {
    dirty.current = true; setDisplayDirty(true); revision.current++;
    draft.current = {...draft.current, [key]: value}; setDisplayDraft(draft.current);
    setInvalidDisplay(previous => ({...previous, [key]: false}));
  }
  async function applyDisplay() {
    if (busy.current || !dirty.current) return;
    const next = {width: Number(draft.current.width), height: Number(draft.current.height), density: Number(draft.current.density)};
    if (!validPhoneUseDisplay(next)) {
      const pixelsInvalid = next.width * next.height > 8_000_000;
      setInvalidDisplay({width: pixelsInvalid || !Number.isInteger(next.width) || next.width < 256 || next.width > 4096,
        height: pixelsInvalid || !Number.isInteger(next.height) || next.height < 256 || next.height > 4096,
        density: !Number.isInteger(next.density) || next.density < 80 || next.density > 640});
      setDisplayError(t("phone_use.display_invalid")); return;
    }
    busy.current = true; revision.current++; setSavingDisplay(true); setDisplayError(null);
    try {
      const saved = await configurePhoneUseDisplay(next);
      if (mounted.current) {
        dirty.current = false; setDisplayDirty(false); setInvalidDisplay({});
        setStatus(saved); acceptDisplay(saved.display);
      }
    } catch (cause) {
      if (mounted.current) setDisplayError(errorText(cause));
      try {const actual = await getPhoneUseStatus();if (mounted.current) setStatus(actual);} catch {}
    }
    finally {busy.current = false;if (mounted.current) setSavingDisplay(false);}
  }
  async function refresh() {
    if (busy.current) return;
    const current = revision.current;
    try {
      const value = await getPhoneUseStatus();
      if (!mounted.current || busy.current || current !== revision.current) return;
      setStatus(value); setEnabled(value.enabled); setMethod(value.method); setError(null);
      acceptDisplay(value.display);
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
  }
  useEffect(() => {
    mounted.current = true;
    void refresh();
    window.addEventListener("focus", refresh);
    return () => { mounted.current = false; window.removeEventListener("focus", refresh); };
  }, []);
  async function update(nextEnabled: boolean, selected = method) {
    if (busy.current) return;
    busy.current = true; revision.current++; setMethod(selected); setPending(true); setError(null);
    try {
      const saved = await configurePhoneUse(nextEnabled, nextEnabled ? selected : status?.method ?? selected);
      if (mounted.current) {
        setStatus(saved); setEnabled(saved.enabled);
        acceptDisplay(saved.display);
        if (!nextEnabled || saved.state === "ready") setDialog(false);
      }
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { busy.current = false; if (mounted.current) setPending(false); }
  }
  return <>
    {!dialog && <StatusNote text={error} error />}
    <SettingsGroup>
      <SettingsRow id="dl-phone-use-enabled" label={t("gui_use.enable")} description={t("gui_use.scope")}>
        {enabled && status?.state !== "ready" && <span role="status" className="dl-settings-caption">{t(`phone_use.${pending ? "connecting" : status?.state ?? "disconnected"}`)}</span>}
        {pending && <Spinner />}
        <Switch id="dl-phone-use-enabled" checked={enabled} disabled={pending || savingDisplay || !status} onCheckedChange={value => {
          if (value) {setError(null);setMethod(status?.method ?? "shizuku");setDialog(true);}
          else void update(false);
        }} />
      </SettingsRow>
    </SettingsGroup>
    {enabled && <p className="dl-settings-caption">{t("phone_use.disable_effect")}</p>}
    {!pending && !dialog && (error || enabled && status?.state !== "ready" && status?.state !== "unsupported") &&
      <Button variant="outline" disabled={savingDisplay} onClick={() => {if (!status) void refresh(); else {setError(null);setDialog(true);}}}>{t("phone_use.reconnect")}</Button>}
    <section aria-busy={savingDisplay}>
      <Button variant="ghost" className="dl-settings-disclosure" aria-expanded={advanced} aria-controls="dl-phone-use-advanced" onClick={() => setAdvanced(!advanced)}>{t("gui_use.advanced")}<ChevronDownIcon data-icon="inline-end" /></Button>
      <div id="dl-phone-use-advanced"><CollapsibleContent open={advanced} className="dl-settings-advanced">
        <form noValidate className="flex flex-col gap-4" onSubmit={event => {event.preventDefault();void applyDisplay();}}>
        <p className="dl-settings-caption">{t("phone_use.display_effect")}</p>
        <SettingsGroup>
          {(["width", "height", "density"] as const).map(key => <SettingsRow key={key} id={`dl-phone-use-${key}`} label={t(`phone_use.${key}`)} invalid={invalidDisplay[key]}>
            <Input id={`dl-phone-use-${key}`} type="number" inputMode="numeric" min={key === "density" ? 80 : 256} max={key === "density" ? 640 : 4096} step={1} value={displayDraft[key]} disabled={pending || savingDisplay || !status} aria-invalid={invalidDisplay[key] || undefined}
              onChange={event => editDisplay(key, event.target.value)} />
          </SettingsRow>)}
        </SettingsGroup>
        <StatusNote text={displayError} error />
        <Button type="submit" className="self-end" disabled={pending || savingDisplay || !status || !displayDirty}>{savingDisplay && <Spinner data-icon="inline-start" />}{t("gui_use.apply")}</Button>
        </form>
      </CollapsibleContent></div>
    </section>
    <Dialog open={dialog} onOpenChange={value => {if (!pending) setDialog(value);}}>
      <DialogContent showCloseButton={!pending}>
        <DialogHeader><DialogTitle>{t("phone_use.authorize")}</DialogTitle><DialogDescription>{t("phone_use.choose_method")}</DialogDescription></DialogHeader>
        <StatusNote text={error} error />
          <ToggleGroup id="dl-phone-use-method" type="single" aria-label={t("phone_use.method")} value={pending ? method : ""} disabled={pending} onValueChange={value => {if (value) void update(true, value as PhoneUseMethod);}}>
            <ToggleGroupItem value="shizuku">{t("phone_use.methods.shizuku")}</ToggleGroupItem><ToggleGroupItem value="root">{t("phone_use.methods.root")}</ToggleGroupItem>
          </ToggleGroup>
        {pending && <p role="status" className="flex items-center gap-2"><Spinner />{t("phone_use.connecting")}</p>}
        {status?.enabled && status.state !== "ready" && <p role="status">{t(`phone_use.${status.state}`)}</p>}
      </DialogContent>
    </Dialog>
  </>;
}
