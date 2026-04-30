import { useEffect, useState } from "react";
import { MenuIcon } from "lucide-react";

import { Button, buttonVariants } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import {
  Sheet,
  SheetClose,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

type NavigationItem = {
  label: string;
  href: string;
  description: string;
  disabled?: boolean;
};

const navigationItems: NavigationItem[] = [
  {
    label: "Agent",
    href: "#agent",
    description: "Agent presence and interaction surface",
  },
  {
    label: "Status",
    href: "#status",
    description: "Runtime metrics and optimization state",
  },
  {
    label: "Settings",
    href: "#settings",
    description: "Daemon and WebUI preferences",
    disabled: true,
  },
  {
    label: "Logs",
    href: "#logs",
    description: "Recent runtime activity",
    disabled: true,
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
        className="w-[min(20rem,calc(100vw-2rem))] border-border/60 bg-background/95 p-0 backdrop-blur supports-[backdrop-filter]:bg-background/85"
      >
        <div className="flex h-full flex-col">
          <SheetHeader className="px-6 pt-6 pb-0 text-left">
            <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <span
                aria-hidden="true"
                className={cn(
                  "size-1.5 rounded-full",
                  isAuthenticated ? "bg-emerald-500" : "bg-muted-foreground/50",
                )}
              />
              {isAuthenticated ? "Daemon connected" : "Token required"}
            </div>
            <SheetTitle className="text-xl tracking-tight">
              Daat Locus
            </SheetTitle>
            <SheetDescription>
              Local agent runtime navigation.
            </SheetDescription>
          </SheetHeader>

          <Separator className="my-5" />

          <nav className="grid gap-1 px-3" aria-label="Primary navigation">
            {navigationItems.map((item) => {
              const isRuntimePage =
                item.href === "#agent" || item.href === "#status";
              const isActive = activeHash === item.href && isAuthenticated;
              const isDisabled =
                item.disabled || (isRuntimePage && !isAuthenticated);
              const itemClassName = cn(
                buttonVariants({ variant: "ghost" }),
                "h-auto w-full justify-start rounded-xl px-3 py-3 text-left",
                isActive && "bg-muted text-foreground",
                isDisabled && "pointer-events-none opacity-45",
              );
              const content = (
                <>
                  <span
                    aria-hidden="true"
                    className={cn(
                      "mt-1 size-2 rounded-full border border-border",
                      isActive && "border-foreground bg-foreground",
                    )}
                  />
                  <span className="grid gap-0.5">
                    <span className="text-sm font-medium leading-none">
                      {item.label}
                    </span>
                    <span className="text-xs font-normal text-muted-foreground">
                      {item.description}
                    </span>
                  </span>
                </>
              );

              return isDisabled ? (
                <span
                  key={item.label}
                  aria-disabled="true"
                  className={itemClassName}
                >
                  {content}
                </span>
              ) : (
                <SheetClose key={item.label} asChild>
                  <a
                    href={item.href}
                    aria-current={isActive ? "page" : undefined}
                    className={itemClassName}
                  >
                    {content}
                  </a>
                </SheetClose>
              );
            })}
          </nav>

          <div className="mt-auto px-6 py-5 text-xs leading-5 text-muted-foreground">
            A quiet shell for checking where the runtime is and what it is
            doing.
          </div>
        </div>
      </SheetContent>
    </Sheet>
  );
}
