import { Fragment, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDownIcon, ChevronLeftIcon, ChevronRightIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { cacheRanking, formatPercent, formatTokens, loadUsage, usageLegend, usageRanking, usageSeries, usageWindow, USAGE_RANGES, type UsageDimension, type UsageEntry, type UsageRange, type UsageRankingRow } from "@/lib/actor-usage";

/** Settings → Usage: recorded agent token usage, daily history and cache hit rates. */
export function UsageSettings() {
  const { i18n } = useTranslation();
  const [entries, setEntries] = useState<UsageEntry[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [range, setRange] = useState<UsageRange>(7);
  const [dimension, setDimension] = useState<UsageDimension>("agent");
  const [page, setPage] = useState(0);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [expandedCache, setExpandedCache] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    setFailed(false);
    void loadUsage({ signal: controller.signal }).then(value => {
      if (!controller.signal.aborted) setEntries(value);
    }).catch(() => {
      if (controller.signal.aborted) return;
      setEntries([]);
      setFailed(true);
    });
    return () => controller.abort();
  }, []);
  const locale = i18n.language || "en-US";
  const dates = useMemo(() => usageWindow(range, page), [range, page]);
  const series = useMemo(() => usageSeries(entries ?? [], dates, dimension), [entries, dates, dimension]);
  const legend = useMemo(() => usageLegend(series), [series]);
  const usageRows = useMemo(() => usageRanking(entries ?? [], dates), [entries, dates]);
  const cacheRows = useMemo(() => cacheRanking(entries ?? [], dates), [entries, dates]);
  const peak = useMemo(() => Math.max(1, ...series.map(day => day.total)), [series]);
  const total = useMemo(() => series.reduce((sum, day) => sum + day.total, 0), [series]);
  function changeRange(value: string) {
    if (!value) return;
    setRange(Number(value) as UsageRange);
    setPage(0);
    setExpanded(null);
    setExpandedCache(null);
  }
  const shortDate = (date: string) => date.slice(5).replace("-", "/");
  // 30-day bars keep every fifth tick so labels never collide.
  const labelStep = range === 30 ? 5 : 1;
  const segmentName = (kind: UsageDimension, key: string, label: string) => kind === "cache" ? t(`usage_page.segment_${key}`) : label;
  const detail = (row: UsageRankingRow) => [
    [t("usage_page.detail_used"), formatTokens(row.usage.total_tokens, locale)],
    [t("usage_page.detail_input"), formatTokens(row.usage.input_tokens, locale)],
    [t("usage_page.detail_cached"), formatTokens(row.usage.cached_input_tokens, locale)],
    [t("usage_page.detail_output"), formatTokens(row.usage.output_tokens, locale)],
    [t("usage_page.detail_reasoning"), formatTokens(row.usage.reasoning_output_tokens, locale)],
    [t("usage_page.detail_total"), formatTokens(row.total.total_tokens, locale)],
    [t("usage_page.detail_cache_rate"), row.cacheRate === null ? t("usage_page.unavailable") : formatPercent(row.cacheRate)],
  ];
  return <div className="dl-usage-page">
    <section className="dl-usage-section" aria-labelledby="dl-usage-history">
      <header className="dl-usage-heading">
        <div><h2 id="dl-usage-history">{t("usage_page.history")}</h2><p className="dl-usage-caption">{t("usage_page.history_description")}</p></div>
        <ToggleGroup type="single" aria-label={t("usage_page.range")} className="dl-usage-range" value={String(range)} onValueChange={changeRange}>
          {USAGE_RANGES.map(days => <ToggleGroupItem key={days} value={String(days)}>{t("usage_page.range_days", { count: days })}</ToggleGroupItem>)}
        </ToggleGroup>
      </header>
      <div className="dl-usage-toolbar">
        <Select value={dimension} onValueChange={value => setDimension(value as UsageDimension)}>
          <SelectTrigger size="sm" className="dl-usage-select" aria-label={t("usage_page.group_by")}><SelectValue /></SelectTrigger>
          <SelectContent><SelectGroup>
            <SelectItem value="agent">{t("usage_page.group_agent")}</SelectItem>
            <SelectItem value="cache">{t("usage_page.group_cache")}</SelectItem>
          </SelectGroup></SelectContent>
        </Select>
        <div className="dl-usage-pager">
          <span>{shortDate(dates[0])} – {shortDate(dates[dates.length - 1])}</span>
          <Button variant="ghost" size="icon-sm" aria-label={t("usage_page.older")} disabled={page >= 3} onClick={() => setPage(value => value + 1)}><ChevronLeftIcon /></Button>
          <Button variant="ghost" size="icon-sm" aria-label={t("usage_page.newer")} disabled={page === 0} onClick={() => setPage(value => Math.max(0, value - 1))}><ChevronRightIcon /></Button>
        </div>
      </div>
      {entries === null ? <div className="dl-settings-loading" role="status"><Spinner />{t("usage_page.loading")}</div>
        : total > 0 ? <div className="dl-usage-chart" role="group" aria-label={t("usage_page.chart_label", { count: range })}>
          {series.map((day, index) => <div className="dl-usage-day" key={day.date}>
            <div className="dl-usage-stack" role="img" aria-label={`${day.date} · ${formatTokens(day.total, locale)}`} title={`${day.date} · ${formatTokens(day.total, locale)}`}>
              {day.segments.map((segment, segmentIndex) => segment.tokens ? <span key={segment.key} style={{ height: `${segment.tokens / peak * 100}%`, background: `var(--dl-usage-${segmentIndex % 6 + 1})` }} /> : null)}
            </div>
            {(series.length - 1 - index) % labelStep === 0 ? <time dateTime={day.date}>{shortDate(day.date)}</time> : <span className="dl-usage-tick" aria-hidden="true" />}
          </div>)}
        </div>
        : <p className="dl-usage-empty" role="status">{failed ? t("usage_page.failed") : t("usage_page.no_data")}</p>}
      {total > 0 && <div className="dl-usage-summary"><span>{t("usage_page.chart_total", { value: formatTokens(total, locale) })}</span><ul className="dl-usage-legend">{legend.map((item, index) => <li key={item.key}>
        <span className="dl-usage-dot" style={{ background: `var(--dl-usage-${index % 6 + 1})` }} />
        <span>{segmentName(item.kind, item.key, item.label)}</span>
        <span className="dl-usage-share">{formatPercent(item.share)}</span>
      </li>)}</ul></div>}
    </section>

    <section className="dl-usage-section" aria-labelledby="dl-usage-agents">
      <header className="dl-usage-heading">
        <div><h2 id="dl-usage-agents">{t("usage_page.agent_ranking")}</h2><p className="dl-usage-caption">{t("usage_page.agent_ranking_description")}</p></div>
      </header>
      {usageRows.length ? <table className="dl-usage-table">
        <thead><tr><th scope="col">{t("usage_page.column_agent")}</th><th scope="col" className="num">{t("usage_page.column_share")}</th><th scope="col" className="num">{t("usage_page.column_used")}</th></tr></thead>
        <tbody>{usageRows.map(row => <Fragment key={row.id}>
          <tr className="dl-usage-row" data-open={expanded === row.id}>
            <th scope="row"><button type="button" aria-expanded={expanded === row.id} onClick={() => setExpanded(expanded === row.id ? null : row.id)}>
              <ChevronDownIcon data-icon="chevron" />{row.name || row.id}{row.status === "archived" && <span className="dl-usage-badge">{t("usage_page.archived")}</span>}
            </button></th>
            <td className="num">{formatPercent(row.share)}</td>
            <td className="num">{formatTokens(row.used, locale)}</td>
          </tr>
          {expanded === row.id && <tr className="dl-usage-detail"><td colSpan={3}><dl>{detail(row).map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl></td></tr>}
        </Fragment>)}</tbody>
      </table> : entries !== null && <p className="dl-usage-empty" role="status">{t("usage_page.no_data")}</p>}
    </section>

    <section className="dl-usage-section" aria-labelledby="dl-usage-cache">
      <header className="dl-usage-heading">
        <div><h2 id="dl-usage-cache">{t("usage_page.cache_ranking")}</h2><p className="dl-usage-caption">{t("usage_page.cache_ranking_description")}</p></div>
      </header>
      {cacheRows.some(row => row.cacheRate !== null) ? <table className="dl-usage-table">
        <thead><tr><th scope="col">{t("usage_page.column_agent")}</th><th scope="col" className="num">{t("usage_page.column_cache_rate")}</th><th scope="col" className="num">{t("usage_page.column_cached")}</th></tr></thead>
        <tbody>{cacheRows.map(row => <Fragment key={row.id}>
          <tr className="dl-usage-row" data-open={expandedCache === row.id}>
            <th scope="row"><button type="button" aria-expanded={expandedCache === row.id} onClick={() => setExpandedCache(expandedCache === row.id ? null : row.id)}>
              <ChevronDownIcon data-icon="chevron" />{row.name || row.id}{row.status === "archived" && <span className="dl-usage-badge">{t("usage_page.archived")}</span>}
            </button></th>
            <td className="num">{row.cacheRate === null ? t("usage_page.unavailable") : formatPercent(row.cacheRate)}</td>
            <td className="num">{formatTokens(row.usage.cached_input_tokens, locale)}</td>
          </tr>
          {expandedCache === row.id && <tr className="dl-usage-detail"><td colSpan={3}><dl>{detail(row).map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl></td></tr>}
        </Fragment>)}</tbody>
      </table> : entries !== null && <p className="dl-usage-empty" role="status">{t("usage_page.no_cache_data")}</p>}
    </section>
  </div>;
}
