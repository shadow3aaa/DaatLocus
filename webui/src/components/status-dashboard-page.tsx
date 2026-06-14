import {
  useEffect,
  useMemo,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";

import {
  CheckIcon,
  ChevronRight,
  GripVerticalIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react";
import { Bar, BarChart, XAxis, YAxis } from "recharts";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
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
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  runDashboardCommand,
  fetchStatusSummary,
  type DashboardPendingAccessRequest,
  type SessionInfo,
  type SessionStatusDashboard,
  type StatusSessionSummary,
  type StatusSummary,
} from "@/lib/daemon-api";
import {
  CONTEXT_COMPOSITION_CHART_CONFIG,
  RUNTIME_OPTIMIZATION_CHART_CONFIG,
  TOKEN_USAGE_CHART_CONFIG,
  PRIMITIVE_OPTIMIZATION_CHART_CONFIG,
  contextCompositionCardData,
  dailyTokenUsageChartDataFromSources,
  formatCompactNumber,
  formatPercent,
  formatPercentAxisTick,
  runtimeOptimizationProgressData,
  primitiveOptimizationProgressData,
  type ContextCompositionPrefixSummaryDatum,
  type DailyTokenUsageSource,
  type DailyTokenUsageChartDatum,
} from "@/lib/dashboard-view-model";
import { cn } from "@/lib/utils";

const STATUS_CARD_ORDER_STORAGE_KEY = "daat-locus.status.card-order";
const STATUS_SUMMARY_REFRESH_MS = 5000;
const DEFAULT_STATUS_CARD_ORDER = [
  "sessions",
  "telegram-approval",
  "runtime-optimization",
  "context-composition",
  "daily-token-usage",
  "primitive-optimization",
] as const;

type StatusCardId = (typeof DEFAULT_STATUS_CARD_ORDER)[number];
type StatusCardPlacement = "before" | "after";

type StatusCardDropIntent = {
  targetId: StatusCardId;
  placement: StatusCardPlacement;
};

type StatusCardContentProps = {
  summary: StatusSummary | null;
  onRefresh: () => void;
  dragHandle: ReactNode;
};

type StatusCardDefinition = {
  label: string;
  render: (props: StatusCardContentProps) => ReactNode;
};

type TokenUsageTooltipPayloadItem = {
  payload?: DailyTokenUsageChartDatum;
};

type ContextPrefixTooltipPayloadItem = {
  payload?: ContextCompositionPrefixSummaryDatum;
};

type OptimizationChartConfig = Record<string, { color: string }>;

type OptimizationProgressDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: string;
  detail: string;
};

type SessionDashboardEntry = {
  session: SessionInfo;
  dashboard: SessionStatusDashboard;
};

type SessionStatusTone = "attention" | "active" | "ready" | "available";

const STATUS_CARD_DEFINITIONS: Record<StatusCardId, StatusCardDefinition> = {
  sessions: {
    label: "Sessions",
    render: (props) => <SessionsCard {...props} />,
  },
  "telegram-approval": {
    label: "Telegram Approval",
    render: (props) => <TelegramApprovalCard {...props} />,
  },
  "runtime-optimization": {
    label: "Runtime Optimization",
    render: (props) => <RuntimeOptimizationCard {...props} />,
  },
  "context-composition": {
    label: "Model Context Composition",
    render: (props) => <ModelContextCompositionCard {...props} />,
  },
  "daily-token-usage": {
    label: "Token Usage",
    render: (props) => <DailyTokenUsageCard {...props} />,
  },
  "primitive-optimization": {
    label: "Primitive Optimization",
    render: (props) => <PrimitiveOptimizationCard {...props} />,
  },
};

