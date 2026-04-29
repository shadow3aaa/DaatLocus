import { Activity, CircleDot, KeyRound, ShieldCheck } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import {
  NavigationMenu,
  NavigationMenuItem,
  NavigationMenuLink,
  NavigationMenuList,
  navigationMenuTriggerStyle,
} from "@/components/ui/navigation-menu";
import { cn } from "@/lib/utils";

export type AuthStatus = "anonymous" | "saved" | "authenticated";

type NavigationItem = {
  label: string;
  href: string;
  active?: boolean;
  disabled?: boolean;
};

const navigationItems: NavigationItem[] = [
  { label: "Login", href: "#login", active: true },
  { label: "Status", href: "#status", disabled: true },
  { label: "Tasks", href: "#tasks", disabled: true },
  { label: "Logs", href: "#logs", disabled: true },
];

function authStatusLabel(authStatus: AuthStatus) {
  switch (authStatus) {
    case "authenticated":
      return "Token verified";
    case "saved":
      return "Saved token";
    case "anonymous":
      return "Signed out";
  }
}

function authStatusVariant(authStatus: AuthStatus) {
  return authStatus === "authenticated" ? "default" : "secondary";
}

function AuthStatusIcon({ authStatus }: { authStatus: AuthStatus }) {
  if (authStatus === "authenticated") {
    return <ShieldCheck className="size-3.5" />;
  }

  if (authStatus === "saved") {
    return <KeyRound className="size-3.5" />;
  }

  return <CircleDot className="size-3.5" />;
}

export function AppNavigation({ authStatus }: { authStatus: AuthStatus }) {
  return (
    <header className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80">
      <div className="mx-auto flex h-16 w-full max-w-6xl items-center justify-between gap-4 px-6">
        <Badge variant="outline" className="h-9 gap-2 px-3 text-sm">
          <Activity className="size-4" />
          Daat Locus
        </Badge>

        <NavigationMenu className="hidden md:flex">
          <NavigationMenuList>
            {navigationItems.map((item) => (
              <NavigationMenuItem key={item.label}>
                <NavigationMenuLink
                  href={item.disabled ? undefined : item.href}
                  active={item.active}
                  aria-disabled={item.disabled}
                  className={cn(
                    navigationMenuTriggerStyle(),
                    item.active && "bg-accent text-accent-foreground",
                    item.disabled && "pointer-events-none opacity-50",
                  )}
                >
                  {item.label}
                </NavigationMenuLink>
              </NavigationMenuItem>
            ))}
          </NavigationMenuList>
        </NavigationMenu>

        <Badge variant={authStatusVariant(authStatus)} className="h-9 gap-1.5 px-3">
          <AuthStatusIcon authStatus={authStatus} />
          {authStatusLabel(authStatus)}
        </Badge>
      </div>
    </header>
  );
}
