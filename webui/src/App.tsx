import { useEffect, useLayoutEffect, useMemo, useState } from "react";

import { AppSidebar } from "@/components/app-sidebar";
import { LoginPage } from "@/components/login-page";
import { LogsPage } from "@/components/logs-page";
import { SetupPage } from "@/components/setup-page";
import { SettingsPage } from "@/components/settings-page";
import { AgentPage, StatusPage } from "@/components/status-page";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { Spinner } from "@/components/ui/spinner";
import { getStoredDaemonToken, storeDaemonToken } from "@/lib/daemon-auth";
import {
  createSession,
  deleteSession,
  fetchConfigReadiness,
  fetchSessions,
  type ConfigReadinessReport,
  type DashboardContextCompositionSnapshot,
  type DashboardSnapshot,
  type SessionInfo,
  type SetupConfigRequest,
  type SetupConfigResponse,
  type StatusSummary,
} from "@/lib/daemon-api";

type AppPage = "agent" | "status" | "settings" | "logs";
type ThemeMode = "light" | "dark";

const SELECTED_SESSION_STORAGE_KEY = "daat-locus.webui.selected-session-id";
const THEME_STORAGE_KEY = "daat-locus.webui.theme";

const APP_DOCUMENT_TITLE = "Daat Locus";

consumeDaemonTokenFromHash();

function consumeDaemonTokenFromHash() {
  if (typeof window === "undefined") {
    return;
  }

  const hash = window.location.hash;
  const hashValue = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!hashValue.includes("=")) {
    return;
  }

  const params = new URLSearchParams(
    hashValue.startsWith("?") ? hashValue.slice(1) : hashValue,
  );
  const token = params.get("daemon_token")?.trim();
  if (!token) {
    return;
  }

  storeDaemonToken(token);
  params.delete("daemon_token");

  const nextHash = params.toString();
  window.history.replaceState(
    window.history.state,
    "",
    `${window.location.pathname}${window.location.search}${nextHash ? `#${nextHash}` : ""}`,
  );
}

export default function App() {
  if (shouldRenderMockSetupPage()) {
    return <MockSetupApp />;
  }

  if (shouldRenderMockAgentPage()) {
    return <MockAgentApp />;
  }

  if (shouldRenderMockStatusPage()) {
    return <MockStatusApp />;
  }

  if (shouldRenderMockSettingsPage()) {
    return <MockSettingsApp />;
  }

  const [isAuthenticated, setIsAuthenticated] = useState(() =>
    Boolean(getStoredDaemonToken()),
  );
  const [activePage, setActivePage] = useState(getCurrentPage);
  const [configReadiness, setConfigReadiness] =
    useState<ConfigReadinessReport | null>(null);
  const [configReadinessError, setConfigReadinessError] = useState<string | null>(
    null,
  );
  const [hasLoadedConfigReadiness, setHasLoadedConfigReadiness] =
    useState(false);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(
    readStoredSelectedSessionId,
  );
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [hasLoadedSessions, setHasLoadedSessions] = useState(false);
  const [isCreatingSession, setIsCreatingSession] = useState(false);
  const [deletingSessionId, setDeletingSessionId] = useState<string | null>(
    null,
  );

  const { themeMode, toggleThemeMode } = useThemeMode();

  const selectedSession = useMemo(
    () =>
      selectedSessionId
        ? (sessions.find(
            (session) => session.session_id === selectedSessionId,
          ) ?? null)
        : null,
    [selectedSessionId, sessions],
  );

  useEffect(() => {
    const controller = new AbortController();
    void refreshConfigReadiness(controller.signal);
    return () => controller.abort();
  }, []);

  useEffect(() => {
    function updateActivePage() {
      setActivePage(getCurrentPage());
    }

    updateActivePage();
    window.addEventListener("hashchange", updateActivePage);

    return () => window.removeEventListener("hashchange", updateActivePage);
  }, []);

  useEffect(() => {
    document.title = pageDocumentTitle(
      activePage,
      selectedSession,
      isAuthenticated,
    );
  }, [activePage, isAuthenticated, selectedSession]);

  useEffect(() => {
    if (!isAuthenticated || configReadiness?.kind !== "complete") {
      return;
    }

    const controller = new AbortController();
    void refreshSessions(controller.signal);
    const interval = window.setInterval(() => {
      void refreshSessions(controller.signal);
    }, 5000);

    return () => {
      controller.abort();
      window.clearInterval(interval);
    };
  }, [configReadiness?.kind, isAuthenticated]);

  useEffect(() => {
    if (selectedSessionId) {
      window.localStorage.setItem(
        SELECTED_SESSION_STORAGE_KEY,
        selectedSessionId,
      );
    } else {
      window.localStorage.removeItem(SELECTED_SESSION_STORAGE_KEY);
    }
  }, [selectedSessionId]);

  async function refreshSessions(signal?: AbortSignal) {
    try {
      const nextSessions = await fetchSessions({ signal });
      setSessions(nextSessions);
      setSessionError(null);
      setHasLoadedSessions(true);
      setSelectedSessionId((current) => {
        if (current && nextSessions.some((s) => s.session_id === current)) {
          return current;
        }
        return preferredSession(nextSessions)?.session_id ?? null;
      });
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setSessionError(error instanceof Error ? error.message : String(error));
      setHasLoadedSessions(true);
    }
  }

  async function refreshConfigReadiness(signal?: AbortSignal) {
    try {
      const readiness = await fetchConfigReadiness({ signal });
      setConfigReadiness(readiness);
      setConfigReadinessError(null);
      setHasLoadedConfigReadiness(true);
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setConfigReadinessError(
        error instanceof Error ? error.message : String(error),
      );
      setHasLoadedConfigReadiness(true);
    }
  }

  async function handleCreateSession(projectDir?: string) {
    if (isCreatingSession) {
      return;
    }
    setIsCreatingSession(true);
    setSessionError(null);
    try {
      const session = await createSession({
        projectDir,
        title: projectDir ? projectLabel(projectDir) : undefined,
      });
      setSessions((current) => [...current, session]);
      setSelectedSessionId(session.session_id);
      if (activePage !== "agent") {
        window.location.hash = "#agent";
      }
    } catch (error) {
      setSessionError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsCreatingSession(false);
    }
  }

  async function handleDeleteSession(sessionId: string) {
    if (deletingSessionId) {
      return;
    }
    setDeletingSessionId(sessionId);
    setSessionError(null);
    try {
      await deleteSession({ sessionId });
      setSessions((current) => {
        const nextSessions = current.filter(
          (session) => session.session_id !== sessionId,
        );
        setSelectedSessionId((currentSelected) => {
          if (currentSelected !== sessionId) {
            return currentSelected;
          }
          return preferredSession(nextSessions)?.session_id ?? null;
        });
        return nextSessions;
      });
    } catch (error) {
      setSessionError(error instanceof Error ? error.message : String(error));
      throw error;
    } finally {
      setDeletingSessionId(null);
    }
  }

  function handleSelectSession(sessionId: string) {
    setSelectedSessionId(sessionId);
    if (activePage !== "agent") {
      window.location.hash = "#agent";
    }
  }

  function handleSetupReadinessChanged(nextReadiness: ConfigReadinessReport) {
    setConfigReadiness(nextReadiness);
    if (nextReadiness.kind === "complete") {
      leaveSetupRoute();
    }
  }

  function leaveSetupRoute() {
    if (typeof window === "undefined") {
      return;
    }

    const url = new URL(window.location.href);
    const wasForcedByQuery = url.searchParams.get("setup") === "1";
    const wasForcedByHash = url.hash === "#setup";

    if (wasForcedByQuery) {
      url.searchParams.delete("setup");
    }
    if (wasForcedByHash) {
      url.hash = "";
    }

    if (wasForcedByQuery || wasForcedByHash) {
      window.history.replaceState(
        null,
        "",
        `${url.pathname}${url.search}${url.hash}`,
      );
      setActivePage(getCurrentPage());
    }
  }

  const shouldShowSetup =
    shouldForceSetupPage() ||
    (configReadiness !== null && configReadiness.kind !== "complete");

  return (
    <main className="min-h-screen bg-background text-foreground">
      {!hasLoadedConfigReadiness ? (
        <SetupLoadingPage />
      ) : configReadinessError && !configReadiness ? (
        <SetupErrorPage
          message={configReadinessError}
          onRefresh={() => void refreshConfigReadiness()}
        />
      ) : shouldShowSetup ? (
        isAuthenticated && configReadiness ? (
          <SetupPage
            readiness={configReadiness}
            onReadinessChanged={handleSetupReadinessChanged}
          />
        ) : (
          <LoginPage onAuthenticated={() => setIsAuthenticated(true)} />
        )
      ) : isAuthenticated ? (
        <SidebarProvider>
          <AppSidebar
            activePage={activePage}
            sessions={sessions}
            selectedSessionId={selectedSessionId}
            sessionError={sessionError}
            isCreatingSession={isCreatingSession}
            deletingSessionId={deletingSessionId}
            themeMode={themeMode}
            onToggleThemeMode={toggleThemeMode}
            onSelectSession={handleSelectSession}
            onCreateSession={handleCreateSession}
            onDeleteSession={handleDeleteSession}
          />
          <SidebarInset className="min-h-screen">
            {renderAuthenticatedPage(activePage, selectedSessionId, {
              hasLoadedSessions,
              sessionError,
            })}
          </SidebarInset>
        </SidebarProvider>
      ) : (
        <LoginPage onAuthenticated={() => setIsAuthenticated(true)} />
      )}
    </main>
  );
}

