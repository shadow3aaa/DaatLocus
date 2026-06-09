import { useEffect, useState } from "react";

import { AppNavigation } from "@/components/app-navigation";
import { LoginPage } from "@/components/login-page";
import { LogsPage } from "@/components/logs-page";
import { SettingsPage } from "@/components/settings-page";
import { AgentPage, StatusPage } from "@/components/status-page";
import { getStoredDaemonToken } from "@/lib/daemon-auth";
import {
  createSession,
  fetchSessions,
  type SessionInfo,
} from "@/lib/daemon-api";

type AppPage = "agent" | "status" | "settings" | "logs";
const SELECTED_SESSION_STORAGE_KEY = "daat-locus.webui.selected-session-id";

export default function App() {
  const [isAuthenticated, setIsAuthenticated] = useState(() =>
    Boolean(getStoredDaemonToken()),
  );
  const [activePage, setActivePage] = useState(getCurrentPage);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(
    readStoredSelectedSessionId,
  );
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [hasLoadedSessions, setHasLoadedSessions] = useState(false);
  const [isCreatingSession, setIsCreatingSession] = useState(false);

  useEffect(() => {
    function updateActivePage() {
      setActivePage(getCurrentPage());
    }

    updateActivePage();
    window.addEventListener("hashchange", updateActivePage);

    return () => window.removeEventListener("hashchange", updateActivePage);
  }, []);

  useEffect(() => {
    if (!isAuthenticated) {
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
  }, [isAuthenticated]);

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

  async function handleCreateSession() {
    if (isCreatingSession) {
      return;
    }
    setIsCreatingSession(true);
    setSessionError(null);
    try {
      const session = await createSession();
      setSessions((current) => [...current, session]);
      setSelectedSessionId(session.session_id);
    } catch (error) {
      setSessionError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsCreatingSession(false);
    }
  }

  return (
    <main className="min-h-screen bg-background text-foreground">
      <AppNavigation isAuthenticated={isAuthenticated} />
      {isAuthenticated ? (
        <>
          <SessionSelector
            sessions={sessions}
            selectedSessionId={selectedSessionId}
            sessionError={sessionError}
            isCreatingSession={isCreatingSession}
            onSelectSession={setSelectedSessionId}
            onCreateSession={handleCreateSession}
          />
          {renderAuthenticatedPage(activePage, selectedSessionId, {
            hasLoadedSessions,
            isCreatingSession,
            sessionError,
            onCreateSession: handleCreateSession,
          })}
        </>
      ) : (
        <LoginPage onAuthenticated={() => setIsAuthenticated(true)} />
      )}
    </main>
  );
}

function renderAuthenticatedPage(
  activePage: AppPage,
  selectedSessionId: string | null,
  sessionState: {
    hasLoadedSessions: boolean;
    isCreatingSession: boolean;
    sessionError: string | null;
    onCreateSession: () => void;
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
        <AgentPage sessionId={selectedSessionId} />
      ) : (
        <NoSessionPage {...sessionState} />
      );
  }
}

function SessionSelector({
  sessions,
  selectedSessionId,
  sessionError,
  isCreatingSession,
  onSelectSession,
  onCreateSession,
}: {
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  sessionError: string | null;
  isCreatingSession: boolean;
  onSelectSession: (sessionId: string) => void;
  onCreateSession: () => void;
}) {
  const hasSessions = sessions.length > 0;
  return (
    <div className="fixed right-4 top-4 z-50 flex max-w-[calc(100vw-5rem)] items-center gap-2 md:right-6 md:top-6">
      <select
        aria-label="Session"
        value={selectedSessionId ?? ""}
        disabled={!hasSessions}
        onChange={(event) => {
          if (event.target.value) {
            onSelectSession(event.target.value);
          }
        }}
        className="h-10 max-w-[14rem] rounded-lg border border-border/70 bg-background/85 px-3 text-sm shadow-sm outline-none backdrop-blur supports-[backdrop-filter]:bg-background/70"
      >
        {hasSessions ? null : <option value="">No sessions</option>}
        {hasSessions && !selectedSessionId ? (
          <option value="" disabled>
            Select session
          </option>
        ) : null}
        {sessions.map((session) => (
          <option key={session.session_id} value={session.session_id}>
            {sessionLabel(session)}
          </option>
        ))}
      </select>
      <button
        type="button"
        onClick={onCreateSession}
        disabled={isCreatingSession}
        className="h-10 rounded-lg border border-border/70 bg-background/85 px-3 text-sm shadow-sm transition hover:border-primary/50 disabled:opacity-50"
      >
        {isCreatingSession ? "Creating" : "New"}
      </button>
      {sessionError ? (
        <span role="alert" className="sr-only">
          {sessionError}
        </span>
      ) : null}
    </div>
  );
}

function NoSessionPage({
  hasLoadedSessions,
  isCreatingSession,
  sessionError,
  onCreateSession,
}: {
  hasLoadedSessions: boolean;
  isCreatingSession: boolean;
  sessionError: string | null;
  onCreateSession: () => void;
}) {
  return (
    <section
      aria-label="Session required"
      className="flex min-h-screen w-full items-center justify-center px-6 py-16"
    >
      <div className="flex w-full max-w-md flex-col items-center gap-4 text-center">
        <h1 className="text-2xl font-semibold tracking-normal">
          {hasLoadedSessions ? "No session selected" : "Loading sessions"}
        </h1>
        <p className="text-sm text-muted-foreground">
          {sessionError ??
            (hasLoadedSessions
              ? "Create a session to open the agent dashboard."
              : "Fetching available sessions.")}
        </p>
        {hasLoadedSessions ? (
          <button
            type="button"
            onClick={onCreateSession}
            disabled={isCreatingSession}
            className="h-10 rounded-lg border border-border/70 bg-background px-4 text-sm shadow-sm transition hover:border-primary/50 disabled:opacity-50"
          >
            {isCreatingSession ? "Creating" : "New session"}
          </button>
        ) : null}
      </div>
    </section>
  );
}

function sessionLabel(session: SessionInfo) {
  const title = session.title?.trim() || "Untitled session";
  const scope =
    session.scope.kind === "project"
      ? session.scope.project_dir.split("/").filter(Boolean).pop()
      : null;
  return scope ? `${title} · ${scope}` : title;
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
