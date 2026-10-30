import { expect, test } from "bun:test";
import { cacheHitRate, cacheRanking, formatPercent, rangeUsage, usageDates, usageLegend, usageRanking, usageSeries, usageWindow, type UsageEntry } from "../src/lib/actor-usage";
import type { TokenUsage } from "../src/lib/actor-api";

const tokens = (input: number, cached: number, output: number): TokenUsage => ({
  input_tokens: input, cached_input_tokens: cached, output_tokens: output, reasoning_output_tokens: 0, total_tokens: input + output,
});

const entry = (id: string, name: string, daily: Record<string, TokenUsage>, status: "idle" | "archived" = "idle"): UsageEntry => ({
  actor: { id, name, status, conversation_url: `/actors/${id}`, apps: [], created_at_ms: 0, updated_at_ms: 0 },
  usage: { total_token_usage: tokens(0, 0, 0), last_token_usage: tokens(0, 0, 0), model_context_window: 200_000,
    daily_token_usage: Object.entries(daily).map(([date, usage]) => ({ date, usage })), models: [] },
});

test("local calendar days end at the requested day and stay ordered", () => {
  const end = new Date(2026, 9, 14, 15, 30);
  expect(usageDates(7, end)).toEqual(["2026-10-08", "2026-10-09", "2026-10-10", "2026-10-11", "2026-10-12", "2026-10-13", "2026-10-14"]);
  expect(usageWindow(7, 1, end).at(-1)).toBe("2026-10-07");
  expect(usageWindow(30, 0, end)).toHaveLength(30);
});

test("range usage only sums days inside the window", () => {
  const actor = entry("a", "Alpha", { "2026-10-13": tokens(100, 60, 10), "2026-10-14": tokens(200, 100, 20), "2026-10-01": tokens(999, 999, 999) });
  expect(rangeUsage(actor, ["2026-10-13", "2026-10-14"])).toEqual(tokens(300, 160, 30));
  expect(rangeUsage(actor, ["2026-09-01"])).toEqual(tokens(0, 0, 0));
});

test("cache hit rate uses cached prompt tokens and refuses to divide by zero", () => {
  expect(cacheHitRate(tokens(1000, 750, 20))).toBe(.75);
  expect(cacheHitRate(tokens(0, 0, 40))).toBeNull();
  expect(cacheHitRate(tokens(10, 20, 0))).toBe(1);
});

test("agent series stacks each agent per day with a stable order and shares", () => {
  const alpha = entry("a", "Alpha", { "2026-10-13": tokens(300, 100, 0), "2026-10-14": tokens(100, 50, 0) });
  const beta = entry("b", "Beta", { "2026-10-14": tokens(300, 150, 0) });
  const series = usageSeries([alpha, beta], ["2026-10-13", "2026-10-14"], "agent");
  expect(series.map(day => day.total)).toEqual([300, 400]);
  expect(series.map(day => day.ratio)).toEqual([.75, 1]);
  expect(series[1].segments.map(segment => [segment.key, segment.tokens, segment.share])).toEqual([["a", 100, .25], ["b", 300, .75]]);
  expect(series[0].segments.map(segment => segment.key)).toEqual(["a", "b"]);
});

test("cache series splits input into cached and uncached tokens", () => {
  const actor = entry("a", "Alpha", { "2026-10-14": tokens(400, 250, 60) });
  const [day] = usageSeries([actor], ["2026-10-14"], "cache");
  expect(day.segments.map(segment => [segment.key, segment.tokens])).toEqual([["cached", 250], ["uncached", 150]]);
  expect(day.segments.reduce((sum, segment) => sum + segment.share, 0)).toBe(1);
});

test("legend percentages cover the whole window and rank by tokens", () => {
  const alpha = entry("a", "Alpha", { "2026-10-13": tokens(100, 0, 0), "2026-10-14": tokens(200, 0, 0) });
  const beta = entry("b", "Beta", { "2026-10-14": tokens(100, 0, 0) });
  const legend = usageLegend(usageSeries([alpha, beta], ["2026-10-13", "2026-10-14"], "agent"));
  expect(legend.map(item => [item.key, item.tokens])).toEqual([["a", 300], ["b", 100]]);
  expect(legend.reduce((sum, item) => sum + item.share, 0)).toBe(1);
  expect(legend.map(item => formatPercent(item.share))).toEqual(["75%", "25%"]);
});

test("usage and cache rankings order agents by the selected range", () => {
  const alpha = entry("a", "Alpha", { "2026-10-14": tokens(100, 80, 0) });
  const beta = entry("b", "Beta", { "2026-10-14": tokens(500, 100, 0) });
  const quiet = entry("c", "Quiet", { "2026-09-01": tokens(900, 900, 0) });
  const dates = ["2026-10-14"];
  expect(usageRanking([alpha, beta, quiet], dates).map(row => [row.id, row.used, row.share])).toEqual([["b", 500, 500 / 600], ["a", 100, 100 / 600]]);
  expect(cacheRanking([alpha, beta, quiet], dates).map(row => [row.id, row.cacheRate])).toEqual([["a", .8], ["b", .2]]);
  expect(usageRanking([alpha, beta], dates)[0].usage).toEqual(tokens(500, 100, 0));
});

test("archived agents keep their usage in the ranking", () => {
  const archived = entry("z", "Old", { "2026-10-14": tokens(50, 10, 0) }, "archived");
  const [row] = usageRanking([archived], ["2026-10-14"]);
  expect(row.status).toBe("archived");
  expect(row.used).toBe(50);
});

test("model statistics merge actors, preserve previous models and retain unattributed history", () => {
  const day = "2026-10-14";
  const alpha = entry("a", "Alpha", { [day]: tokens(300, 200, 30) });
  const beta = entry("b", "Beta", { [day]: tokens(200, 50, 20) }, "archived");
  const model = (name: string, usage: TokenUsage, provider = "gateway") => ({
    model: name, provider, model_id: name === "large" ? "large-v1" : "small-v1",
    total_token_usage: usage, last_token_usage: usage, daily_token_usage: [{ date: day, usage }], model_context_window: 200_000,
  });
  alpha.usage.models = [model("large", tokens(200, 180, 20)), model("small", tokens(50, 10, 5))];
  beta.usage.models = [model("large", tokens(100, 30, 10)), model("large", tokens(100, 20, 10), "other")];
  alpha.usage.total_token_usage = tokens(300, 200, 30);
  beta.usage.total_token_usage = tokens(200, 50, 20);
  const entries = [alpha, beta];
  const rows = usageRanking(entries, [day], "model");
  expect(rows.map(row => [row.name, row.used])).toEqual([["large", 330], ["large", 110], ["", 55], ["small", 55]]);
  expect(rows[0].usage).toEqual(tokens(300, 210, 30));
  expect(rows[0].total.total_tokens).toBe(330);
  expect(rows[1].description).toBe("other · large-v1");
  expect(rows.reduce((sum, row) => sum + row.used, 0)).toBe(550);
  const series = usageSeries(entries, [day], "model");
  expect(series[0].total).toBe(550);
  expect(usageLegend(series).map(row => row.tokens)).toEqual([330, 110, 55, 55]);
  expect(cacheRanking(entries, [day], "model")[0].cacheRate).toBe(.7);
  expect(usageRanking(entries, ["2026-09-01"], "model")).toEqual([]);
  // Projection cannot mutate recorded source totals while merging Actors.
  expect(alpha.usage.models[0].total_token_usage).toEqual(tokens(200, 180, 20));
});