function MockAgentApp() {
  const { themeMode, toggleThemeMode } = useThemeMode();
  useEffect(() => {
    document.title = pageDocumentTitle("agent", MOCK_SESSION, true);
  }, []);
  return (
    <main className="min-h-screen bg-background text-foreground">
      <SidebarProvider>
        <AppSidebar
          activePage="agent"
          sessions={MOCK_SIDEBAR_SESSIONS}
          selectedSessionId={MOCK_SESSION.session_id}
          sessionError={null}
          isCreatingSession={false}
          deletingSessionId={null}
          themeMode={themeMode}
          onToggleThemeMode={toggleThemeMode}
          onSelectSession={() => undefined}
          onCreateSession={() => undefined}
          onDeleteSession={async () => undefined}
        />
        <SidebarInset className="min-h-screen">
          <AgentPage
            sessionId={MOCK_SESSION.session_id}
            mockSnapshot={MOCK_DASHBOARD_SNAPSHOT}
          />
        </SidebarInset>
      </SidebarProvider>
    </main>
  );
}

function MockStatusApp() {
  const { themeMode, toggleThemeMode } = useThemeMode();
  useEffect(() => {
    document.title = pageDocumentTitle("status", null, true);
  }, []);

  return (
    <main className="min-h-screen bg-background text-foreground">
      <SidebarProvider>
        <AppSidebar
          activePage="status"
          sessions={MOCK_SIDEBAR_SESSIONS}
          selectedSessionId={MOCK_SESSION.session_id}
          sessionError={null}
          isCreatingSession={false}
          deletingSessionId={null}
          themeMode={themeMode}
          onToggleThemeMode={toggleThemeMode}
          onSelectSession={() => undefined}
          onCreateSession={() => undefined}
          onDeleteSession={async () => undefined}
        />
        <SidebarInset className="min-h-screen">
          <StatusPage mockSummary={MOCK_STATUS_SUMMARY} />
        </SidebarInset>
      </SidebarProvider>
    </main>
  );
}

