import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";

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

const idleMouthPoints = [
  22, 106, 32, 126, 46, 139, 66, 139, 86, 139, 100, 126, 110, 106,
] as const;
const workingMouthPoints = [
  36, 121, 48, 121, 54, 121, 66, 121, 78, 121, 84, 121, 96, 121,
] as const;
const idleMouthPath = getMouthPath(0);
const workingMouthPath = getMouthPath(1);
const expressionTransitionDurationMs = 620;
const expressionTransitionDuration = `${expressionTransitionDurationMs}ms`;
const expressionTransitionKeyTimes = "0;0.52;1";
const expressionTransitionKeySplines = "0.2 0 0 1;0.2 0 0 1";
const mouthPathByVisualKind = {
  idle: idleMouthPath,
  working: workingMouthPath,
} as const;
const eyeTransitionFrames = {
  left: {
    height: "41;17;41",
    rx: "8.5;5;8.5",
    width: "17;17;17",
    x: "37;37;37",
    y: "31;43;31",
  },
  right: {
    height: "41;17;41",
    rx: "8.5;5;8.5",
    width: "17;17;17",
    x: "78;78;78",
    y: "31;43;31",
  },
} as const;
const workingEyeDuration = "2.2s";
const workingEyeKeyTimes = "0;0.12;0.38;0.5;0.62;0.88;1";
const leftWorkingEyeFrames = {
  height: "41;17;17;41;41;41;41",
  rx: "8.5;5;5;8.5;8.5;8.5;8.5",
  width: "17;17;17;17;17;17;17",
  x: "37;37;37;37;37;37;37",
  y: "31;43;43;31;31;31;31",
} as const;
const rightWorkingEyeFrames = {
  height: "41;41;41;41;17;17;41",
  rx: "8.5;8.5;8.5;8.5;5;5;8.5",
  width: "17;17;17;17;17;17;17",
  x: "78;78;78;78;78;78;78",
  y: "31;31;31;31;43;43;31",
} as const;

type ExpressionVisualKind = keyof typeof mouthPathByVisualKind;

type ExpressionTransition = {
  from: ExpressionVisualKind;
  id: number;
  progress: number;
  to: ExpressionVisualKind;
};

function isWorkingStatus(status: AgentAnimationStatus) {
  return status === "thinking" || status === "running" || status === "tooling";
}

function lerp(from: number, to: number, progress: number) {
  return from + (to - from) * progress;
}

function easeOutCubic(progress: number) {
  return 1 - Math.pow(1 - progress, 3);
}

function formatSvgNumber(value: number) {
  return Number(value.toFixed(3)).toString();
}

function getMouthPath(progress: number) {
  const points = idleMouthPoints.map((idlePoint, index) =>
    formatSvgNumber(lerp(idlePoint, workingMouthPoints[index], progress)),
  );

  return `M ${points[0]} ${points[1]} C ${points[2]} ${points[3]} ${points[4]} ${points[5]} ${points[6]} ${points[7]} C ${points[8]} ${points[9]} ${points[10]} ${points[11]} ${points[12]} ${points[13]}`;
}

export const AgentStatusAnimation = memo(function AgentStatusAnimation({
  status,
  className,
}: AgentStatusAnimationProps) {
  const prefersReducedMotion = usePrefersReducedMotion();
  const isWorking = isWorkingStatus(status);
  const visualKind = isWorking ? "working" : "idle";
  const expressionTransition = useExpressionTransition(
    visualKind,
    prefersReducedMotion,
  );
  const shouldAnimateWorking =
    isWorking && !prefersReducedMotion && expressionTransition === null;
  const mouthPath =
    expressionTransition === null
      ? mouthPathByVisualKind[visualKind]
      : getMouthPath(
          lerp(
            expressionTransition.from === "working" ? 1 : 0,
            expressionTransition.to === "working" ? 1 : 0,
            expressionTransition.progress,
          ),
        );

  return (
    <div
      data-animation-kind={isWorking ? "working" : status}
      data-status={status}
      className={cn(
        "relative flex aspect-[11/15] w-64 items-center justify-center p-5",
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
            {expressionTransition && (
              <>
                <animate
                  key={`left-eye-height-transition-${expressionTransition.id}`}
                  attributeName="height"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.left.height}
                />
                <animate
                  key={`left-eye-rx-transition-${expressionTransition.id}`}
                  attributeName="rx"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.left.rx}
                />
                <animate
                  key={`left-eye-width-transition-${expressionTransition.id}`}
                  attributeName="width"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.left.width}
                />
                <animate
                  key={`left-eye-x-transition-${expressionTransition.id}`}
                  attributeName="x"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.left.x}
                />
                <animate
                  key={`left-eye-y-transition-${expressionTransition.id}`}
                  attributeName="y"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.left.y}
                />
              </>
            )}
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
            {expressionTransition && (
              <>
                <animate
                  key={`right-eye-height-transition-${expressionTransition.id}`}
                  attributeName="height"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.right.height}
                />
                <animate
                  key={`right-eye-rx-transition-${expressionTransition.id}`}
                  attributeName="rx"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.right.rx}
                />
                <animate
                  key={`right-eye-width-transition-${expressionTransition.id}`}
                  attributeName="width"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.right.width}
                />
                <animate
                  key={`right-eye-x-transition-${expressionTransition.id}`}
                  attributeName="x"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.right.x}
                />
                <animate
                  key={`right-eye-y-transition-${expressionTransition.id}`}
                  attributeName="y"
                  begin="0s"
                  calcMode="spline"
                  dur={expressionTransitionDuration}
                  fill="freeze"
                  keySplines={expressionTransitionKeySplines}
                  keyTimes={expressionTransitionKeyTimes}
                  values={eyeTransitionFrames.right.y}
                />
              </>
            )}
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
          d={mouthPath}
          fill="none"
          stroke="black"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="14"
        />
      </svg>
    </div>
  );
});

function useExpressionTransition(
  visualKind: ExpressionVisualKind,
  prefersReducedMotion: boolean,
) {
  const [transition, setTransition] = useState<ExpressionTransition | null>(
    null,
  );
  const previousVisualKindRef = useRef<ExpressionVisualKind>(visualKind);
  const transitionIdRef = useRef(0);

  useLayoutEffect(() => {
    if (prefersReducedMotion) {
      previousVisualKindRef.current = visualKind;
      setTransition(null);
      return;
    }

    const previousVisualKind = previousVisualKindRef.current;

    if (previousVisualKind === visualKind) {
      return;
    }

    previousVisualKindRef.current = visualKind;

    const nextTransition = {
      from: previousVisualKind,
      id: (transitionIdRef.current += 1),
      progress: 0,
      to: visualKind,
    };

    setTransition(nextTransition);

    const startTime = performance.now();
    let animationFrameId = 0;

    const animateTransition = (currentTime: number) => {
      const elapsed = currentTime - startTime;
      const rawProgress = Math.min(elapsed / expressionTransitionDurationMs, 1);
      const progress = easeOutCubic(rawProgress);

      setTransition((currentTransition) =>
        currentTransition?.id === nextTransition.id
          ? { ...currentTransition, progress }
          : currentTransition,
      );

      if (rawProgress < 1) {
        animationFrameId = requestAnimationFrame(animateTransition);
        return;
      }

      setTransition((currentTransition) =>
        currentTransition?.id === nextTransition.id ? null : currentTransition,
      );
    };

    animationFrameId = requestAnimationFrame(animateTransition);

    return () => cancelAnimationFrame(animationFrameId);
  }, [prefersReducedMotion, visualKind]);

  return transition;
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

