import { useEffect, useMemo, useState } from "react";

import {
  AgentStatusAnimation,
  type AgentAnimationStatus,
} from "@/components/agent-status-animation";
import { Bar, BarChart, Cell, LabelList, XAxis, YAxis } from "recharts";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  type ChartConfig,
} from "@/components/ui/chart";
import {
  subscribeDashboardSnapshots,
  type DashboardSnapshot,
  type TokenUsage,
  type TokenUsageInfo,
} from "@/lib/daemon-api";

const DASHBOARD_STREAM_RECONNECT_MS = 1500;
const SUMMARY_TYPE_INTERVAL_MS = 28;
const TOKEN_USAGE_MAX_VISIBLE_DAYS = 7;
const TOKEN_USAGE_CHART_CONFIG = {
  cached: {
    label: "Cached",
    color: "var(--chart-1)",
  },
  uncached: {
    label: "Usage",
    color: "var(--chart-2)",
  },
} satisfies ChartConfig;
const WORKFLOW_OPTIMIZATION_CHART_CONFIG = {
  queued: {
    label: "Queued",
    color: "var(--chart-1)",
  },
  evidence: {
    label: "Evidence",
    color: "var(--chart-2)",
  },
  reflections: {
    label: "Reflections",
    color: "var(--chart-3)",
  },
  candidates: {
    label: "Candidates",
    color: "var(--chart-4)",
  },
  evaluations: {
    label: "Evaluations",
    color: "var(--chart-5)",
  },
  applied: {
    label: "Applied",
    color: "var(--chart-1)",
  },
} satisfies ChartConfig;

type AgentStatusView = {
  animationStatus: AgentAnimationStatus;
  label: string;
};

export function StatusPage() {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [loadError, setLoadError] = useState<Error | null>(null);

  useEffect(() => {
    let isActive = true;
    let reconnectTimeout: number | undefined;
    let subscription: ReturnType<typeof subscribeDashboardSnapshots> | null = null;

    function connect() {
      try {
        subscription = subscribeDashboardSnapshots({
          onSnapshot: (nextSnapshot) => {
            if (!isActive) {
              return;
            }

            setSnapshot(nextSnapshot);
            setLoadError(null);
            setIsLoading(false);
          },
          onError: (error) => {
            if (!isActive) {
              return;
            }

            setLoadError(error);
            setIsLoading(false);
          },
          onClose: (event) => {
            if (!isActive) {
              return;
            }

            subscription = null;
            if (event.code !== 1000) {
              setLoadError(
                new Error(
                  `Dashboard stream closed unexpectedly (${event.code || "unknown"}).`,
                ),
              );
              setIsLoading(false);
              reconnectTimeout = window.setTimeout(
                connect,
                DASHBOARD_STREAM_RECONNECT_MS,
              );
            }
          },
        });
      } catch (error) {
        if (!isActive) {
          return;
        }

        setLoadError(error instanceof Error ? error : new Error(String(error)));
        setIsLoading(false);
        reconnectTimeout = window.setTimeout(connect, DASHBOARD_STREAM_RECONNECT_MS);
      }
    }

    connect();

    return () => {
      isActive = false;
      if (reconnectTimeout !== undefined) {
        window.clearTimeout(reconnectTimeout);
      }
      subscription?.close();
    };
  }, []);

  const agentStatus = deriveAgentStatus({
    hasLoadError: Boolean(loadError),
    isLoading,
    snapshot,
  });
  const summaryText = derivePlanSummaryText(snapshot);
  const { isTyping, text: typedSummaryText } = useTypewriterText(summaryText);

  return (
    <section
      id="status"
      className="h-[calc(100vh-4rem)] w-full snap-y snap-mandatory overflow-y-auto overscroll-contain scroll-smooth"
    >
      <div className="flex min-h-full snap-start items-center justify-center px-6 py-10">
        <div className="flex flex-col items-center justify-center gap-5 text-center">
          <AgentStatusAnimation
            status={agentStatus.animationStatus}
            className="w-64 md:w-80"
          />
          <p
            aria-live="polite"
            className="min-h-6 max-w-[min(32rem,calc(100vw-3rem))] text-balance text-sm font-medium leading-6 text-muted-foreground md:text-base"
          >
            {typedSummaryText ? (
              <>
                <span>{typedSummaryText}</span>
                {isTyping ? (
                  <span
                    aria-hidden="true"
                    className="ml-0.5 inline-block h-4 w-px translate-y-0.5 bg-muted-foreground/70 motion-reduce:hidden"
                  />
                ) : null}
              </>
            ) : null}
          </p>
          <span
            aria-live="polite"
            className="sr-only"
          >
            {agentStatus.label}
          </span>
        </div>
      </div>
      <div
        className="min-h-full w-full snap-start px-6 py-10 md:py-12"
      >
        <div className="grid w-full grid-cols-1 items-start gap-4 sm:grid-cols-2 xl:grid-cols-3">
          <DailyTokenUsageCard snapshot={snapshot} />
          <WorkflowOptimizationCard snapshot={snapshot} />
        </div>
      </div>
    </section>
  );
}