function MockSettingsApp() {
  const { themeMode, toggleThemeMode } = useThemeMode();
  useEffect(() => {
    document.title = pageDocumentTitle("settings", null, true);
  }, []);

  async function handleMockSave(request: SetupConfigRequest) {
    return {
      ...MOCK_SETTINGS_SETUP_CONFIG.readiness,
      kind: "complete" as const,
      port: request.daemon_port ?? MOCK_SETTINGS_SETUP_CONFIG.readiness.port,
      message: "mock settings configuration is complete",
      recovery_note: null,
    };
  }

  return (
    <main className="min-h-screen bg-background text-foreground">
      <SidebarProvider>
        <AppSidebar
          activePage="settings"
          sessions={MOCK_SIDEBAR_SESSIONS}
          selectedSessionId={MOCK_SESSION.session_id}
          sessionError={null}
          isCreatingSession={false}
          deletingSessionId={null}
          themeMode={themeMode}
          onToggleThemeMode={toggleThemeMode}
          onSelectSession={() => undefined}
          onCreateSession={() => undefined}
          onDeleteSession={async () => undefined}
        />
        <SidebarInset className="min-h-screen">
          <SettingsPage
            mockSetupConfig={MOCK_SETTINGS_SETUP_CONFIG}
            onSaveSetupConfig={handleMockSave}
          />
        </SidebarInset>
      </SidebarProvider>
    </main>
  );
}

function MockSetupApp() {
  const readiness = MOCK_SETUP_READINESS;

  useEffect(() => {
    document.title = "Daat Locus Setup Mock";
  }, []);

  async function handleMockSave(request: SetupConfigRequest) {
    return {
      ...readiness,
      kind: "complete" as const,
      port: request.daemon_port ?? readiness.port,
      message: "mock setup configuration is complete",
      recovery_note: null,
    };
  }

  return (
    <SetupPage
      readiness={readiness}
      onReadinessChanged={() => undefined}
      onSaveSetupConfig={handleMockSave}
    />
  );
}

function renderAuthenticatedPage(
  activePage: AppPage,
  selectedSessionId: string | null,
  sessionState: {
    hasLoadedSessions: boolean;
    sessionError: string | null;
  },
) {
  switch (activePage) {
    case "status":
      return <StatusPage />;
    case "settings":
      return <SettingsPage />;
    case "logs":
      return <LogsPage />;
    case "agent":
    default:
      return selectedSessionId ? (
        <AgentPage key={selectedSessionId} sessionId={selectedSessionId} />
      ) : (
        <NoSessionPage {...sessionState} />
      );
  }
}

function NoSessionPage({
  hasLoadedSessions,
  sessionError,
}: {
  hasLoadedSessions: boolean;
  sessionError: string | null;
}) {
  return (
    <section
      aria-label="Session required"
      className="flex min-h-screen w-full items-center justify-center px-6 py-16"
    >
      <Empty className="w-full max-w-lg border border-dashed bg-card/60">
        <EmptyHeader>
          <EmptyTitle>
            {hasLoadedSessions ? "No session selected" : "Loading sessions"}
          </EmptyTitle>
          <EmptyDescription>
            {sessionError
              ? "Session list could not be loaded."
              : hasLoadedSessions
                ? "Create or select a session from the sidebar."
                : "Fetching available sessions."}
          </EmptyDescription>
        </EmptyHeader>
        {sessionError ? (
          <EmptyContent>
            <Alert variant="destructive" className="w-full">
              <AlertDescription>{sessionError}</AlertDescription>
            </Alert>
          </EmptyContent>
        ) : null}
      </Empty>
    </section>
  );
}

function SetupLoadingPage() {
  return (
    <section
      aria-label="Loading configuration readiness"
      className="flex min-h-screen w-full items-center justify-center px-6 py-16"
    >
      <Empty className="w-full max-w-lg border border-dashed bg-card/60">
        <EmptyHeader>
          <EmptyTitle>Checking configuration</EmptyTitle>
          <EmptyDescription>
            Loading Manager readiness before opening the agent workspace.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <div className="flex items-center justify-center">
            <Spinner />
          </div>
        </EmptyContent>
      </Empty>
    </section>
  );
}

function SetupErrorPage({
  message,
  onRefresh,
}: {
  message: string;
  onRefresh: () => void;
}) {
  return (
    <section
      aria-label="Configuration readiness error"
      className="flex min-h-screen w-full items-center justify-center px-6 py-16"
    >
      <Empty className="w-full max-w-lg border border-dashed bg-card/60">
        <EmptyHeader>
          <EmptyTitle>Unable to read configuration state</EmptyTitle>
          <EmptyDescription>
            The WebUI could not determine whether the agent can run.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <div className="flex w-full flex-col gap-4">
            <Alert variant="destructive" className="w-full">
              <AlertDescription>{message}</AlertDescription>
            </Alert>
            <Button
              type="button"
              variant="outline"
              onClick={onRefresh}
            >
              Retry
            </Button>
          </div>
        </EmptyContent>
      </Empty>
    </section>
  );
}

function preferredSession(sessions: SessionInfo[]) {
  return (
    sessions.find((session) => session.scope.kind === "general") ??
    sessions[0] ??
    null
  );
}

function readStoredSelectedSessionId() {
  if (typeof window === "undefined") {
    return null;
  }
  return (
    window.localStorage.getItem(SELECTED_SESSION_STORAGE_KEY)?.trim() || null
  );
}

function useThemeMode() {
  const [themeMode, setThemeMode] = useState<ThemeMode>(readStoredThemeMode);

  useLayoutEffect(() => {
    applyThemeMode(themeMode);
  }, [themeMode]);

  function toggleThemeMode() {
    setThemeMode((current) => (current === "dark" ? "light" : "dark"));
  }

  return { themeMode, toggleThemeMode };
}

function readStoredThemeMode(): ThemeMode {
  if (typeof window === "undefined") {
    return "light";
  }

  try {
    const storedTheme = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (storedTheme === "dark" || storedTheme === "light") {
      return storedTheme;
    }
  } catch {
    // Ignore localStorage failures, e.g. private mode or disabled storage.
  }

  if (
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  ) {
    return "dark";
  }

  return "light";
}

function applyThemeMode(themeMode: ThemeMode) {
  if (typeof document === "undefined") {
    return;
  }

  document.documentElement.classList.toggle("dark", themeMode === "dark");
  document.documentElement.style.colorScheme = themeMode;

  if (typeof window === "undefined") {
    return;
  }

  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, themeMode);
  } catch {
    // Ignore localStorage failures, e.g. private mode or disabled storage.
  }
}

