import {
  ActivityIcon,
  MessageSquareIcon,
  PlusIcon,
  ScrollTextIcon,
  SettingsIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
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
  onSelectSession: (sessionId: string) => void;
  onCreateSession: () => void;
};

type NavigationItem = {
  label: string;
  href: string;
  page: AppPage;
  icon: typeof MessageSquareIcon;
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
  onSelectSession,
  onCreateSession,
}: AppSidebarProps) {
  const { setOpenMobile } = useSidebar();

  function closeMobile() {
    setOpenMobile(false);
  }

  return (
    <>
      <SidebarHeader className="h-14 flex-row items-center justify-between border-b border-sidebar-border px-3">
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold">Daat Locus</div>
          <div className="truncate text-xs text-sidebar-foreground/65">
            {sessions.length} session{sessions.length === 1 ? "" : "s"}
          </div>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label="New session"
          title="New session"
          onClick={() => {
            onCreateSession();
            closeMobile();
          }}
          disabled={isCreatingSession}
          className="shrink-0"
        >
          <PlusIcon className={cn("size-4", isCreatingSession && "opacity-60")} />
        </Button>
      </SidebarHeader>

      <SidebarContent>
        {sessionError ? (
          <div
            role="alert"
            className="mx-3 mt-3 rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-2 text-xs text-destructive"
          >
            {sessionError}
          </div>
        ) : null}

        <SidebarGroup className="min-h-0 flex-1">
          <SidebarGroupLabel>Sessions</SidebarGroupLabel>
          <SidebarGroupContent className="min-h-0">
            {sessions.length === 0 ? (
              <div className="rounded-md border border-sidebar-border bg-sidebar-accent/45 px-3 py-2 text-sm text-sidebar-foreground/65">
                No sessions
              </div>
            ) : (
              <SidebarMenu>
                {sessions.map((session) => (
                  <SidebarMenuItem key={session.session_id}>
                    <SidebarMenuButton
                      type="button"
                      size="lg"
                      isActive={session.session_id === selectedSessionId}
                      onClick={() => {
                        onSelectSession(session.session_id);
                        closeMobile();
                      }}
                      className="h-auto min-h-12 flex-col items-start gap-0.5 py-2"
                    >
                      <span className="block max-w-full truncate font-medium">
                        {sessionTitle(session)}
                      </span>
                      {sessionSubtitle(session) ? (
                        <span className="block max-w-full truncate text-xs font-normal text-sidebar-foreground/60">
                          {sessionSubtitle(session)}
                        </span>
                      ) : null}
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            )}
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarSeparator />

      <SidebarFooter>
        <SidebarMenu>
          {navigationItems.map((item) => {
            const Icon = item.icon;
            const isActive = activePage === item.page;

            return (
              <SidebarMenuItem key={item.href}>
                <SidebarMenuButton asChild isActive={isActive}>
                  <a
                    href={item.href}
                    aria-current={isActive ? "page" : undefined}
                    onClick={closeMobile}
                  >
                    <Icon className="size-4" />
                    <span>{item.label}</span>
                  </a>
                </SidebarMenuButton>
              </SidebarMenuItem>
            );
          })}
        </SidebarMenu>
      </SidebarFooter>
    </>
  );
}

function sessionTitle(session: SessionInfo) {
  return session.title?.trim() || "Untitled session";
}

function sessionSubtitle(session: SessionInfo) {
  if (session.scope.kind === "project") {
    return session.scope.project_dir.split("/").filter(Boolean).pop() ?? "Project";
  }
  return null;
}
