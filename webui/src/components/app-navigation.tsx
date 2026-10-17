import { HomeIcon, LogOutIcon, PanelLeftIcon, PuzzleIcon, SettingsIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import type { Access } from "@/lib/actor-api";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function NavigationRail({access, settings, apps, onApps, onHome, onSettings, onLogout}: {
  access: Access; settings: boolean; apps: boolean; onApps(): void; onHome(): void; onSettings(): void; onLogout(): void;
}) {
  return <nav className="dl-navigation-rail" aria-label={t("navigation.app_navigation")}>
    <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" className="dl-rail-home" aria-label={t("navigation.home")} aria-current={!settings && !apps ? "page" : undefined} onClick={onHome}><HomeIcon /></Button></TooltipTrigger><TooltipContent side="right">{t("navigation.home")}</TooltipContent></Tooltip>
    <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" aria-label={t("apps.title")} aria-current={apps && !settings ? "page" : undefined} onClick={onApps}><PuzzleIcon /></Button></TooltipTrigger><TooltipContent side="right">{t("apps.title")}</TooltipContent></Tooltip>
    <div className="dl-rail-account"><AccountMenu access={access} onSettings={onSettings} onLogout={onLogout} /></div>
  </nav>;
}

export function AccountMenu({access, onSettings, onLogout}: {access: Access; onSettings(): void; onLogout(): void}) {
  const name = access.account?.name || t("app.account");
  return <DropdownMenu><DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="dl-profile-button" aria-label={t("navigation.profile_menu")}><Avatar><AvatarFallback>{Array.from(name).slice(0, 2).join("")}</AvatarFallback></Avatar></Button></DropdownMenuTrigger>
    <DropdownMenuContent align="start" side="right" sideOffset={12} className="dl-account-menu">
      <DropdownMenuLabel className="dl-account-menu-header"><Avatar className="size-9"><AvatarFallback>{Array.from(name).slice(0, 2).join("")}</AvatarFallback></Avatar><span><strong>{name}</strong><small>{t("app.signed_in")}</small></span></DropdownMenuLabel><DropdownMenuSeparator />
      <DropdownMenuGroup><DropdownMenuItem onSelect={onSettings}><SettingsIcon data-icon="inline-start" />{t("app.settings")}</DropdownMenuItem></DropdownMenuGroup><DropdownMenuSeparator />
      <DropdownMenuGroup><DropdownMenuItem onSelect={onLogout}><LogOutIcon data-icon="inline-start" />{t("app.sign_out")}</DropdownMenuItem></DropdownMenuGroup>
    </DropdownMenuContent>
  </DropdownMenu>;
}

export function NavigationDrawer({access, open, onOpenChange, onApps, onHome, onSettings, onLogout}: {
  access: Access; open: boolean; onOpenChange(open: boolean): void; onHome(): void;
  onApps(): void; onSettings(): void; onLogout(): void;
}) {
  return <Dialog open={open} onOpenChange={onOpenChange}><DialogContent className="dl-sidebar-drawer" showCloseButton={false} aria-describedby={undefined}>
    <div className="dl-sidebar-heading"><DialogTitle>Daat Locus</DialogTitle><Button variant="ghost" size="icon-sm" aria-label={t("navigation.close_sidebar")} onClick={() => onOpenChange(false)}><PanelLeftIcon /></Button></div>
    <nav className="dl-drawer-navigation" aria-label={t("navigation.app_navigation")}><Button variant="ghost" className="dl-sidebar-row" onClick={onHome}><HomeIcon data-icon="inline-start" />{t("navigation.home")}</Button><Button variant="ghost" className="dl-sidebar-row" onClick={onApps}><PuzzleIcon data-icon="inline-start" />{t("apps.title")}</Button><Button variant="ghost" className="dl-sidebar-row" onClick={onSettings}><SettingsIcon data-icon="inline-start" />{t("app.settings")}</Button></nav>
    <div className="dl-sidebar-account"><AccountMenu access={access} onSettings={onSettings} onLogout={onLogout} /><span>{access.account?.name || t("app.account")}</span></div>
  </DialogContent></Dialog>;
}
