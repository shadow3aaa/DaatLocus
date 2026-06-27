import {
  useEffect,
  useMemo,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
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
  formatPercentAxisTick,
  type DailyTokenUsageChartDatum,
  type DailyTokenUsageSource,
} from "@/lib/dashboard-view-model";
import { cn } from "@/lib/utils";

const STATUS_CARD_ORDER_STORAGE_KEY = "daat-locus.status.card-order";
const STATUS_SUMMARY_REFRESH_MS = 5000;
const CONTEXT_COMPOSITION_UNIT_TOKENS = 1000;
const DEFAULT_STATUS_CARD_ORDER = [
  "context-composition",
  "daily-token-usage",
] as const;

const CONTEXT_COMPOSITION_GRID_LABEL =
  "Context composition heatmap. Each cell represents one thousand tokens.";

type StatusCardId = (typeof DEFAULT_STATUS_CARD_ORDER)[number];
type StatusCardPlacement = "before" | "after";

type StatusCardDropIntent = {
  targetId: StatusCardId;
  placement: StatusCardPlacement;
};

type StatusCardContentProps = {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
};

type StatusCardDefinition = {
  label: string;
  render: (props: StatusCardContentProps) => ReactNode;
};

type TokenUsageTooltipPayloadItem = {
  payload?: DailyTokenUsageChartDatum;
};

type SessionDashboardEntry = {
  session: SessionInfo;
  dashboard: SessionStatusDashboard;
};

type ContextCompositionCell = {
  key: string;
  label: string;
  segment: DashboardContextCompositionSegment;
  unitIndex: number;
  unitCount: number;
};

type ContextCompositionCapacitySource = Pick<
  SessionStatusDashboard,
  "token_usage"
>;

const STATUS_CARD_DEFINITIONS: Record<StatusCardId, StatusCardDefinition> = {
  "context-composition": {
    label: "Context Composition",
    render: (props) => <ContextCompositionCard {...props} />,
  },
  "daily-token-usage": {
    label: "Token Usage",
    render: (props) => <DailyTokenUsageCard {...props} />,
  },
};
type StatusPageProps = {
  mockSummary?: StatusSummary;
};

