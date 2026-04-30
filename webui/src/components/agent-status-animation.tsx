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

const faceViewBoxWidth = 132;
const faceViewBoxHeight = 180;

const expressionLabelByStatus: Record<AgentAnimationStatus, string> = {
  idle: "Idle smooth expression",
  thinking: "Working smooth expression",
  running: "Working smooth expression",
  tooling: "Working smooth expression",
  waiting: "Waiting smooth expression",
  error: "Error smooth expression",
};

const expressionMotionByStatus: Record<AgentAnimationStatus, string> = {
  idle: "opacity-100",
  thinking: "scale-[1.015]",
  running: "scale-[1.025]",
  tooling: "scale-[1.025]",
  waiting: "opacity-80",
  error: "scale-[1.025]",
};

const idleMouthPath =
  "M 22 106 C 32 126 46 139 66 139 C 86 139 100 126 110 106";
const workingMouthPath = "M 36 121 L 96 121";
const workingEyeDuration = "2.2s";
const workingEyeKeyTimes = "0;0.12;0.38;0.5;0.62;0.88;1";
const leftWorkingEyeFrames = {
  height: "41;9;9;41;41;41;41",
  rx: "8.5;4.5;4.5;8.5;8.5;8.5;8.5",
  width: "17;26;26;17;17;17;17",
  x: "37;32.5;32.5;37;37;37;37",
  y: "31;47;47;31;31;31;31",
} as const;
const rightWorkingEyeFrames = {
  height: "41;41;41;41;9;9;41",
  rx: "8.5;8.5;8.5;8.5;4.5;4.5;8.5",
  width: "17;17;17;17;26;26;17",
  x: "78;78;78;78;73.5;73.5;78",
  y: "31;31;31;31;47;47;31",
} as const;

function isWorkingStatus(status: AgentAnimationStatus) {
  return status === "thinking" || status === "running" || status === "tooling";
}

export function AgentStatusAnimation({
  status,
  className,
}: AgentStatusAnimationProps) {
  const prefersReducedMotion = usePrefersReducedMotion();
  const isWorking = isWorkingStatus(status);
  const shouldBreathe = status === "idle" && !prefersReducedMotion;
  const shouldAnimateWorking = isWorking && !prefersReducedMotion;

  return (
    <div
      data-animation-kind={isWorking ? "working" : status}
      data-status={status}
      className={cn(
        "relative flex aspect-[11/15] w-64 items-center justify-center overflow-hidden",
        "rounded-[2rem] border border-border/50 bg-card/70 p-5 shadow-sm",
        "transition-colors duration-500",
        "after:absolute after:inset-x-8 after:bottom-2 after:h-10 after:rounded-full after:bg-primary/10 after:blur-2xl after:content-['']",
        isWorking && "border-primary/25 bg-primary/[0.03] shadow-primary/10",
        shouldBreathe && "motion-safe:animate-pulse",
        className,
      )}
    >
      <svg
        aria-label={expressionLabelByStatus[status]}
        className={cn(
          "relative z-10 h-full w-full origin-center transition duration-500",
          !prefersReducedMotion && expressionMotionByStatus[status],
        )}
        role="img"
        viewBox={`0 0 ${faceViewBoxWidth} ${faceViewBoxHeight}`}
      >
        <g fill="black">
          <rect height="41" rx="8.5" width="17" x="37" y="31">
            {shouldAnimateWorking && (
              <>
                <animate
                  attributeName="height"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={leftWorkingEyeFrames.height}
                />
                <animate
                  attributeName="rx"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={leftWorkingEyeFrames.rx}
                />
                <animate
                  attributeName="width"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={leftWorkingEyeFrames.width}
                />
                <animate
                  attributeName="x"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={leftWorkingEyeFrames.x}
                />
                <animate
                  attributeName="y"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={leftWorkingEyeFrames.y}
                />
              </>
            )}
          </rect>
          <rect height="41" rx="8.5" width="17" x="78" y="31">
            {shouldAnimateWorking && (
              <>
                <animate
                  attributeName="height"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={rightWorkingEyeFrames.height}
                />
                <animate
                  attributeName="rx"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={rightWorkingEyeFrames.rx}
                />
                <animate
                  attributeName="width"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={rightWorkingEyeFrames.width}
                />
                <animate
                  attributeName="x"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={rightWorkingEyeFrames.x}
                />
                <animate
                  attributeName="y"
                  dur={workingEyeDuration}
                  keyTimes={workingEyeKeyTimes}
                  repeatCount="indefinite"
                  values={rightWorkingEyeFrames.y}
                />
              </>
            )}
          </rect>
        </g>
        <path
          d={isWorking ? workingMouthPath : idleMouthPath}
          fill="none"
          stroke="black"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="14"
        />
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

