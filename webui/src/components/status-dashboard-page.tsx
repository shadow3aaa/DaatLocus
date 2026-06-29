import type { TFunction } from "i18next";
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronDownIcon,
  GripVerticalIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { Bar, BarChart, XAxis, YAxis } from "recharts";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { ChartContainer, ChartTooltip } from "@/components/ui/chart";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  fetchStatusSummary,
  type DashboardContextCompositionSegment,
  type DashboardContextCompositionSnapshot,
  type SessionInfo,
  type SessionStatusDashboard,
  type StatusSummary,
} from "@/lib/daemon-api";
import {
  TOKEN_USAGE_CHART_CONFIG,
  dailyTokenUsageChartDataFromSources,
  formatCompactNumber,
  type DailyTokenUsageChartDatum,
  type DailyTokenUsageSource,
} from "@/lib/dashboard-view-model";
import { cn } from "@/lib/utils";

const STATUS_CARD_ORDER_STORAGE_KEY = "daat-locus.status.card-order";
const STATUS_SUMMARY_REFRESH_MS = 5000;
const CONTEXT_COMPOSITION_REFERENCE_COLUMN_COUNT = 30;
const CONTEXT_COMPOSITION_REFERENCE_ROW_COUNT = 10;
const CONTEXT_COMPOSITION_REFERENCE_TOKENS = 1_500_000;
const CONTEXT_COMPOSITION_REFERENCE_CELL_COUNT =
  CONTEXT_COMPOSITION_REFERENCE_COLUMN_COUNT *
  CONTEXT_COMPOSITION_REFERENCE_ROW_COUNT;
const CONTEXT_COMPOSITION_UNIT_TOKENS = Math.ceil(
  CONTEXT_COMPOSITION_REFERENCE_TOKENS / CONTEXT_COMPOSITION_REFERENCE_CELL_COUNT,
);
const CONTEXT_COMPOSITION_CELL_SIZE_PX = 12;
const CONTEXT_COMPOSITION_CELL_GAP_PX = 4;
const TOKEN_USAGE_AXIS_TARGET_INTERVALS = 4;
const TOKEN_USAGE_AXIS_MIN_STEP = 1_000;
const DEFAULT_STATUS_CARD_ORDER = [
  "context-composition",
  "daily-token-usage",
] as const;


type StatusCardId = (typeof DEFAULT_STATUS_CARD_ORDER)[number];
type StatusCardPlacement = "before" | "after";

type StatusCardDropIntent = {
  targetId: StatusCardId;
  placement: StatusCardPlacement;
};

type StatusCardContentProps = {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
  t: TFunction;
};

type StatusCardDefinition = {
  labelKey: string;
  render: (props: StatusCardContentProps) => ReactNode;
};

type TokenUsageTooltipPayloadItem = {
  payload?: DailyTokenUsageChartDatum;
};

type SessionDashboardEntry = {
  session: SessionInfo;
  dashboard: SessionStatusDashboard;
};

type ContextCompositionRun = {
  key: string;
  label: string;
  segment: DashboardContextCompositionSegment;
  tokens: number;
};

type ContextCompositionCell = {
  key: string;
  label: string;
  segment: DashboardContextCompositionSegment;
};
const STATUS_CARD_DEFINITIONS: Record<StatusCardId, StatusCardDefinition> = {
  "context-composition": {
    labelKey: "status.cards.contextComposition",
    render: (props) => <ContextCompositionCard {...props} />,
  },
  "daily-token-usage": {
    labelKey: "status.cards.tokenUsage",
    render: (props) => <DailyTokenUsageCard {...props} />,
  },
};
type StatusPageProps = {
  mockSummary?: StatusSummary;
};