function projectLabel(projectDir: string) {
  const parts = projectDir.split(/[\\/]+/).filter(Boolean);
  return parts.at(-1) ?? projectDir;
}

function sessionDocumentTitle(session: SessionInfo) {
  return (
    session.title?.trim() ||
    (session.project_dir ? projectLabel(session.project_dir) : null) ||
    "Untitled session"
  );
}

function pageLabel(page: AppPage) {
  switch (page) {
    case "status":
      return "Status";
    case "settings":
      return "Settings";
    case "logs":
      return "Logs";
    case "agent":
    default:
      return "Agent";
  }
}

function pageDocumentTitle(
  activePage: AppPage,
  selectedSession: SessionInfo | null,
  isAuthenticated: boolean,
) {
  if (!isAuthenticated) {
    return `Sign in · ${APP_DOCUMENT_TITLE}`;
  }

  const selectedSessionTitle = selectedSession
    ? sessionDocumentTitle(selectedSession)
    : null;

  if (activePage === "agent") {
    return selectedSessionTitle
      ? `${selectedSessionTitle} · ${APP_DOCUMENT_TITLE}`
      : `Agent · ${APP_DOCUMENT_TITLE}`;
  }

  const activePageLabel = pageLabel(activePage);
  return selectedSessionTitle
    ? `${activePageLabel} · ${selectedSessionTitle} · ${APP_DOCUMENT_TITLE}`
    : `${activePageLabel} · ${APP_DOCUMENT_TITLE}`;
}

function getCurrentPage(): AppPage {
  if (typeof window === "undefined") {
    return "agent";
  }

  if (window.location.hash === "#status") {
    return "status";
  }
  if (window.location.hash === "#logs") {
    return "logs";
  }
  if (window.location.hash === "#settings") {
    return "settings";
  }
  return "agent";
}

function shouldRenderMockAgentPage() {
  if (!import.meta.env.DEV || typeof window === "undefined") {
    return false;
  }

  return new URLSearchParams(window.location.search).get("mock") === "agent";
}

function shouldRenderMockStatusPage() {
  if (!import.meta.env.DEV || typeof window === "undefined") {
    return false;
  }

  return new URLSearchParams(window.location.search).get("mock") === "status";
}

function shouldRenderMockSettingsPage() {
  if (!import.meta.env.DEV || typeof window === "undefined") {
    return false;
  }

  return new URLSearchParams(window.location.search).get("mock") === "settings";
}

function shouldRenderMockSetupPage() {
  if (!import.meta.env.DEV || typeof window === "undefined") {
    return false;
  }

  return new URLSearchParams(window.location.search).get("mock") === "setup";
}

function shouldForceSetupPage() {
  if (typeof window === "undefined") {
    return false;
  }
  const params = new URLSearchParams(window.location.search);
  return params.get("setup") === "1" || window.location.hash === "#setup";
}

const MOCK_NOW_MS = Date.now();
const MOCK_PROJECT_DIR = "C:\\Users\\13940\\DaatLocus";

const MOCK_SETUP_READINESS: ConfigReadinessReport = {
  kind: "unconfigured",
  config_path: "C:\\Users\\13940\\.daat-locus\\config\\config.toml",
  backup_path: "C:\\Users\\13940\\.daat-locus\\config\\config.toml.bak",
  port: 53825,
  message: "configuration has no provider/model setup",
  recovery_note: null,
};

const MOCK_SESSION: SessionInfo = {
  session_id: "mock-agent-session",
  scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
  project_dir: MOCK_PROJECT_DIR,
  title: "Find legacy arg helper remnants",
  started_at_ms: MOCK_NOW_MS - 31 * 24 * 60 * 60 * 1000,
  last_seen_at_ms: MOCK_NOW_MS - 31 * 24 * 60 * 60 * 1000,
};

const MOCK_SIDEBAR_SESSIONS: SessionInfo[] = [
  MOCK_SESSION,
  {
    session_id: "mock-agent-session-fluid-switch",
    scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
    project_dir: MOCK_PROJECT_DIR,
    title: "Add fluid_switch behavior tests",
    started_at_ms: MOCK_NOW_MS - 33 * 24 * 60 * 60 * 1000,
    last_seen_at_ms: MOCK_NOW_MS - 33 * 24 * 60 * 60 * 1000,
  },
  {
    session_id: "mock-agent-session-render-slot",
    scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
    project_dir: MOCK_PROJECT_DIR,
    title: "Make render slot behavior stable",
    started_at_ms: MOCK_NOW_MS - 36 * 24 * 60 * 60 * 1000,
    last_seen_at_ms: MOCK_NOW_MS - 36 * 24 * 60 * 60 * 1000,
  },
  {
    session_id: "mock-agent-session-basic-components",
    scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
    project_dir: MOCK_PROJECT_DIR,
    title: "Debug hidden basic components",
    started_at_ms: MOCK_NOW_MS - 38 * 24 * 60 * 60 * 1000,
    last_seen_at_ms: MOCK_NOW_MS - 38 * 24 * 60 * 60 * 1000,
  },
  {
    session_id: "mock-agent-session-tessera-generic",
    scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
    project_dir: MOCK_PROJECT_DIR,
    title: "Explain tessera generic function support",
    started_at_ms: MOCK_NOW_MS - 63 * 24 * 60 * 60 * 1000,
    last_seen_at_ms: MOCK_NOW_MS - 63 * 24 * 60 * 60 * 1000,
  },
];

const MOCK_ACTIVITY_STARTED_AT = 1_786_800_000_000;