export function StatusPage() {
  const { summary, loadError, reload } = useStatusSummary();
  const [cardOrder, setCardOrder] = useState<StatusCardId[]>(
    readStoredStatusCardOrder,
  );
  const [draggedCardId, setDraggedCardId] = useState<StatusCardId | null>(null);
  const [dropIntent, setDropIntent] = useState<StatusCardDropIntent | null>(
    null,
  );
  const cardColumns = useMemo(() => statusCardColumns(cardOrder), [cardOrder]);

  useEffect(() => {
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
      <div className="grid w-full grid-cols-1 items-start gap-4 sm:grid-cols-2 xl:grid-cols-3">
        {cardColumns.map((column, columnIndex) => (
          <div
            key={columnIndex}
            className={cn(
              "flex min-w-0 flex-col gap-4",
              columnIndex === 2 && "sm:col-span-2 xl:col-span-1",
            )}
          >
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
                    onRefresh: reload,
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

function useStatusSummary() {
  const [summary, setSummary] = useState<StatusSummary | null>(null);
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
    const controller = new AbortController();
    void load(controller.signal);
    const interval = window.setInterval(() => {
      void load(controller.signal);
    }, STATUS_SUMMARY_REFRESH_MS);

    return () => {
      controller.abort();
      window.clearInterval(interval);
    };
  }, []);

  return {
    summary,
    loadError,
    reload: () => {
      void load();
    },
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

function SessionsCard({
  summary,
  dragHandle,
}: {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
}) {
  const sessions = summary?.sessions ?? [];
  const runningSessions = sessions.filter((entry) => entry.runtime_status?.ready);

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Sessions</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <div className="grid grid-cols-3 gap-2">
          <ContextCompositionMetric
            label="Daemon"
            value={summary?.daemon.state ?? "loading"}
            detail={summary ? `:${summary.daemon.port}` : "manager"}
          />
          <ContextCompositionMetric
            label="Sessions"
            value={formatCompactNumber(sessions.length)}
            detail={`${formatCompactNumber(runningSessions.length)} ready`}
          />
          <ContextCompositionMetric
            label="Clients"
            value={formatCompactNumber(summary?.daemon.connected_clients ?? 0)}
            detail="web/tui"
          />
        </div>
        {sessions.length > 0 ? (
          <div className="flex flex-col gap-2">
            {sessions.map((entry) => (
              <SessionStatusLine key={entry.session.session_id} entry={entry} />
            ))}
          </div>
        ) : (
          <DashboardEmptyState
            title="No sessions registered"
            description="Sessions will appear here after the manager starts or reconnects them."
          />
        )}
      </CardContent>
    </Card>
  );
}

function SessionStatusLine({ entry }: { entry: StatusSessionSummary }) {
  const runtime = entry.runtime_status;
  const status: SessionStatusTone = entry.error
    ? "attention"
    : runtime?.active_runtime_turn
      ? "active"
      : runtime?.ready
        ? "ready"
        : "available";
  const detail = entry.error
    ? entry.error
    : runtime
      ? `${runtime.pending_work_count} pending${runtime.focused_app ? ` · ${runtime.focused_app}` : ""}`
      : sessionScopeLabel(entry.session);

  return (
    <div className="flex min-w-0 items-center gap-3 rounded-lg border bg-muted/20 px-3 py-2">
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium">
          {sessionDisplayName(entry.session, entry.dashboard)}
        </div>
        <div className="truncate text-xs text-muted-foreground">{detail}</div>
      </div>
      <Badge
        variant={sessionStatusBadgeVariant(status)}
        className="font-mono tabular-nums"
      >
        {status}
      </Badge>
    </div>
  );
}

function TelegramApprovalCard({
  summary,
  onRefresh,
  dragHandle,
}: {
  summary: StatusSummary | null;
  onRefresh: () => void;
  dragHandle: ReactNode;
}) {
  const requests = summary?.pending_access_requests ?? [];
  const [busyChatId, setBusyChatId] = useState<number | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  async function handleRequestAction(
    request: DashboardPendingAccessRequest,
    action: "approve" | "reject",
  ) {
    setBusyChatId(request.chat_id);
    setActionError(null);

    try {
      await runDashboardCommand(`/telegram ${action} ${request.chat_id}`, {});
      onRefresh();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusyChatId(null);
    }
  }

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Telegram Approval</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        {requests.length > 0 ? (
          <div className="flex flex-col gap-3">
            {requests.map((request) => {
              const isBusy = busyChatId === request.chat_id;
              const label = telegramApprovalDisplayName(request);

              return (
                <div
                  key={request.chat_id}
                  className="flex items-center gap-3 rounded-xl border bg-muted/20 p-3"
                >
                  <Avatar className="size-10 border border-border bg-background">
                    <AvatarFallback className="text-sm font-medium">
                      {telegramApprovalInitials(label)}
                    </AvatarFallback>
                  </Avatar>
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium">{label}</div>
                    <div className="truncate font-mono text-xs text-muted-foreground">
                      {request.chat_id}
                    </div>
                  </div>
                  <div className="flex items-center gap-1.5">
                    <Button
                      type="button"
                      variant="default"
                      size="icon-sm"
                      aria-label={`Approve ${label}`}
                      disabled={busyChatId !== null}
                      onClick={() => handleRequestAction(request, "approve")}
                    >
                      <CheckIcon data-icon="inline-start" aria-hidden="true" />
                    </Button>
                    <Button
                      type="button"
                      variant="destructive"
                      size="icon-sm"
                      aria-label={`Reject ${label}`}
                      disabled={busyChatId !== null}
                      onClick={() => handleRequestAction(request, "reject")}
                    >
                      <XIcon data-icon="inline-start" aria-hidden="true" />
                    </Button>
                  </div>
                  {isBusy ? <span className="sr-only">Processing</span> : null}
                </div>
              );
            })}
          </div>
        ) : (
          <DashboardEmptyState
            title="No pending Telegram approvals"
            description="Incoming access requests will appear here when Telegram control needs review."
          />
        )}
        {actionError ? (
          <Alert variant="destructive" className="mt-3 px-2 py-1">
            <AlertDescription className="text-xs">{actionError}</AlertDescription>
          </Alert>
        ) : null}
      </CardContent>
    </Card>
  );
}

function DailyTokenUsageCard({
  summary,
  dragHandle,
}: {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
}) {
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

function ModelContextCompositionCard({
  summary,
  dragHandle,
}: {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
}) {
  const entries = useMemo(
    () =>
      sessionDashboardEntries(summary).filter(
        (entry) => entry.dashboard.context_composition,
      ),
    [summary],
  );

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>Model Context Composition</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {entries.length > 0 ? (
          <>
            {entries.map((entry) => (
              <SessionContextComposition
                key={entry.session.session_id}
                entry={entry}
              />
            ))}
          </>
        ) : (
          <DashboardEmptyState
            title="Waiting for context captures"
            description="Context composition appears after a session sends a model request."
          />
        )}
      </CardContent>
    </Card>
  );
}

function SessionContextComposition({
  entry,
}: {
  entry: SessionDashboardEntry;
}) {
  const composition = entry.dashboard.context_composition;
  const compositionData = useMemo(
    () => contextCompositionCardData(entry.dashboard),
    [entry.dashboard],
  );

  if (!composition) {
    return null;
  }

  return (
    <div className="flex flex-col gap-3 rounded-lg border bg-muted/20 p-3">
      <div className="flex min-w-0 items-center justify-between gap-3">
        <div className="truncate text-sm font-medium">
          {sessionDisplayName(entry.session, entry.dashboard)}
        </div>
        <div className="shrink-0 font-mono text-xs text-muted-foreground">
          {composition.model ?? "unknown"}
        </div>
      </div>
      <div className="grid grid-cols-3 gap-2">
        <ContextCompositionMetric
          label="Total"
          value={formatCompactNumber(composition.total_estimated_tokens)}
          detail="est. tokens"
        />
        <ContextCompositionMetric
          label="New suffix"
          value={formatPercent(compositionData.newSuffixRatio)}
          detail={formatCompactNumber(composition.new_suffix_tokens)}
        />
        <ContextCompositionMetric
          label="Stable"
          value={formatPercent(compositionData.stablePrefixRatio)}
          detail={formatCompactNumber(composition.stable_prefix_tokens)}
        />
      </div>
      <ChartContainer
        config={CONTEXT_COMPOSITION_CHART_CONFIG}
        className="h-10 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
      >
        <BarChart
          accessibilityLayer
          data={compositionData.prefixSummaryData}
          layout="vertical"
          margin={{ top: 8, right: 0, left: 0, bottom: 8 }}
          stackOffset="expand"
        >
          <XAxis type="number" hide domain={[0, 1]} />
          <YAxis type="category" dataKey="label" hide />
          <ChartTooltip
            cursor={{ fill: "transparent" }}
            wrapperStyle={{ zIndex: 50 }}
            content={<ContextPrefixTooltip />}
          />
          <Bar
            dataKey="stable"
            stackId="prefix"
            fill="var(--color-stable)"
            isAnimationActive={false}
            radius={[4, 0, 0, 4]}
          />
          <Bar
            dataKey="changed"
            stackId="prefix"
            fill="var(--color-changed)"
            isAnimationActive={false}
            radius={[0, 0, 0, 0]}
          />
          <Bar
            dataKey="new"
            stackId="prefix"
            fill="var(--color-new)"
            isAnimationActive={false}
            radius={[0, 4, 4, 0]}
          />
          <Bar
            dataKey="unknown"
            stackId="prefix"
            fill="var(--color-unknown)"
            isAnimationActive={false}
            radius={[0, 4, 4, 0]}
          />
        </BarChart>
      </ChartContainer>
      <div className="grid gap-2 text-xs text-muted-foreground">
        <ContextCompositionDetailRow
          label="Messages / tools"
          value={`${composition.message_count} / ${composition.tool_count}`}
        />
        <ContextCompositionDetailRow
          label="Tool schema"
          value={`${formatCompactNumber(composition.tools_schema_tokens)} tokens`}
        />
      </div>
    </div>
  );
}

function ContextCompositionMetric({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail: string;
}) {
  return (
    <div className="rounded-xl border bg-muted/20 p-2">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 font-mono text-lg font-medium tabular-nums text-foreground">
        {value}
      </div>
      <div className="truncate text-xs text-muted-foreground">{detail}</div>
    </div>
  );
}

function ContextCompositionDetailRow({
  label,
  value,
}: {
  label: string;
  value: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      <span className="min-w-0 flex-1">{label}</span>
      <span className="truncate font-mono font-medium tabular-nums text-foreground">
        {value}
      </span>
    </div>
  );
}

function PrimitiveOptimizationCard({
  summary,
  dragHandle,
}: {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
}) {
  const rows = useMemo(
    () =>
      sessionDashboardEntries(summary).map((entry) => {
        const progressData = primitiveOptimizationProgressData(entry.dashboard);
        const total = progressData.reduce((sum, item) => sum + item.value, 0);
        return { entry, progressData, total };
      }),
    [summary],
  );
  const visibleRows = rows.filter((row) => row.total > 0);

  if (visibleRows.length === 0) {
    return (
      <Card className="w-full">
        <CardHeader>
          <CardTitle>Primitive Optimization</CardTitle>
          <CardAction>{dragHandle}</CardAction>
        </CardHeader>
        <CardContent>
          <DashboardEmptyState
            title="No primitive optimization data"
            description="Optimization progress appears after primitive evidence is processed."
          />
        </CardContent>
      </Card>
    );
  }

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Primitive Optimization</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        <div className="flex flex-col gap-4">
          {visibleRows.map((row) => (
            <OptimizationSessionRow
              key={row.entry.session.session_id}
              label={sessionDisplayName(row.entry.session, row.entry.dashboard)}
              data={row.progressData}
              total={row.total}
              config={PRIMITIVE_OPTIMIZATION_CHART_CONFIG}
            />
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

function RuntimeOptimizationCard({
  summary,
  dragHandle,
}: {
  summary: StatusSummary | null;
  dragHandle: ReactNode;
}) {
  const rows = useMemo(
    () =>
      sessionDashboardEntries(summary).map((entry) => {
        const progressData = runtimeOptimizationProgressData(entry.dashboard);
        const total = progressData.reduce((sum, item) => sum + item.value, 0);
        return { entry, progressData, total };
      }),
    [summary],
  );
  const visibleRows = rows.filter((row) => row.total > 0);

  if (visibleRows.length === 0) {
    return (
      <Card className="w-full">
        <CardHeader>
          <CardTitle>Runtime Optimization</CardTitle>
          <CardAction>{dragHandle}</CardAction>
        </CardHeader>
        <CardContent>
          <DashboardEmptyState
            title="No runtime optimization data"
            description="Runtime optimization progress appears after error cases are processed."
          />
        </CardContent>
      </Card>
    );
  }

  return (
    <Card className="w-full">
      <CardHeader>
        <CardTitle>Runtime Optimization</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        <div className="flex flex-col gap-4">
          {visibleRows.map((row) => (
            <OptimizationSessionRow
              key={row.entry.session.session_id}
              label={sessionDisplayName(row.entry.session, row.entry.dashboard)}
              data={row.progressData}
              total={row.total}
              config={RUNTIME_OPTIMIZATION_CHART_CONFIG}
            />
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

function OptimizationSessionRow({
  label,
  data,
  total,
  config,
}: {
  label: string;
  data: OptimizationProgressDatum[];
  total: number;
  config: OptimizationChartConfig;
}) {
  return (
    <div className="flex flex-col gap-2">
      <div className="flex min-w-0 items-center justify-between gap-3 text-xs text-muted-foreground">
        <span className="truncate font-medium text-foreground">{label}</span>
        <span className="shrink-0 font-mono tabular-nums">
          {formatCompactNumber(total)} total
        </span>
      </div>
      <OptimizationProgressBar data={data} total={total} config={config} />
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <span>Queued</span>
        <ChevronRight className="size-3" />
        <span>Applied</span>
      </div>
    </div>
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
  return (summary?.sessions ?? [])
    .filter((entry) => entry.dashboard)
    .map((entry) => ({
      session: entry.session,
      dashboard: entry.dashboard as SessionStatusDashboard,
    }));
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

function sessionStatusBadgeVariant(
  status: SessionStatusTone,
): "default" | "secondary" | "destructive" | "outline" | "ghost" {
  switch (status) {
    case "attention":
      return "destructive";
    case "active":
      return "default";
    case "ready":
      return "secondary";
    case "available":
      return "outline";
  }
}

function sessionScopeLabel(session: SessionInfo) {
  if (session.scope.kind === "project") {
    return session.scope.project_dir;
  }
  return "general";
}

function statusCardColumns(order: StatusCardId[]) {
  const normalizedOrder = normalizeStatusCardOrder(order);
  const firstColumnLength = Math.ceil(normalizedOrder.length / 3);
  const secondColumnLength = Math.ceil(
    (normalizedOrder.length - firstColumnLength) / 2,
  );

  return [
    normalizedOrder.slice(0, firstColumnLength),
    normalizedOrder.slice(
      firstColumnLength,
      firstColumnLength + secondColumnLength,
    ),
    normalizedOrder.slice(firstColumnLength + secondColumnLength),
  ];
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

function OptimizationProgressBar({
  data,
  total,
  config,
}: {
  data: OptimizationProgressDatum[];
  total: number;
  config: OptimizationChartConfig;
}) {
  const visibleItems = data.filter((item) => item.value > 0);

  if (total <= 0 || visibleItems.length === 0) {
    return null;
  }

  return (
    <div className="flex flex-col gap-2">
      <div
        className="flex h-10 w-full overflow-hidden rounded-md border bg-muted/30"
        role="img"
        aria-label={visibleItems
          .map(
            (item) =>
              `${item.label} ${formatCompactNumber(item.value)}: ${item.detail}`,
          )
          .join("; ")}
      >
        {visibleItems.map((item) => {
          const color = config[item.colorKey]?.color ?? "var(--muted)";
          const width = `${(item.value / total) * 100}%`;

          return (
            <div
              key={item.key}
              aria-hidden="true"
              className="min-w-[2px]"
              style={{ width, backgroundColor: color }}
            />
          );
        })}
      </div>
      <div className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {visibleItems.map((item) => {
          const color = config[item.colorKey]?.color ?? "var(--muted)";

          return (
            <div key={item.key} className="flex min-w-0 items-center gap-1.5">
              <span
                aria-hidden="true"
                className="h-2 w-2 flex-none rounded-full"
                style={{ backgroundColor: color }}
              />
              <span className="min-w-0 flex-1 truncate">{item.label}</span>
              <span className="font-mono tabular-nums text-foreground">
                {formatCompactNumber(item.value)}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
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
      {datum.models.length ? (
        <div className="grid gap-2 border-t pt-2">
          {datum.models.map((model) => (
            <div key={model.key} className="grid gap-1">
              <div className="truncate font-medium text-foreground">
                {model.label}
              </div>
              <div className="grid gap-1 text-muted-foreground">
                <TokenUsageTooltipRow
                  label="Total"
                  value={model.usage.total_tokens}
                />
                <TokenUsageTooltipRow
                  label="Input"
                  value={model.usage.input_tokens}
                />
                <TokenUsageTooltipRow
                  label="Cached"
                  value={model.usage.cached_input_tokens}
                />
                <TokenUsageTooltipRow
                  label="Output"
                  value={model.usage.output_tokens}
                />
                <TokenUsageTooltipRow
                  label="Reasoning"
                  value={model.usage.reasoning_output_tokens}
                />
              </div>
            </div>
          ))}
        </div>
      ) : null}
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

function ContextPrefixTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: ContextPrefixTooltipPayloadItem[];
}) {
  if (!active) {
    return null;
  }

  const datum = payload?.[0]?.payload;
  if (!datum) {
    return null;
  }

  return (
    <div className="grid min-w-64 gap-1.5 rounded-lg border bg-background px-3 py-2.5 text-xs shadow-xl">
      <div className="font-medium text-foreground">
        Cache-affecting prefix comparison
      </div>
      <div className="grid gap-1 text-muted-foreground">
        {datum.bars.map((bar) => (
          <div key={bar.key} className="flex min-w-0 items-center gap-2">
            <span
              className="size-2 shrink-0 rounded-[2px]"
              style={{ backgroundColor: `var(--color-${bar.colorKey})` }}
            />
            <span className="min-w-0 flex-1 truncate">{bar.label}</span>
            <span className="font-mono font-medium tabular-nums text-foreground">
              {formatCompactNumber(bar.tokens)}
            </span>
            <span className="font-mono tabular-nums">
              {formatPercent(bar.ratio)}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function telegramApprovalDisplayName(request: DashboardPendingAccessRequest) {
  return request.sender || request.title || "Unknown";
}

function telegramApprovalInitials(value: string) {
  const characters = Array.from(value.trim());
  return characters.slice(0, 2).join("").toUpperCase() || "TG";
}