export function StatusPage({ mockSummary }: StatusPageProps = {}) {
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
      aria-label="Status"
      className="min-h-screen w-full px-6 pb-10 pt-20 md:pb-12 md:pt-8"
    >
      {loadError ? (
        <Alert variant="destructive" className="mb-4">
          <TriangleAlertIcon aria-hidden="true" />
          <AlertTitle>Unable to load status</AlertTitle>
          <AlertDescription>{loadError.message}</AlertDescription>
        </Alert>
      ) : null}
      <div className="grid w-full grid-cols-1 items-start gap-4 lg:grid-cols-2">
        {cardColumns.map((column, columnIndex) => (
          <div key={columnIndex} className="flex min-w-0 flex-col gap-4">
            {column.map((cardId) => {
              const definition = STATUS_CARD_DEFINITIONS[cardId];
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
                    dragHandle: (
                      <StatusCardDragHandle
                        cardId={cardId}
                        label={definition.label}
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
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-sm"
      draggable
      aria-label={`Reorder ${label} card`}
      title={`Drag to reorder ${label}`}
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
    ? sessionDisplayName(selectedEntry.session, selectedEntry.dashboard)
    : "No session";
  const cells = useMemo(() => contextCompositionCells(composition), [composition]);
  const cellCapacity = contextCompositionCellCapacity(
    selectedEntry?.dashboard ?? null,
    composition,
  );
  const emptyCellCount = Math.max(0, cellCapacity - cells.length);
  const hasComposition = Boolean(composition && cellCapacity > 0);

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Context Composition</CardTitle>
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
            <DropdownMenuLabel>Session</DropdownMenuLabel>
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
                        {sessionDisplayName(entry.session, entry.dashboard)}
                      </span>
                      <span className="truncate text-xs text-muted-foreground">
                        {context
                          ? `${formatContextTokenCount(
                              context.total_estimated_tokens,
                            )} context`
                          : "No context snapshot"}
                      </span>
                    </span>
                  </DropdownMenuRadioItem>
                );
              })}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        {hasComposition ? (
          <div className="overflow-x-auto px-1 pb-1 pt-0.5">
            <div
              className="grid min-w-max grid-cols-[repeat(32,minmax(0,0.75rem))] gap-1"
              role="img"
              aria-label={`${CONTEXT_COMPOSITION_GRID_LABEL} Showing ${cells.length} used units out of ${cellCapacity} total units for ${selectedSessionLabel}.`}
            >
              {cells.map((cell) => (
                <span
                  key={cell.key}
                  aria-hidden="true"
                  title={`${cell.label} · unit ${cell.unitIndex + 1}/${cell.unitCount} · ${formatContextTokenCount(cell.segment.tokens)}`}
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
                  title={`Unused context · unit ${cells.length + unitIndex + 1}/${cellCapacity}`}
                  className="size-3 rounded-[3px] bg-background ring-1 ring-border/80"
                />
              ))}
            </div>
          </div>
        ) : (
          <DashboardEmptyState
            compact
            title={entries.length ? "No context snapshot" : "No sessions found"}
            description={
              entries.length
                ? "This session has not assembled a model request context yet."
                : "Context composition appears after a session publishes status data."
            }
          />
        )}
      </CardContent>
    </Card>
  );
}


function DailyTokenUsageCard({ summary, dragHandle }: StatusCardContentProps) {
  const chartData = useMemo(
    () => dailyTokenUsageChartDataFromSources(tokenUsageSources(summary)),
    [summary],
  );
  const hasUsage = chartData.some((day) => day.total > 0);

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>Token Usage</CardTitle>
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
            margin={{ top: 18, right: 16, left: -8, bottom: 0 }}
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
              width={44}
              tickLine={false}
              axisLine={false}
              domain={[0, 1]}
              ticks={[0, 1]}
              tickFormatter={formatPercentAxisTick}
            />
            <ChartTooltip
              allowEscapeViewBox={{ y: true }}
              cursor={{ fill: "var(--muted)" }}
              wrapperStyle={{ zIndex: 50 }}
              content={<TokenUsageTooltip />}
            />
            <Bar
              dataKey="cachedRatio"
              stackId="tokens"
              fill="var(--color-cached)"
              isAnimationActive={false}
              radius={[0, 0, 0, 0]}
            />
            <Bar
              dataKey="uncachedRatio"
              stackId="tokens"
              fill="var(--color-uncached)"
              isAnimationActive={false}
              radius={[4, 4, 0, 0]}
            />
          </BarChart>
        </ChartContainer>
        {hasUsage ? null : (
          <DashboardEmptyState
            title="No token usage recorded"
            description="Usage bars appear after sessions make model requests."
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

  return composition.segments.flatMap((segment, segmentIndex) => {
    const unitCount = Math.ceil(
      Math.max(0, segment.tokens) / CONTEXT_COMPOSITION_UNIT_TOKENS,
    );
    const label = segment.label || segment.name || segment.source || "Unknown";

    return Array.from({ length: unitCount }, (_, unitIndex) => ({
      key: `${segment.hash}-${segmentIndex}-${unitIndex}`,
      label,
      segment,
      unitIndex,
      unitCount,
    }));
  });
}

function contextCompositionCellCapacity(
  dashboard: ContextCompositionCapacitySource | null,
  composition: DashboardContextCompositionSnapshot | null,
) {
  const contextWindow =
    dashboard?.token_usage.main?.model_context_window ??
    dashboard?.token_usage.judge?.model_context_window ??
    null;
  const totalTokens =
    contextWindow && contextWindow > 0
      ? contextWindow
      : composition?.total_estimated_tokens ?? 0;

  return Math.ceil(Math.max(0, totalTokens) / CONTEXT_COMPOSITION_UNIT_TOKENS);
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
) {
  return (
    session.title?.trim() ||
    dashboard?.session_title?.title.trim() ||
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

function formatContextTokenCount(tokens: number) {
  return `${formatCompactNumber(Math.max(0, tokens))} tokens`;
}


function TokenUsageTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: TokenUsageTooltipPayloadItem[];
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
          <TokenUsageTooltipRow label="Total" value={datum.total} />
          <TokenUsageTooltipRow
            label="Cached"
            value={datum.cached}
            color="var(--color-cached)"
          />
          <TokenUsageTooltipRow
            label="Uncached"
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