const MOCK_DASHBOARD_SNAPSHOT: DashboardSnapshot = {
  agent_name: "Daat Locus",
  session_title: {
    title: "WebUI alignment pass",
    generated: true,
    updated_at_ms: MOCK_ACTIVITY_STARTED_AT,
  },
  status_output: "",
  status_command: {
    runtime_turn: "running (tool execution)",
    bound_primitive:
      "inspect-repository-status-modify-local-project-run-required-checks-report-result",
    active_plans: 3,
    events: "1 active (claimed=1)",
    plan_steps: [
      { status: "completed", step: "Read current WebUI and TUI data flow" },
      { status: "in_progress", step: "Reshape Agent page into activity workbench" },
      { status: "pending", step: "Verify responsive desktop and mobile layouts" },
    ],
  },
  sleep_status_output: "",
  inspect_telegram_output: "",
  system_prompt_output: "",
  preturn_context_output: "",
  app_status_outputs: [["coding", "project_root=C:/Users/13940/DaatLocus"]],
  skills: [
    {
      name: "shadcn",
      description: "Manage shadcn components and project UI conventions.",
      path: "C:/Users/13940/.agents/skills/shadcn/SKILL.md",
      scope: "user",
      allow_implicit_invocation: true,
      user_disabled: false,
      auto_use_enabled: true,
    },
    {
      name: "rust-crate-lints",
      description: "Review a Rust crate after formatter, clippy, and tests pass.",
      path: "C:/Users/13940/.agents/skills/rust-crate-lints/SKILL.md",
      scope: "user",
      allow_implicit_invocation: true,
      user_disabled: true,
      auto_use_enabled: false,
    },
  ],
  skill_errors: [],
  pending_access_requests: [
    {
      chat_id: 139400001,
      title: "Design review",
      sender: "Ada",
      last_message_preview: "Please approve access for WebUI inspection.",
      first_seen_at_ms: MOCK_ACTIVITY_STARTED_AT - 120_000,
      last_seen_at_ms: MOCK_ACTIVITY_STARTED_AT - 30_000,
    },
  ],
  activity_events: [
    {
      User: {
        title: "Align the WebUI with the new TUI direction.",
        body_lines: [],
        full_body:
          "Align the WebUI with the new TUI direction.\nUse shadcn components where the web has a native control.",
      },
    },
    {
      PlanResult: {
        steps: [
          { status: "Completed", text: "Read current WebUI and TUI data flow" },
          { status: "InProgress", text: "Reshape Agent page into activity workbench" },
          { status: "Pending", text: "Verify responsive desktop and mobile layouts" },
        ],
      },
    },
    {
      Explored: {
        stable_id: "mock-explored",
        title: "Explored",
        calls: [
          {
            tool_name: "Read",
            action: "read",
            target: "webui/src/components/status-page.tsx",
            secondary_target: null,
            summary: "webui/src/components/status-page.tsx",
            detail_lines: [],
            detail_title: null,
          },
          {
            tool_name: "Read",
            action: "read",
            target: "src/dashboard/cells/tui.rs",
            secondary_target: null,
            summary: "src/dashboard/cells/tui.rs",
            detail_lines: [],
            detail_title: null,
          },
          {
            tool_name: "Search",
            action: "search",
            target: "render_explored_cell_lines",
            secondary_target: "src/dashboard/cells/tui.rs",
            summary:
              "render_explored_cell_lines — 1 target in src/dashboard/cells/tui.rs",
            detail_lines: [],
            detail_title: null,
          },
        ],
      },
    },
    {
      ExecResult: {
        title: "bun run typecheck",
        meta: "exit=0",
        output_lines: ["$ tsc -p tsconfig.json --noEmit", "Finished in 1.2s"],
      },
    },
    {
      Patch: {
        summary_line: "updated Agent page layout",
        files: [
          {
            path: "webui/src/components/status-page.tsx",
            operation: "update",
            added_lines: 42,
            removed_lines: 36,
            diff_lines: [
              {
                kind: "context",
                old_lineno: 120,
                new_lineno: 120,
                text: "return (",
              },
              {
                kind: "delete",
                old_lineno: 121,
                new_lineno: null,
                text: "<AgentExpression />",
              },
              {
                kind: "add",
                old_lineno: null,
                new_lineno: 121,
                text: "<AgentWorkspaceHeader />",
              },
            ],
          },
        ],
      },
    },
    { FinalMessageSeparator: { elapsed_seconds: 154 } },
    {
      Reply: {
        disposition: "resolved",
        subject: "message",
        message_lines: [
          "Agent reply should use the activity marker.",
          "It must not be rendered as a user prompt.",
        ],
      },
    },
    {
      Assistant: {
        title: "The Agent page now behaves like a web workbench.",
        body_lines: [
          "Activity is the primary surface.",
          "Composer status mirrors the TUI command bar without copying terminal text.",
        ],
        full_body:
          "The Agent page now behaves like a web workbench.\n\n- Activity is the primary surface.\n- Composer status mirrors the TUI command bar without copying terminal text.",
      },
    },
  ],
  live_activity_events: [
    {
      key: "mock-live-exec",
      event: {
        LiveExec: {
          title: "cargo check",
          call_lines: ["cargo check"],
          meta: null,
          output_lines: ["Checking daat-locus v0.1.1"],
          started_at_ms: MOCK_ACTIVITY_STARTED_AT + 12_000,
        },
      },
    },
  ],
  last_cycle_elapsed_ms: 2300,
  runtime_status: "Working",
  runtime_status_level: "info",
  runtime_activity: {
    status: "running",
    label: "Working",
    detail: "cargo check",
    active_runtime_turn: true,
    active_runtime_phase: "verification",
  },
  current_plan_step: {
    status: "in_progress",
    step: "Verify responsive desktop and mobile layouts",
  },
  primitive_optimization: {
    running: false,
    current_trigger: null,
    last_result: "frontier unchanged",
    last_completed_at_ms: MOCK_ACTIVITY_STARTED_AT - 600_000,
    primitive_evidence_records: 3,
    total_primitive_evidence_run_records: 8,
    total_primitive_reflections: 2,
    total_primitive_patch_candidates: 1,
    total_primitive_merge_candidates: 0,
    total_primitive_candidate_evaluations: 4,
    total_primitive_frontier_entries: 5,
    latest_primitive_frontier_root_entries: 2,
    latest_primitive_frontier_branched_entries: 3,
    latest_primitive_frontier_max_generation: 2,
    total_primitive_patch_applied: 1,
    total_primitive_merge_applied: 0,
    total_primitive_update_rollbacks: 0,
    total_primitive_optimization_rounds: 2,
  },
  runtime_optimization: {
    running: false,
    current_trigger: null,
    last_result: "no runtime error cases",
    last_completed_at_ms: MOCK_ACTIVITY_STARTED_AT - 480_000,
    unread_runtime_error_backlog: 0,
    total_runtime_error_cases_consumed: 0,
    total_runtime_error_cases: 0,
    total_runtime_error_reflections: 0,
    total_runtime_contract_candidates: 0,
    total_runtime_contract_candidate_evaluations: 0,
    total_runtime_contract_system_additions: 0,
    total_runtime_contract_updates: 0,
  },
  footer_context: "gpt-5.5 · Coding · 42k/128k used",
  footer_estimated_input_tokens: 42_000,
};

