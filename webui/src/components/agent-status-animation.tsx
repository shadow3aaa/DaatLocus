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

type MatrixDot = {
  columnIndex: number;
  rowIndex: number;
};

const expressionLabelByStatus: Record<AgentAnimationStatus, string> = {
  idle: "Idle dot-matrix expression",
  thinking: "Thinking dot-matrix expression",
  running: "Running dot-matrix expression",
  tooling: "Tooling dot-matrix expression",
  waiting: "Waiting dot-matrix expression",
  error: "Error dot-matrix expression",
};

const matrixCellSize = 6;
const activeBlockSize = 4.7;
const inactiveBlockSize = 2.1;
const matrixColumnCount = 22;
const matrixRowCount = 30;
const faceViewBoxWidth = matrixColumnCount * matrixCellSize;
const faceViewBoxHeight = matrixRowCount * matrixCellSize;
const inactiveDots = Array.from(
  { length: matrixColumnCount * matrixRowCount },
  (_, index) => ({
    columnIndex: index % matrixColumnCount,
    rowIndex: Math.floor(index / matrixColumnCount),
  }),
);

function createRectangleDots({
  columnStart,
  rowStart,
  columnCount,
  rowCount,
}: {
  columnStart: number;
  rowStart: number;
  columnCount: number;
  rowCount: number;
}): MatrixDot[] {
  return Array.from({ length: columnCount * rowCount }, (_, index) => ({
    columnIndex: columnStart + (index % columnCount),
    rowIndex: rowStart + Math.floor(index / columnCount),
  }));
}

function createDotsFromRows(
  rows: {
    rowIndex: number;
    columnIndexes: number[];
  }[],
): MatrixDot[] {
  return rows.flatMap(({ rowIndex, columnIndexes }) =>
    columnIndexes.map((columnIndex) => ({
      columnIndex,
      rowIndex,
    })),
  );
}

const eyeDots: MatrixDot[] = [
  ...createRectangleDots({
    columnStart: 6,
    rowStart: 5,
    columnCount: 3,
    rowCount: 7,
  }),
  ...createRectangleDots({
    columnStart: 13,
    rowStart: 5,
    columnCount: 3,
    rowCount: 7,
  }),
];

const mouthDots: MatrixDot[] = createDotsFromRows([
  { rowIndex: 17, columnIndexes: [3, 4, 5, 16, 17, 18] },
  { rowIndex: 18, columnIndexes: [3, 4, 5, 6, 15, 16, 17, 18] },
  { rowIndex: 19, columnIndexes: [4, 5, 6, 7, 14, 15, 16, 17] },
  { rowIndex: 20, columnIndexes: [5, 6, 7, 8, 13, 14, 15, 16] },
  { rowIndex: 21, columnIndexes: [7, 8, 9, 12, 13, 14] },
  { rowIndex: 22, columnIndexes: [8, 9, 10, 11, 12, 13] },
  { rowIndex: 23, columnIndexes: [9, 10, 11, 12] },
]);

const activeDots: MatrixDot[] = [...eyeDots, ...mouthDots];

function getBlockOrigin({
  blockSize,
  columnIndex,
  rowIndex,
}: MatrixDot & {
  blockSize: number;
}) {
  const cellOffset = (matrixCellSize - blockSize) / 2;

  return {
    x: columnIndex * matrixCellSize + cellOffset,
    y: rowIndex * matrixCellSize + cellOffset,
  };
}

export function AgentStatusAnimation({
  status,
  className,
}: AgentStatusAnimationProps) {
  const prefersReducedMotion = usePrefersReducedMotion();
  const shouldBreathe = status === "idle" && !prefersReducedMotion;

  return (
    <div
      data-status={status}
      className={cn(
        "relative flex aspect-[11/15] w-64 items-center justify-center overflow-hidden",
        "rounded-[2rem] border border-border/50 bg-card/70 p-5 shadow-sm",
        "transition-colors duration-500",
        "after:absolute after:inset-x-8 after:bottom-2 after:h-10 after:rounded-full after:bg-primary/10 after:blur-2xl after:content-['']",
        shouldBreathe && "motion-safe:animate-pulse",
        className,
      )}
    >
      <svg
        aria-label={expressionLabelByStatus[status]}
        className="relative z-10 h-full w-full"
        role="img"
        viewBox={`0 0 ${faceViewBoxWidth} ${faceViewBoxHeight}`}
      >
        {inactiveDots.map(({ columnIndex, rowIndex }) => {
          const { x, y } = getBlockOrigin({
            blockSize: inactiveBlockSize,
            columnIndex,
            rowIndex,
          });

          return (
            <rect
              key={`${rowIndex}-${columnIndex}`}
              fill="black"
              height={inactiveBlockSize}
              opacity={0.08}
              width={inactiveBlockSize}
              x={x}
              y={y}
            />
          );
        })}
        {activeDots.map(({ columnIndex, rowIndex }) => {
          const { x, y } = getBlockOrigin({
            blockSize: activeBlockSize,
            columnIndex,
            rowIndex,
          });

          return (
            <rect
              key={`active-${rowIndex}-${columnIndex}`}
              fill="black"
              height={activeBlockSize}
              width={activeBlockSize}
              x={x}
              y={y}
            />
          );
        })}
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

