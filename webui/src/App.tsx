import { useEffect, useMemo, useState } from "react";

import { AppSidebar } from "@/components/app-sidebar";
import { LoginPage } from "@/components/login-page";
import { LogsPage } from "@/components/logs-page";
import { SettingsPage } from "@/components/settings-page";
import { AgentPage, StatusPage } from "@/components/status-page";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { getStoredDaemonToken } from "@/lib/daemon-auth";
import {
  createSession,
  deleteSession,
  fetchSessions,
  type DashboardSnapshot,
  type SessionInfo,
} from "@/lib/daemon-api";

type AppPage = "agent" | "status" | "settings" | "logs";
const SELECTED_SESSION_STORAGE_KEY = "daat-locus.webui.selected-session-id";

const APP_DOCUMENT_TITLE = "Daat Locus";

export default function App() {
  if (shouldRenderMockAgentPage()) {
    return <MockAgentApp />;
  }

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
  const [deletingSessionId, setDeletingSessionId] = useState<string | null>(
    null,
  );

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

  return (
    <main className="min-h-screen bg-background text-foreground">
      {isAuthenticated ? (
        <SidebarProvider>
          <AppSidebar
            activePage={activePage}
            sessions={sessions}
            selectedSessionId={selectedSessionId}
            sessionError={sessionError}
            isCreatingSession={isCreatingSession}
            deletingSessionId={deletingSessionId}
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

const MOCK_NOW_MS = Date.now();
const MOCK_PROJECT_DIR = "C:\\Users\\13940\\DaatLocus";

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
  activity_cells: [],
  live_activity_cells: [],
  web_activity_version: 1,
  web_activity_items: [
    {
      web_activity_version: 1,
      id: "mock-user",
      kind: "message",
      status: "completed",
      title: "Align the WebUI with the new TUI direction.",
      actor: "user",
      created_at: MOCK_ACTIVITY_STARTED_AT,
      updated_at: MOCK_ACTIVITY_STARTED_AT,
      blocks: [],
      cell: {
        User: {
          title: "Align the WebUI with the new TUI direction.",
          body_lines: [],
          full_body:
            "Align the WebUI with the new TUI direction.\nUse shadcn components where the web has a native control.",
        },
      },
    },
    {
      web_activity_version: 1,
      id: "mock-plan",
      kind: "plan",
      status: "completed",
      title: "Updated Plan",
      actor: "system",
      created_at: MOCK_ACTIVITY_STARTED_AT + 1_000,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 1_000,
      blocks: [],
      cell: {
        PlanResult: {
          steps: [
            { status: "Completed", text: "Read current WebUI and TUI data flow" },
            { status: "InProgress", text: "Reshape Agent page into activity workbench" },
            { status: "Pending", text: "Verify responsive desktop and mobile layouts" },
          ],
        },
      },
    },
    {
      web_activity_version: 1,
      id: "mock-explored",
      kind: "tool",
      status: "completed",
      title: "Explored",
      actor: "tool",
      created_at: MOCK_ACTIVITY_STARTED_AT + 1_500,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 1_800,
      tool: {
        name: "explored",
        app: "Coding",
        duration_ms: null,
        exit_code: null,
      },
      blocks: [],
      cell: {
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
    },
    {
      web_activity_version: 1,
      id: "mock-exec",
      kind: "tool",
      status: "completed",
      title: "Ran bun run typecheck",
      actor: "tool",
      created_at: MOCK_ACTIVITY_STARTED_AT + 2_000,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 4_000,
      tool: {
        name: "terminal",
        app: "Terminal",
        duration_ms: 2_000,
        exit_code: 0,
      },
      blocks: [],
      cell: {
        ExecResult: {
          title: "bun run typecheck",
          meta: "exit=0",
          output_lines: ["$ tsc -p tsconfig.json --noEmit", "Finished in 1.2s"],
        },
      },
    },
    {
      web_activity_version: 1,
      id: "mock-patch",
      kind: "patch",
      status: "completed",
      title: "Edited WebUI Agent surface",
      actor: "tool",
      created_at: MOCK_ACTIVITY_STARTED_AT + 5_000,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 6_500,
      blocks: [],
      cell: {
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
                  text: "<AgentStatusAnimation />",
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
    },
    {
      web_activity_version: 1,
      id: "mock-worked",
      kind: "message",
      status: "completed",
      ui_hint: "final-message-separator",
      title: "Worked for 2m 34s",
      actor: "system",
      created_at: MOCK_ACTIVITY_STARTED_AT + 9_000,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 9_000,
      blocks: [],
      cell: null,
    },
    {
      web_activity_version: 1,
      id: "mock-reply",
      kind: "message",
      status: "completed",
      title: "Agent reply",
      actor: "assistant",
      created_at: MOCK_ACTIVITY_STARTED_AT + 9_500,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 9_500,
      blocks: [],
      cell: {
        Reply: {
          disposition: "resolved",
          subject: "message",
          message_lines: [
            "Agent reply should use the activity marker.",
            "It must not be rendered as a user prompt.",
          ],
        },
      },
    },
    {
      web_activity_version: 1,
      id: "mock-assistant",
      kind: "message",
      status: "completed",
      title: "The Agent page now behaves like a web workbench.",
      actor: "assistant",
      created_at: MOCK_ACTIVITY_STARTED_AT + 10_000,
      updated_at: MOCK_ACTIVITY_STARTED_AT + 10_000,
      blocks: [],
      cell: {
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
    },
  ],
  live_web_activity_items: [
    {
      key: "mock-live-exec",
      item: {
        web_activity_version: 1,
        id: "mock-live-exec",
        kind: "tool",
        status: "running",
        title: "Running cargo check",
        actor: "tool",
        created_at: MOCK_ACTIVITY_STARTED_AT + 12_000,
        updated_at: MOCK_ACTIVITY_STARTED_AT + 13_000,
        tool: {
          name: "terminal",
          app: "Terminal",
          duration_ms: null,
          exit_code: null,
        },
        blocks: [],
        cell: {
          LiveExec: {
            title: "cargo check",
            call_lines: ["cargo check"],
            meta: null,
            output_lines: ["Checking daat-locus v0.1.1"],
            started_at_ms: MOCK_ACTIVITY_STARTED_AT + 12_000,
          },
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
