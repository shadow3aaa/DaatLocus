import { getActorUsage, listActors, type ActorStatus, type ActorSummary, type ActorUsage, type FetchOptions, type TokenUsage, type TokenUsageInfo } from "./actor-api";

/** One actor's recorded model usage: totals plus the last 30 local days. */
export type UsageEntry = { actor: ActorSummary; usage: ActorUsage };
export type UsageGrouping = "agent" | "model";
export type UsageDimension = UsageGrouping | "cache";
export const USAGE_RANGES = [7, 30] as const;
export type UsageRange = (typeof USAGE_RANGES)[number];

export const emptyTokenUsage = (): TokenUsage => ({
  input_tokens: 0, cached_input_tokens: 0, output_tokens: 0, reasoning_output_tokens: 0, total_tokens: 0,
});

export function addTokenUsage(total: TokenUsage, usage: TokenUsage): TokenUsage {
  return {
    input_tokens: total.input_tokens + usage.input_tokens,
    cached_input_tokens: total.cached_input_tokens + usage.cached_input_tokens,
    output_tokens: total.output_tokens + usage.output_tokens,
    reasoning_output_tokens: total.reasoning_output_tokens + usage.reasoning_output_tokens,
    total_tokens: total.total_tokens + usage.total_tokens,
  };
}

type UsageSubject = { id: string; name: string; status?: ActorStatus; description?: string; usage: TokenUsageInfo };

/** Historical records without model attribution stay separate from current settings. */
function usageSubjects(entries: UsageEntry[], grouping: UsageGrouping): UsageSubject[] {
  if (grouping === "agent") return entries.map(({ actor, usage }) => ({ ...actor, usage }));
  const subjects = new Map<string, UsageSubject>();
  const merge = (id: string, name: string, description: string | undefined, usage: TokenUsageInfo) => {
    const current = subjects.get(id);
    if (!current) {
      subjects.set(id, { id, name, description, usage });
      return;
    }
    const days = new Map(current.usage.daily_token_usage.map(day => [day.date, day.usage]));
    for (const day of usage.daily_token_usage) days.set(day.date, addTokenUsage(days.get(day.date) ?? emptyTokenUsage(), day.usage));
    current.usage = { ...current.usage,
      total_token_usage: addTokenUsage(current.usage.total_token_usage, usage.total_token_usage),
      daily_token_usage: [...days].map(([date, usage]) => ({ date, usage })),
    };
  };
  const remainder = (total: TokenUsage, known: TokenUsage) => Object.fromEntries(
    (Object.keys(total) as (keyof TokenUsage)[]).map(key => [key, Math.max(0, total[key] - known[key])]),
  ) as TokenUsage;
  for (const entry of entries) {
    const models = entry.usage.models ?? [];
    for (const model of models) merge(JSON.stringify([model.model, model.provider, model.model_id]), model.model,
      `${model.provider} · ${model.model_id}`, model);
    const total = remainder(entry.usage.total_token_usage, models.reduce((sum, model) => addTokenUsage(sum, model.total_token_usage), emptyTokenUsage()));
    const daily = entry.usage.daily_token_usage.map(day => ({ date: day.date, usage: remainder(day.usage,
      models.reduce((sum, model) => addTokenUsage(sum, model.daily_token_usage.find(item => item.date === day.date)?.usage ?? emptyTokenUsage()), emptyTokenUsage())) }));
    if (total.total_tokens > 0 || daily.some(day => day.usage.total_tokens > 0)) merge("unattributed", "", undefined, {
      total_token_usage: total, daily_token_usage: daily, last_token_usage: emptyTokenUsage(), model_context_window: null,
    });
  }
  return [...subjects.values()];
}