function DailyTokenUsageCard({
  snapshot,
}: {
  snapshot: DashboardSnapshot | null;
}) {
  const chartData = useMemo(() => dailyTokenUsageChartData(snapshot), [snapshot]);
  const hasUsage = chartData.some((day) => day.total > 0);

  return (
    <Card className="overflow-visible">
      <CardHeader>
        <CardTitle>Token Usage</CardTitle>
      </CardHeader>
      <CardContent>
        <ChartContainer
          config={TOKEN_USAGE_CHART_CONFIG}
          className="h-64 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
        >
          <BarChart
            accessibilityLayer
            data={chartData}
            margin={{ top: 18, right: 16, left: 0, bottom: 0 }}
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
              width={52}
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
          <p className="mt-2 text-xs text-muted-foreground">
            No token usage recorded yet.
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function WorkflowOptimizationCard({
  snapshot,
}: {
  snapshot: DashboardSnapshot | null;
}) {
  const progressData = useMemo(
    () => workflowOptimizationProgressData(snapshot),
    [snapshot],
  );

  return (
    <Card className="overflow-visible">
      <CardHeader>
        <CardTitle>Workflow Optimization</CardTitle>
      </CardHeader>
      <CardContent>
        <ChartContainer
          config={WORKFLOW_OPTIMIZATION_CHART_CONFIG}
          className="h-64 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
        >
          <BarChart
            accessibilityLayer
            data={progressData}
            layout="vertical"
            margin={{ top: 8, right: 36, left: 8, bottom: 0 }}
            barCategoryGap={12}
          >
            <XAxis
              type="number"
              hide
              domain={[0, "dataMax"]}
            />
            <YAxis
              dataKey="label"
              type="category"
              width={92}
              tickLine={false}
              axisLine={false}
              tickMargin={8}
            />
            <ChartTooltip
              allowEscapeViewBox={{ y: true }}
              cursor={{ fill: "var(--muted)" }}
              wrapperStyle={{ zIndex: 50 }}
              content={<WorkflowOptimizationTooltip />}
            />
            <Bar
              dataKey="value"
              radius={[0, 4, 4, 0]}
              isAnimationActive={false}
            >
              {progressData.map((item) => (
                <Cell
                  key={item.key}
                  fill={`var(--color-${item.colorKey})`}
                />
              ))}
              <LabelList
                dataKey="value"
                position="right"
                formatter={(value) => formatCompactNumber(Number(value ?? 0))}
                className="fill-foreground font-mono text-[10px] font-medium"
              />
            </Bar>
          </BarChart>
        </ChartContainer>
      </CardContent>
    </Card>
  );
}

type DailyTokenUsageChartDatum = {
  date: string;
  label: string;
  cached: number;
  cachedRatio: number;
  uncached: number;
  uncachedRatio: number;
  total: number;
  models: DailyTokenUsageModelBreakdown[];
};

type DailyTokenUsageModelBreakdown = {
  key: string;
  label: string;
  usage: TokenUsage;
};

type TokenUsageTooltipPayloadItem = {
  payload?: DailyTokenUsageChartDatum;
};

type WorkflowOptimizationChartDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: keyof typeof WORKFLOW_OPTIMIZATION_CHART_CONFIG;
  detail: string;
};

type WorkflowOptimizationTooltipPayloadItem = {
  payload?: WorkflowOptimizationChartDatum;
};

function dailyTokenUsageChartData(
  snapshot: DashboardSnapshot | null,
): DailyTokenUsageChartDatum[] {
  const usageByDate = new Map<string, DailyTokenUsageAccumulator>();

  for (const source of tokenUsageSources(snapshot)) {
    mergeDailyTokenUsage(usageByDate, source);
  }

  const dates = recentTokenUsageDates(usageByDate);
  const maxTotal = Math.max(
    1,
    ...dates.map((date) => usageByDate.get(date)?.total ?? 0),
  );

  return dates.map((date, index) => {
    const accumulator =
      usageByDate.get(date) ?? createDailyTokenUsageAccumulator();

    return {
      date,
      label:
        index === 0 || index === dates.length - 1 ? formatDateLabel(date) : "",
      cached: accumulator.cached,
      cachedRatio: accumulator.cached / maxTotal,
      uncached: accumulator.uncached,
      uncachedRatio: accumulator.uncached / maxTotal,
      total: accumulator.total,
      models: Array.from(accumulator.models.entries())
        .map(([key, usage]) => ({
          key,
          label: key,
          usage,
        }))
        .sort((left, right) => right.usage.total_tokens - left.usage.total_tokens),
    };
  });
}

type DailyTokenUsageAccumulator = {
  cached: number;
  uncached: number;
  total: number;
  models: Map<string, TokenUsage>;
};

type TokenUsageSource = {
  label: string;
  info: TokenUsageInfo | null | undefined;
};

function tokenUsageSources(snapshot: DashboardSnapshot | null): TokenUsageSource[] {
  const tokenUsage = snapshot?.token_usage;

  return [
    {
      label: tokenUsageModelLabel("main", tokenUsage?.main_model),
      info: tokenUsage?.main,
    },
    {
      label: tokenUsageModelLabel("judge", tokenUsage?.judge_model),
      info: tokenUsage?.judge,
    },
  ];
}

function tokenUsageModelLabel(role: string, model: string | null | undefined) {
  const normalizedModel = model?.trim();

  return normalizedModel || role;
}

function mergeDailyTokenUsage(
  usageByDate: Map<string, DailyTokenUsageAccumulator>,
  source: TokenUsageSource,
) {
  for (const day of source.info?.daily_token_usage ?? []) {
    const accumulator =
      usageByDate.get(day.date) ?? createDailyTokenUsageAccumulator();
    const usage = normalizedTokenUsage(day.usage);
    const cachedTokens = Math.min(
      usage.cached_input_tokens,
      usage.total_tokens,
    );
    const existingModelUsage =
      accumulator.models.get(source.label) ?? createEmptyTokenUsage();

    accumulator.cached += cachedTokens;
    accumulator.uncached += Math.max(0, usage.total_tokens - cachedTokens);
    accumulator.total += usage.total_tokens;
    accumulator.models.set(source.label, addTokenUsage(existingModelUsage, usage));
    usageByDate.set(day.date, accumulator);
  }
}

function recentTokenUsageDates(
  usageByDate: Map<string, DailyTokenUsageAccumulator>,
) {
  return Array.from(usageByDate.keys())
    .filter(isDateKey)
    .sort()
    .slice(-TOKEN_USAGE_MAX_VISIBLE_DAYS);
}

function createDailyTokenUsageAccumulator(): DailyTokenUsageAccumulator {
  return {
    cached: 0,
    uncached: 0,
    total: 0,
    models: new Map<string, TokenUsage>(),
  };
}

function createEmptyTokenUsage(): TokenUsage {
  return {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: 0,
  };
}

function normalizedTokenUsage(usage: TokenUsage): TokenUsage {
  return {
    input_tokens: Math.max(0, usage.input_tokens),
    cached_input_tokens: Math.max(0, usage.cached_input_tokens),
    output_tokens: Math.max(0, usage.output_tokens),
    reasoning_output_tokens: Math.max(0, usage.reasoning_output_tokens),
    total_tokens: Math.max(0, usage.total_tokens),
  };
}

function addTokenUsage(left: TokenUsage, right: TokenUsage): TokenUsage {
  return {
    input_tokens: left.input_tokens + right.input_tokens,
    cached_input_tokens: left.cached_input_tokens + right.cached_input_tokens,
    output_tokens: left.output_tokens + right.output_tokens,
    reasoning_output_tokens:
      left.reasoning_output_tokens + right.reasoning_output_tokens,
    total_tokens: left.total_tokens + right.total_tokens,
  };
}

function workflowOptimizationProgressData(
  snapshot: DashboardSnapshot | null,
): WorkflowOptimizationChartDatum[] {
  const workflow = snapshot?.workflow_optimization;
  const patchCandidates = Math.max(
    0,
    workflow?.total_workflow_patch_candidates ?? 0,
  );
  const mergeCandidates = Math.max(
    0,
    workflow?.total_workflow_merge_candidates ?? 0,
  );
  const patchApplied = Math.max(0, workflow?.total_workflow_patch_applied ?? 0);
  const mergeApplied = Math.max(0, workflow?.total_workflow_merge_applied ?? 0);

  return [
    {
      key: "queued",
      label: "Queued",
      value: Math.max(0, workflow?.workflow_evidence_records ?? 0),
      colorKey: "queued",
      detail: "Workflow evidence waiting for sleep-time review",
    },
    {
      key: "evidence",
      label: "Evidence",
      value: Math.max(0, workflow?.total_workflow_evidence_run_records ?? 0),
      colorKey: "evidence",
      detail: "Workflow run records consumed by optimization",
    },
    {
      key: "reflections",
      label: "Reflect",
      value: Math.max(0, workflow?.total_workflow_reflections ?? 0),
      colorKey: "reflections",
      detail: "Generated workflow reflections",
    },
    {
      key: "candidates",
      label: "Candidates",
      value: patchCandidates + mergeCandidates,
      colorKey: "candidates",
      detail: `${formatCompactNumber(patchCandidates)} patches · ${formatCompactNumber(
        mergeCandidates,
      )} merges`,
    },
    {
      key: "evaluations",
      label: "Evaluate",
      value: Math.max(0, workflow?.total_workflow_candidate_evaluations ?? 0),
      colorKey: "evaluations",
      detail: "Workflow patch/merge candidate evaluations",
    },
    {
      key: "applied",
      label: "Applied",
      value: patchApplied + mergeApplied,
      colorKey: "applied",
      detail: `${formatCompactNumber(patchApplied)} patches · ${formatCompactNumber(
        mergeApplied,
      )} merges`,
    },
  ];
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
          <TokenUsageTooltipRow
            label="Total"
            value={datum.total}
          />
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
            <div
              key={model.key}
              className="grid gap-1"
            >
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

function WorkflowOptimizationTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: WorkflowOptimizationTooltipPayloadItem[];
}) {
  if (!active) {
    return null;
  }

  const datum = payload?.[0]?.payload;
  if (!datum) {
    return null;
  }

  return (
    <div className="grid min-w-56 gap-1.5 rounded-lg border bg-background px-3 py-2.5 text-xs shadow-xl">
      <div className="flex items-center gap-2 font-medium text-foreground">
        <span
          className="size-2 shrink-0 rounded-[2px]"
          style={{ backgroundColor: `var(--color-${datum.colorKey})` }}
        />
        <span>{datum.label}</span>
        <span className="ml-auto font-mono tabular-nums">
          {formatCompactNumber(datum.value)}
        </span>
      </div>
      <div className="text-muted-foreground">{datum.detail}</div>
    </div>
  );
}

