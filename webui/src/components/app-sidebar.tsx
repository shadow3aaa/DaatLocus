import type { TFunction } from "i18next";
import {
  ActivityIcon,
  ChevronDownIcon,
  FolderIcon,
  FolderPlusIcon,
  MessageSquareIcon,
  MoonIcon,
  PlusIcon,
  ScrollTextIcon,
  SettingsIcon,
  SunIcon,
  Trash2Icon,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarSeparator,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar";
import type { SessionInfo } from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

type AppPage = "agent" | "status" | "settings" | "logs";
type ThemeMode = "light" | "dark";

type AppSidebarProps = {
  activePage: AppPage;
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  sessionError: string | null;
  isCreatingSession: boolean;
  deletingSessionId: string | null;
  themeMode: ThemeMode;
  onToggleThemeMode: () => void;
  onSelectSession: (sessionId: string) => void;
  onCreateSession: (projectDir?: string) => void;
  onDeleteSession: (sessionId: string) => Promise<void>;
};

type NavigationItem = {
  labelKey: string;
  href: string;
  page: AppPage;
  icon: typeof MessageSquareIcon;
};

type SessionProjectGroup = {
  projectDir: string;
  label: string;
  sessions: SessionInfo[];
};

type SessionTree = {
  general: SessionInfo[];
  projectGroups: SessionProjectGroup[];
};

const PROJECT_VISIBLE_SESSION_COUNT = 4;
const GENERAL_VISIBLE_SESSION_COUNT = 8;

const navigationItems: NavigationItem[] = [
  {
    labelKey: "navigation.agent",
    href: "#agent",
    page: "agent",
    icon: MessageSquareIcon,
  },
  {
    labelKey: "navigation.status",
    href: "#status",
    page: "status",
    icon: ActivityIcon,
  },
  {
    labelKey: "navigation.settings",
    href: "#settings",
    page: "settings",
    icon: SettingsIcon,
  },
  {
    labelKey: "navigation.logs",
    href: "#logs",
    page: "logs",
    icon: ScrollTextIcon,
  },
];

export function AppSidebar(props: AppSidebarProps) {
  const { t } = useTranslation();

  return (
    <>
      <SidebarTrigger
        aria-label={t("sidebar.open")}
        className="fixed top-4 left-4 z-50 rounded-full border-border/60 bg-background/80 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/60 md:hidden"
      />
      <Sidebar>
        <AppSidebarBody {...props} />
      </Sidebar>
    </>
  );
}

function AppSidebarBody({
  activePage,
  sessions,
  selectedSessionId,
  sessionError,
  isCreatingSession,
  deletingSessionId,
  themeMode,
  onToggleThemeMode,
  onSelectSession,
  onCreateSession,
  onDeleteSession,
}: AppSidebarProps) {
  const { setOpenMobile } = useSidebar();
  const { t } = useTranslation();
  const sessionTree = buildSessionTree(sessions);
  const [projectsOpen, setProjectsOpen] = useState(true);
  const [conversationsOpen, setConversationsOpen] = useState(true);
  const [deleteCandidate, setDeleteCandidate] = useState<SessionInfo | null>(
    null,
  );
  const [isConfirmingDelete, setIsConfirmingDelete] = useState(false);

  function closeMobile() {
    setOpenMobile(false);
  }

  function navigateTo(item: NavigationItem) {
    window.location.hash = item.href;
    closeMobile();
  }

  async function confirmDeleteSession() {
    if (!deleteCandidate || isConfirmingDelete) {
      return;
    }
    setIsConfirmingDelete(true);
    try {
      await onDeleteSession(deleteCandidate.session_id);
      setDeleteCandidate(null);
    } catch {
      // The parent renders the API error in the sidebar alert.
    } finally {
      setIsConfirmingDelete(false);
    }
  }

  return (
    <>
      <SidebarContent className="px-2 py-2">
        {sessionError ? (
          <Alert variant="destructive" className="mb-2">
            <AlertDescription className="text-xs">
              {sessionError}
            </AlertDescription>
          </Alert>
        ) : null}

        <SidebarSessionSection
          label={t("sidebar.projects")}
          open={projectsOpen}
          onOpenChange={setProjectsOpen}
          actions={
            <NewCodingSessionMenu
              projectGroups={sessionTree.projectGroups}
              disabled={isCreatingSession}
              onCreateSession={(projectDir) => {
                onCreateSession(projectDir);
                closeMobile();
              }}
            />
          }
        >
          {sessionTree.projectGroups.length > 0 ? (
            <div className="flex flex-col gap-1">
              {sessionTree.projectGroups.map((group) => (
                <ProjectSessionGroup
                  key={group.projectDir}
                  group={group}
                  selectedSessionId={selectedSessionId}
                  isCreatingSession={isCreatingSession}
                  deletingSessionId={deletingSessionId}
                  onCreateSession={() => {
                    onCreateSession(group.projectDir);
                    closeMobile();
                  }}
                  onSelectSession={(sessionId) => {
                    onSelectSession(sessionId);
                    closeMobile();
                  }}
                  onRequestDeleteSession={setDeleteCandidate}
                />
              ))}
            </div>
          ) : (
            <SidebarEmptyText>{t("sidebar.noProjects")}</SidebarEmptyText>
          )}
        </SidebarSessionSection>

        <ConversationSessionGroup
          sessions={sessionTree.general}
          selectedSessionId={selectedSessionId}
          open={conversationsOpen}
          isCreatingSession={isCreatingSession}
          deletingSessionId={deletingSessionId}
          onOpenChange={setConversationsOpen}
          onCreateSession={() => {
            onCreateSession();
            closeMobile();
          }}
          onSelectSession={(sessionId) => {
            onSelectSession(sessionId);
            closeMobile();
          }}
          onRequestDeleteSession={setDeleteCandidate}
        />
      </SidebarContent>

      <SidebarFooter>
        <SidebarSeparator className="mx-0" />
        <div className="flex flex-col gap-1">
          {navigationItems.map((item) => {
            const Icon = item.icon;
            const label = t(item.labelKey);

            return (
              <Button
                key={item.href}
                type="button"
                variant="ghost"
                aria-current={activePage === item.page ? "page" : undefined}
                onClick={() => navigateTo(item)}
                className="w-full justify-start"
              >
                <Icon data-icon="inline-start" />
                {label}
              </Button>
            );
          })}
          <Button
            type="button"
            variant="ghost"
            aria-label={
              themeMode === "dark"
                ? t("theme.switchToLight")
                : t("theme.switchToDark")
            }
            aria-pressed={themeMode === "dark"}
            title={
              themeMode === "dark"
                ? t("theme.switchToLight")
                : t("theme.switchToDark")
            }
            onClick={onToggleThemeMode}
            className="w-full justify-start"
          >
            {themeMode === "dark" ? (
              <SunIcon data-icon="inline-start" />
            ) : (
              <MoonIcon data-icon="inline-start" />
            )}
            {themeMode === "dark" ? t("theme.lightMode") : t("theme.darkMode")}
          </Button>
        </div>
      </SidebarFooter>

      <DeleteSessionDialog
        session={deleteCandidate}
        deleting={isConfirmingDelete || deletingSessionId !== null}
        onOpenChange={(open) => {
          if (!open && !isConfirmingDelete) {
            setDeleteCandidate(null);
          }
        }}
        onConfirm={confirmDeleteSession}
      />
    </>
  );
}

function SidebarSessionSection({
  label,
  open,
  actions,
  children,
  onOpenChange,
}: {
  label: string;
  open: boolean;
  actions?: ReactNode;
  children: ReactNode;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <SidebarGroup className="gap-1 p-0">
      <div className="flex h-8 min-w-0 items-center gap-1">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-expanded={open}
          onClick={() => onOpenChange(!open)}
          className="h-8 min-w-0 flex-1 justify-start px-2 text-base font-normal"
        >
          <span className="truncate">{label}</span>
          <ChevronDownIcon
            data-icon="inline-end"
            className={cn("transition-transform", !open && "-rotate-90")}
          />
        </Button>
        {actions ? <div className="flex items-center gap-1">{actions}</div> : null}
      </div>
      {open ? (
        <SidebarGroupContent className="pt-2">{children}</SidebarGroupContent>
      ) : null}
    </SidebarGroup>
  );
}

function NewCodingSessionMenu({
  projectGroups,
  disabled,
  onCreateSession,
}: {
  projectGroups: SessionProjectGroup[];
  disabled: boolean;
  onCreateSession: (projectDir: string) => void;
}) {
  const { t } = useTranslation();

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={t("sidebar.newCodingSession")}
          title={t("sidebar.newCodingSession")}
          disabled={disabled || projectGroups.length === 0}
        >
          <FolderPlusIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-60">
        <DropdownMenuLabel>{t("sidebar.newProjectSession")}</DropdownMenuLabel>
        <DropdownMenuGroup>
          {projectGroups.map((group) => (
            <DropdownMenuItem
              key={group.projectDir}
              onSelect={() => onCreateSession(group.projectDir)}
            >
              <FolderIcon />
              <span className="truncate">{group.label}</span>
            </DropdownMenuItem>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ProjectSessionGroup({
  group,
  selectedSessionId,
  isCreatingSession,
  deletingSessionId,
  onCreateSession,
  onSelectSession,
  onRequestDeleteSession,
}: {
  group: SessionProjectGroup;
  selectedSessionId: string | null;
  isCreatingSession: boolean;
  deletingSessionId: string | null;
  onCreateSession: () => void;
  onSelectSession: (sessionId: string) => void;
  onRequestDeleteSession: (session: SessionInfo) => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const visibleSessions = expanded
    ? group.sessions
    : group.sessions.slice(0, PROJECT_VISIBLE_SESSION_COUNT);
  const hiddenSessionCount = group.sessions.length - visibleSessions.length;
  const newSessionLabel = t("sidebar.newSessionInProject", {
    project: group.label,
  });

  return (
    <div className="min-w-0">
      <div className="flex h-9 min-w-0 items-center gap-1">
        <div
          className="flex min-w-0 flex-1 items-center gap-2 px-2 text-sm font-medium"
          title={group.projectDir}
        >
          <FolderIcon className="size-4 shrink-0 text-sidebar-foreground/75" />
          <span className="truncate">{group.label}</span>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={newSessionLabel}
          title={newSessionLabel}
          disabled={isCreatingSession}
          onClick={onCreateSession}
        >
          <PlusIcon />
        </Button>
      </div>

      <SessionRows
        className="pl-8"
        sessions={visibleSessions}
        selectedSessionId={selectedSessionId}
        deletingSessionId={deletingSessionId}
        onSelectSession={onSelectSession}
        onRequestDeleteSession={onRequestDeleteSession}
      />

      {hiddenSessionCount > 0 ? (
        <button
          type="button"
          className="h-8 px-8 text-left text-sm text-sidebar-foreground/45 hover:text-sidebar-foreground"
          onClick={() => setExpanded(true)}
        >
          {t("sidebar.showMore")}
        </button>
      ) : expanded && group.sessions.length > PROJECT_VISIBLE_SESSION_COUNT ? (
        <button
          type="button"
          className="h-8 px-8 text-left text-sm text-sidebar-foreground/45 hover:text-sidebar-foreground"
          onClick={() => setExpanded(false)}
        >
          {t("sidebar.showLess")}
        </button>
      ) : null}
    </div>
  );
}

function ConversationSessionGroup({
  sessions,
  selectedSessionId,
  open,
  isCreatingSession,
  deletingSessionId,
  onOpenChange,
  onCreateSession,
  onSelectSession,
  onRequestDeleteSession,
}: {
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  open: boolean;
  isCreatingSession: boolean;
  deletingSessionId: string | null;
  onOpenChange: (open: boolean) => void;
  onCreateSession: () => void;
  onSelectSession: (sessionId: string) => void;
  onRequestDeleteSession: (session: SessionInfo) => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const visibleSessions = expanded
    ? sessions
    : sessions.slice(0, GENERAL_VISIBLE_SESSION_COUNT);
  const hiddenSessionCount = sessions.length - visibleSessions.length;

  return (
    <SidebarSessionSection
      label={t("sidebar.conversations")}
      open={open}
      onOpenChange={onOpenChange}
      actions={
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={t("sidebar.newConversation")}
          title={t("sidebar.newConversation")}
          disabled={isCreatingSession}
          onClick={onCreateSession}
        >
          <PlusIcon />
        </Button>
      }
    >
        {visibleSessions.length > 0 ? (
          <SessionRows
            sessions={visibleSessions}
            selectedSessionId={selectedSessionId}
            deletingSessionId={deletingSessionId}
            onSelectSession={onSelectSession}
            onRequestDeleteSession={onRequestDeleteSession}
          />
        ) : (
          <SidebarEmptyText>{t("sidebar.noChats")}</SidebarEmptyText>
        )}

        {hiddenSessionCount > 0 ? (
          <button
            type="button"
            className="h-8 px-2 text-left text-sm text-sidebar-foreground/45 hover:text-sidebar-foreground"
            onClick={() => setExpanded(true)}
          >
            {t("sidebar.showMore")}
          </button>
        ) : expanded && sessions.length > GENERAL_VISIBLE_SESSION_COUNT ? (
          <button
            type="button"
            className="h-8 px-2 text-left text-sm text-sidebar-foreground/45 hover:text-sidebar-foreground"
            onClick={() => setExpanded(false)}
          >
            {t("sidebar.showLess")}
          </button>
        ) : null}
    </SidebarSessionSection>
  );
}

function SessionRows({
  className,
  sessions,
  selectedSessionId,
  deletingSessionId,
  onSelectSession,
  onRequestDeleteSession,
}: {
  className?: string;
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  deletingSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onRequestDeleteSession: (session: SessionInfo) => void;
}) {
  return (
    <SidebarMenu className={cn("gap-0.5", className)}>
      {sessions.map((session) => (
        <SessionRow
          key={session.session_id}
          session={session}
          selected={session.session_id === selectedSessionId}
          deletingSessionId={deletingSessionId}
          onSelectSession={onSelectSession}
          onRequestDeleteSession={onRequestDeleteSession}
        />
      ))}
    </SidebarMenu>
  );
}

function SessionRow({
  session,
  selected,
  deletingSessionId,
  onSelectSession,
  onRequestDeleteSession,
}: {
  session: SessionInfo;
  selected: boolean;
  deletingSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onRequestDeleteSession: (session: SessionInfo) => void;
}) {
  const { t } = useTranslation();
  const title = sessionTitle(session, t("common.untitledSession"));
  const isDeleting = deletingSessionId === session.session_id;

  return (
    <SidebarMenuItem>
      <div className="group/session-row relative min-w-0">
        <SidebarMenuButton
          type="button"
          aria-selected={selected}
          isActive={selected}
          onClick={() => onSelectSession(session.session_id)}
          className="h-8 gap-2 pr-8 pl-2 text-sidebar-foreground/85"
        >
          <span className="min-w-0 flex-1 truncate">{title}</span>
          <span className="shrink-0 text-xs font-normal text-sidebar-foreground/45">
            {relativeSessionTime(session, t)}
          </span>
        </SidebarMenuButton>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={t("sidebar.deleteSessionAria", { title })}
          title={t("sidebar.deleteSessionTitle")}
          disabled={deletingSessionId !== null}
          onClick={() => onRequestDeleteSession(session)}
          className={cn(
            "absolute top-1 right-1 opacity-0 transition-opacity group-hover/session-row:opacity-100 focus-visible:opacity-100",
            isDeleting && "opacity-100",
          )}
        >
          <Trash2Icon />
        </Button>
      </div>
    </SidebarMenuItem>
  );
}

function SidebarEmptyText({ children }: { children: ReactNode }) {
  return (
    <div className="px-2 py-3 text-sm text-sidebar-foreground/35">
      {children}
    </div>
  );
}

function DeleteSessionDialog({
  session,
  deleting,
  onOpenChange,
  onConfirm,
}: {
  session: SessionInfo | null;
  deleting: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();
  const title = session
    ? sessionTitle(session, t("common.untitledSession"))
    : t("common.thisSession");
  const id = session ? shortSessionId(session.session_id) : t("common.unknown");

  return (
    <Dialog open={session !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("sidebar.deleteDialogTitle")}</DialogTitle>
          <DialogDescription>
            {t("sidebar.deleteDialogDescription", { title, id })}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose asChild>
            <Button type="button" variant="outline" disabled={deleting}>
              {t("common.cancel")}
            </Button>
          </DialogClose>
          <Button
            type="button"
            variant="destructive"
            disabled={deleting}
            onClick={onConfirm}
          >
            {deleting ? t("common.deleting") : t("common.delete")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function sessionTitle(session: SessionInfo, fallback = "Untitled session") {
  return session.title?.trim() || fallback;
}

function relativeSessionTime(session: SessionInfo, t: TFunction) {
  const timestamp = session.last_seen_at_ms ?? session.started_at_ms;
  const elapsedMs = Math.max(0, Date.now() - timestamp);
  const minuteMs = 60_000;
  const hourMs = 60 * minuteMs;
  const dayMs = 24 * hourMs;
  const monthMs = 30 * dayMs;
  const yearMs = 365 * dayMs;

  if (elapsedMs < minuteMs) {
    return t("sidebar.relativeTime.now");
  }
  if (elapsedMs < hourMs) {
    return t("sidebar.relativeTime.minute", {
      count: Math.floor(elapsedMs / minuteMs),
    });
  }
  if (elapsedMs < dayMs) {
    return t("sidebar.relativeTime.hour", {
      count: Math.floor(elapsedMs / hourMs),
    });
  }
  if (elapsedMs < monthMs) {
    return t("sidebar.relativeTime.day", {
      count: Math.floor(elapsedMs / dayMs),
    });
  }
  if (elapsedMs < yearMs) {
    return t("sidebar.relativeTime.month", {
      count: Math.floor(elapsedMs / monthMs),
    });
  }
  return t("sidebar.relativeTime.year", {
    count: Math.floor(elapsedMs / yearMs),
  });
}

function buildSessionTree(sessions: SessionInfo[]): SessionTree {
  const general = sortSessions(
    sessions.filter((session) => session.scope.kind === "general"),
  );
  const projects = new Map<string, SessionInfo[]>();

  for (const session of sessions) {
    if (session.scope.kind !== "project") {
      continue;
    }

    const projectDir = session.scope.project_dir;
    projects.set(projectDir, [...(projects.get(projectDir) ?? []), session]);
  }

  const projectGroups = Array.from(projects.entries())
    .map(([projectDir, projectSessions]) => ({
      projectDir,
      label: projectLabel(projectDir),
      sessions: sortSessions(projectSessions),
    }))
    .sort((a, b) => {
      const recencyOrder =
        latestSessionTime(b.sessions) - latestSessionTime(a.sessions);
      if (recencyOrder !== 0) {
        return recencyOrder;
      }
      return a.label.localeCompare(b.label);
    });

  return {
    general,
    projectGroups,
  };
}

function sortSessions(sessions: SessionInfo[]) {
  return [...sessions].sort((a, b) => {
    const recencyOrder = sessionTime(b) - sessionTime(a);
    if (recencyOrder !== 0) {
      return recencyOrder;
    }

    const titleOrder = sessionTitle(a)
      .toLocaleLowerCase()
      .localeCompare(sessionTitle(b).toLocaleLowerCase());
    if (titleOrder !== 0) {
      return titleOrder;
    }

    return a.session_id.localeCompare(b.session_id);
  });
}

function latestSessionTime(sessions: SessionInfo[]) {
  return sessions.reduce(
    (latest, session) => Math.max(latest, sessionTime(session)),
    0,
  );
}

function sessionTime(session: SessionInfo) {
  return session.last_seen_at_ms ?? session.started_at_ms;
}

function projectLabel(projectDir: string) {
  const parts = projectDir.split(/[\\/]/).filter(Boolean);
  return parts.at(-1) ?? projectDir;
}

function shortSessionId(sessionId: string) {
  return sessionId.slice(0, 8);
}