const MOCK_STATUS_SUMMARY: StatusSummary = {
  loaded_at_ms: MOCK_NOW_MS,
  daemon: {
    pid: 53825,
    started_at_ms: MOCK_NOW_MS - 6 * 60 * 60 * 1000,
    version: "mock-webui",
    bind_host: "127.0.0.1",
    port: 53825,
    state: "ready",
    connected_clients: 2,
  },
  pending_access_requests: [],
  sessions: [
    mockStatusSessionSummary({
      session: {
        ...MOCK_SESSION,
        title: "Mock token usage history",
        last_seen_at_ms: MOCK_NOW_MS,
      },
      dashboardTitle: "Status page mock session",
      runtimeStatus: "Ready",
      runtimeDetail: "Mock status data",
      mainModel: "gpt-5.5",
      judgeModel: "gpt-5.5-mini",
      mainTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(6, 11_600_000, 8_900_000, 84_000, 35_000),
        mockDailyTokenUsage(5, 17_300_000, 13_100_000, 96_000, 42_000),
        mockDailyTokenUsage(4, 13_800_000, 9_200_000, 78_000, 28_000),
        mockDailyTokenUsage(3, 24_500_000, 18_600_000, 132_000, 64_000),
        mockDailyTokenUsage(2, 18_200_000, 13_900_000, 101_000, 49_000),
        mockDailyTokenUsage(1, 32_400_000, 25_700_000, 165_000, 83_000),
        mockDailyTokenUsage(0, 41_600_000, 38_900_000, 204_800, 129_600),
      ]),
      judgeTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(6, 1_200_000, 430_000, 21_000, 0),
        mockDailyTokenUsage(5, 1_680_000, 510_000, 24_000, 0),
        mockDailyTokenUsage(4, 940_000, 280_000, 18_000, 0),
        mockDailyTokenUsage(3, 2_180_000, 790_000, 31_000, 0),
        mockDailyTokenUsage(2, 1_540_000, 620_000, 22_000, 0),
        mockDailyTokenUsage(1, 2_760_000, 1_080_000, 35_000, 0),
        mockDailyTokenUsage(0, 2_920_000, 1_420_000, 38_000, 0),
      ]),
      contextComposition: mockContextCompositionSnapshot({
        model: "gpt-5.5",
        messageCount: 42,
        toolCount: 18,
        stablePrefixTokens: 26_000,
        changedPrefixTokens: 8_000,
        newSuffixTokens: 18_000,
        segments: [
          mockContextSegment("system_messages", "System messages", "system", 8_400, "prefix"),
          mockContextSegment("afterclaim_context", "Afterclaim context", "user", 4_800, "history"),
          mockContextSegment("preturn_context", "Preturn context", "user", 9_600, "history"),
          mockContextSegment("conversation_history", "Conversation history", "user", 12_300, "history"),
          mockContextSegment("assistant_messages", "Assistant messages", "assistant", 7_400, "history"),
          mockContextSegment("tool_messages", "Tool outputs", "tool", 5_700, "history"),
          mockContextSegment("tools_schema", "Tools schema", "request_tools", 13_100, "tools"),
        ],
      }),
    }),
    mockStatusSessionSummary({
      session: {
        session_id: "mock-status-session-compact",
        scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
        project_dir: MOCK_PROJECT_DIR,
        title: "Mock compact coding pass",
        started_at_ms: MOCK_NOW_MS - 6 * 24 * 60 * 60 * 1000,
        last_seen_at_ms: MOCK_NOW_MS - 40 * 60 * 1000,
      },
      dashboardTitle: "Mock compact coding pass",
      runtimeStatus: "Working",
      runtimeDetail: "Context preview",
      mainModel: "gpt-5.5",
      judgeModel: "gpt-5.5-mini",
      mainTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(2, 9_400_000, 6_200_000, 58_000, 21_000),
        mockDailyTokenUsage(1, 15_800_000, 10_900_000, 74_000, 32_000),
        mockDailyTokenUsage(0, 19_600_000, 14_400_000, 92_000, 41_000),
      ]),
      judgeTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(2, 640_000, 180_000, 14_000, 0),
        mockDailyTokenUsage(1, 920_000, 260_000, 18_000, 0),
        mockDailyTokenUsage(0, 1_180_000, 340_000, 22_000, 0),
      ]),
      contextComposition: mockContextCompositionSnapshot({
        model: "gpt-5.5",
        messageCount: 24,
        toolCount: 11,
        stablePrefixTokens: 18_000,
        changedPrefixTokens: 3_000,
        newSuffixTokens: 9_000,
        segments: [
          mockContextSegment("system_messages", "System messages", "system", 8_000, "prefix"),
          mockContextSegment("preturn_context", "Preturn context", "user", 5_400, "history"),
          mockContextSegment("conversation_history", "Conversation history", "user", 7_200, "history"),
          mockContextSegment("tool_messages", "Tool outputs", "tool", 2_900, "history"),
          mockContextSegment("tools_schema", "Tools schema", "request_tools", 6_500, "tools"),
        ],
      }),
    }),
    mockStatusSessionSummary({
      session: {
        session_id: "mock-status-session-large-context",
        scope: { kind: "project", project_dir: MOCK_PROJECT_DIR },
        project_dir: MOCK_PROJECT_DIR,
        title: "Mock long context review",
        started_at_ms: MOCK_NOW_MS - 11 * 24 * 60 * 60 * 1000,
        last_seen_at_ms: MOCK_NOW_MS - 2 * 60 * 60 * 1000,
      },
      dashboardTitle: "Mock long context review",
      runtimeStatus: "Ready",
      runtimeDetail: "Large request assembled",
      mainModel: "gpt-5.5",
      judgeModel: "gpt-5.5-mini",
      mainTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(3, 22_100_000, 16_000_000, 104_000, 52_000),
        mockDailyTokenUsage(2, 28_800_000, 20_200_000, 136_000, 68_000),
        mockDailyTokenUsage(1, 34_500_000, 27_900_000, 156_000, 71_000),
        mockDailyTokenUsage(0, 49_200_000, 39_600_000, 214_000, 118_000),
      ]),
      judgeTokenUsage: mockTokenUsageInfo([
        mockDailyTokenUsage(3, 1_100_000, 310_000, 22_000, 0),
        mockDailyTokenUsage(2, 1_480_000, 390_000, 29_000, 0),
        mockDailyTokenUsage(1, 2_140_000, 720_000, 34_000, 0),
        mockDailyTokenUsage(0, 2_680_000, 1_100_000, 39_000, 0),
      ]),
      contextComposition: mockContextCompositionSnapshot({
        model: "gpt-5.5",
        messageCount: 76,
        toolCount: 25,
        stablePrefixTokens: 41_000,
        changedPrefixTokens: 14_000,
        newSuffixTokens: 36_000,
        segments: [
          mockContextSegment("system_messages", "System messages", "system", 9_200, "prefix"),
          mockContextSegment("afterclaim_context", "Afterclaim context", "user", 7_600, "history"),
          mockContextSegment("preturn_context", "Preturn context", "user", 16_800, "history"),
          mockContextSegment("summarized_history", "Summarized history", "user", 18_400, "history"),
          mockContextSegment("conversation_history", "Conversation history", "user", 22_900, "history"),
          mockContextSegment("assistant_messages", "Assistant messages", "assistant", 11_700, "history"),
          mockContextSegment("tool_messages", "Tool outputs", "tool", 13_300, "history"),
          mockContextSegment("tools_schema", "Tools schema", "request_tools", 19_500, "tools"),
        ],
      }),
    }),
  ],
};

