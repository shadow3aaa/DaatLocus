import { browserSupported } from "@/lib/browser-workspace";
import { useTranslation } from "react-i18next";
import { t } from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDownIcon, MessagesSquareIcon, PanelLeftIcon, SearchIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { NavigationDrawer, NavigationRail, type NavigationPage } from "@/components/app-navigation";
import { AccessGate } from "@/components/access-gate";
import { AccountError } from "@/components/account-error";
import { accountErrorCode } from "@/lib/account-errors";
import { DesktopTitlebar } from "@/components/desktop-titlebar";
import { NativeResourcePreview } from "@/components/native-resource-preview";
import { BrowserPage } from "@/components/browser-page";
import { VirtualDisplaysPage } from "@/components/virtual-displays-page";
import { virtualDisplayCapabilities } from "@/lib/virtual-displays";
import { SessionsPage } from "@/components/sessions-page";
import { NewSession, clearNewSessionDraft } from "@/components/new-session";
import { SessionActions, sessionTitle } from "@/components/session-actions";
import { AppsPage } from "@/components/apps-page";
import { ActorDialog } from "@/components/actor-dialog";
import { ConversationPane } from "@/components/conversation-pane";
import { SettingsPage, settingsPath, settingsTabFromPath, type SettingsPageHandle, type SettingsTab } from "@/components/settings-page";
import { useStageStatus } from "@/hooks/use-stage-status";
import { getStage } from "@/lib/stage-api";
import type { ComposerModel } from "@/components/composer";
import { ActorApiError, accessKey, restoreSession, renewSession, getActor, getConfig, getConfigReadiness, logoutSession, type Access, type ActorDetail } from "@/lib/actor-api";
import { clearConversationDrafts } from "@/hooks/use-actor-conversation";
import { useSessionWork } from "@/hooks/use-active-actors";
import { delegationStatuses } from "@/lib/delegations";
import { actorIdFromUrl, conversationUrl, configureConversationDevice, localActorId } from "@/lib/conversation-links";
import { applyPreferences, readPreferences, savePreferences, type Preferences } from "@/lib/preferences";
import { setWebUiLanguage } from "@/lib/i18n";

type Page = "apps" | "virtual-displays" | "browser" | "sessions" | null;
const PAGE_PATHS: Record<Exclude<Page, null>, string> = { apps: "/apps", "virtual-displays": "/virtual-displays", browser: "/browser", sessions: "/sessions" };
const pageFromPath = (path: string): Page => (Object.entries(PAGE_PATHS).find(([, value]) => value === path)?.[0] as Page | undefined) ?? null;
const remotePrefix = (address: string) => address.includes("~") ? `${address.split("~")[0]}~` : "";
/** The session an Actor belongs to, addressed on the same device as the Actor. */
const sessionOf = (address: string, actor: ActorDetail) => actor.role === "session" ? address : localActorId(`${remotePrefix(address)}${actor.session_id}`);

