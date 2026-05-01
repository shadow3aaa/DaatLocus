import { useEffect, useState } from "react";
import { MenuIcon } from "lucide-react";

import { Button, buttonVariants } from "@/components/ui/button";
import {
  Sheet,
  SheetClose,
  SheetContent,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

type NavigationItem = {
  label: string;
  href: string;
  disabled?: boolean;
};

const navigationItems: NavigationItem[] = [
  {
    label: "Agent",
    href: "#agent",
  },
  {
    label: "Status",
    href: "#status",
  },
  {
    label: "Settings",
    href: "#settings",
    disabled: true,
  },
  {
    label: "Logs",
    href: "#logs",
  },
];

export function AppNavigation({
  isAuthenticated,
}: {
  isAuthenticated: boolean;
}) {
  const [activeHash, setActiveHash] = useState(() =>
    typeof window === "undefined" ? "#agent" : window.location.hash || "#agent",
  );

  useEffect(() => {
    function updateActiveHash() {
      setActiveHash(window.location.hash || "#agent");
    }

    updateActiveHash();
    window.addEventListener("hashchange", updateActiveHash);

    return () => window.removeEventListener("hashchange", updateActiveHash);
  }, []);

  return (
    <Sheet>
      <SheetTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="icon-lg"
          aria-label="Open navigation"
          className="fixed top-4 left-4 z-50 rounded-full border-border/60 bg-background/70 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/55 md:top-6 md:left-6"
        >
          <MenuIcon className="size-4" />
        </Button>
      </SheetTrigger>

      <SheetContent
        side="left"
        aria-describedby={undefined}
        className="w-56 border-border/60 bg-background/95 p-3 pt-14 backdrop-blur supports-[backdrop-filter]:bg-background/85"
      >
        <SheetTitle className="sr-only">Navigation</SheetTitle>

        <nav className="grid gap-1" aria-label="Primary navigation">
          {navigationItems.map((item) => {
            const isRuntimePage =
              item.href === "#agent" ||
              item.href === "#status" ||
              item.href === "#logs";
            const isActive = activeHash === item.href && isAuthenticated;
            const isDisabled =
              item.disabled || (isRuntimePage && !isAuthenticated);
            const itemClassName = cn(
              buttonVariants({ variant: "ghost" }),
              "w-full justify-start rounded-lg px-3",
              isActive && "bg-muted text-foreground",
              isDisabled && "pointer-events-none opacity-45",
            );

            return isDisabled ? (
              <span
                key={item.label}
                aria-disabled="true"
                className={itemClassName}
              >
                {item.label}
              </span>
            ) : (
              <SheetClose key={item.label} asChild>
                <a
                  href={item.href}
                  aria-current={isActive ? "page" : undefined}
                  className={itemClassName}
                >
                  {item.label}
                </a>
              </SheetClose>
            );
          })}
        </nav>
      </SheetContent>
    </Sheet>
  );
}