const MOCK_SETTINGS_SETUP_CONFIG: SetupConfigResponse = {
  config: {
    locale: "en-US",
    persona_name: "DaatLocus",
    persona_language: "zh-CN",
    persona_identity_summary: "{{name}} is concise, operational, and explicit about runtime state.",
    providers: [
      {
        name: "openai-main",
        kind: "openai_compatible",
        api_key: "$OPENAI_API_KEY",
        base_url: "https://api.openai.example/v1",
      },
      {
        name: "codex-oauth",
        kind: "openai_codex_oauth",
        base_url: null,
        codex_auth_method: "existing_auth_file",
        codex_auth_file: "C:\\Users\\13940\\.codex\\auth.json",
      },
      {
        name: "local-ollama",
        kind: "ollama",
        api_key: null,
        base_url: "http://127.0.0.1:11434",
        keep_alive: "5m",
      },
    ],
    models: [
      {
        name: "gpt-5.5",
        provider_name: "openai-main",
        model_id: "gpt-5.5",
        temperature: 0.2,
        thinking_budget: "medium",
        rpm: 120,
        request_timeout_secs: 180,
        stream_idle_timeout_secs: 45,
        context_window_tokens: 200_000,
        effective_context_window_percent: 80,
        auto_compact_token_limit: 144_000,
        max_completion_tokens: 32_768,
        tool_output_max_tokens: 40_000,
        supports_vision: true,
      },
      {
        name: "gpt-5.5-mini",
        provider_name: "openai-main",
        model_id: "gpt-5.5-mini",
        temperature: 0,
        thinking_budget: "low",
        rpm: 240,
        request_timeout_secs: 120,
        stream_idle_timeout_secs: 30,
        context_window_tokens: 128_000,
        effective_context_window_percent: 75,
        auto_compact_token_limit: 86_000,
        max_completion_tokens: 16_384,
        tool_output_max_tokens: 24_000,
        supports_vision: false,
      },
      {
        name: "codex-reasoner",
        provider_name: "codex-oauth",
        model_id: "codex-reasoner-2026-06",
        temperature: 0.1,
        thinking_budget: "high",
        rpm: null,
        request_timeout_secs: 240,
        stream_idle_timeout_secs: 60,
        context_window_tokens: 256_000,
        effective_context_window_percent: 70,
        auto_compact_token_limit: 160_000,
        max_completion_tokens: 65_536,
        tool_output_max_tokens: 60_000,
        supports_vision: true,
      },
      {
        name: "local-qwen",
        provider_name: "local-ollama",
        model_id: "qwen3:32b",
        temperature: 0.4,
        thinking_budget: null,
        rpm: null,
        request_timeout_secs: 90,
        stream_idle_timeout_secs: 20,
        context_window_tokens: 32_768,
        effective_context_window_percent: 85,
        auto_compact_token_limit: 25_000,
        max_completion_tokens: 8192,
        tool_output_max_tokens: 12_000,
        supports_vision: false,
      },
    ],
    main_model: "gpt-5.5",
    efficient_model: "gpt-5.5-mini",
    daemon_port: 53825,
    telegram_enabled: true,
    telegram_bot_token: "$TELEGRAM_BOT_TOKEN",
  },
  readiness: {
    ...MOCK_SETUP_READINESS,
    kind: "complete",
    message: "configuration is complete",
  },
};

type MockStatusSessionSummaryConfig = {
  session: SessionInfo;
  dashboardTitle: string;
  runtimeStatus: string;
  runtimeDetail: string;
  mainModel: string;
  judgeModel: string;
  mainTokenUsage: ReturnType<typeof mockTokenUsageInfo>;
  judgeTokenUsage: ReturnType<typeof mockTokenUsageInfo>;
  contextComposition: DashboardContextCompositionSnapshot;
};

