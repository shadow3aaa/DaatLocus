import { DotLottieReact } from "@lottiefiles/dotlottie-react";
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

const placeholderAnimation = `${import.meta.env.BASE_URL}lottie/agent-placeholder.json`;

const animationByStatus: Record<AgentAnimationStatus, string> = {
  idle: placeholderAnimation,
  thinking: placeholderAnimation,
  running: placeholderAnimation,
  tooling: placeholderAnimation,
  waiting: placeholderAnimation,
  error: placeholderAnimation,
};

const animationToneByStatus: Record<AgentAnimationStatus, string> = {
  idle: "opacity-90",
  thinking: "scale-[1.02]",
  running: "scale-105",
  tooling: "scale-105",
  waiting: "opacity-80 grayscale",
  error: "scale-105 saturate-150",
};

const animationSpeedByStatus: Record<AgentAnimationStatus, number> = {
  idle: 0.75,
  thinking: 0.95,
  running: 1.3,
  tooling: 1.45,
  waiting: 0.55,
  error: 1.1,
};

export function AgentStatusAnimation({
  status,
  className,
}: AgentStatusAnimationProps) {
  const prefersReducedMotion = usePrefersReducedMotion();

  return (
    <div
      className={cn(
        "relative flex aspect-square w-full max-w-60 items-center justify-center rounded-[2rem] bg-muted/50 p-5 ring-1 ring-border",
        "after:absolute after:inset-6 after:rounded-full after:bg-primary/5 after:blur-xl after:content-['']",
        className,
      )}
    >
      <DotLottieReact
        aria-label="Agent status animation"
        autoplay={!prefersReducedMotion}
        className={cn(
          "relative z-10 h-full w-full transition duration-500",
          "motion-safe:animate-in motion-safe:fade-in motion-safe:zoom-in-95",
          animationToneByStatus[status],
        )}
        loop={!prefersReducedMotion}
        speed={animationSpeedByStatus[status]}
        src={animationByStatus[status]}
      />
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

