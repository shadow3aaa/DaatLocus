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
  disabled?: boolean;
};

const navigationItems: NavigationItem[] = [
  { label: "Chat", href: "#chat", disabled: true },
  { label: "Status", href: "#status" },
  { label: "Settings", href: "#settings", disabled: true },
  { label: "Log", href: "#log", disabled: true },
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
            {navigationItems.map((item) => {
              const isStatus = item.label === "Status";
              const isActive = isStatus && isAuthenticated;
              const isDisabled = item.disabled || (isStatus && !isAuthenticated);

              return (
                <NavigationMenuItem key={item.label}>
                  <NavigationMenuLink
                    href={isDisabled ? undefined : item.href}
                    active={isActive}
                    aria-disabled={isDisabled}
                    className={cn(
                      navigationMenuTriggerStyle(),
                      isActive && "bg-accent text-accent-foreground",
                      isDisabled && "pointer-events-none opacity-50",
                    )}
                  >
                    {item.label}
                  </NavigationMenuLink>
                </NavigationMenuItem>
              );
            })}
          </NavigationMenuList>
        </NavigationMenu>

      </div>
    </header>
  );
}
