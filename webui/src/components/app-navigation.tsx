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
  { label: "Login", href: "#login", active: true },
  { label: "Status", href: "#status", disabled: true },
  { label: "Tasks", href: "#tasks", disabled: true },
  { label: "Logs", href: "#logs", disabled: true },
];

export function AppNavigation() {
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

      </div>
    </header>
  );
}
