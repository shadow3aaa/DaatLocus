import { useState, useCallback, type ReactNode } from "react";
import { ChevronDownIcon } from "lucide-react";
import { cn } from "@/lib/utils";

type CollapsibleContextValue = {
  open: boolean;
  toggle: () => void;
};

function useCollapsibleState(
  defaultOpen: boolean,
  controlledOpen?: boolean,
  onOpenChange?: (open: boolean) => void,
): CollapsibleContextValue {
  const [internalOpen, setInternalOpen] = useState(defaultOpen);
  const open = controlledOpen ?? internalOpen;

  const toggle = useCallback(() => {
    const next = !open;
    if (onOpenChange) {
      onOpenChange(next);
    } else {
      setInternalOpen(next);
    }
  }, [open, onOpenChange]);

  return { open, toggle };
}

export function CollapsibleTrigger({
  children,
  className,
  open,
  onToggle,
}: {
  children: ReactNode;
  className?: string;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      className={cn(
        "flex w-full items-center gap-1.5 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors cursor-pointer select-none",
        className,
      )}
    >
      <ChevronDownIcon
        className={cn(
          "size-3.5 shrink-0 transition-transform duration-200",
          open && "rotate-180",
        )}
      />
      {children}
    </button>
  );
}

export function CollapsibleContent({
  children,
  open,
  className,
}: {
  children: ReactNode;
  open: boolean;
  className?: string;
}) {
  if (!open) return null;
  return (
    <div
      className={cn(
        "mt-2 overflow-hidden animate-in slide-in-from-top-1 fade-in-0 duration-200",
        className,
      )}
    >
      {children}
    </div>
  );
}

export { useCollapsibleState };
