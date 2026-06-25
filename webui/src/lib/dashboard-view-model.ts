import type { AgentExpressionStatus } from "@/components/agent-expression";
import type {
  DashboardContextCompositionSnapshot,
  DashboardContextCompositionSegment,
  DashboardPrimitiveOptimizationSnapshot,
  DashboardRuntimeOptimizationSnapshot,
  DashboardSnapshot,
  DashboardTokenUsageSnapshot,
  TokenUsage,
  TokenUsageInfo,
} from "@/lib/daemon-api";

export const TOKEN_USAGE_MAX_VISIBLE_DAYS = 7;
export const CONTEXT_COMPOSITION_MAX_VISIBLE_SEGMENTS = 8;

export const TOKEN_USAGE_CHART_CONFIG = {
  cached: {
    label: "Cached",
    color: "var(--chart-1)",
  },
  uncached: {
    label: "Usage",
    color: "var(--chart-2)",
  },
};

export const CONTEXT_COMPOSITION_CHART_CONFIG = {
  tokens: {
    label: "Tokens",
    color: "var(--chart-1)",
  },
  stable: {
    label: "Stable prefix",
    color: "var(--chart-1)",
  },
  changed: {
    label: "Changed prefix",
    color: "var(--chart-4)",
  },
  new: {
    label: "New suffix",
    color: "var(--chart-2)",
  },
  unknown: {
    label: "First request / no previous snapshot",
    color: "var(--muted)",
  },
};

export const PRIMITIVE_OPTIMIZATION_CHART_CONFIG = {
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
  empty: {
    label: "No data",
    color: "var(--muted)",
  },
};

export const RUNTIME_OPTIMIZATION_CHART_CONFIG = {
  queued: {
    label: "Queued",
    color: "var(--chart-1)",
  },
  consumed: {
    label: "Consumed",
    color: "var(--chart-2)",
  },
  cases: {
    label: "Cases",
    color: "var(--chart-3)",
  },
  reflections: {
    label: "Reflections",
    color: "var(--chart-4)",
  },
  candidates: {
    label: "Candidates",
    color: "var(--chart-5)",
  },
  evaluations: {
    label: "Evaluations",
    color: "var(--chart-1)",
  },
  applied: {
    label: "Applied",
    color: "var(--chart-2)",
  },
  empty: {
    label: "No data",
    color: "var(--muted)",
  },
};

export type AgentStatusView = {
  expressionStatus: AgentExpressionStatus;
  label: string;
};

export type AgentStatusInput = {
  isLoading: boolean;
  loadError: Error | null;
  snapshot: DashboardSnapshot | null;
};

export type DailyTokenUsageChartDatum = {
  date: string;
  label: string;
  cached: number;
  cachedRatio: number;
  uncached: number;
  uncachedRatio: number;
  total: number;
  models: DailyTokenUsageModelBreakdown[];
};

export type DailyTokenUsageModelBreakdown = {
  key: string;
  label: string;
  usage: TokenUsage;
};

export type ContextCompositionSegmentChartDatum = {
  key: string;
  label: string;
  shortLabel: string;
  source: string;
  tokens: number;
  bytes: number;
  percent: number;
  cacheRole: string;
};

export type ContextCompositionPrefixBar = {
  key: string;
  label: string;
  shortLabel: string;
  tokens: number;
  ratio: number;
  colorKey: keyof typeof CONTEXT_COMPOSITION_CHART_CONFIG;
};

export type ContextCompositionPrefixSummaryDatum = {
  label: string;
  stable: number;
  changed: number;
  new: number;
  unknown: number;
  bars: ContextCompositionPrefixBar[];
};

export type ContextCompositionCardData = {
  segmentChartData: ContextCompositionSegmentChartDatum[];
  maxSegmentTokens: number;
  stablePrefixRatio: number;
  newSuffixRatio: number;
  prefixSummaryData: ContextCompositionPrefixSummaryDatum[];
  prefixLegend: ContextCompositionPrefixBar[];
};

export type PrimitiveOptimizationChartDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: keyof typeof PRIMITIVE_OPTIMIZATION_CHART_CONFIG;
  detail: string;
};

export type PrimitiveOptimizationDonutDatum = PrimitiveOptimizationChartDatum & {
  chartValue: number;
};

export type RuntimeOptimizationChartDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: keyof typeof RUNTIME_OPTIMIZATION_CHART_CONFIG;
  detail: string;
};

