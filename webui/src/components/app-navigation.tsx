import type { ReactNode } from "react";
import { GlobeIcon, MessagesSquareIcon, MonitorIcon, LogOutIcon, PanelLeftIcon, PuzzleIcon, SettingsIcon, SquarePenIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import type { Access } from "@/lib/actor-api";
import { Button } from "@/components/ui/button";
import { GeneratedAvatar } from "@/components/generated-avatar";
import { accountAvatarSeed } from "@/lib/conversation-avatars";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export type NavigationPage = "new" | "session" | "sessions" | "apps" | "browser" | "virtual-displays" | "settings";

export function NavigationRail({access, page, virtualDisplaysSupported, onVirtualDisplays, browserSupported, onBrowser, onNewSession, onSessions, onApps, onSettings, onLogout}: {
  access: Access; page: NavigationPage; browserSupported?: boolean; onBrowser?(): void; virtualDisplaysSupported?: boolean; onVirtualDisplays?(): void; onNewSession(): void; onSessions(): void; onApps(): void; onSettings(): void; onLogout(): void;
}) {
  const item = (label: string, icon: ReactNode, current: boolean, onClick: () => void, className?: string) => <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" className={className} aria-label={label} aria-current={current ? "page" : undefined} onClick={onClick}>{icon}</Button></TooltipTrigger><TooltipContent side="right">{label}</TooltipContent></Tooltip>;
  return <nav className="dl-navigation-rail" aria-label={t("navigation.app_navigation")}>
    {item(t("navigation.new_session"), <SquarePenIcon />, page === "new", onNewSession, "dl-rail-home")}
    {item(t("sessions.title"), <MessagesSquareIcon />, page === "sessions" || page === "session", onSessions)}
    {item(t("apps.title"), <PuzzleIcon />, page === "apps", onApps)}
    {browserSupported && onBrowser && item(t("browser_use.title"), <GlobeIcon />, page === "browser", onBrowser)}
    {virtualDisplaysSupported && onVirtualDisplays && item(t("virtual_displays.title"), <MonitorIcon />, page === "virtual-displays", onVirtualDisplays)}
    <div className="dl-rail-account"><AccountMenu access={access} onSettings={onSettings} onLogout={onLogout} /></div>
  </nav>;
}

export function AccountMenu({access, onSettings, onLogout}: {access: Access; onSettings(): void; onLogout(): void}) {
  const name = access.account?.name || t("app.account");
  const seed = accountAvatarSeed(access.account?.account_id);
  return <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="dl-profile-button" aria-label={t("navigation.profile_menu")}><GeneratedAvatar seed={seed} /></Button></DropdownMenuTrigger>
    <DropdownMenuContent align="start" side="right" sideOffset={12} className="dl-account-menu">
      <DropdownMenuLabel className="dl-account-menu-header"><GeneratedAvatar seed={seed} className="size-9" /><span><strong>{name}</strong><small>{t("app.signed_in")}</small></span></DropdownMenuLabel><DropdownMenuSeparator />
      <DropdownMenuGroup><DropdownMenuItem onSelect={onSettings}><SettingsIcon data-icon="inline-start" />{t("app.settings")}</DropdownMenuItem></DropdownMenuGroup><DropdownMenuSeparator />
      <DropdownMenuGroup><DropdownMenuItem onSelect={onLogout}><LogOutIcon data-icon="inline-start" />{t("app.sign_out")}</DropdownMenuItem></DropdownMenuGroup>
    </DropdownMenuContent>
  </DropdownMenu>;
}

export function NavigationDrawer({access, open, onOpenChange, virtualDisplaysSupported, onVirtualDisplays, browserSupported, onBrowser, onNewSession, onSessions, onApps, onSettings, onLogout}: {
  access: Access; open: boolean; onOpenChange(open: boolean): void; onNewSession(): void; onSessions(): void;
  browserSupported?: boolean; onBrowser?(): void; virtualDisplaysSupported?: boolean; onVirtualDisplays?(): void; onApps(): void; onSettings(): void; onLogout(): void;
}) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="dl-sidebar-drawer" showCloseButton={false} aria-describedby={undefined}>
    <div className="dl-sidebar-heading"><DialogTitle>Daat Locus</DialogTitle><Button variant="ghost" size="icon-sm" aria-label={t("navigation.close_sidebar")} onClick={() => onOpenChange(false)}><PanelLeftIcon /></Button></div>
    <nav className="dl-drawer-navigation" aria-label={t("navigation.app_navigation")}><Button variant="ghost" className="dl-sidebar-row" onClick={onNewSession}><SquarePenIcon data-icon="inline-start" />{t("navigation.new_session")}</Button><Button variant="ghost" className="dl-sidebar-row" onClick={onSessions}><MessagesSquareIcon data-icon="inline-start" />{t("sessions.title")}</Button><Button variant="ghost" className="dl-sidebar-row" onClick={onApps}><PuzzleIcon data-icon="inline-start" />{t("apps.title")}</Button>{browserSupported && <Button variant="ghost" className="dl-sidebar-row" onClick={onBrowser}><GlobeIcon data-icon="inline-start" />{t("browser_use.title")}</Button>}{virtualDisplaysSupported && <Button variant="ghost" className="dl-sidebar-row" onClick={onVirtualDisplays}><MonitorIcon data-icon="inline-start" />{t("virtual_displays.title")}</Button>}<Button variant="ghost" className="dl-sidebar-row" onClick={onSettings}><SettingsIcon data-icon="inline-start" />{t("app.settings")}</Button></nav>
    <div className="dl-sidebar-account"><AccountMenu access={access} onSettings={onSettings} onLogout={onLogout} /><span>{access.account?.name || t("app.account")}</span></div>
  </DialogContent></Dialog>;
}