type MockContextCompositionConfig = {
  model: string;
  messageCount: number;
  toolCount: number;
  stablePrefixTokens: number;
  changedPrefixTokens: number;
  newSuffixTokens: number;
  segments: DashboardContextCompositionSnapshot["segments"];
};

function mockStatusSessionSummary({
  session,
  dashboardTitle,
  runtimeStatus,
  runtimeDetail,
  mainModel,
  judgeModel,
  mainTokenUsage,
  judgeTokenUsage,
  contextComposition,
}: MockStatusSessionSummaryConfig): StatusSummary["sessions"][number] {
  return {
    session,
    runtime_status: {
      ready: true,
      pending_work_count: 0,
      active_runtime_turn: runtimeStatus === "Working",
    },
    dashboard: {
      agent_name: "Daat Locus",
      session_title: {
        title: dashboardTitle,
        generated: false,
        updated_at_ms: MOCK_NOW_MS,
      },
      last_cycle_elapsed_ms: 1180,
      runtime_status: runtimeStatus,
      runtime_status_level: "info",
      runtime_activity: {
        status: runtimeStatus === "Working" ? "running" : "idle",
        label: runtimeStatus,
        detail: runtimeDetail,
        active_runtime_turn: runtimeStatus === "Working",
        active_runtime_phase: runtimeStatus === "Working" ? "mock" : null,
      },
      current_plan_step: null,
      token_usage: {
        main_model: mainModel,
        judge_model: judgeModel,
        efficient_model: judgeModel,
        main: mainTokenUsage,
        judge: judgeTokenUsage,
      },
      primitive_optimization: MOCK_DASHBOARD_SNAPSHOT.primitive_optimization!,
      runtime_optimization: MOCK_DASHBOARD_SNAPSHOT.runtime_optimization!,
      context_composition: contextComposition,
    },
    error: null,
  };
}

function mockContextCompositionSnapshot({
  model,
  messageCount,
  toolCount,
  stablePrefixTokens,
  changedPrefixTokens,
  newSuffixTokens,
  segments,
}: MockContextCompositionConfig): DashboardContextCompositionSnapshot {
  const totalEstimatedTokens = segments.reduce(
    (total, segment) => total + segment.tokens,
    0,
  );
  const totalBytes = segments.reduce((total, segment) => total + segment.bytes, 0);
  const segmentsWithPercent = segments.map((segment) => ({
    ...segment,
    percent:
      totalEstimatedTokens > 0
        ? (segment.tokens / totalEstimatedTokens) * 100
        : 0,
  }));

  return {
    captured_at_ms: MOCK_NOW_MS,
    model,
    model_context_window: 200_000,
    total_estimated_tokens: totalEstimatedTokens,
    total_bytes: totalBytes,
    message_count: messageCount,
    tool_count: toolCount,
    tools_schema_tokens: segments
      .filter((segment) => segment.name === "tools_schema")
      .reduce((total, segment) => total + segment.tokens, 0),
    stable_prefix_tokens: stablePrefixTokens,
    new_suffix_tokens: newSuffixTokens,
    changed_prefix_tokens: changedPrefixTokens,
    previous_common_prefix_tokens: stablePrefixTokens,
    previous_request_hash: "mock-previous-context",
    current_request_hash: "mock-current-context",
    segments: segmentsWithPercent,
    prefix_units: segments.map((segment) => ({
      hash: segment.hash,
      tokens: segment.tokens,
    })),
  };
}

function mockContextSegment(
  name: string,
  label: string,
  source: string,
  tokens: number,
  cacheRole: string,
): DashboardContextCompositionSnapshot["segments"][number] {
  return {
    name,
    label,
    source,
    tokens,
    bytes: tokens * 4,
    percent: 0,
    hash: `${name}-${tokens}-${cacheRole}`,
    cache_role: cacheRole,
  };
}

function mockDailyTokenUsage(
  daysAgo: number,
  inputTokens: number,
  cachedInputTokens: number,
  outputTokens: number,
  reasoningOutputTokens: number,
) {
  return {
    date: mockDateDaysAgo(daysAgo),
    usage: mockTokenUsage(
      inputTokens,
      cachedInputTokens,
      outputTokens,
      reasoningOutputTokens,
    ),
  };
}

function mockTokenUsageInfo(dailyTokenUsage: ReturnType<typeof mockDailyTokenUsage>[]) {
  const totalTokenUsage = dailyTokenUsage.reduce(
    (total, day) => addMockTokenUsage(total, day.usage),
    mockTokenUsage(0, 0, 0, 0),
  );

  return {
    total_token_usage: totalTokenUsage,
    last_token_usage: dailyTokenUsage.at(-1)?.usage ?? mockTokenUsage(0, 0, 0, 0),
    model_context_window: 200_000,
    daily_token_usage: dailyTokenUsage,
  };
}

function addMockTokenUsage(
  left: ReturnType<typeof mockTokenUsage>,
  right: ReturnType<typeof mockTokenUsage>,
) {
  return mockTokenUsage(
    left.input_tokens + right.input_tokens,
    left.cached_input_tokens + right.cached_input_tokens,
    left.output_tokens + right.output_tokens,
    left.reasoning_output_tokens + right.reasoning_output_tokens,
  );
}

function mockTokenUsage(
  inputTokens: number,
  cachedInputTokens: number,
  outputTokens: number,
  reasoningOutputTokens: number,
) {
  return {
    input_tokens: inputTokens,
    cached_input_tokens: cachedInputTokens,
    output_tokens: outputTokens,
    reasoning_output_tokens: reasoningOutputTokens,
    total_tokens: inputTokens + outputTokens + reasoningOutputTokens,
  };
}

function mockDateDaysAgo(daysAgo: number) {
  const dayMs = 24 * 60 * 60 * 1000;
  return new Date(MOCK_NOW_MS - daysAgo * dayMs).toISOString().slice(0, 10);
}