export type RuntimeOptimizationDonutDatum = RuntimeOptimizationChartDatum & {
  chartValue: number;
};

export function derivePlanSummaryText(snapshot: DashboardSnapshot | null) {
  const planStep = snapshot?.current_plan_step;

  if (!planStep?.step.trim()) {
    return "";
  }

  const prefix = planStep.status === "pending" ? "Next" : "Now";

  return `${prefix}: ${planStep.step.trim()}`;
}

export function deriveAgentStatus({
  isLoading,
  loadError,
  snapshot,
}: AgentStatusInput): AgentStatusView {
  if (isLoading && !snapshot) {
    return { expressionStatus: "waiting", label: "Loading" };
  }

  if (loadError && !snapshot) {
    return { expressionStatus: "waiting", label: "Status unavailable" };
  }

  if (!snapshot) {
    return { expressionStatus: "idle", label: "Idle" };
  }

  return {
    expressionStatus: snapshot.runtime_activity?.status ?? "idle",
    label: snapshot.runtime_activity?.label ?? "Idle",
  };
}

type TokenUsageSnapshotInput = {
  token_usage?: DashboardTokenUsageSnapshot;
} | null;

type ContextCompositionSnapshotInput = {
  context_composition?: DashboardContextCompositionSnapshot | null;
} | null;

type PrimitiveOptimizationSnapshotInput = {
  primitive_optimization?: DashboardPrimitiveOptimizationSnapshot;
} | null;

type RuntimeOptimizationSnapshotInput = {
  runtime_optimization?: DashboardRuntimeOptimizationSnapshot;
} | null;

export type DailyTokenUsageSource = {
  label: string;
  info: TokenUsageInfo | null | undefined;
};

export function dailyTokenUsageChartData(
  snapshot: TokenUsageSnapshotInput,
): DailyTokenUsageChartDatum[] {
  return dailyTokenUsageChartDataFromSources(tokenUsageSources(snapshot));
}

export function dailyTokenUsageChartDataFromSources(
  sources: DailyTokenUsageSource[],
): DailyTokenUsageChartDatum[] {
  const usageByDate = new Map<string, DailyTokenUsageAccumulator>();

  for (const source of sources) {
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
        .sort(
          (left, right) => right.usage.total_tokens - left.usage.total_tokens,
        ),
    };
  });
}

export function contextCompositionCardData(
  snapshot: ContextCompositionSnapshotInput,
): ContextCompositionCardData {
  const composition = snapshot?.context_composition;
  const totalTokens = Math.max(0, composition?.total_estimated_tokens ?? 0);
  const stablePrefixTokens = Math.max(
    0,
    composition?.stable_prefix_tokens ?? 0,
  );
  const changedPrefixTokens = Math.max(
    0,
    composition?.changed_prefix_tokens ?? 0,
  );
  const newSuffixTokens = Math.max(0, composition?.new_suffix_tokens ?? 0);
  const knownPrefixTokens =
    stablePrefixTokens + changedPrefixTokens + newSuffixTokens;
  const unknownTokens =
    composition && knownPrefixTokens === 0 ? Math.max(1, totalTokens) : 0;
  const prefixTotal = Math.max(1, knownPrefixTokens || unknownTokens);
  const prefixLegend: ContextCompositionPrefixBar[] = [
    {
      key: "stable",
      label: "Stable prefix",
      shortLabel: "Stable",
      tokens: stablePrefixTokens,
      ratio: stablePrefixTokens / prefixTotal,
      colorKey: "stable",
    },
    {
      key: "changed",
      label: "Changed prefix",
      shortLabel: "Changed",
      tokens: changedPrefixTokens,
      ratio: changedPrefixTokens / prefixTotal,
      colorKey: "changed",
    },
    {
      key: "new",
      label: "New suffix",
      shortLabel: "New",
      tokens: newSuffixTokens,
      ratio: newSuffixTokens / prefixTotal,
      colorKey: "new",
    },
  ];
  const prefixSummaryData: ContextCompositionPrefixSummaryDatum[] = [
    {
      label: "Prefix",
      stable: stablePrefixTokens,
      changed: changedPrefixTokens,
      new: newSuffixTokens,
      unknown: unknownTokens,
      bars:
        unknownTokens > 0
          ? [
              {
                key: "unknown",
                label: "No previous snapshot",
                shortLabel: "No previous",
                tokens: unknownTokens,
                ratio: 1,
                colorKey: "unknown",
              },
            ]
          : prefixLegend,
    },
  ];
  const segmentChartData = contextCompositionSegmentChartData(
    composition?.segments ?? [],
  );
  const maxSegmentTokens = Math.max(
    1,
    ...segmentChartData.map((segment) => segment.tokens),
  );

  return {
    segmentChartData,
    maxSegmentTokens,
    stablePrefixRatio:
      totalTokens > 0 ? Math.min(1, stablePrefixTokens / totalTokens) : 0,
    newSuffixRatio:
      totalTokens > 0 ? Math.min(1, newSuffixTokens / totalTokens) : 0,
    prefixSummaryData,
    prefixLegend,
  };
}

