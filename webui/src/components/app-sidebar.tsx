import {
  ActivityIcon,
  ChevronRightIcon,
  Code2Icon,
  FolderIcon,
  MessageSquareIcon,
  PlusIcon,
  ScrollTextIcon,
  SettingsIcon,
  Trash2Icon,
} from "lucide-react";
import { useState, type ReactNode } from "react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
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
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
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

type AppSidebarProps = {
  activePage: AppPage;
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  sessionError: string | null;
  isCreatingSession: boolean;
  deletingSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onCreateSession: (projectDir?: string) => void;
  onDeleteSession: (sessionId: string) => Promise<void>;
};

type NavigationItem = {
  label: string;
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

const navigationItems: NavigationItem[] = [
  {
    label: "Agent",
    href: "#agent",
    page: "agent",
    icon: MessageSquareIcon,
  },
  {
    label: "Status",
    href: "#status",
    page: "status",
    icon: ActivityIcon,
  },
  {
    label: "Settings",
    href: "#settings",
    page: "settings",
    icon: SettingsIcon,
  },
  {
    label: "Logs",
    href: "#logs",
    page: "logs",
    icon: ScrollTextIcon,
  },
];

export function AppSidebar(props: AppSidebarProps) {
  return (
    <>
      <SidebarTrigger
        aria-label="Open sidebar"
        className="fixed top-4 left-4 rounded-full border-border/60 bg-background/80 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/60 md:hidden"
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
  onSelectSession,
  onCreateSession,
  onDeleteSession,
}: AppSidebarProps) {
  const { setOpenMobile } = useSidebar();
  const sessionTree = buildSessionTree(sessions);
  const [deleteCandidate, setDeleteCandidate] = useState<SessionInfo | null>(
    null,
  );
  const [isConfirmingDelete, setIsConfirmingDelete] = useState(false);

  function closeMobile() {
    setOpenMobile(false);
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
      <SidebarHeader className="border-b border-sidebar-border p-3">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              type="button"
              size="lg"
              className="h-12 cursor-default gap-3 rounded-lg px-2"
            >
              <Avatar className="size-8 rounded-lg">
                <AvatarFallback className="rounded-lg text-xs font-semibold">
                  DL
                </AvatarFallback>
              </Avatar>
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate text-sm font-semibold">
                  Daat Locus
                </span>
                <span className="truncate text-xs font-normal text-sidebar-foreground/55">
                  Local agent runtime
                </span>
              </span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>

      <SidebarContent className="gap-0 px-2 py-3">
        {sessionError ? (
          <Alert variant="destructive" className="mb-3">
            <AlertDescription className="text-xs">
              {sessionError}
            </AlertDescription>
          </Alert>
        ) : null}

        <SidebarGroup className="min-h-0 flex-1 p-0">
          <SidebarGroupLabel className="h-7 justify-between px-1">
            <span>Sessions</span>
            <Badge variant="secondary">{sessions.length}</Badge>
          </SidebarGroupLabel>
          <SidebarGroupContent className="min-h-0">
            <div
              role="tree"
              aria-label="Sessions"
              className="flex flex-col gap-2"
            >
              <SessionTreeBranch
                icon={MessageSquareIcon}
                label="General"
                count={sessionTree.general.length}
              >
                <SessionLeafList
                  createLabel="New general session"
                  isCreatingSession={isCreatingSession}
                  deletingSessionId={deletingSessionId}
                  sessions={sessionTree.general}
                  selectedSessionId={selectedSessionId}
                  onCreateSession={() => {
                    onCreateSession();
                    closeMobile();
                  }}
                  onSelectSession={onSelectSession}
                  onRequestDeleteSession={setDeleteCandidate}
                  onCloseMobile={closeMobile}
                />
              </SessionTreeBranch>

              {sessionTree.projectGroups.length > 0 ? (
                <SessionTreeBranch
                  icon={Code2Icon}
                  label="Coding"
                  count={sessionTree.projectGroups.reduce(
                    (count, group) => count + group.sessions.length,
                    0,
                  )}
                >
                  <div className="flex flex-col gap-2">
                    {sessionTree.projectGroups.map((group) => (
                      <SessionProjectBranch key={group.projectDir} group={group}>
                        <SessionLeafList
                          createLabel="New coding session"
                          isCreatingSession={isCreatingSession}
                          deletingSessionId={deletingSessionId}
                          sessions={group.sessions}
                          selectedSessionId={selectedSessionId}
                          onCreateSession={() => {
                            onCreateSession(group.projectDir);
                            closeMobile();
                          }}
                          onSelectSession={onSelectSession}
                          onRequestDeleteSession={setDeleteCandidate}
                          onCloseMobile={closeMobile}
                        />
                      </SessionProjectBranch>
                    ))}
                  </div>
                </SessionTreeBranch>
              ) : null}
            </div>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarSeparator />

      <SidebarFooter className="p-2">
        <SidebarGroupLabel className="h-6 px-1">Navigation</SidebarGroupLabel>
        <SidebarMenu className="grid grid-cols-2 gap-1">
          {navigationItems.map((item) => {
            const Icon = item.icon;
            const isActive = activePage === item.page;

            return (
              <SidebarMenuItem key={item.href}>
                <SidebarMenuButton
                  asChild
                  isActive={isActive}
                  className="h-9 justify-center gap-1.5"
                >
                  <a
                    href={item.href}
                    aria-current={isActive ? "page" : undefined}
                    onClick={closeMobile}
                  >
                    <Icon />
                    <span>{item.label}</span>
                  </a>
                </SidebarMenuButton>
              </SidebarMenuItem>
            );
          })}
        </SidebarMenu>
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

function SessionTreeBranch({
  icon: Icon,
  label,
  count,
  children,
}: {
  icon: typeof MessageSquareIcon;
  label: string;
  count: number;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(true);

  return (
    <div
      role="group"
      className="min-w-0 rounded-lg border border-sidebar-border bg-sidebar"
    >
      <div className="flex h-9 items-center gap-2 px-2">
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={`${open ? "Collapse" : "Expand"} ${label}`}
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
          className="text-sidebar-foreground/55 hover:text-sidebar-foreground"
        >
          <ChevronRightIcon
            className={cn("transition-transform", open && "rotate-90")}
          />
        </Button>
        <Icon className="size-3.5 shrink-0 text-sidebar-foreground/60" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-sidebar-foreground/80">
          {label}
        </span>
        <Badge variant="outline">{count}</Badge>
      </div>
      {open ? (
        <div className="flex flex-col gap-2 p-1 pt-0">{children}</div>
      ) : null}
    </div>
  );
}

function SessionProjectBranch({
  group,
  children,
}: {
  group: SessionProjectGroup;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(true);

  return (
    <div role="group" className="min-w-0 rounded-md bg-sidebar-accent/35">
      <div className="flex min-h-10 items-center gap-2 px-2 py-1.5">
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={`${open ? "Collapse" : "Expand"} ${group.label}`}
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
          className="text-sidebar-foreground/55 hover:text-sidebar-foreground"
        >
          <ChevronRightIcon
            className={cn("transition-transform", open && "rotate-90")}
          />
        </Button>
        <FolderIcon className="size-3.5 shrink-0 text-sidebar-foreground/60" />
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium text-sidebar-foreground/80">
            {group.label}
          </div>
          <div className="truncate text-[11px] text-sidebar-foreground/45">
            {group.projectDir}
          </div>
        </div>
        <Badge variant="secondary">{group.sessions.length}</Badge>
      </div>
      {open ? (
        <div className="flex flex-col gap-1 p-1 pt-0">{children}</div>
      ) : null}
    </div>
  );
}

function SessionLeafList({
  createLabel,
  isCreatingSession,
  deletingSessionId,
  sessions,
  selectedSessionId,
  onCreateSession,
  onSelectSession,
  onRequestDeleteSession,
  onCloseMobile,
}: {
  createLabel: string;
  isCreatingSession: boolean;
  deletingSessionId: string | null;
  sessions: SessionInfo[];
  selectedSessionId: string | null;
  onCreateSession: () => void;
  onSelectSession: (sessionId: string) => void;
  onRequestDeleteSession: (session: SessionInfo) => void;
  onCloseMobile: () => void;
}) {
  return (
    <SidebarMenu className="gap-1">
      <SidebarMenuItem>
        <SidebarMenuButton
          type="button"
          role="treeitem"
          disabled={isCreatingSession}
          onClick={onCreateSession}
          className="h-8 justify-start text-sidebar-foreground/75"
        >
          <PlusIcon />
          <span className="block max-w-full truncate font-medium">
            {isCreatingSession ? "Creating session" : createLabel}
          </span>
        </SidebarMenuButton>
      </SidebarMenuItem>
      {sessions.map((session) => (
        <SidebarMenuItem key={session.session_id}>
          <div className="group/session-row flex min-w-0 items-stretch gap-1 rounded-md">
            <SidebarMenuButton
              type="button"
              size="lg"
              role="treeitem"
              aria-selected={session.session_id === selectedSessionId}
              isActive={session.session_id === selectedSessionId}
              onClick={() => {
                onSelectSession(session.session_id);
                onCloseMobile();
              }}
              className="h-12 flex-1 flex-col items-start justify-center gap-0.5 px-2 py-1.5"
            >
              <span className="block max-w-full truncate font-medium">
                {sessionTitle(session)}
              </span>
              <span className="block max-w-full truncate text-xs font-normal text-sidebar-foreground/55">
                {sessionSubtitle(session)}
              </span>
            </SidebarMenuButton>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Delete ${sessionTitle(session)}`}
              title="Delete session"
              disabled={deletingSessionId !== null}
              onClick={() => onRequestDeleteSession(session)}
              className="mt-1 opacity-100 transition-opacity md:opacity-0 md:group-hover/session-row:opacity-100 md:focus-visible:opacity-100"
            >
              <Trash2Icon />
            </Button>
          </div>
        </SidebarMenuItem>
      ))}
    </SidebarMenu>
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
  return (
    <Dialog open={session !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete session?</DialogTitle>
          <DialogDescription>
            This permanently deletes{" "}
            <span className="font-medium text-foreground">
              {session ? sessionTitle(session) : "this session"}
            </span>{" "}
            ({session ? shortSessionId(session.session_id) : "unknown"}).
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose asChild>
            <Button type="button" variant="outline" disabled={deleting}>
              Cancel
            </Button>
          </DialogClose>
          <Button
            type="button"
            variant="destructive"
            disabled={deleting}
            onClick={onConfirm}
          >
            {deleting ? "Deleting" : "Delete"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function sessionTitle(session: SessionInfo) {
  return session.title?.trim() || "Untitled session";
}

function sessionSubtitle(session: SessionInfo) {
  return shortSessionId(session.session_id);
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
    .sort((a, b) => a.projectDir.localeCompare(b.projectDir));

  return {
    general,
    projectGroups,
  };
}

function sortSessions(sessions: SessionInfo[]) {
  return [...sessions].sort((a, b) => {
    const titleOrder = sessionTitle(a)
      .toLocaleLowerCase()
      .localeCompare(sessionTitle(b).toLocaleLowerCase());
    if (titleOrder !== 0) {
      return titleOrder;
    }

    if (a.started_at_ms !== b.started_at_ms) {
      return a.started_at_ms - b.started_at_ms;
    }

    return a.session_id.localeCompare(b.session_id);
  });
}

function projectLabel(projectDir: string) {
  const parts = projectDir.split("/").filter(Boolean);
  return parts.at(-1) ?? projectDir;
}

function shortSessionId(sessionId: string) {
  return sessionId.slice(0, 8);
}