/** The backend stores days as local `YYYY-MM-DD` within a 30-day window. */
export function localDateKey(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

/** Local calendar days ending at `end`, oldest first. */
export function usageDates(days: number, end = new Date()): string[] {
  return Array.from({ length: days }, (_, index) => {
    const offset = days - 1 - index;
    return localDateKey(new Date(end.getFullYear(), end.getMonth(), end.getDate() - offset));
  });
}

/** `page` counts backwards from the current day; the newest page is 0. */
export function usageWindow(days: number, page: number, now = new Date()): string[] {
  return usageDates(days, new Date(now.getFullYear(), now.getMonth(), now.getDate() - page * days));
}

export function rangeUsage(entry: { usage: TokenUsageInfo }, dates: readonly string[]): TokenUsage {
  return dates.reduce((total, date) => {
    const usage = entry.usage.daily_token_usage.find(day => day.date === date)?.usage;
    return usage ? addTokenUsage(total, usage) : total;
  }, emptyTokenUsage());
}

/** Cached prompt tokens are reported inside `input_tokens`, so they set the ratio. */
export function cacheHitRate(usage: TokenUsage): number | null {
  if (usage.input_tokens <= 0) return null;
  return Math.min(1, usage.cached_input_tokens / usage.input_tokens);
}

export const uncachedInput = (usage: TokenUsage): number => Math.max(0, usage.input_tokens - usage.cached_input_tokens);

export type UsageSegment = { key: string; kind: UsageDimension; label: string; tokens: number; share: number };
export type UsageDay = { date: string; total: number; ratio: number; segments: UsageSegment[] };
type RawSegment = Omit<UsageSegment, "share">;

function segmentsFor(entry: UsageSubject, usage: TokenUsage, dimension: UsageDimension): RawSegment[] {
  if (dimension === "cache") {
    return [
      { key: "cached", kind: dimension, label: "", tokens: usage.cached_input_tokens },
      { key: "uncached", kind: dimension, label: "", tokens: uncachedInput(usage) },
    ];
  }
  return [{ key: entry.id, kind: dimension, label: entry.name, tokens: usage.total_tokens }];
}

/** Daily stacked bars plus each segment's share of that day and of the window. */
export function usageSeries(entries: UsageEntry[], dates: readonly string[], dimension: UsageDimension): UsageDay[] {
  const subjects = usageSubjects(entries, dimension === "model" ? "model" : "agent");
  const days = dates.map(date => {
    const byKey = new Map<string, RawSegment>();
    for (const entry of subjects) {
      const usage = entry.usage.daily_token_usage.find(day => day.date === date)?.usage;
      if (!usage) continue;
      for (const segment of segmentsFor(entry, usage, dimension)) {
        if (segment.tokens <= 0) continue;
        const current = byKey.get(segment.key);
        byKey.set(segment.key, current ? { ...current, tokens: current.tokens + segment.tokens } : segment);
      }
    }
    const segments = [...byKey.values()];
    const total = segments.reduce((sum, segment) => sum + segment.tokens, 0);
    return { date, total, segments };
  });
  const peak = Math.max(1, ...days.map(day => day.total));
  // Keep one stable segment order for bars and legend across the window.
  const windowed = new Map<string, number>();
  const labels = new Map<string, string>();
  for (const day of days) for (const segment of day.segments) {
    windowed.set(segment.key, (windowed.get(segment.key) ?? 0) + segment.tokens);
    if (segment.label) labels.set(segment.key, segment.label);
  }
  const order = [...windowed.entries()].sort((left, right) => right[1] - left[1]).map(([key]) => key);
  return days.map(day => ({
    date: day.date,
    total: day.total,
    ratio: day.total / peak,
    segments: order.map(key => {
      const segment = day.segments.find(item => item.key === key);
      return { key, kind: dimension, label: segment?.label || labels.get(key) || "", tokens: segment?.tokens ?? 0, share: day.total ? (segment?.tokens ?? 0) / day.total : 0 };
    }),
  }));
}

export type UsageLegendItem = { key: string; kind: UsageDimension; label: string; tokens: number; share: number };

export function usageLegend(series: UsageDay[]): UsageLegendItem[] {
  const totals = new Map<string, UsageLegendItem>();
  for (const day of series) for (const segment of day.segments) {
    const current = totals.get(segment.key);
    totals.set(segment.key, { ...segment, tokens: (current?.tokens ?? 0) + segment.tokens });
  }
  const all = [...totals.values()];
  const total = all.reduce((sum, item) => sum + item.tokens, 0);
  return all.sort((left, right) => right.tokens - left.tokens).map(item => ({ ...item, share: total ? item.tokens / total : 0 }));
}

export type UsageRankingRow = {
  id: string;
  name: string;
  status?: ActorStatus;
  description?: string;
  used: number;
  share: number;
  usage: TokenUsage;
  total: TokenUsage;
  cacheRate: number | null;
};

function ranking(entries: UsageEntry[], dates: readonly string[], grouping: UsageGrouping): UsageRankingRow[] {
  const rows = usageSubjects(entries, grouping).map(entry => {
    const usage = rangeUsage(entry, dates);
    return {
      id: entry.id,
      name: entry.name,
      status: entry.status,
      description: entry.description,
      used: usage.total_tokens,
      share: 0,
      usage,
      total: entry.usage.total_token_usage,
      cacheRate: cacheHitRate(usage),
    };
  });
  const total = rows.reduce((sum, row) => sum + row.used, 0);
  return rows.map(row => ({ ...row, share: total ? row.used / total : 0 }));
}

/** Agents ordered by tokens used in the window; silent agents stay out of the ranking. */
export function usageRanking(entries: UsageEntry[], dates: readonly string[], grouping: UsageGrouping = "agent"): UsageRankingRow[] {
  return ranking(entries, dates, grouping).filter(row => row.used > 0).sort((left, right) => right.used - left.used || left.name.localeCompare(right.name));
}

/** Agents ordered by cache hit rate; agents without input tokens come last. */
export function cacheRanking(entries: UsageEntry[], dates: readonly string[], grouping: UsageGrouping = "agent"): UsageRankingRow[] {
  return ranking(entries, dates, grouping).filter(row => row.cacheRate !== null).sort((left, right) => {
    if (left.cacheRate === null || right.cacheRate === null) return (left.cacheRate === null ? 1 : 0) - (right.cacheRate === null ? 1 : 0);
    return right.cacheRate - left.cacheRate || right.usage.input_tokens - left.usage.input_tokens;
  });
}

export function formatTokens(value: number, locale: string): string {
  return new Intl.NumberFormat(locale, { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

export function formatPercent(ratio: number): string {
  const percent = ratio * 100;
  return `${percent > 0 && percent < 1 ? percent.toFixed(1) : Math.round(percent)}%`;
}

/** Owner-only usage for every local actor; archived actors keep their history. */
export async function loadUsage(options: FetchOptions = {}): Promise<UsageEntry[]> {
  const actors: ActorSummary[] = [];
  let after: string | undefined;
  for (let page = 0; page < 8; page++) {
    const result = await listActors({ after, limit: 50, signal: options.signal });
    actors.push(...result.items);
    if (!result.next_cursor) break;
    after = result.next_cursor;
  }
  const settled = await Promise.all(actors.map(async actor => {
    try {
      return { actor, usage: await getActorUsage(actor.id, options) };
    } catch (error) {
      if (options.signal?.aborted) throw error;
      return null;
    }
  }));
  return settled.filter((entry): entry is UsageEntry => entry !== null);
}
