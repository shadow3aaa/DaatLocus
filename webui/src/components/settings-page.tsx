import { t } from "@/lib/i18n";
import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState } from "react";
import { ArrowLeftIcon, DownloadIcon, MonitorIcon, SearchIcon, SettingsIcon, ShieldCheckIcon, SlidersHorizontalIcon, SunIcon, UserRoundIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { Button } from "@/components/ui/button";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { AppearanceSettings } from "@/components/settings/appearance-settings";
import { ImportSettings } from "@/components/settings/import-settings";
import { getImportCapabilities } from "@/lib/conversation-import";
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
import { errorText, Picker, StatusNote } from "@/components/settings/controls";
import { PhoneUseSettings } from "@/components/settings/phone-use-settings";
import { ComputerUseSettings } from "@/components/settings/computer-use-settings";
import { getComputerUseCapabilities } from "@/lib/computer-use";
import { isAndroid } from "@/lib/mobile";
import { isMacDesktop } from "@/lib/macos-computer-use";
import { MacComputerUseSettings } from "@/components/settings/macos-computer-use-settings";

export type SettingsTab = "general" | "personal" | "account" | "permissions" | "models" | "import" | "gui";
export type SettingsPageHandle = { close(): void };
const paths: Record<SettingsTab, string> = {general: "general", personal: "appearance", account: "devices", permissions: "permissions", models: "providers", import: "import", gui: "gui"};
export function settingsPath(tab: SettingsTab) { return `/settings/${paths[tab]}`; }
export function settingsTabFromPath(path: string): SettingsTab | null {
  return (Object.keys(paths) as SettingsTab[]).find(tab => path === settingsPath(tab)) ?? null;
}

export const SettingsPage = forwardRef<SettingsPageHandle, {
  open: boolean; onOpenChange: (open: boolean) => void; actorId: string;
  onActorChanged: (actorId?: string) => void;
  onViewCore(): void;
  preferences: Preferences; onPreferencesChanged: (preferences: Preferences) => void;
  initialTab?: SettingsTab; canManage?: boolean; onTabChange(tab: SettingsTab): void;
}>(function SettingsPage({open, onOpenChange, actorId, onActorChanged, onViewCore, preferences, onPreferencesChanged, initialTab = "general", canManage = true, onTabChange}, ref) {
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
  const [mobileDetail, setMobileDetail] = useState(false);
  const [accountVisited, setAccountVisited] = useState(initialTab === "account");
  const [guiVisited, setGuiVisited] = useState(initialTab === "gui");
  const [query, setQuery] = useState("");
  const [importsSupported, setImportsSupported] = useState(false);
  const [computerUseSupported, setComputerUseSupported] = useState(false);
  useEffect(() => {
    if (!open || !canManage || isAndroid() || isMacDesktop()) return;
    const controller = new AbortController();
    void getComputerUseCapabilities({signal: controller.signal}).then(status => {
      if (controller.signal.aborted) return;
      setComputerUseSupported(status.supported);
      if (!status.supported && initialTab === "gui") {setTab("general"); onTabChange("general");}
    }).catch(() => {if (!controller.signal.aborted) setComputerUseSupported(false);});
    return () => controller.abort();
  }, [open, canManage]);
  useEffect(() => {
    if (!open || !canManage) return;
    const controller = new AbortController();
    void getImportCapabilities(controller.signal).then(capabilities => {
      if(controller.signal.aborted)return;
      setImportsSupported(capabilities.codex);
      if(!capabilities.codex&&initialTab==="import"){setTab("general");onTabChange("general");}
    }).catch(()=>{if(!controller.signal.aborted){setImportsSupported(false);if(initialTab==="import"){setTab("general");onTabChange("general");}}});
    return ()=>controller.abort();
  }, [open, canManage]);
  const actorRef = useRef(actorId);
  const scroll = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { if (scroll.current) scroll.current.scrollTop = 0; }, [tab]);
  const savedConfig = useRef("");
  actorRef.current = actorId;
  useEffect(() => { setActor(null); }, [actorId]);

  useEffect(() => {
    if (!open) return;
    setError(null); setNotice(null);setActor(null);setConfig(null);setDirty(false);setLoading(false);setConfirmClose(false);setQuery("");setMobileDetail(initialTab !== "general");setAccountVisited(initialTab === "account");
  }, [open]);
  useEffect(() => { setTab(canManage ? initialTab : "personal"); if (initialTab === "account") setAccountVisited(true); }, [initialTab, canManage]);
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
  useImperativeHandle(ref, () => ({close: () => requestClose(false)}));
  const categories = [
    {id: "general" as const, label: t("settings_page.general"), icon: SettingsIcon, keywords: t("common.language")},
    {id: "personal" as const, label: t("settings_dialog.appearance"), icon: SunIcon, keywords: [t("settings_dialog.theme"), t("settings_dialog.light"), t("settings_dialog.dark"), t("settings_dialog.system"), t("appearance.code_theme"), t("appearance.accent"), t("appearance.interface_size"), t("appearance.content_size"), t("appearance.full_width")].join(" ")},
    ...(canManage ? [
      ...(importsSupported ? [{id: "import" as const, label: t("imports.title"), icon: DownloadIcon, keywords: "Codex"}] : []),
      {id: "account" as const, label: t("settings_dialog.configuration"), icon: UserRoundIcon, keywords: [t("access_gate.account_name"), t("account_editor.devices"), "SSH", t("access_gate.public_connection_service"), t("reset_work.title")].join(" ")},
      ...(isAndroid() || isMacDesktop() || computerUseSupported ? [{id: "gui" as const, label: t("gui_use.title"), icon: MonitorIcon, keywords: ["Shizuku Root ComputerUse PhoneUse macOS", t("gui_use.advanced"), t("computer_use_settings.resolution")].join(" ")}] : []),
      {id: "permissions" as const, label: t("settings_dialog.permissions"), icon: ShieldCheckIcon, keywords: t("settings_dialog.approval_mode")},
      {id: "models" as const, label: t("providers_settings.providers"), icon: SlidersHorizontalIcon, keywords: [t("composer.models"), t("composer.default_model"), t("provider_editor.api_key")].join(" ")},
    ] : []),
  ];
  const filtered = categories.filter(category => `${category.label} ${category.keywords}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()));
  function selectTab(next: SettingsTab) {setTab(next);setNotice(null);setError(null);setLoading(false);setMobileDetail(true);if(next === "account")setAccountVisited(true);if(next === "gui")setGuiVisited(true);onTabChange(next);}
  if (!open) return null;

  return <section className="dl-settings-page" data-mobile-detail={mobileDetail} data-category={tab} aria-label={t("app.settings")}>
    <aside className="dl-settings-sidebar">
      <header data-window-drag-region><Button variant="ghost" size="icon-sm" className="dl-settings-mobile-back" aria-label={t("settings_page.back_to_chat")} onClick={() => requestClose(false)}><ArrowLeftIcon /></Button><h1>{t("app.settings")}</h1></header>
      <InputGroup className="dl-settings-search"><InputGroupAddon><SearchIcon /></InputGroupAddon><InputGroupInput type="search" aria-label={t("settings_page.search_settings")} placeholder={t("settings_page.search_placeholder")} value={query} onChange={event => setQuery(event.target.value)} /></InputGroup>
      <nav aria-label={t("settings_dialog.settings_categories")}><p>{t("settings_page.personal")}</p>{filtered.map(category => <Button key={category.id} variant="ghost" disabled={locked} className="dl-settings-category" aria-current={tab === category.id ? "page" : undefined} onClick={() => selectTab(category.id)}><category.icon data-icon="inline-start" />{category.label}</Button>)}{!filtered.length && <p role="status">{t("search_dialog.no_results")}</p>}</nav>
    </aside>
    <div ref={scroll} className="dl-settings-scroll">
      <div className="dl-settings-content">
        <header className="dl-settings-page-heading" data-window-drag-region><Button variant="ghost" size="icon-sm" className="dl-settings-mobile-back" aria-label={t("settings_page.back_to_settings")} onClick={() => setMobileDetail(false)}><ArrowLeftIcon /></Button><h1>{categories.find(category => category.id === tab)?.label}</h1></header>
        <StatusNote text={error} error /><StatusNote text={notice} />
        {error && !loading && (tab === "models" && !config || tab === "permissions" && !actor) && <Button variant="outline" onClick={() => { setError(null); setRetry(value => value + 1); }}>{t("app.retry")}</Button>}
        <div hidden={tab !== "general"}><SettingsGroup>
          <Picker id="dl-client-language" label={t("common.language")} description={t("settings_page.interface_language")} value={preferences.language} options={[{value: "system", label: t("settings_page.detect_language")}, {value: "zh-CN", label: t("common.chinese")}, {value: "en-US", label: t("common.english")}]} onChange={value=>onPreferencesChanged({...preferences,language:value as Preferences["language"]})} />
        </SettingsGroup></div>
        <div hidden={tab !== "personal"}><AppearanceSettings preferences={preferences} onChange={onPreferencesChanged} /></div>
        {canManage && importsSupported && <div hidden={tab !== "import"}><ImportSettings onViewCore={onViewCore} /></div>}
        {canManage&&<div hidden={tab !== "account"}>{accountVisited && <AccountEditor onChanged={()=>onActorChanged()}/>}</div>}
        {canManage && (isAndroid() || isMacDesktop() || computerUseSupported) && <div hidden={tab !== "gui"}>{(guiVisited || initialTab === "gui") && (isAndroid() ? <PhoneUseSettings /> : isMacDesktop() ? <MacComputerUseSettings /> : <ComputerUseSettings />)}</div>}
        {canManage&&<div hidden={tab !== "permissions"}><SettingsSection title={t("settings_dialog.action_approval")}><SettingsGroup><SettingsRow label={t("settings_dialog.approval_mode")}>
          <ToggleGroup type="single" aria-label={t("settings_dialog.approval_mode")} value={actor?.config?.permissions.approval_mode||"auto"} disabled={locked||loading||!actor} className="dl-settings-options" onValueChange={mode=>{if(mode)void changeMode(mode as ApprovalMode);}}>
            {(["none","ask","auto"] as const).map(mode=><ToggleGroupItem key={mode} value={mode}>{modeLabel(mode)}</ToggleGroupItem>)}
          </ToggleGroup>
        </SettingsRow></SettingsGroup></SettingsSection></div>}
        {canManage ? <div hidden={tab !== "models"}>
          {loading && !config ? <div className="dl-settings-loading" role="status"><Spinner />{t("settings_dialog.loading_configuration")}</div> : config ? <ProvidersSettings config={config} onChange={value => {setConfig(value);setDirty(JSON.stringify(value) !== savedConfig.current);setNotice(null);}} onSave={submitConfig} pending={locked} dirty={dirty} /> : null}
        </div> : null}
      </div>
    </div>
    <Dialog open={confirmClose} onOpenChange={setConfirmClose}><DialogContent aria-describedby={undefined}>
      <DialogHeader><DialogTitle>{t("settings_dialog.discard_changes")}</DialogTitle></DialogHeader>
      <div className="dl-settings-actions"><Button variant="outline" onClick={() => setConfirmClose(false)}>{t("settings_dialog.keep_editing")}</Button><Button variant="destructive" onClick={() => { setConfirmClose(false); setDirty(false); onOpenChange(false); }}>{t("settings_dialog.discard")}</Button></div>
    </DialogContent></Dialog>
  </section>;
});
