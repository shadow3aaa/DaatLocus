import { t } from "@/lib/i18n";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { MonitorIcon, PaletteIcon, ShieldCheckIcon, SlidersHorizontalIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SettingsSection, SettingsGroup, SettingsRow } from "@/components/settings/structure";
import { AccountEditor } from "@/components/settings/account-editor";
import {
  getActor, getConfig, saveConfig, patchActorPreferences,
  type ActorDetail, type ApprovalMode, type SetupConfigRequest,
} from "@/lib/actor-api";
import type { Preferences } from "@/lib/preferences";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { modeLabel } from "@/components/composer";
import { ProvidersSettings } from "@/components/settings/providers-settings";
import { errorText, StatusNote } from "@/components/settings/controls";

export type SettingsTab = "personal" | "account" | "permissions" | "models";

export function SettingsDialog({ open, onOpenChange, actorId, onActorChanged, preferences, onPreferencesChanged, initialTab = "account", canManage = true }: {
  open: boolean; onOpenChange: (open: boolean) => void; actorId: string;
  onActorChanged: (actorId?: string) => void;
  preferences: Preferences; onPreferencesChanged: (preferences: Preferences) => void;
  initialTab?: SettingsTab; canManage?: boolean;
}) {
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const [actor, setActor] = useState<ActorDetail | null>(null);
  const [config, setConfig] = useState<SetupConfigRequest | null>(null);
  const [loading, setLoading] = useState(false);
  const [pending, setPending] = useState<"mode" | "config" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const [narrow, setNarrow] = useState(false);
  const actorRef = useRef(actorId);
  const scroll = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { if (scroll.current) scroll.current.scrollTop = 0; }, [tab]);
  const savedConfig = useRef("");
  actorRef.current = actorId;
  useEffect(() => {
    const media = matchMedia("(max-width: 640px)");
    const change = () => setNarrow(media.matches);
    change(); media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);
  useEffect(() => { setActor(null); }, [actorId]);

  useEffect(() => {
    if (!open) return;
    setTab(canManage ? initialTab : "personal"); setError(null); setNotice(null);setActor(null);setConfig(null);setDirty(false);setLoading(false);setConfirmClose(false);
  }, [open, initialTab, canManage]);
  useEffect(() => {
    if (!open || !canManage || tab!=="models"&&tab!=="permissions" || tab==="models"&&config!==null || tab==="permissions"&&actor!==null) return;
    const controller = new AbortController();
    setLoading(true);
    void Promise.allSettled([tab==="permissions"?getActor(actorId, { signal: controller.signal }):Promise.resolve(null), tab==="models"?getConfig({ signal: controller.signal }):Promise.resolve(null)]).then(([actorResult, configResult]) => {
      if (controller.signal.aborted) return;
      if (actorResult.status === "fulfilled"&&actorResult.value) setActor(actorResult.value);
      if (configResult.status === "fulfilled"&&configResult.value) { setConfig(configResult.value); savedConfig.current = JSON.stringify(configResult.value); }
      if (actorResult.status === "rejected") setError(errorText(actorResult.reason));
      else if (configResult.status === "rejected") setError(errorText(configResult.reason));
      setLoading(false);
    });
    return () => controller.abort();
  }, [open, canManage, actorId, tab, config===null, actor===null, retry]);

  async function changeMode(approval_mode: ApprovalMode) {
    if (!actor || pending) return;
    const targetActor=actorId;setPending("mode");setError(null);setNotice(null);
    try {const result=await patchActorPreferences(targetActor,{approval_mode});if(actorRef.current===targetActor){setActor(result);onActorChanged(targetActor);}}
    catch(cause){if(actorRef.current===targetActor)setError(errorText(cause));}
    finally{setPending(null);}
  }
  async function submitConfig(): Promise<boolean> {
    if (!config) return false;
    setPending("config"); setError(null); setNotice(null);
    try {
      await saveConfig(config);
      const saved = await getConfig();
      setConfig(saved); savedConfig.current = JSON.stringify(saved);
      setDirty(false);
      setNotice(t("settings_dialog.saved")); onActorChanged();
      return true;
    } catch (cause) { setError(errorText(cause)); return false; }
    finally { setPending(null); }
  }
  const locked = Boolean(pending);
  function requestClose(next: boolean) {
    if (locked) return;
    if (!next && dirty) { setConfirmClose(true); return; }
    onOpenChange(next);
  }

  return <Dialog open={open} onOpenChange={requestClose}><DialogContent className="dl-settings-dialog" aria-describedby={undefined}>
    <DialogHeader><DialogTitle>{t("app.settings")}</DialogTitle></DialogHeader>
    <Tabs value={tab} orientation={narrow ? "horizontal" : "vertical"} onValueChange={(value) => { setTab(value as SettingsTab); setNotice(null); setError(null); setLoading(false); }} className="dl-settings-tabs">
      <TabsList aria-label={t("settings_dialog.settings_categories")}>
        {canManage&&<TabsTrigger value="account"><MonitorIcon aria-hidden="true" />{t("settings_dialog.configuration")}</TabsTrigger>}
        <TabsTrigger value="personal"><PaletteIcon aria-hidden="true" />{t("settings_dialog.appearance")}</TabsTrigger>
        {canManage&&<><TabsTrigger value="permissions"><ShieldCheckIcon aria-hidden="true" />{t("settings_dialog.permissions")}</TabsTrigger><TabsTrigger value="models"><SlidersHorizontalIcon aria-hidden="true" />{t("providers_settings.providers")}</TabsTrigger></>}
      </TabsList>
      <div ref={scroll} className="dl-settings-scroll">
        <StatusNote text={error} error /><StatusNote text={notice} />
        {error && !loading && (tab === "models" && !config || tab === "permissions" && !actor) && <Button variant="outline" onClick={() => { setError(null); setRetry(value => value + 1); }}>{t("app.retry")}</Button>}
        <TabsContent value="personal"><SettingsSection title={t("settings_dialog.appearance")}><SettingsGroup><SettingsRow id="dl-client-theme" label={t("settings_dialog.theme")}>
          <select id="dl-client-theme" className="dl-setting-native-select" value={preferences.theme} onChange={event=>onPreferencesChanged({...preferences,theme:event.target.value as Preferences["theme"]})}><option value="system">{t("settings_dialog.system")}</option><option value="light">{t("settings_dialog.light")}</option><option value="dark">{t("settings_dialog.dark")}</option></select>
        </SettingsRow><SettingsRow id="dl-client-language" label={t("common.language")}>
          <select id="dl-client-language" className="dl-setting-native-select" value={preferences.language} onChange={event=>onPreferencesChanged({...preferences,language:event.target.value as Preferences["language"]})}>
            <option value="system">{t("common.browser_language")}</option><option value="zh-CN">{t("common.chinese")}</option><option value="en-US">{t("common.english")}</option>
          </select>
        </SettingsRow></SettingsGroup></SettingsSection></TabsContent>
        {canManage&&<TabsContent value="account" forceMount hidden={tab !== "account"}><AccountEditor onChanged={()=>onActorChanged()}/></TabsContent>}
        {canManage&&<TabsContent value="permissions"><SettingsSection title={t("settings_dialog.action_approval")}><SettingsGroup><SettingsRow label={t("settings_dialog.approval_mode")} stacked>
          <ToggleGroup type="single" aria-label={t("settings_dialog.approval_mode")} value={actor?.config?.permissions.approval_mode||"auto"} disabled={locked||loading||!actor} className="dl-settings-options" onValueChange={mode=>{if(mode)void changeMode(mode as ApprovalMode);}}>
            {(["none","ask","auto"] as const).map(mode=><ToggleGroupItem key={mode} value={mode}>{modeLabel(mode)}</ToggleGroupItem>)}
          </ToggleGroup>
        </SettingsRow></SettingsGroup></SettingsSection></TabsContent>}
        {canManage ? <><TabsContent value="models" forceMount hidden={tab !== "models"}>
          {loading && !config ? <div className="dl-settings-loading" role="status"><Spinner />{t("settings_dialog.loading_configuration")}</div> : config ? <ProvidersSettings config={config} onChange={value => {setConfig(value);setDirty(JSON.stringify(value) !== savedConfig.current);setNotice(null);}} onSave={submitConfig} pending={locked} dirty={dirty} /> : null}
        </TabsContent></> : null}
      </div>
    </Tabs>
    <Dialog open={confirmClose} onOpenChange={setConfirmClose}><DialogContent aria-describedby={undefined}>
      <DialogHeader><DialogTitle>{t("settings_dialog.discard_changes")}</DialogTitle></DialogHeader>
      <div className="dl-settings-actions"><Button variant="outline" onClick={() => setConfirmClose(false)}>{t("settings_dialog.keep_editing")}</Button><Button variant="destructive" onClick={() => { setConfirmClose(false); setDirty(false); onOpenChange(false); }}>{t("settings_dialog.discard")}</Button></div>
    </DialogContent></Dialog>
  </DialogContent></Dialog>;
}