export default function App() {
  useTranslation();
  const [preferences, setPreferences] = useState<Preferences>(readPreferences);
  const [access, setAccess] = useState<Access | null>(null);
  const [checking, setChecking] = useState(true);
  const [startupError, setStartupError] = useState<string | null>(null);
  const [startupAttempt, setStartupAttempt] = useState(0);
  /** The open session's address; null is the new-session page. */
  const [session, setSession] = useState<string | null>(null);
  const [actorStack, setActorStack] = useState<string[]>([]);
  const [route, setRoute] = useState<{ address: string; failed: boolean } | null>(null);
  const [models, setModels] = useState<ComposerModel[]>([]);
  const [ready, setReady] = useState(false);
  const [virtualDisplaysSupported, setVirtualDisplaysSupported] = useState(false);
  const [page, setPage] = useState<Page>(() => pageFromPath(location.pathname));
  const [settings, setSettings] = useState(() => settingsTabFromPath(location.pathname) !== null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>(() => settingsTabFromPath(location.pathname) ?? "general");
  const [mobile, setMobile] = useState(() => matchMedia("(max-width: 767px)").matches);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const settingsPage = useRef<SettingsPageHandle>(null);
  const settingsReturnPath = useRef("/");
  const [searching, setSearching] = useState(false);

  const [refreshVersion, setRefreshVersion] = useState(0);
  const [logoutError, setLogoutError] = useState(false);
  const authEpoch = useRef(0);
  const routeEpoch = useRef(0);
  const peek = actorStack.at(-1) || null;
  const sessionRef = useRef(session);
  sessionRef.current = session;
  useEffect(() => { const media = matchMedia("(max-width: 767px)"); const update = () => { setMobile(media.matches); setSidebarOpen(false); }; media.addEventListener("change", update); return () => media.removeEventListener("change", update); }, []);

  useEffect(() => { applyPreferences(preferences); savePreferences(preferences); void setWebUiLanguage(preferences.language); const media = matchMedia("(prefers-color-scheme: dark)"); const change = () => applyPreferences(preferences); media.addEventListener("change", change); const motion = matchMedia("(prefers-reduced-motion: reduce)"); motion.addEventListener("change", change); const languageChange = () => { void setWebUiLanguage(preferences.language); }; window.addEventListener("languagechange", languageChange); return () => { media.removeEventListener("change", change); motion.removeEventListener("change", change); window.removeEventListener("languagechange", languageChange); }; }, [preferences]);
  const revoked = useCallback(() => { authEpoch.current++; routeEpoch.current++; configureConversationDevice(null); clearConversationDrafts(); clearNewSessionDraft(); delegationStatuses.reset(); setAccess(null); setSession(null); setRoute(null); setActorStack([]); setSettings(false); setPage(null); setVirtualDisplaysSupported(false); setSearching(false); setSidebarOpen(false); }, []);
  const work = useSessionWork(access ? accessKey(access) : null, session, revoked);
  useEffect(() => {
    if (!access) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function capabilities() {
      try {
        const result = await virtualDisplayCapabilities({ signal: controller.signal });
        if (!controller.signal.aborted) setVirtualDisplaysSupported(result.supported);
      } catch (error) {
        if (controller.signal.aborted) return;
        if (error instanceof ActorApiError && [401, 403, 410].includes(error.status)) revoked();
        else timer = setTimeout(() => { void capabilities(); }, 2000);
      }
    }
    void capabilities();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [access, revoked]);
  const stage = useStageStatus(access ? accessKey(access) : null, revoked);
  // Configuration may arrive after login; refresh model choices when readiness changes.
  useEffect(() => {
    if (!access || !stage) return;
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
  }, [access, Boolean(stage), stage?.model_ready]);

  /** Open a conversation link: sessions open in the main pane, workers over their session. */
  const resolveRoute = useCallback(async (address: string) => {
    const epoch = ++routeEpoch.current;
    setRoute({ address, failed: false });
    try {
      const actor = await getActor(address);
      if (epoch !== routeEpoch.current) return;
      const owner = sessionOf(address, actor);
      setSession(owner); setActorStack(owner === address ? [] : [address]); setRoute(null);
    } catch (error) {
      if (epoch !== routeEpoch.current) return;
      if (error instanceof ActorApiError && [401, 403, 410].includes(error.status)) { revoked(); return; }
      setRoute({ address, failed: true });
    }
  }, [revoked]);
  const applyLocation = useCallback(() => {
    routeEpoch.current++;
    setPage(pageFromPath(location.pathname));
    const linked = actorIdFromUrl(location.href, location.href);
    if (linked && localActorId(linked) !== sessionRef.current) { void resolveRoute(localActorId(linked)); return; }
    setRoute(null); setActorStack([]);
    if (location.pathname === "/") setSession(null);
  }, [resolveRoute]);

  const configure = useCallback(async (signal?: AbortSignal) => {
    const epoch = ++authEpoch.current;
    const result = await restoreSession({signal}); if (epoch !== authEpoch.current) return;
    if (!result) { revoked(); return; }
    const info = await getStage({signal});
    if (epoch !== authEpoch.current) return;
    configureConversationDevice(info?.stage_id ?? null);
    const [configuration, readiness] = await Promise.allSettled([getConfig({signal}), getConfigReadiness({signal})]);
    if (epoch !== authEpoch.current) return;
    if (configuration.status === "fulfilled") setModels(configuration.value.models.map(model => ({ name: model.name, label: model.model_id || model.name, effort: model.thinking_budget || "", isDefault: configuration.value.main_model === model.name })));
    setReady(readiness.status === "fulfilled" && readiness.value.ready);
    sessionRef.current = null; setSession(null); applyLocation();
    setAccess(result);
  }, [revoked, applyLocation]);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    setChecking(true); setStartupError(null);
    void configure(controller.signal).catch(error => {
      if (controller.signal.aborted) return;
      if (error instanceof ActorApiError && error.status === 401) revoked();
      else {
        setStartupError(accountErrorCode(error));
        timer = setTimeout(() => setStartupAttempt(value => value + 1), 2000);
      }
    }).finally(() => { if (!controller.signal.aborted) setChecking(false); });
    return () => { controller.abort(); clearTimeout(timer); authEpoch.current++; };
  }, [configure, revoked, startupAttempt]);
  useEffect(() => {
    if (!access) return;
    const controller = new AbortController();
    let pending = false;
    async function refresh() {
      if (document.hidden || pending) return;
      pending = true;
      try { await renewSession({signal:controller.signal}); }
      catch (error) {
        if (!controller.signal.aborted && error instanceof ActorApiError && error.status === 401) revoked();
      } finally { pending = false; }
    }
    const timer = setInterval(() => { void refresh(); }, 6 * 60 * 60 * 1000);
    const resume = () => { void refresh(); };
    document.addEventListener("visibilitychange", resume);
    window.addEventListener("online", resume);
    return () => { controller.abort(); clearInterval(timer); document.removeEventListener("visibilitychange", resume); window.removeEventListener("online", resume); };
  }, [access, revoked]);
  useEffect(() => { function pop() { const tab = settingsTabFromPath(location.pathname); if (tab) {setSettingsTab(tab);setSettings(true);return;} if (settings) {history.pushState(null, "", settingsPath(settingsTab));settingsPage.current?.close();return;} applyLocation(); } window.addEventListener("popstate", pop); return () => window.removeEventListener("popstate", pop); }, [settings, settingsTab, applyLocation]);
  const conversationVisible = Boolean(access) && !settings && !page && !route && Boolean(session);
  useEffect(() => {
    function shortcut(event: KeyboardEvent) {
      const key = event.key.toLowerCase();
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey || event.isComposing || event.defaultPrevented || !["f", "k"].includes(key) || !conversationVisible || peek) return;
      event.preventDefault();
      if (event.repeat) return;
      if (key === "k") setSearching(value => !value);
      else {
        setSearching(true);
        const input = document.querySelector<HTMLInputElement>(".dl-primary-conversation .dl-floating-search input");
        input?.focus({preventScroll: true}); input?.select();
      }
    }
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [conversationVisible, peek]);
  const sessionPath = (address: string | null) => address ? conversationUrl(address) : "/";
  function leaveSettings(path: string) { settingsReturnPath.current = path; settingsPage.current?.close(); }
  function openActor(address: string) {
    if (mobile) setSidebarOpen(false);
    const id = localActorId(address);
    if (id === session) { if (peek) closeActor(); return; }
    setActorStack(old => old.at(-1) === id ? old : [...old, id]); history.pushState(null, "", conversationUrl(id));
  }
  function closeActor() { setActorStack([]); history.replaceState(null, "", page ? PAGE_PATHS[page] : sessionPath(session)); }
  function backActor() { const next = actorStack.slice(0, -1); setActorStack(next); history.replaceState(null, "", next.length ? conversationUrl(next.at(-1)!) : page ? PAGE_PATHS[page] : sessionPath(session)); }
  function openSettings(tab: SettingsTab) { if (!settings) settingsReturnPath.current = location.pathname; setSettingsTab(tab); setSettings(true); setSidebarOpen(false); history.pushState(null, "", settingsPath(tab)); }
  function closeSettings(open: boolean) { if (open) return; setSettings(false); history.replaceState(null, "", settingsReturnPath.current); applyLocation(); }
  /** Show a session in the main pane. */
  function openSession(address: string | null, replace = false) {
    const path = sessionPath(address);
    if (settings) { sessionRef.current = address; setSession(address); leaveSettings(path); return; }
    routeEpoch.current++; setRoute(null); setPage(null); setActorStack([]); setSearching(false); setSidebarOpen(false); setSession(address);
    if (replace) history.replaceState(null, "", path); else history.pushState(null, "", path);
  }
  function openPage(next: Exclude<Page, null>) {
    if (settings) { leaveSettings(PAGE_PATHS[next]); return; }
    setPage(next); setSearching(false); setActorStack([]); setSidebarOpen(false); history.pushState(null, "", PAGE_PATHS[next]);
  }
  function loadedDialogActor(address: string, actor: ActorDetail) {
    // A session link opened inside a dialog becomes the main conversation.
    if (actor.role === "session" && localActorId(address) !== session) openSession(localActorId(address));
  }
  function search() { if (mobile) setSidebarOpen(false); setSearching(true); }
  async function changed(actorId?: string) {
    const epoch = authEpoch.current;
    setRefreshVersion(old => old + 1);
    if (actorId) {
      try { await getActor(actorId); }
      catch { if (actorStack.includes(actorId)) closeActor(); }
    }
    if (access?.kind === "owner") {
      const [config, status] = await Promise.allSettled([getConfig(), getConfigReadiness()]);
      if (epoch !== authEpoch.current) return;
      if (config.status === "fulfilled") setModels(config.value.models.map(model => ({ name: model.name, label: model.model_id || model.name, effort: model.thinking_budget || "", isDefault: config.value.main_model === model.name })));
      if (status.status === "fulfilled") setReady(status.value.ready);
    }
  }
  async function logout() { setLogoutError(false); try { await logoutSession(); revoked(); history.replaceState(null, "", "/"); } catch { setLogoutError(true); } }
  const current: NavigationPage = settings ? "settings" : page ?? (session || route ? "session" : "new");
  const inactive = settings || Boolean(page);
  const navigation = { access: access!, browserSupported: browserSupported(), onBrowser: () => openPage("browser"), virtualDisplaysSupported, onVirtualDisplays: () => openPage("virtual-displays"), onNewSession: () => openSession(null), onSessions: () => openPage("sessions"), onApps: () => openPage("apps"), onSettings: () => openSettings("general"), onLogout: () => void logout() };
  const sessionMenu = work.session && access?.kind === "owner" && session && <SessionActions address={session} session={work.session} onChanged={work.update} onDeleted={() => openPage("sessions")}
    trigger={<Button variant="ghost" size="sm" className="dl-main-title"><span>{sessionTitle(work.session)}</span><ChevronDownIcon data-icon="inline-end" /></Button>} />;
  return <TooltipProvider delayDuration={250}><main className="dl-app">
    <DesktopTitlebar />
    <NativeResourcePreview />
    {access && logoutError && <div className="dl-action-error" role="alert"><span>{t("app.could_not_sign_out_try_again")}</span><Button variant="ghost" size="sm" onClick={() => { void logout(); }}>{t("app.retry")}</Button></div>}
    {checking ? <div className="dl-app-loading" role="status"><span className="dl-loading-orbit" />{t("app.connecting")}</div> : !access && startupError ? <div className="dl-app-loading" role="status"><AccountError code={startupError} /><Button variant="outline" onClick={() => setStartupAttempt(value => value + 1)}>{t("app.retry")}</Button></div> : !access ? <AccessGate onAuthenticated={() => setStartupAttempt(value => value + 1)} /> : <>
      <div className="dl-app-shell"><NavigationRail {...navigation} page={current} /><div className="dl-shell-body">
      {/* Keep the session pane laid out so other pages do not reset its reading position. */}
      <div className="dl-home-layout" data-inactive={inactive} aria-hidden={inactive || undefined} inert={inactive || undefined}>
      <div className="dl-main-view"><header className="dl-main-heading" data-window-drag-region>{mobile && <Button variant="ghost" size="icon-sm" className="dl-open-sidebar" aria-label={t("navigation.open_sidebar")} onClick={() => setSidebarOpen(true)}><PanelLeftIcon /></Button>}
        {route ? null : session ? sessionMenu || <span className="dl-main-title">{work.session ? sessionTitle(work.session) : ""}</span> : <span className="dl-main-title">{t("navigation.new_session")}</span>}
        {session && !route && !work.missing && <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon-sm" className="dl-main-search" aria-label={t("app.search_conversation")} aria-keyshortcuts="Control+f Meta+f Control+k Meta+k" onClick={search}><SearchIcon /></Button></TooltipTrigger><TooltipContent>{t("app.search_conversation_shortcut")}</TooltipContent></Tooltip>}</header>
      {route ? <div className="dl-app-loading" role="status">{route.failed ? <><span>{t("sessions.open_failed")}</span><Button variant="outline" onClick={() => { void resolveRoute(route.address); }}>{t("app.retry")}</Button></> : <span className="dl-loading-orbit" />}</div>
        : session && work.missing ? <Empty className="dl-session-missing"><EmptyHeader><EmptyMedia variant="icon"><MessagesSquareIcon /></EmptyMedia><EmptyTitle>{t("sessions.missing")}</EmptyTitle></EmptyHeader><Button variant="outline" onClick={() => openPage("sessions")}>{t("sessions.title")}</Button></Empty>
        : session ? <ConversationPane key={session} actorId={session} access={access} onActor={openActor} onAccessRevoked={revoked} models={models} ready={ready} refreshVersion={refreshVersion} searchOpen={searching} onSearchOpenChange={setSearching} onConfigureModels={() => openSettings("models")} activeActors={work.actors} onActivity={work.refresh} />
        : <NewSession access={access} models={models} ready={ready} onCreated={id => openSession(id, true)} onConfigureModels={() => openSettings("models")} />}
      </div></div>
      {page === "sessions" && !settings && <SessionsPage active={!peek} onNavigation={() => setSidebarOpen(true)} onNewSession={() => openSession(null)} onSession={address => openSession(localActorId(address))} onAccessRevoked={revoked} />}
      {page === "apps" && !settings && <AppsPage onNavigation={() => setSidebarOpen(true)} onAccessRevoked={revoked} />}
      {page === "virtual-displays" && !settings && <VirtualDisplaysPage onNavigation={() => setSidebarOpen(true)} onAccessRevoked={revoked} />}
      {page === "browser" && !settings && <BrowserPage onNavigation={() => setSidebarOpen(true)} />}
      {mobile && <NavigationDrawer {...navigation} open={sidebarOpen && !settings} onOpenChange={setSidebarOpen} />}
      <SettingsPage onViewSession={id => openSession(localActorId(id))} ref={settingsPage} open={settings} onOpenChange={closeSettings} initialTab={settingsTab} onTabChange={tab => {setSettingsTab(tab);history.replaceState(null, "", settingsPath(tab));}} preferences={preferences} onPreferencesChanged={setPreferences} onActorChanged={id => { void changed(id); }} canManage={access.kind === "owner"} />
      </div></div>
      <ActorDialog key={peek} actorId={peek} open={Boolean(peek) && !settings && page !== "apps" && page !== "virtual-displays" && page !== "browser"} onClose={closeActor} onActor={openActor} onActorLoaded={actor => { if (peek) loadedDialogActor(peek, actor); }} onBack={actorStack.length > 1 ? backActor : undefined} access={access} models={models} ready={ready} onAccessRevoked={revoked} refreshVersion={refreshVersion} />
    </>}
  </main></TooltipProvider>;
}
