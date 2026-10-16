import { useTranslation } from "react-i18next";
import { t } from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { LogOutIcon, SearchIcon, SettingsIcon, } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { AccessGate } from "@/components/access-gate";
import { DesktopTitlebar } from "@/components/desktop-titlebar";
import { MobileResourcePreview } from "@/components/mobile-resource-preview";
import { ActorDialog } from "@/components/actor-dialog";
import { ConversationPane } from "@/components/conversation-pane";
import { SearchDialog } from "@/components/search-dialog";
import { SettingsDialog } from "@/components/settings-dialog";
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
  const [entry, setEntry] = useState("root");
  const [actorStack, setActorStack] = useState<string[]>([]);
  const [actorTarget, setActorTarget] = useState<{actorId:string; id:string; revision:number} | null>(null);
  const [models, setModels] = useState<ComposerModel[]>([]);
  const [ready, setReady] = useState(false);
  const [settings, setSettings] = useState(false);
  const [settingsTab, setSettingsTab] = useState<"personal" | "account" | "permissions" | "models">("account");
  const [searching, setSearching] = useState(false);

  const [refreshVersion, setRefreshVersion] = useState(0);
  const [targetMessage, setTargetMessage] = useState<{ id: string; revision: number } | null>(null);
  const [logoutError, setLogoutError] = useState(false);
  const authEpoch = useRef(0);
  const peek = actorStack.at(-1) || null;

  useEffect(() => { applyPreferences(preferences); savePreferences(preferences); void setWebUiLanguage(preferences.language); const media = matchMedia("(prefers-color-scheme: dark)"); const change = () => applyPreferences(preferences); media.addEventListener("change", change); const languageChange = () => { void setWebUiLanguage(preferences.language); }; window.addEventListener("languagechange", languageChange); return () => { media.removeEventListener("change", change); window.removeEventListener("languagechange", languageChange); }; }, [preferences]);
  const revoked = useCallback(() => { authEpoch.current++; configureConversationDevice(null); clearConversationDrafts(); setAccess(null); setActorStack([]); setActorTarget(null); setTargetMessage(null); setSettings(false); setSearching(false); }, []);
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
    const linked = actorIdFromUrl(location.href, location.origin);
      const stage = await getStage();
      if (epoch !== authEpoch.current) return;
      configureConversationDevice(stage?.stage_id ?? null);
      const entry = stage?.primary_actor_id ?? "root";
      setEntry(entry); configurePrimaryActor(entry); setActorStack(linked && localActorId(linked) !== entry ? [localActorId(linked)] : []);
      const [configuration, readiness] = await Promise.allSettled([getConfig(), getConfigReadiness()]);
      if (epoch !== authEpoch.current) return;
      if (configuration.status === "fulfilled") setModels(configuration.value.models.map(model => ({ name: model.name, label: model.model_id || model.name, effort: model.thinking_budget || "", isDefault: configuration.value.main_model === model.name })));
      setReady(readiness.status === "fulfilled" && readiness.value.ready);
      setAccess(result);
  }, []);
  useEffect(() => { let live = true; setChecking(true); void configure().catch(error => { if (!live) return; if (error instanceof ActorApiError && [401,403,410].includes(error.status)) revoked(); else setAccess(null); }).finally(() => { if (live) setChecking(false); }); return () => { live = false; authEpoch.current++; }; }, [configure, revoked]);
  useEffect(() => { function pop() { const linked = actorIdFromUrl(location.href, location.origin); const id = linked ? localActorId(linked) : null; setActorStack(id && id !== entry ? [id] : []); } window.addEventListener("popstate", pop); return () => window.removeEventListener("popstate", pop); }, [entry]);
  useEffect(() => { function shortcut(event: KeyboardEvent) { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k" && access && !peek && !settings) { event.preventDefault(); setSearching(true); } } window.addEventListener("keydown", shortcut); return () => window.removeEventListener("keydown", shortcut); }, [access, peek, settings]);
  function openActor(address: string, messageId?: string) { const id = localActorId(address); setActorTarget(messageId ? {actorId:id,id:messageId,revision:Date.now()} : null); if (id === entry && !peek) { if (messageId) setTargetMessage(old => ({id: messageId, revision: (old?.revision || 0) + 1})); return; } setActorStack(old => old.at(-1) === id ? old : [...old, id]); history.pushState(null, "", conversationUrl(address)); }
  function closeActor() { setActorStack([]); history.replaceState(null, "", "/"); }
  function backActor() { const next = actorStack.slice(0, -1); setActorStack(next); history.replaceState(null, "", next.length ? conversationUrl(next.at(-1)!) : "/"); }
  function openSettings(tab: typeof settingsTab) { setSettingsTab(tab); setSettings(true); }
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
    <MobileResourcePreview />
    {access && logoutError && <div className="dl-action-error" role="alert"><span>{t("app.could_not_sign_out_try_again")}</span><Button variant="ghost" size="sm" onClick={() => { void logout(); }}>{t("app.retry")}</Button></div>}
    {checking ? <div className="dl-app-loading" role="status"><span className="dl-loading-orbit" />{t("app.connecting")}</div> : !access ? <AccessGate onAuthenticated={configure} /> : <>
      <div className="dl-floating-account"><DropdownMenu><DropdownMenuTrigger asChild><Button variant="outline" size="icon-lg" className="dl-profile-button" aria-label={t("app.configuration_and_settings")}><Avatar><AvatarFallback>{Array.from(access.account?.name || t("app.account")).slice(0, 2).join("")}</AvatarFallback></Avatar></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="start" sideOffset={12} className="dl-account-menu"><DropdownMenuLabel className="dl-account-menu-header"><Avatar className="size-9"><AvatarFallback>{Array.from(access.account?.name || t("app.account")).slice(0, 2).join("")}</AvatarFallback></Avatar><span><strong>{access.account?.name || t("app.account")}</strong><small>{t("app.signed_in")}</small></span></DropdownMenuLabel><DropdownMenuSeparator /><DropdownMenuGroup>
          <DropdownMenuItem onSelect={() => openSettings("account")}><SettingsIcon data-icon="inline-start" />{t("app.settings")}</DropdownMenuItem>
        </DropdownMenuGroup><DropdownMenuSeparator /><DropdownMenuGroup><DropdownMenuItem onSelect={() => { void logout(); }}><LogOutIcon data-icon="inline-start" />{t("app.sign_out")}</DropdownMenuItem></DropdownMenuGroup></DropdownMenuContent>
      </DropdownMenu></div>
      <div className="dl-floating-actions"><Tooltip><TooltipTrigger asChild><Button variant="outline" size="icon-lg" className="dl-floating-button" aria-label={t("app.search_conversation")} onClick={() => setSearching(true)}><SearchIcon /></Button></TooltipTrigger><TooltipContent>{t("app.search_conversation_ctrl_k")}</TooltipContent></Tooltip></div>
      <ConversationPane key={entry} actorId={entry} connectionMessage={connectionMessage} access={access} onActor={openActor} onAccessRevoked={revoked} models={models} ready={conversationReady} refreshVersion={refreshVersion} targetMessage={targetMessage} onConfigureModels={() => openSettings("models")} />
      <ActorDialog connectionMessage={peek === entry ? connectionMessage : null} initialTarget={actorTarget?.actorId === peek ? actorTarget : null} actorId={peek} open={Boolean(peek)} onClose={closeActor} onActor={openActor} onBack={actorStack.length > 1 ? backActor : undefined} access={access} models={models} ready={peek === entry ? conversationReady : ready} onAccessRevoked={revoked} refreshVersion={refreshVersion} />
      <SettingsDialog open={settings} onOpenChange={setSettings} actorId={entry} initialTab={settingsTab} preferences={preferences} onPreferencesChanged={setPreferences} onActorChanged={id => { void changed(id); }} canManage={access.kind === "owner"} />
      <SearchDialog open={searching} onOpenChange={setSearching} actorId={entry} onSelect={message => setTargetMessage(old => ({ id: message.id, revision: (old?.revision || 0) + 1 }))} />
    </>}
  </main></TooltipProvider>;
}