function formatDateLabel(date: string) {
  const [, month, day] = date.match(/^(\d{4})-(\d{2})-(\d{2})$/) ?? [];

  if (!month || !day) {
    return date;
  }

  return `${Number(month)}月${Number(day)}日`;
}

function formatPercentAxisTick(value: number) {
  return `${Math.round(value * 100)}%`;
}

function isDateKey(value: string) {
  return parseDateKey(value) !== null;
}

function parseDateKey(value: string) {
  const [, year, month, day] = value.match(/^(\d{4})-(\d{2})-(\d{2})$/) ?? [];

  if (!year || !month || !day) {
    return null;
  }

  return new Date(Number(year), Number(month) - 1, Number(day));
}

function formatCompactNumber(value: number) {
  return new Intl.NumberFormat("en", {
    compactDisplay: "short",
    maximumFractionDigits: value >= 1000 ? 1 : 0,
    notation: "compact",
  }).format(value);
}

function useTypewriterText(text: string) {
  const characters = useMemo(() => Array.from(text), [text]);
  const [visibleCharacters, setVisibleCharacters] = useState(0);

  useEffect(() => {
    setVisibleCharacters(0);

    if (characters.length === 0) {
      return;
    }

    let nextLength = 0;
    const intervalId = window.setInterval(() => {
      nextLength += 1;
      setVisibleCharacters(nextLength);

      if (nextLength >= characters.length) {
        window.clearInterval(intervalId);
      }
    }, SUMMARY_TYPE_INTERVAL_MS);

    return () => window.clearInterval(intervalId);
  }, [characters]);

  return {
    isTyping: visibleCharacters < characters.length,
    text: characters.slice(0, visibleCharacters).join(""),
  };
}