export function StatusPage({ mockSummary }: StatusPageProps = {}) {
  const { t } = useTranslation();
  const { summary, loadError } = useStatusSummary(mockSummary);
  const [cardOrder, setCardOrder] = useState<StatusCardId[]>(
    readStoredStatusCardOrder,
  );
  const [draggedCardId, setDraggedCardId] = useState<StatusCardId | null>(null);
  const [dropIntent, setDropIntent] = useState<StatusCardDropIntent | null>(
    null,
  );
  const cardColumns = useMemo(() => statusCardColumns(cardOrder), [cardOrder]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }

    try {
      window.localStorage.setItem(
        STATUS_CARD_ORDER_STORAGE_KEY,
        JSON.stringify(cardOrder),
      );
    } catch {
      // Ignore storage failures, e.g. private mode or disabled storage.
    }
  }, [cardOrder]);

  function handleDragStart(
    event: DragEvent<HTMLButtonElement>,
    cardId: StatusCardId,
  ) {
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", cardId);
    setDraggedCardId(cardId);
  }

  function handleDragOver(
    event: DragEvent<HTMLDivElement>,
    targetId: StatusCardId,
  ) {
    if (!draggedCardId || draggedCardId === targetId) {
      setDropIntent(null);
      return;
    }

    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    setDropIntent({
      targetId,
      placement: dropPlacementFromEvent(event),
    });
  }

  function handleDragLeave(event: DragEvent<HTMLDivElement>) {
    if (
      event.relatedTarget instanceof Node &&
      event.currentTarget.contains(event.relatedTarget)
    ) {
      return;
    }

    setDropIntent(null);
  }

  function handleDrop(
    event: DragEvent<HTMLDivElement>,
    targetId: StatusCardId,
  ) {
    event.preventDefault();
    const sourceId =
      statusCardIdFromValue(event.dataTransfer.getData("text/plain")) ??
      draggedCardId;

    if (sourceId && sourceId !== targetId) {
      const placement =
        dropIntent?.targetId === targetId
          ? dropIntent.placement
          : dropPlacementFromEvent(event);
      setCardOrder((current) =>
        reorderStatusCards(current, sourceId, targetId, placement),
      );
    }

    setDraggedCardId(null);
    setDropIntent(null);
  }

  function handleDragEnd() {
    setDraggedCardId(null);
    setDropIntent(null);
  }

  function handleKeyboardMove(
    event: KeyboardEvent<HTMLButtonElement>,
    cardId: StatusCardId,
  ) {
    if (event.key !== "ArrowUp" && event.key !== "ArrowLeft") {
      if (event.key !== "ArrowDown" && event.key !== "ArrowRight") {
        return;
      }
      event.preventDefault();
      setCardOrder((current) => moveStatusCardByDelta(current, cardId, 1));
      return;
    }

    event.preventDefault();
    setCardOrder((current) => moveStatusCardByDelta(current, cardId, -1));
  }

  return (
    <section
      id="status"
      aria-label={t("status.pageAria")}
      className="min-h-screen w-full px-6 pb-10 pt-20 md:pb-12 md:pt-8"
    >
      {loadError ? (
        <Alert variant="destructive" className="mb-4">
          <TriangleAlertIcon aria-hidden="true" />
          <AlertTitle>{t("status.unableToLoad")}</AlertTitle>
          <AlertDescription>{loadError.message}</AlertDescription>
        </Alert>
      ) : null}
      <div className="grid w-full grid-cols-1 items-start gap-4 lg:grid-cols-2">
        {cardColumns.map((column, columnIndex) => (
          <div key={columnIndex} className="flex min-w-0 flex-col gap-4">
            {column.map((cardId) => {
              const definition = STATUS_CARD_DEFINITIONS[cardId];
              const label = t(definition.labelKey);
              return (
                <div
                  key={cardId}
                  onDragOver={(event) => handleDragOver(event, cardId)}
                  onDragLeave={handleDragLeave}
                  onDrop={(event) => handleDrop(event, cardId)}
                  className={cn(
                    "relative rounded-xl transition-opacity",
                    draggedCardId === cardId && "opacity-45",
                    dropIntent?.targetId === cardId &&
                      dropIntent.placement === "before" &&
                      "before:absolute before:-top-2 before:left-2 before:right-2 before:z-10 before:h-1 before:rounded-full before:bg-primary",
                    dropIntent?.targetId === cardId &&
                      dropIntent.placement === "after" &&
                      "after:absolute after:-bottom-2 after:left-2 after:right-2 after:z-10 after:h-1 after:rounded-full after:bg-primary",
                  )}
                >
                  {definition.render({
                    summary,
                    t,
                    dragHandle: (
                      <StatusCardDragHandle
                        cardId={cardId}
                        label={label}
                        onDragStart={handleDragStart}
                        onDragEnd={handleDragEnd}
                        onKeyboardMove={handleKeyboardMove}
                      />
                    ),
                  })}
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </section>
  );
}

function useStatusSummary(mockSummary?: StatusSummary) {
  const [summary, setSummary] = useState<StatusSummary | null>(
    () => mockSummary ?? null,
  );
  const [loadError, setLoadError] = useState<Error | null>(null);

  async function load(signal?: AbortSignal) {
    try {
      const nextSummary = await fetchStatusSummary({ signal });
      setSummary(nextSummary);
      setLoadError(null);
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setLoadError(error instanceof Error ? error : new Error(String(error)));
    }
  }

  useEffect(() => {
    if (mockSummary) {
      setSummary(mockSummary);
      setLoadError(null);
      return;
    }

    const controller = new AbortController();
    void load(controller.signal);
    const interval = window.setInterval(() => {
      void load(controller.signal);
    }, STATUS_SUMMARY_REFRESH_MS);

    return () => {
      controller.abort();
      window.clearInterval(interval);
    };
  }, [mockSummary]);

  return {
    summary,
    loadError,
  };
}


function StatusCardDragHandle({
  cardId,
  label,
  onDragStart,
  onDragEnd,
  onKeyboardMove,
}: {
  cardId: StatusCardId;
  label: string;
  onDragStart: (
    event: DragEvent<HTMLButtonElement>,
    cardId: StatusCardId,
  ) => void;
  onDragEnd: () => void;
  onKeyboardMove: (
    event: KeyboardEvent<HTMLButtonElement>,
    cardId: StatusCardId,
  ) => void;
}) {
  const { t } = useTranslation();

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-sm"
      draggable
      aria-label={t("status.reorderCard", { label })}
      title={t("status.dragToReorder", { label })}
      onDragStart={(event) => onDragStart(event, cardId)}
      onDragEnd={onDragEnd}
      onKeyDown={(event) => onKeyboardMove(event, cardId)}
      className="cursor-grab text-muted-foreground hover:text-foreground active:cursor-grabbing"
    >
      <GripVerticalIcon data-icon="inline-start" aria-hidden="true" />
    </Button>
  );
}

function ContextCompositionCard({
  summary,
  dragHandle,
  t,
}: StatusCardContentProps) {
  const entries = useMemo(() => sessionDashboardEntries(summary), [summary]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);

  useEffect(() => {
    setSelectedSessionId((current) => {
      if (current && entries.some((entry) => entry.session.session_id === current)) {
        return current;
      }

      return entries[0]?.session.session_id ?? null;
    });
  }, [entries]);

  const selectedEntry =
    entries.find((entry) => entry.session.session_id === selectedSessionId) ??
    entries[0] ??
    null;
  const composition = selectedEntry?.dashboard.context_composition ?? null;
  const selectedSessionLabel = selectedEntry
    ? sessionDisplayName(selectedEntry.session, selectedEntry.dashboard, t)
    : t("status.noSession");
  const cells = useMemo(() => contextCompositionCells(composition), [composition]);
  const gridFrameRef = useRef<HTMLDivElement>(null);
  const [gridColumnCount, setGridColumnCount] = useState(
    CONTEXT_COMPOSITION_REFERENCE_COLUMN_COUNT,
  );

  useLayoutEffect(() => {
    const node = gridFrameRef.current;
    if (!node) {
      return;
    }

    const updateColumnCount = () => {
      const nextColumnCount = contextCompositionColumnCount(node.clientWidth);
      setGridColumnCount((current) =>
        current === nextColumnCount ? current : nextColumnCount,
      );
    };

    updateColumnCount();

    if (typeof ResizeObserver === "undefined") {
      return;
    }

    const resizeObserver = new ResizeObserver(updateColumnCount);
    resizeObserver.observe(node);
    return () => resizeObserver.disconnect();
  }, [cells.length]);

  const cellCapacity = contextCompositionCellCapacity(
    cells.length,
    gridColumnCount,
  );
  const emptyCellCount = Math.max(0, cellCapacity - cells.length);
  const gridRowCount = Math.ceil(cellCapacity / gridColumnCount);
  const hasComposition = Boolean(composition && cellCapacity > 0);
  const displayScaleLabel = formatContextTokenCount(
    cellCapacity * CONTEXT_COMPOSITION_UNIT_TOKENS,
    t,
  );
  const gridLabel = `${t("status.contextHeatmapLabel", {
    columns: gridColumnCount,
    rows: gridRowCount,
  })} ${t("status.contextCellLabel", {
    tokens: CONTEXT_COMPOSITION_UNIT_TOKENS.toLocaleString("en-US"),
  })}`;

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>{t("status.cards.contextComposition")}</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="w-fit max-w-full"
              disabled={entries.length === 0}
            >
              <span className="max-w-64 truncate">{selectedSessionLabel}</span>
              <ChevronDownIcon data-icon="inline-end" aria-hidden="true" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="start"
            className="w-72 max-w-[calc(100vw-2rem)]"
          >
            <DropdownMenuLabel>{t("status.session")}</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              value={selectedEntry?.session.session_id ?? ""}
              onValueChange={setSelectedSessionId}
            >
              {entries.map((entry) => {
                const context = entry.dashboard.context_composition;
                return (
                  <DropdownMenuRadioItem
                    key={entry.session.session_id}
                    value={entry.session.session_id}
                  >
                    <span className="flex min-w-0 flex-col">
                      <span className="truncate">
                        {sessionDisplayName(entry.session, entry.dashboard, t)}
                      </span>
                      <span className="truncate text-xs text-muted-foreground">
                        {context
                          ? `${formatContextTokenCount(
                              context.total_estimated_tokens,
                              t,
                            )} ${t("status.context")}`
                          : t("status.noContextSnapshot")}
                      </span>
                    </span>
                  </DropdownMenuRadioItem>
                );
              })}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        {hasComposition ? (
          <div ref={gridFrameRef} className="overflow-hidden px-1 pb-1 pt-0.5">
            <div
              className="grid max-w-full gap-1"
              style={{
                gridTemplateColumns: `repeat(${gridColumnCount}, minmax(0, 0.75rem))`,
              }}
              role="img"
              aria-label={t("status.contextDisplayAria", {
                gridLabel,
                occupied: cells.length,
                displayScale: displayScaleLabel,
                session: selectedSessionLabel,
              })}
            >
              {cells.map((cell) => (
                <span
                  key={cell.key}
                  aria-hidden="true"
                  title={cell.label}
                  className={cn(
                    "size-3 rounded-[3px] ring-1 ring-background/70 transition-transform hover:scale-125",
                    contextCompositionShadeClass(cell.segment),
                  )}
                />
              ))}

              {Array.from({ length: emptyCellCount }, (_, unitIndex) => (
                <span
                  key={`empty-${unitIndex}`}
                  aria-hidden="true"
                  className="size-3 rounded-[3px] bg-background ring-1 ring-border/80"
                />
              ))}
            </div>
          </div>
        ) : (
          <DashboardEmptyState
            compact
            title={entries.length ? t("status.noContextSnapshot") : t("status.noSessionsFound")}
            description={
              entries.length
                ? t("status.contextNoSnapshotDescription")
                : t("status.contextNoSessionsDescription")
            }
          />
        )}
      </CardContent>
    </Card>
  );
}
function DailyTokenUsageCard({ summary, dragHandle, t }: StatusCardContentProps) {
  const chartData = useMemo(
    () => dailyTokenUsageChartDataFromSources(tokenUsageSources(summary)),
    [summary],
  );
  const hasUsage = chartData.some((day) => day.total > 0);
  const tokenAxisScale = tokenUsageAxisScale(chartData);

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>{t("status.cards.tokenUsage")}</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        <ChartContainer
          config={TOKEN_USAGE_CHART_CONFIG}
          className="h-64 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
        >
          <BarChart
            accessibilityLayer
            data={chartData}
            margin={{ top: 18, right: 16, left: 8, bottom: 0 }}
            barCategoryGap="34%"
          >
            <XAxis
              dataKey="label"
              tickLine={false}
              axisLine={false}
              tickMargin={10}
              interval={0}
            />
            <YAxis
              width={56}
              tickLine={false}
              axisLine={false}
              allowDecimals={false}
              domain={[0, tokenAxisScale.max]}
              ticks={tokenAxisScale.ticks}
              tickFormatter={formatTokenAxisTick}
            />
            <ChartTooltip
              allowEscapeViewBox={{ y: true }}
              cursor={{ fill: "var(--muted)" }}
              wrapperStyle={{ zIndex: 50 }}
              content={<TokenUsageTooltip t={t} />}
            />
            <Bar
              dataKey="cached"
              stackId="tokens"
              fill="var(--color-cached)"
              isAnimationActive={false}
              radius={[0, 0, 0, 0]}
            />
            <Bar
              dataKey="uncached"
              stackId="tokens"
              fill="var(--color-uncached)"
              isAnimationActive={false}
              radius={[4, 4, 0, 0]}
            />
          </BarChart>
        </ChartContainer>
        {hasUsage ? null : (
          <DashboardEmptyState
            title={t("status.noTokenUsage")}
            description={t("status.tokenUsageDescription")}
            compact
          />
        )}
      </CardContent>
    </Card>
  );
}

function DashboardEmptyState({
  title,
  description,
  compact = false,
}: {
  title: string;
  description: string;
  compact?: boolean;
}) {
  return (
    <Empty className={cn("border-0", compact ? "py-3" : "min-h-32 py-6")}>
      <EmptyHeader>
        <EmptyTitle>{title}</EmptyTitle>
        <EmptyDescription>{description}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function sessionDashboardEntries(
  summary: StatusSummary | null,
): SessionDashboardEntry[] {
  return (summary?.sessions ?? []).flatMap((entry) =>
    entry.dashboard
      ? [
          {
            session: entry.session,
            dashboard: entry.dashboard,
          },
        ]
      : [],
  );
}

function contextCompositionCells(
  composition: DashboardContextCompositionSnapshot | null,
): ContextCompositionCell[] {
  if (!composition) {
    return [];
  }

  const runs = contextCompositionDisplayRuns(composition.segments);
  const totalTokens = runs.reduce((total, run) => total + run.tokens, 0);
  const cellCount = Math.ceil(totalTokens / CONTEXT_COMPOSITION_UNIT_TOKENS);
  const cells: ContextCompositionCell[] = [];
  let runIndex = 0;
  let runStart = 0;
  let runEnd = runs[0]?.tokens ?? 0;

  for (let cellIndex = 0; cellIndex < cellCount; cellIndex += 1) {
    const cellStart = cellIndex * CONTEXT_COMPOSITION_UNIT_TOKENS;
    const cellEnd = Math.min(
      totalTokens,
      cellStart + CONTEXT_COMPOSITION_UNIT_TOKENS,
    );

    while (runIndex < runs.length && runEnd <= cellStart) {
      runStart = runEnd;
      runIndex += 1;
      runEnd += runs[runIndex]?.tokens ?? 0;
    }

    const run = contextCompositionDominantRunForCell(
      runs,
      runIndex,
      runStart,
      runEnd,
      cellStart,
      cellEnd,
    );
    if (!run) {
      continue;
    }

    cells.push({
      key: `${cellIndex}-${run.key}`,
      label: run.label,
      segment: run.segment,
    });
  }

  return cells;
}

function contextCompositionDominantRunForCell(
  runs: ContextCompositionRun[],
  initialRunIndex: number,
  initialRunStart: number,
  initialRunEnd: number,
  cellStart: number,
  cellEnd: number,
) {
  let bestRun: ContextCompositionRun | null = null;
  let bestOverlap = 0;
  let runIndex = initialRunIndex;
  let runStart = initialRunStart;
  let runEnd = initialRunEnd;

  while (runIndex < runs.length && runStart < cellEnd) {
    const overlap = Math.max(
      0,
      Math.min(runEnd, cellEnd) - Math.max(runStart, cellStart),
    );

    if (overlap > bestOverlap) {
      bestOverlap = overlap;
      bestRun = runs[runIndex];
    }

    runStart = runEnd;
    runIndex += 1;
    runEnd += runs[runIndex]?.tokens ?? 0;
  }

  return bestRun;
}

function contextCompositionDisplayRuns(
  segments: DashboardContextCompositionSegment[],
): ContextCompositionRun[] {
  const runs: ContextCompositionRun[] = [];

  for (const segment of contextCompositionDisplaySegments(segments)) {
    const tokens = Math.max(0, segment.tokens);
    if (tokens === 0) {
      continue;
    }

    const key = contextCompositionSegmentTypeKey(segment);
    const label = contextCompositionSegmentLabel(segment);
    const previousRun = runs[runs.length - 1];

    if (previousRun?.key === key) {
      previousRun.tokens += tokens;
      continue;
    }

    runs.push({
      key,
      label,
      segment,
      tokens,
    });
  }

  return runs;
}

function contextCompositionDisplaySegments(
  segments: DashboardContextCompositionSegment[],
) {
  return segments
    .map((segment, index) => ({ segment, index }))
    .sort((left, right) => {
      const priorityDelta =
        contextCompositionDisplayPriority(left.segment) -
        contextCompositionDisplayPriority(right.segment);

      return priorityDelta || left.index - right.index;
    })
    .map(({ segment }) => segment);
}

function contextCompositionSegmentTypeKey(
  segment: DashboardContextCompositionSegment,
) {
  return segment.name || segment.label || segment.source || "unknown";
}

function contextCompositionSegmentLabel(segment: DashboardContextCompositionSegment) {
  return segment.label || segment.name || segment.source || "Unknown";
}

function contextCompositionDisplayPriority(
  segment: DashboardContextCompositionSegment,
) {
  if (segment.name === "tools_schema" || segment.source === "request_tools") {
    return 0;
  }

  if (segment.name === "system_messages" || segment.source === "system") {
    return 1;
  }

  return 2;
}

function contextCompositionColumnCount(containerWidth: number) {
  if (containerWidth <= 0) {
    return CONTEXT_COMPOSITION_REFERENCE_COLUMN_COUNT;
  }

  return Math.max(
    1,
    Math.floor(
      (containerWidth + CONTEXT_COMPOSITION_CELL_GAP_PX) /
        (CONTEXT_COMPOSITION_CELL_SIZE_PX + CONTEXT_COMPOSITION_CELL_GAP_PX),
    ),
  );
}

function contextCompositionCellCapacity(
  occupiedCellCount: number,
  columnCount: number,
) {
  const safeColumnCount = Math.max(1, Math.floor(columnCount));
  const minimumRows = Math.ceil(
    CONTEXT_COMPOSITION_REFERENCE_CELL_COUNT / safeColumnCount,
  );
  const occupiedRows = Math.ceil(
    Math.max(0, occupiedCellCount) / safeColumnCount,
  );

  return Math.max(minimumRows, occupiedRows) * safeColumnCount;
}
function contextCompositionShadeClass(segment: DashboardContextCompositionSegment) {
  const sourceKey = `${segment.name} ${segment.source} ${segment.cache_role}`.toLowerCase();

  if (sourceKey.includes("system")) {
    return "bg-foreground/80";
  }

  if (sourceKey.includes("tools")) {
    return "bg-foreground/65";
  }

  if (
    sourceKey.includes("afterclaim") ||
    sourceKey.includes("preturn") ||
    sourceKey.includes("claimed")
  ) {
    return "bg-foreground/50";
  }

  if (sourceKey.includes("conversation") || sourceKey.includes("summary")) {
    return "bg-foreground/35";
  }

  if (sourceKey.includes("assistant") || sourceKey.includes("tool")) {
    return "bg-foreground/25";
  }

  return "bg-foreground/15";
}

function tokenUsageSources(
  summary: StatusSummary | null,
): DailyTokenUsageSource[] {
  return sessionDashboardEntries(summary).flatMap((entry) => {
    const tokenUsage = entry.dashboard.token_usage;
    const sessionLabel = sessionDisplayName(entry.session, entry.dashboard);
    return [
      {
        label: `${sessionLabel} / ${tokenUsageModelLabel("main", tokenUsage.main_model)}`,
        info: tokenUsage.main,
      },
      {
        label: `${sessionLabel} / ${tokenUsageModelLabel("judge", tokenUsage.judge_model)}`,
        info: tokenUsage.judge,
      },
    ];
  });
}

function tokenUsageModelLabel(role: string, model: string | null | undefined) {
  return model?.trim() || role;
}

function sessionDisplayName(
  session: SessionInfo,
  dashboard?: SessionStatusDashboard | null,
  t?: TFunction,
) {
  return (
    session.title?.trim() ||
    dashboard?.session_title?.title.trim() ||
    t?.("common.untitledSession") ||
    "Untitled session"
  );
}


function statusCardColumns(order: StatusCardId[]) {
  const normalizedOrder = normalizeStatusCardOrder(order);
  const columnCount = Math.min(Math.max(normalizedOrder.length, 1), 2);
  const columnLength = Math.ceil(normalizedOrder.length / columnCount);

  return Array.from({ length: columnCount }, (_, columnIndex) => {
    const startIndex = columnIndex * columnLength;
    return normalizedOrder.slice(startIndex, startIndex + columnLength);
  }).filter((column) => column.length > 0);
}

function moveStatusCardByDelta(
  order: StatusCardId[],
  cardId: StatusCardId,
  delta: number,
) {
  const normalizedOrder = normalizeStatusCardOrder(order);
  const currentIndex = normalizedOrder.indexOf(cardId);
  if (currentIndex === -1) {
    return normalizedOrder;
  }

  const nextIndex = Math.max(
    0,
    Math.min(normalizedOrder.length - 1, currentIndex + delta),
  );
  if (nextIndex === currentIndex) {
    return normalizedOrder;
  }

  const nextOrder = [...normalizedOrder];
  const [movedCard] = nextOrder.splice(currentIndex, 1);
  nextOrder.splice(nextIndex, 0, movedCard);
  return nextOrder;
}

function reorderStatusCards(
  order: StatusCardId[],
  sourceId: StatusCardId,
  targetId: StatusCardId,
  placement: StatusCardPlacement,
) {
  if (sourceId === targetId) {
    return normalizeStatusCardOrder(order);
  }

  const withoutSource = normalizeStatusCardOrder(order).filter(
    (cardId) => cardId !== sourceId,
  );
  const targetIndex = withoutSource.indexOf(targetId);
  if (targetIndex === -1) {
    return withoutSource;
  }

  const insertIndex = placement === "after" ? targetIndex + 1 : targetIndex;
  const nextOrder = [...withoutSource];
  nextOrder.splice(insertIndex, 0, sourceId);
  return normalizeStatusCardOrder(nextOrder);
}

function dropPlacementFromEvent(
  event: DragEvent<HTMLElement>,
): StatusCardPlacement {
  const bounds = event.currentTarget.getBoundingClientRect();
  const midpoint = bounds.top + bounds.height / 2;
  return event.clientY > midpoint ? "after" : "before";
}

function readStoredStatusCardOrder(): StatusCardId[] {
  if (typeof window === "undefined") {
    return [...DEFAULT_STATUS_CARD_ORDER];
  }

  try {
    const storedOrder = window.localStorage.getItem(
      STATUS_CARD_ORDER_STORAGE_KEY,
    );
    if (!storedOrder) {
      return [...DEFAULT_STATUS_CARD_ORDER];
    }

    const parsed: unknown = JSON.parse(storedOrder);
    if (!Array.isArray(parsed)) {
      return [...DEFAULT_STATUS_CARD_ORDER];
    }

    return normalizeStatusCardOrder(parsed);
  } catch {
    return [...DEFAULT_STATUS_CARD_ORDER];
  }
}

function normalizeStatusCardOrder(order: readonly unknown[]): StatusCardId[] {
  const nextOrder: StatusCardId[] = [];

  for (const value of order) {
    const cardId = statusCardIdFromValue(value);
    if (cardId && !nextOrder.includes(cardId)) {
      nextOrder.push(cardId);
    }
  }

  for (const cardId of DEFAULT_STATUS_CARD_ORDER) {
    if (!nextOrder.includes(cardId)) {
      nextOrder.push(cardId);
    }
  }

  return nextOrder;
}

function statusCardIdFromValue(value: unknown): StatusCardId | null {
  return typeof value === "string" &&
    DEFAULT_STATUS_CARD_ORDER.includes(value as StatusCardId)
    ? (value as StatusCardId)
    : null;
}

function formatContextTokenCount(tokens: number, t?: TFunction) {
  const value = formatCompactNumber(Math.max(0, tokens));
  return t ? t("status.tokenCount", { count: value }) : `${value} tokens`;
}

function tokenUsageAxisScale(chartData: DailyTokenUsageChartDatum[]) {
  const maxTotal = Math.max(0, ...chartData.map((day) => day.total));
  const step = tokenUsageAxisStep(maxTotal);
  const max = step * Math.max(1, Math.ceil(maxTotal / step));
  const intervalCount = Math.max(1, Math.round(max / step));

  return {
    max,
    ticks: Array.from(
      { length: intervalCount + 1 },
      (_, index) => index * step,
    ),
  };
}

function tokenUsageAxisStep(maxTotal: number) {
  const safeTotal = Number.isFinite(maxTotal) && maxTotal > 0
    ? maxTotal
    : TOKEN_USAGE_AXIS_MIN_STEP;
  const rawStep = Math.max(
    TOKEN_USAGE_AXIS_MIN_STEP,
    safeTotal / TOKEN_USAGE_AXIS_TARGET_INTERVALS,
  );
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const normalizedStep = rawStep / magnitude;

  if (normalizedStep <= 1) {
    return magnitude;
  }
  if (normalizedStep <= 2) {
    return 2 * magnitude;
  }
  if (normalizedStep <= 5) {
    return 5 * magnitude;
  }
  return 10 * magnitude;
}

function formatTokenAxisTick(value: number) {
  return formatCompactNumber(Math.max(0, value));
}


function TokenUsageTooltip({
  active,
  payload,
  t,
}: {
  active?: boolean;
  payload?: TokenUsageTooltipPayloadItem[];
  t: TFunction;
}) {
  if (!active) {
    return null;
  }

  const datum = payload?.[0]?.payload;
  if (!datum) {
    return null;
  }

  return (
    <div className="grid min-w-72 gap-3 rounded-lg border bg-background px-3 py-2.5 text-xs shadow-xl">
      <div>
        <div className="font-medium text-foreground">{datum.date}</div>
        <div className="mt-1 grid gap-1 text-muted-foreground">
          <TokenUsageTooltipRow label={t("status.total")} value={datum.total} />
          <TokenUsageTooltipRow
            label={t("status.cached")}
            value={datum.cached}
            color="var(--color-cached)"
          />
          <TokenUsageTooltipRow
            label={t("status.uncached")}
            value={datum.uncached}
            color="var(--color-uncached)"
          />
        </div>
      </div>
    </div>
  );
}

function TokenUsageTooltipRow({
  label,
  value,
  color,
}: {
  label: string;
  value: number;
  color?: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      {color ? (
        <span
          className="size-2 shrink-0 rounded-[2px]"
          style={{ backgroundColor: color }}
        />
      ) : null}
      <span className="min-w-0 flex-1">{label}</span>
      <span className="font-mono font-medium tabular-nums text-foreground">
        {formatCompactNumber(value)}
      </span>
    </div>
  );
}
