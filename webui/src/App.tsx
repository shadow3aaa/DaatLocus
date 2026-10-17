import { useTranslation } from "react-i18next";
import { t } from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { PanelLeftIcon, SearchIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { NavigationDrawer, NavigationRail } from "@/components/app-navigation";
import { AccessGate } from "@/components/access-gate";
import { DesktopTitlebar } from "@/components/desktop-titlebar";
import { NativeResourcePreview } from "@/components/native-resource-preview";
import { AppsPage } from "@/components/apps-page";
import { ActorDialog } from "@/components/actor-dialog";
import { ConversationPane } from "@/components/conversation-pane";
import { SearchDialog } from "@/components/search-dialog";
import { SettingsPage, settingsPath, settingsTabFromPath, type SettingsPageHandle, type SettingsTab } from "@/components/settings-page";
import { useCoreStatus } from "@/hooks/use-core-status";
import { coreConnectionMessage, getStage } from "@/lib/stage-api";
import { configurePrimaryActor } from "@/lib/work-timeline";
import type { ComposerModel } from "@/components/composer";
import { ActorApiError, accessKey, getAccess, getActor, getConfig, getConfigReadiness, logoutSession, type Access } from "@/lib/actor-api";
import { clearConversationDrafts } from "@/hooks/use-actor-conversation";
import { actorIdFromUrl, conversationUrl, configureConversationDevice, localActorId } from "@/lib/conversation-links";
import { applyPreferences, readPreferences, savePreferences, type Preferences } from "@/lib/preferences";
import { setWebUiLanguage } from "@/lib/i18n";



export default function App() {
  useTranslation();
  const [preferences, setPreferences] = useState<Preferences>(readPreferences);
  const [access, setAccess] = useState<Access | null>(null);
  const [checking, setChecking] = useState(true);
  const [entry, setEntry] = useState("core");
  const [actorStack, setActorStack] = useState<string[]>([]);
  const [actorTarget, setActorTarget] = useState<{actorId:string; id:string; revision:number} | null>(null);
  const [models, setModels] = useState<ComposerModel[]>([]);
  const [ready, setReady] = useState(false);
  const [appsOpen, setAppsOpen] = useState(() => location.pathname === "/apps");
  const [settings, setSettings] = useState(() => settingsTabFromPath(location.pathname) !== null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>(() => settingsTabFromPath(location.pathname) ?? "general");
  const [mobile, setMobile] = useState(() => matchMedia("(max-width: 767px)").matches);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const settingsPage = useRef<SettingsPageHandle>(null);
  const settingsReturnPath = useRef("/");
  const [searching, setSearching] = useState(false);

  const [refreshVersion, setRefreshVersion] = useState(0);
  const [targetMessage, setTargetMessage] = useState<{ id: string; revision: number } | null>(null);
  const [logoutError, setLogoutError] = useState(false);
  const authEpoch = useRef(0);
  const peek = actorStack.at(-1) || null;
  useEffect(() => { const media = matchMedia("(max-width: 767px)"); const update = () => { setMobile(media.matches); setSidebarOpen(false); }; media.addEventListener("change", update); return () => media.removeEventListener("change", update); }, []);

  useEffect(() => { applyPreferences(preferences); savePreferences(preferences); void setWebUiLanguage(preferences.language); const media = matchMedia("(prefers-color-scheme: dark)"); const change = () => applyPreferences(preferences); media.addEventListener("change", change); const motion = matchMedia("(prefers-reduced-motion: reduce)"); motion.addEventListener("change", change); const languageChange = () => { void setWebUiLanguage(preferences.language); }; window.addEventListener("languagechange", languageChange); return () => { media.removeEventListener("change", change); motion.removeEventListener("change", change); window.removeEventListener("languagechange", languageChange); }; }, [preferences]);
  const revoked = useCallback(() => { authEpoch.current++; configureConversationDevice(null); clearConversationDrafts(); setAccess(null); setActorStack([]); setActorTarget(null); setTargetMessage(null); setSettings(false); setAppsOpen(false); setSearching(false); setSidebarOpen(false); }, []);
  const core = useCoreStatus(access ? accessKey(access) : null, revoked);
  const conversationReady = entry === "core" && core ? core.model_ready : ready;
  const connectionMessage = entry === "core" ? coreConnectionMessage(core) : null;
  // Configuration may arrive after login; refresh model choices when sync catches up.
  useEffect(() => {
    if (!access || !core?.synchronized) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function refreshModels() {
      const [configuration, readiness] = await Promise.allSettled([getConfig({signal:controller.signal}), getConfigReadiness({signal:controller.signal})]);
      if (controller.signal.aborted) return;
      if (configuration.status === "fulfilled") setModels(configuration.value.models.map(model => ({name:model.name,label:model.model_id || model.name,effort:model.thinking_budget || "",isDefault:configuration.value.main_model === model.name})));
      setReady(readiness.status === "fulfilled" && readiness.value.ready);
      if (configuration.status === "rejected" || readiness.status === "rejected") timer = setTimeout(() => { void refreshModels(); }, 2000);
      else setRefreshVersion(old => old + 1);
    }
    void refreshModels();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [access, core?.synchronized, core?.model_ready, core?.config_revision]);
  const configure = useCallback(async () => {
    const epoch = ++authEpoch.current;
    const result = await getAccess(); if (epoch !== authEpoch.current) return;
    const linked = actorIdFromUrl(location.href, location.href);
      const stage = await getStage();
      if (epoch !== authEpoch.current) return;
      configureConversationDevice(stage?.stage_id ?? null);
      const entry = stage?.primary_actor_id ?? "core";
      setEntry(entry); configurePrimaryActor(entry); setActorStack(linked && localActorId(linked) !== entry ? [localActorId(linked)] : []);
      const [configuration, readiness] = await Promise.allSettled([getConfig(), getConfigReadiness()]);
      if (epoch !== authEpoch.current) return;
      if (configuration.status === "fulfilled") setModels(configuration.value.models.map(model => ({ name: model.name, label: model.model_id || model.name, effort: model.thinking_budget || "", isDefault: configuration.value.main_model === model.name })));
      setReady(readiness.status === "fulfilled" && readiness.value.ready);
      setAppsOpen(location.pathname === "/apps");
      setAccess(result);
  }, []);
  useEffect(() => { let live = true; setChecking(true); void configure().catch(error => { if (!live) return; if (error instanceof ActorApiError && [401,403,410].includes(error.status)) revoked(); else setAccess(null); }).finally(() => { if (live) setChecking(false); }); return () => { live = false; authEpoch.current++; }; }, [configure, revoked]);
  useEffect(() => { function pop() { const tab = settingsTabFromPath(location.pathname); if (tab) {setSettingsTab(tab);setSettings(true);return;} if (settings) {history.pushState(null, "", settingsPath(settingsTab));settingsPage.current?.close();return;} setAppsOpen(location.pathname === "/apps"); const linked = actorIdFromUrl(location.href, location.href); const id = linked ? localActorId(linked) : null; setActorStack(id && id !== entry ? [id] : []); } window.addEventListener("popstate", pop); return () => window.removeEventListener("popstate", pop); }, [entry, settings, settingsTab]);
  useEffect(() => { function shortcut(event: KeyboardEvent) { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k" && access && !peek && !settings && !appsOpen) { event.preventDefault(); setSearching(true); } } window.addEventListener("keydown", shortcut); return () => window.removeEventListener("keydown", shortcut); }, [access, peek, settings, appsOpen]);
  function openActor(address: string, messageId?: string) { if (mobile) setSidebarOpen(false); const id = localActorId(address); setActorTarget(messageId ? {actorId:id,id:messageId,revision:Date.now()} : null); if (id === entry && !peek) { if (messageId) setTargetMessage(old => ({id: messageId, revision: (old?.revision || 0) + 1})); return; } setActorStack(old => old.at(-1) === id ? old : [...old, id]); history.pushState(null, "", conversationUrl(address)); }
  function closeActor() { setActorStack([]); history.replaceState(null, "", "/"); }
  function backActor() { const next = actorStack.slice(0, -1); setActorStack(next); history.replaceState(null, "", next.length ? conversationUrl(next.at(-1)!) : "/"); }
  function openSettings(tab: SettingsTab) { if (!settings) settingsReturnPath.current = location.pathname; setSettingsTab(tab); setSettings(true); setSidebarOpen(false); history.pushState(null, "", settingsPath(tab)); }
  function closeSettings(open: boolean) { if (open) return; setSettings(false); setAppsOpen(settingsReturnPath.current === "/apps"); history.replaceState(null, "", settingsReturnPath.current); }
  function home() { if (settings) { settingsPage.current?.close(); return; } setAppsOpen(false); closeActor(); if (mobile) setSidebarOpen(false); }
  function openApps() { if (settings) { settingsReturnPath.current = "/apps"; settingsPage.current?.close(); return; } setAppsOpen(true); setSearching(false); setActorStack([]); setSidebarOpen(false); history.pushState(null, "", "/apps"); }
  function search() { if (mobile) setSidebarOpen(false); setSearching(true); }
  async function changed(actorId?: string) {
    const epoch = authEpoch.current;
    setRefreshVersion(old => old + 1);
    if (actorId) {
      try { await getActor(actorId); }
      catch { if (actorStack.includes(actorId)) closeActor(); }
    }
    if (access?.kind === "owner") {
      const [config, status, stage] = await Promise.allSettled([getConfig(), getConfigReadiness(), getStage()]);
      if (epoch !== authEpoch.current) return;
      if (config.status === "fulfilled") setModels(config.value.models.map(model => ({ name: model.name, label: model.model_id || model.name, effort: model.thinking_budget || "", isDefault: config.value.main_model === model.name })));
      if (stage.status === "fulfilled") {
        const next = stage.value.primary_actor_id;
        configurePrimaryActor(next); setEntry(next);
      }
      if (status.status === "fulfilled") setReady(status.value.ready);
    }
  }
  async function logout() { setLogoutError(false); try { await logoutSession(); revoked(); history.replaceState(null, "", "/"); } catch { setLogoutError(true); } }
  return <TooltipProvider delayDuration={250}><main className="dl-app">
    <DesktopTitlebar />
    <NativeResourcePreview />
    {access && logoutError && <div className="dl-action-error" role="alert"><span>{t("app.could_not_sign_out_try_again")}</span><Button variant="ghost" size="sm" onClick={() => { void logout(); }}>{t("app.retry")}</Button></div>}
    {checking ? <div className="dl-app-loading" role="status"><span className="dl-loading-orbit" />{t("app.connecting")}</div> : !access ? <AccessGate onAuthenticated={configure} /> : <>
      <div className="dl-app-shell"><NavigationRail access={access} settings={settings} apps={appsOpen} onApps={openApps} onHome={home} onSettings={() => openSettings("general")} onLogout={() => void logout()} /><div className="dl-shell-body">
      {/* Keep the Core pane laid out so settings do not reset its reading position. */}
      <div className="dl-home-layout" data-inactive={settings || appsOpen} aria-hidden={settings || appsOpen || undefined} inert={settings || appsOpen || undefined}>
      <div className="dl-main-view"><header className="dl-main-heading">{mobile && <Button variant="ghost" size="icon-sm" className="dl-open-sidebar" aria-label={t("navigation.open_sidebar")} onClick={() => setSidebarOpen(true)}><PanelLeftIcon /></Button>}<Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon-sm" className="dl-main-search" aria-label={t("app.search_conversation")} onClick={search}><SearchIcon /></Button></TooltipTrigger><TooltipContent>{t("app.search_conversation_ctrl_k")}</TooltipContent></Tooltip></header>
      <ConversationPane key={entry} actorId={entry} connectionMessage={connectionMessage} access={access} onActor={openActor} onAccessRevoked={revoked} models={models} ready={conversationReady} refreshVersion={refreshVersion} targetMessage={targetMessage} onConfigureModels={() => openSettings("models")} />
      </div></div>
      {appsOpen && !settings && <AppsPage onNavigation={() => setSidebarOpen(true)} onAccessRevoked={revoked} />}
      {mobile && <NavigationDrawer access={access} open={sidebarOpen && !settings} onOpenChange={setSidebarOpen} onApps={openApps} onHome={home} onSettings={() => openSettings("general")} onLogout={() => void logout()} />}
      <SettingsPage onViewCore={() => {settingsReturnPath.current="/";setActorStack([]);closeSettings(false);void changed(entry);}} ref={settingsPage} open={settings} onOpenChange={closeSettings} actorId={entry} initialTab={settingsTab} onTabChange={tab => {setSettingsTab(tab);history.replaceState(null, "", settingsPath(tab));}} preferences={preferences} onPreferencesChanged={setPreferences} onActorChanged={id => { void changed(id); }} canManage={access.kind === "owner"} />
      </div></div>
      <ActorDialog connectionMessage={peek === entry ? connectionMessage : null} initialTarget={actorTarget?.actorId === peek ? actorTarget : null} actorId={peek} open={Boolean(peek) && !settings && !appsOpen} onClose={closeActor} onActor={openActor} onBack={actorStack.length > 1 ? backActor : undefined} access={access} models={models} ready={peek === entry ? conversationReady : ready} onAccessRevoked={revoked} refreshVersion={refreshVersion} />
      <SearchDialog open={searching} onOpenChange={setSearching} actorId={entry} onSelect={message => setTargetMessage(old => ({ id: message.id, revision: (old?.revision || 0) + 1 }))} />
    </>}
  </main></TooltipProvider>;
}
