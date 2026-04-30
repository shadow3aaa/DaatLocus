import { useEffect, useState } from "react";

import { cn } from "@/lib/utils";

export type AgentAnimationStatus =
  | "idle"
  | "thinking"
  | "running"
  | "tooling"
  | "waiting"
  | "error";

type AgentStatusAnimationProps = {
  status: AgentAnimationStatus;
  className?: string;
};

const idleExpression = [
  ".................",
  "..........##.....",
  "............##...",
  "..####.......##..",
  "..............#..",
  "..####.......##..",
  "............##...",
  "..........##.....",
  ".................",
] as const;

const expressionByStatus: Record<AgentAnimationStatus, readonly string[]> = {
  idle: idleExpression,
  thinking: idleExpression,
  running: idleExpression,
  tooling: idleExpression,
  waiting: idleExpression,
  error: idleExpression,
};

const expressionToneByStatus: Record<AgentAnimationStatus, string> = {
  idle: "text-foreground",
  thinking: "text-primary",
  running: "text-primary",
  tooling: "text-primary",
  waiting: "text-muted-foreground",
  error: "text-destructive",
};

const expressionLabelByStatus: Record<AgentAnimationStatus, string> = {
  idle: "Idle dot-matrix expression",
  thinking: "Thinking dot-matrix expression",
  running: "Running dot-matrix expression",
  tooling: "Tooling dot-matrix expression",
  waiting: "Waiting dot-matrix expression",
  error: "Error dot-matrix expression",
};

const matrixCellSize = 12;
const activeDotRadius = 3.7;
const inactiveDotRadius = 2.2;
const matrixColumnCount = idleExpression[0].length;
const matrixRowCount = idleExpression.length;

export function AgentStatusAnimation({
  status,
  className,
}: AgentStatusAnimationProps) {
  const prefersReducedMotion = usePrefersReducedMotion();
  const expression = expressionByStatus[status];
  const shouldBreathe = status === "idle" && !prefersReducedMotion;

  return (
    <div
      data-status={status}
      className={cn(
        "relative flex aspect-[17/9] w-64 items-center justify-center overflow-hidden",
        "rounded-[2rem] border border-border/50 bg-card/70 p-5 shadow-sm",
        "transition-colors duration-500",
        "after:absolute after:inset-x-8 after:bottom-2 after:h-10 after:rounded-full after:bg-primary/10 after:blur-2xl after:content-['']",
        expressionToneByStatus[status],
        shouldBreathe && "motion-safe:animate-pulse",
        className,
      )}
    >
      <svg
        aria-label={expressionLabelByStatus[status]}
        className="relative z-10 h-full w-full"
        role="img"
        viewBox={`0 0 ${matrixColumnCount * matrixCellSize} ${
          matrixRowCount * matrixCellSize
        }`}
      >
        {expression.map((row, rowIndex) =>
          Array.from(row).map((cell, columnIndex) => {
            const isActive = cell === "#";

            return (
              <circle
                key={`${rowIndex}-${columnIndex}`}
                cx={columnIndex * matrixCellSize + matrixCellSize / 2}
                cy={rowIndex * matrixCellSize + matrixCellSize / 2}
                fill="currentColor"
                opacity={isActive ? 1 : 0.1}
                r={isActive ? activeDotRadius : inactiveDotRadius}
              />
            );
          }),
        )}
      </svg>
    </div>
  );
}

function usePrefersReducedMotion() {
  const [prefersReducedMotion, setPrefersReducedMotion] = useState(false);

  useEffect(() => {
    const mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    const handleChange = () => setPrefersReducedMotion(mediaQuery.matches);

    handleChange();
    mediaQuery.addEventListener("change", handleChange);

    return () => mediaQuery.removeEventListener("change", handleChange);
  }, []);

  return prefersReducedMotion;
}