export function primitiveOptimizationProgressData(
  snapshot: PrimitiveOptimizationSnapshotInput,
): PrimitiveOptimizationChartDatum[] {
  const primitive = snapshot?.primitive_optimization;
  const patchCandidates = Math.max(
    0,
    primitive?.total_primitive_patch_candidates ?? 0,
  );
  const mergeCandidates = Math.max(
    0,
    primitive?.total_primitive_merge_candidates ?? 0,
  );
  const patchApplied = Math.max(0, primitive?.total_primitive_patch_applied ?? 0);
  const mergeApplied = Math.max(0, primitive?.total_primitive_merge_applied ?? 0);

  return [
    {
      key: "queued",
      label: "Queued",
      value: Math.max(0, primitive?.primitive_evidence_records ?? 0),
      colorKey: "queued",
      detail: "Primitive evidence waiting for sleep-time review",
    },
    {
      key: "evidence",
      label: "Evidence",
      value: Math.max(0, primitive?.total_primitive_evidence_run_records ?? 0),
      colorKey: "evidence",
      detail: "Primitive run records consumed by optimization",
    },
    {
      key: "reflections",
      label: "Reflect",
      value: Math.max(0, primitive?.total_primitive_reflections ?? 0),
      colorKey: "reflections",
      detail: "Generated primitive reflections",
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
      value: Math.max(0, primitive?.total_primitive_candidate_evaluations ?? 0),
      colorKey: "evaluations",
      detail: "Primitive patch/merge candidate evaluations",
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

export function primitiveOptimizationDonutData(
  progressData: PrimitiveOptimizationChartDatum[],
): PrimitiveOptimizationDonutDatum[] {
  const activeData = progressData
    .filter((item) => item.value > 0)
    .map((item) => ({
      ...item,
      chartValue: item.value,
    }));

  if (activeData.length > 0) {
    return activeData;
  }

  return [
    {
      key: "empty",
      label: "No data",
      value: 0,
      chartValue: 1,
      colorKey: "empty",
      detail: "No primitive optimization activity yet",
    },
  ];
}

export function runtimeOptimizationProgressData(
  snapshot: RuntimeOptimizationSnapshotInput,
): RuntimeOptimizationChartDatum[] {
  const runtime = snapshot?.runtime_optimization;
  const appliedAdditions = Math.max(
    0,
    runtime?.total_runtime_contract_system_additions ?? 0,
  );
  const compiledUpdates = Math.max(
    0,
    runtime?.total_runtime_contract_updates ?? 0,
  );

  return [
    {
      key: "queued",
      label: "Queued",
      value: Math.max(0, runtime?.unread_runtime_error_backlog ?? 0),
      colorKey: "queued",
      detail: "Runtime error cases waiting for sleep-time review",
    },
    {
      key: "consumed",
      label: "Consumed",
      value: Math.max(0, runtime?.total_runtime_error_cases_consumed ?? 0),
      colorKey: "consumed",
      detail: "Runtime error cases consumed by optimization",
    },
    {
      key: "cases",
      label: "Cases",
      value: Math.max(0, runtime?.total_runtime_error_cases ?? 0),
      colorKey: "cases",
      detail: "Runtime error cases analyzed",
    },
    {
      key: "reflections",
      label: "Reflect",
      value: Math.max(0, runtime?.total_runtime_error_reflections ?? 0),
      colorKey: "reflections",
      detail: "Generated runtime error reflections",
    },
    {
      key: "candidates",
      label: "Candidates",
      value: Math.max(0, runtime?.total_runtime_contract_candidates ?? 0),
      colorKey: "candidates",
      detail: "Runtime contract correction candidates",
    },
    {
      key: "evaluations",
      label: "Evaluate",
      value: Math.max(
        0,
        runtime?.total_runtime_contract_candidate_evaluations ?? 0,
      ),
      colorKey: "evaluations",
      detail: "Runtime contract candidate evaluations",
    },
    {
      key: "applied",
      label: "Applied",
      value: appliedAdditions + compiledUpdates,
      colorKey: "applied",
      detail: `${formatCompactNumber(appliedAdditions)} additions · ${formatCompactNumber(
        compiledUpdates,
      )} updates`,
    },
  ];
}

export function runtimeOptimizationDonutData(
  progressData: RuntimeOptimizationChartDatum[],
): RuntimeOptimizationDonutDatum[] {
  const activeData = progressData
    .filter((item) => item.value > 0)
    .map((item) => ({
      ...item,
      chartValue: item.value,
    }));

  if (activeData.length > 0) {
    return activeData;
  }

  return [
    {
      key: "empty",
      label: "No data",
      value: 0,
      chartValue: 1,
      colorKey: "empty",
      detail: "No runtime optimization activity yet",
    },
  ];
}

export function formatDateLabel(date: string) {
  const parsedDate = parseDateKey(date);
  if (!parsedDate) {
    return date;
  }

  return new Intl.DateTimeFormat("en", {
    day: "numeric",
    month: "short",
  }).format(parsedDate);
}

export function formatPercentAxisTick(value: number) {
  return `${Math.round(value * 100)}%`;
}

export function formatPercent(value: number) {
  return new Intl.NumberFormat("en", {
    maximumFractionDigits: value >= 0.1 ? 0 : 1,
    style: "percent",
  }).format(Number.isFinite(value) ? value : 0);
}

export function formatCompactNumber(value: number) {
  return new Intl.NumberFormat("en", {
    compactDisplay: "short",
    maximumFractionDigits: value >= 1000 ? 1 : 0,
    notation: "compact",
  }).format(value);
}

function contextCompositionSegmentChartData(
  segments: DashboardContextCompositionSegment[],
): ContextCompositionSegmentChartDatum[] {
  const grouped = new Map<string, ContextCompositionSegmentChartDatum>();

  for (const segment of segments) {
    const key = segment.name || segment.label || segment.source || "unknown";
    const current =
      grouped.get(key) ??
      ({
        key,
        label: segment.label || key,
        shortLabel: shortContextCompositionLabel(segment.label || key),
        source: segment.source,
        tokens: 0,
        bytes: 0,
        percent: 0,
        cacheRole: segment.cache_role,
      } satisfies ContextCompositionSegmentChartDatum);

    current.tokens += Math.max(0, segment.tokens);
    current.bytes += Math.max(0, segment.bytes);
    current.percent += Math.max(0, segment.percent);
    grouped.set(key, current);
  }

  return Array.from(grouped.values())
    .sort((left, right) => right.tokens - left.tokens)
    .slice(0, CONTEXT_COMPOSITION_MAX_VISIBLE_SEGMENTS);
}

function shortContextCompositionLabel(label: string) {
  return label
    .replace(/^Assistant tool-call protocol$/, "Tool-call protocol")
    .replace(/^System messages$/, "System")
    .replace(/^Conversation history$/, "History")
    .replace(/^Summarized history$/, "Summary")
    .replace(/^Afterclaim context$/, "Afterclaim")
    .replace(/^Preturn context$/, "Preturn")
    .replace(/^Memory recall$/, "Memory")
    .replace(/^Assistant messages$/, "Assistant")
    .replace(/^Tool outputs$/, "Tool outputs")
    .replace(/^Tools schema$/, "Tools");
}

type DailyTokenUsageAccumulator = {
  cached: number;
  uncached: number;
  total: number;
  models: Map<string, TokenUsage>;
};

function tokenUsageSources(
  snapshot: TokenUsageSnapshotInput,
): DailyTokenUsageSource[] {
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
  source: DailyTokenUsageSource,
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
    accumulator.models.set(
      source.label,
      addTokenUsage(existingModelUsage, usage),
    );
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

function isDateKey(value: string) {
  return /^\d{4}-\d{2}-\d{2}$/.test(value);
}

function parseDateKey(value: string) {
  if (!isDateKey(value)) {
    return null;
  }

  const [year, month, day] = value.split("-").map(Number);
  const date = new Date(Date.UTC(year, month - 1, day));

  return Number.isNaN(date.getTime()) ? null : date;
}
