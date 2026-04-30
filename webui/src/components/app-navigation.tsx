import {
  NavigationMenu,
  NavigationMenuItem,
  NavigationMenuLink,
  NavigationMenuList,
  navigationMenuTriggerStyle,
} from "@/components/ui/navigation-menu";
import { cn } from "@/lib/utils";

type NavigationItem = {
  label: string;
  href: string;
  active?: boolean;
  disabled?: boolean;
};

const navigationItems: NavigationItem[] = [
  { label: "Login", href: "#login" },
  { label: "Status", href: "#status" },
  { label: "Tasks", href: "#tasks", disabled: true },
  { label: "Logs", href: "#logs", disabled: true },
];

export function AppNavigation({
  isAuthenticated,
}: {
  isAuthenticated: boolean;
}) {
  return (
    <header className="sticky top-0 z-10 border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/80">
      <div className="flex h-16 w-full items-center justify-between gap-4 px-4">
        <span className="shrink-0 text-base font-semibold tracking-tight">
          Daat Locus
        </span>

        <NavigationMenu className="hidden md:flex">
          <NavigationMenuList>
            {navigationItems.map((item) => (
              <NavigationMenuItem key={item.label}>
                <NavigationMenuLink
                  href={
                    item.disabled || (item.label === "Login" && isAuthenticated)
                      ? undefined
                      : item.href
                  }
                  active={
                    item.active ||
                    (item.label === "Login" && !isAuthenticated) ||
                    (item.label === "Status" && isAuthenticated)
                  }
                  aria-disabled={
                    item.disabled || (item.label === "Login" && isAuthenticated)
                  }
                  className={cn(
                    navigationMenuTriggerStyle(),
                    (item.active ||
                      (item.label === "Login" && !isAuthenticated) ||
                      (item.label === "Status" && isAuthenticated)) &&
                      "bg-accent text-accent-foreground",
                    (item.disabled ||
                      (item.label === "Login" && isAuthenticated)) &&
                      "pointer-events-none opacity-50",
                  )}
                >
                  {item.label}
                </NavigationMenuLink>
              </NavigationMenuItem>
            ))}
          </NavigationMenuList>
        </NavigationMenu>

      </div>
    </header>
  );
}