function derivePlanSummaryText(snapshot: DashboardSnapshot | null) {
  const planStep = snapshot?.current_plan_step;

  if (!planStep?.step.trim()) {
    return "";
  }

  const prefix = planStep.status === "pending" ? "下一步" : "正在";

  return `${prefix}：${planStep.step.trim()}`;
}

function deriveAgentStatus({
  hasLoadError,
  isLoading,
  snapshot,
}: {
  hasLoadError: boolean;
  isLoading: boolean;
  snapshot: DashboardSnapshot | null;
}): AgentStatusView {
  if (isLoading && !snapshot) {
    return { animationStatus: "waiting", label: "加载中" };
  }

  if (hasLoadError && !snapshot) {
    return { animationStatus: "waiting", label: "状态不可用" };
  }

  if (!snapshot?.runtime_status) {
    return { animationStatus: "idle", label: "空闲" };
  }

  const runtimeStatus = snapshot.runtime_status.toLowerCase();
  const dashboardText = [snapshot.runtime_status, snapshot.status_output]
    .join(" ")
    .toLowerCase();

  if (/\b(error|failed|failure|panic)\b/.test(dashboardText)) {
    return { animationStatus: "error", label: "异常" };
  }

  if (/\b(waiting|backlog|pending|sleep)\b/.test(runtimeStatus)) {
    return { animationStatus: "waiting", label: "等待中" };
  }

  if (
    snapshot.focused_app &&
    /\b(action|app|browser|terminal|tool)\b/.test(dashboardText)
  ) {
    return { animationStatus: "tooling", label: "调用工具" };
  }

  if (/\b(compacting|context|model|reason|thinking|working)\b/.test(dashboardText)) {
    return { animationStatus: "thinking", label: "思考中" };
  }

  return { animationStatus: "running", label: "执行中" };
}
