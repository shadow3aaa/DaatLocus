import { useEffect, useRef, useState } from "react";
import { ChevronDownIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { configureComputerUse, getComputerUseStatus, type ComputerUsePatch, type ComputerUseStatus } from "@/lib/computer-use";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Spinner } from "@/components/ui/spinner";
import { CollapsibleContent } from "@/components/ui/collapsible";
import { errorText, Picker, StatusNote } from "./controls";
import { SettingsGroup, SettingsRow } from "./structure";

export function ComputerUseSettings() {
  const [status, setStatus] = useState<ComputerUseStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [displayError, setDisplayError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [resolution, setResolution] = useState("");
  const mounted = useRef(true);
  const busy = useRef(false);
  const dirty = useRef(false);
  const revision = useRef(0);
  function accept(value: ComputerUseStatus) {
    setStatus(value);
    if (!dirty.current) setResolution(`${value.display.width}x${value.display.height}`);
  }
  async function refresh() {
    if (busy.current) return;
    const current = ++revision.current;
    try {
      const value = await getComputerUseStatus();
      if (mounted.current && current === revision.current && !busy.current) { accept(value); setError(null); }
    } catch (cause) {
      if (mounted.current && current === revision.current) setError(errorText(cause));
    }
  }
  useEffect(() => {
    mounted.current = true; void refresh();
    window.addEventListener("focus", refresh);
    return () => { mounted.current = false; revision.current++; window.removeEventListener("focus", refresh); };
  }, []);
  async function update(patch: ComputerUsePatch) {
    if (busy.current) return;
    busy.current = true; revision.current++; setPending(true);
    if (patch.display) setDisplayError(null); else setError(null);
    try {
      const value = await configureComputerUse(patch);
      if (mounted.current) {
        if (patch.display) dirty.current = false;
        setError(null); setDisplayError(null);
        accept(value);
      }
    } catch (cause) {
      if (mounted.current) {
        if (patch.display) setDisplayError(errorText(cause)); else setError(errorText(cause));
      }
      try { const actual = await getComputerUseStatus(); if (mounted.current) accept(actual); } catch {}
    } finally { busy.current = false; if (mounted.current) setPending(false); }
  }
  const locked = pending || !status?.supported;
  const savedResolution = status ? `${status.display.width}x${status.display.height}` : "";
  return <>
    <StatusNote text={error} error />
    <SettingsGroup>
      <SettingsRow id="dl-computer-use-enabled" label={t("gui_use.enable")} description={t("gui_use.scope")}>
        {status?.enabled && status.state !== "ready" && <span role="status" className="dl-settings-caption">{t(`computer_use_settings.${status.state}`)}</span>}
        {pending && <Spinner />}
        <Switch id="dl-computer-use-enabled" checked={status?.enabled ?? false} disabled={locked} onCheckedChange={enabled => void update({enabled})} />
      </SettingsRow>
    </SettingsGroup>
    {(error || status?.enabled && status.state === "unavailable") && <Button variant="outline" disabled={pending} onClick={() => status ? void update({enabled: status.enabled}) : void refresh()}>{t("app.retry")}</Button>}
    <section aria-busy={pending}>
      <Button variant="ghost" className="dl-settings-disclosure" aria-expanded={advanced} aria-controls="dl-computer-use-advanced" onClick={() => setAdvanced(!advanced)}>{t("gui_use.advanced")}<ChevronDownIcon data-icon="inline-end" /></Button>
      <div id="dl-computer-use-advanced"><CollapsibleContent open={advanced} className="dl-settings-advanced">
        <form className="flex flex-col gap-4" onSubmit={event => {
          event.preventDefault();
          const display = status?.modes.find(mode => `${mode.width}x${mode.height}` === resolution);
          if (display) void update({display});
        }}>
          <p className="dl-settings-caption">{t("computer_use_settings.display_effect")}</p>
          <SettingsGroup><Picker id="dl-computer-use-resolution" label={t("computer_use_settings.resolution")} value={resolution} disabled={locked}
            options={status?.modes.map(mode => ({value: `${mode.width}x${mode.height}`, label: `${mode.width} × ${mode.height}`})) ?? []}
            onChange={value => {dirty.current = true; setResolution(value);}} /></SettingsGroup>
          <StatusNote text={displayError} error />
          <Button type="submit" className="self-end" disabled={locked || resolution === savedResolution}>{pending && <Spinner data-icon="inline-start" />}{t("gui_use.apply")}</Button>
        </form>
      </CollapsibleContent></div>
    </section>
  </>;
}
