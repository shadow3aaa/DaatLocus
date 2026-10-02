import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ArrowDownToLineIcon,
  FileTextIcon,
  ListFilterIcon,
  SearchIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";
import {
  fetchLogSources,
  readLogSource,
  type LogReadResponse,
  type LogSource,
} from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

const LOG_READ_LIMIT = 1_000;
const FOLLOW_POLL_MS = 1_500;
const MAX_RENDERED_LINES = 5_000;
const LEVEL_FILTER_STORAGE_KEY = "daat-locus.logs.level-filter";

const LOG_LEVEL_FILTERS = [
  { value: "trace", label: "TRACE" },
  { value: "debug", label: "DEBUG" },
  { value: "info", label: "INFO" },
  { value: "warn", label: "WARNING" },
  { value: "error", label: "ERROR" },
] as const;

type LogLevelFilter = (typeof LOG_LEVEL_FILTERS)[number]["value"];

const LOG_LEVEL_RANK: Record<LogLevelFilter, number> = {
  trace: 0,
  debug: 1,
  info: 2,
  warn: 3,
  error: 4,
};

type LogLine = {
  id: string;
  text: string;
};

type LogEntry = {
  id: string;
  raw: string;
  timestamp: string | null;
  level: string | null;
  target: string | null;
  message: string;
};

type LoadState = "idle" | "loading" | "error";

export type LogsPageMockData = {
  sources: LogSource[];
  linesBySource: Record<string, string[]>;
};

type LogsPageProps = {
  mockData?: LogsPageMockData;
};

export function LogsPage({ mockData }: LogsPageProps = {}) {
  const { t } = useTranslation();
  const [sources, setSources] = useState<LogSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string | null>(null);
  const [sourceLoadState, setSourceLoadState] = useState<LoadState>("idle");
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [readLoadState, setReadLoadState] = useState<LoadState>("idle");
  const [readError, setReadError] = useState<string | null>(null);
  const [lines, setLines] = useState<LogLine[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const [levelFilter, setLevelFilter] = useState<LogLevelFilter>(
    readStoredLevelFilter,
  );
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const selectedSource =
    sources.find((source) => source.id === selectedSourceId) ?? null;
  const isSearchVisible = isSearchOpen || query.trim().length > 0;
  const normalizedQuery = query.trim().toLowerCase();

  const entries = useMemo(
    () => lines.map((line) => parseLogEntry(line, t("logs.blank"))),
    [lines, t],
  );

  const filteredEntries = useMemo(() => {
    return entries.filter((entry) =>
      entryMatchesLevelFilter(entry, levelFilter) &&
      (!normalizedQuery ||
        [
          entry.raw,
          entry.timestamp,
          displayLevel(entry.level),
          entry.target,
          entry.message,
        ]
          .filter(Boolean)
          .join("\n")
          .toLowerCase()
          .includes(normalizedQuery)),
    );
  }, [entries, levelFilter, normalizedQuery]);

  const virtualizer = useVirtualizer({
    count: filteredEntries.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 24,
    overscan: 16,
    getItemKey: (index) => filteredEntries[index]?.id ?? index,
  });

  useEffect(() => {
    if (!isSearchVisible) {
      return;
    }

    const frameId = window.requestAnimationFrame(() => {
      searchInputRef.current?.focus();
    });

    return () => window.cancelAnimationFrame(frameId);
  }, [isSearchVisible]);

  useEffect(() => {
    if (mockData) {
      setSourceLoadState("idle");
      setSourceError(null);
      setSources(mockData.sources);
      setSelectedSourceId((current) => {
        if (
          current &&
          mockData.sources.some((source) => source.id === current)
        ) {
          return current;
        }
        return (
          mockData.sources.find((source) => source.id === "daemon-main")?.id ??
          mockData.sources.find((source) => source.exists)?.id ??
          mockData.sources[0]?.id ??
          null
        );
      });
      return;
    }

    const controller = new AbortController();

    async function loadSources() {
      setSourceLoadState("loading");
      setSourceError(null);

      try {
        const nextSources = await fetchLogSources({ signal: controller.signal });
        setSources(nextSources);
        setSelectedSourceId((current) => {
          if (current && nextSources.some((source) => source.id === current)) {
            return current;
          }
          return (
            nextSources.find((source) => source.id === "daemon-main")?.id ??
            nextSources.find((source) => source.exists)?.id ??
            nextSources[0]?.id ??
            null
          );
        });
        setSourceLoadState("idle");
      } catch (error) {
        if (controller.signal.aborted) {
          return;
        }
        setSourceLoadState("error");
        setSourceError(error instanceof Error ? error.message : String(error));
      }
    }

    void loadSources();

    return () => controller.abort();
  }, [mockData]);

  useEffect(() => {
    setLines([]);
    setCursor(null);
    setReadError(null);
    if (!selectedSourceId) {
      return;
    }

    const controller = new AbortController();
    void loadInitialLog(selectedSourceId, controller.signal);

    return () => controller.abort();
  }, [mockData, selectedSourceId]);

  useEffect(() => {
    if (!selectedSourceId || cursor === null || mockData) {
      return;
    }

    const intervalId = window.setInterval(() => {
      void refreshLog({ onlyNew: true });
    }, FOLLOW_POLL_MS);

    return () => window.clearInterval(intervalId);
  }, [cursor, mockData, readLoadState, selectedSourceId]);

  const scrollToLatest = useCallback(() => {
    const lastIndex = filteredEntries.length - 1;
    if (lastIndex >= 0) {
      virtualizer.scrollToIndex(lastIndex, { align: "end" });
    }
  }, [filteredEntries.length, virtualizer]);

  useEffect(() => {
    if (normalizedQuery || filteredEntries.length === 0) {
      return;
    }
    const frameId = window.requestAnimationFrame(scrollToLatest);
    return () => window.cancelAnimationFrame(frameId);
  }, [filteredEntries.length, normalizedQuery, scrollToLatest]);

  useEffect(() => {
    try {
      window.localStorage.setItem(LEVEL_FILTER_STORAGE_KEY, levelFilter);
    } catch {
      // Ignore localStorage failures, e.g. private mode or disabled storage.
    }
  }, [levelFilter]);

  async function loadInitialLog(sourceId: string, signal?: AbortSignal) {
    setReadLoadState("loading");
    setReadError(null);

    try {
      const response = mockData
        ? readMockLogSource({
            mockData,
            source: sourceId,
            limit: LOG_READ_LIMIT,
          })
        : await readLogSource({
            source: sourceId,
            limit: LOG_READ_LIMIT,
            signal,
          });
      applyLogRead(response, { append: false });
      setReadLoadState("idle");
    } catch (error) {
      if (signal?.aborted) {
        return;
      }
      setReadLoadState("error");
      setReadError(error instanceof Error ? error.message : String(error));
    }
  }

  async function refreshLog({ onlyNew }: { onlyNew: boolean }) {
    if (!selectedSourceId || readLoadState === "loading") {
      return;
    }

    const nextCursor = onlyNew && cursor !== null ? cursor : undefined;
    setReadLoadState("loading");
    setReadError(null);

    try {
      const response = mockData
        ? readMockLogSource({
            mockData,
            source: selectedSourceId,
            cursor: nextCursor,
            limit: LOG_READ_LIMIT,
          })
        : await readLogSource({
            source: selectedSourceId,
            cursor: nextCursor,
            limit: LOG_READ_LIMIT,
          });
      applyLogRead(response, {
        append: onlyNew && cursor !== null && !response.reset,
      });
      setReadLoadState("idle");
    } catch (error) {
      setReadLoadState("error");
      setReadError(error instanceof Error ? error.message : String(error));
    }
  }

  function applyLogRead(
    response: LogReadResponse,
    { append }: { append: boolean },
  ) {
    const nextLines = toLogLines(response.lines, response.next_cursor);
    setLines((current) =>
      append
        ? trimLogLines([...current, ...nextLines], MAX_RENDERED_LINES)
        : nextLines,
    );
    setCursor(response.next_cursor);
  }

  const emptyMessage = emptyStateMessage({
    sourceLoadState,
    sourceError,
    readLoadState,
    readError,
    selectedSource,
    entriesCount: entries.length,
    filteredCount: filteredEntries.length,
    levelFilter,
    query,
    t,
  });

  return (
    <section
      id="logs"
      aria-label={t("logs.pageAria")}
      className="flex h-screen flex-col overflow-hidden bg-background"
    >
      <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 pb-2 pl-14 pr-4 pt-3 md:px-6">
        <div className="flex min-w-0 items-baseline gap-3">
          <h1 className="text-base font-semibold tracking-tight">
            {t("logs.title")}
          </h1>
          <span className="hidden truncate font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground sm:inline">
            {selectedSource ? selectedSource.label : t("logs.noSourceSelected")}
          </span>
        </div>
        <div className="flex shrink-0 items-center justify-end gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="outline"
                disabled={sourceLoadState === "loading" && sources.length === 0}
                aria-label={selectedSource?.label ?? t("logs.title")}
                className="h-9 gap-2 border-border bg-background"
              >
                <FileTextIcon aria-hidden="true" />
                <span className="hidden max-w-40 truncate md:inline">
                  {selectedSource?.label ?? t("logs.title")}
                </span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent className="w-72 max-w-[calc(100vw-2rem)]">
              {sourceLoadState === "error" ? (
                <>
                  <DropdownMenuLabel className="text-destructive">
                    {sourceError ?? t("logs.sourceLoadFailed")}
                  </DropdownMenuLabel>
                  <DropdownMenuSeparator />
                </>
              ) : null}
              <DropdownMenuRadioGroup
                value={selectedSourceId ?? ""}
                onValueChange={setSelectedSourceId}
              >
                {sources.map((source) => (
                  <DropdownMenuRadioItem
                    key={source.id}
                    value={source.id}
                    className="items-start gap-3 py-2 pr-8"
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-medium">
                        {source.label}
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        {source.description}
                      </span>
                    </span>
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                type="button"
                variant="outline"
                aria-label={t("logs.levelFilterAria", {
                  level: displayLevel(levelFilter),
                })}
                className="h-9 gap-2 border-border bg-background"
              >
                <ListFilterIcon aria-hidden="true" />
                <span className="hidden font-mono text-[11px] tracking-[0.18em] md:inline">
                  {displayLevel(levelFilter)}
                </span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent className="w-40">
              <DropdownMenuRadioGroup
                value={levelFilter}
                onValueChange={(value) => {
                  const nextLevel = logLevelFilterFromValue(value);
                  if (nextLevel) {
                    setLevelFilter(nextLevel);
                  }
                }}
              >
                {LOG_LEVEL_FILTERS.map((level) => (
                  <DropdownMenuRadioItem key={level.value} value={level.value}>
                    {level.label}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>

          {isSearchVisible ? (
            <InputGroup className="h-9 w-40 min-w-0 overflow-hidden border-border bg-background sm:w-56 lg:w-72">
              <InputGroupAddon align="inline-start">
                <SearchIcon aria-hidden="true" />
              </InputGroupAddon>
              <InputGroupInput
                ref={searchInputRef}
                id="logs-search-input"
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onBlur={() => {
                  if (!query.trim()) {
                    setIsSearchOpen(false);
                  }
                }}
                placeholder={t("logs.search")}
                aria-label={t("logs.search")}
              />
            </InputGroup>
          ) : (
            <Button
              type="button"
              variant="outline"
              aria-label={t("logs.search")}
              aria-controls="logs-search-input"
              aria-expanded={isSearchVisible}
              onClick={() => {
                setIsSearchOpen(true);
                window.requestAnimationFrame(() => {
                  searchInputRef.current?.focus();
                });
              }}
              className="size-9 border-border bg-background"
            >
              <SearchIcon aria-hidden="true" />
            </Button>
          )}

          <Button
            type="button"
            variant="outline"
            size="icon"
            aria-label={t("logs.jumpToLatest")}
            onClick={scrollToLatest}
            className="size-9 border-border bg-background"
          >
            <ArrowDownToLineIcon aria-hidden="true" />
          </Button>
        </div>
      </header>

      <div
        ref={scrollRef}
        className="min-h-0 flex-1 overflow-y-auto scrollbar-thin"
      >
        {emptyMessage ? (
          <EmptyLogState title={t("logs.title")} message={emptyMessage} />
        ) : (
          <div
            role="list"
            aria-label={t("logs.pageAria")}
            className="relative w-full"
            style={{ height: virtualizer.getTotalSize() }}
          >
            {virtualizer.getVirtualItems().map((virtualRow) => {
              const entry = filteredEntries[virtualRow.index];
              return (
                <div
                  key={virtualRow.key}
                  ref={virtualizer.measureElement}
                  data-index={virtualRow.index}
                  className="absolute inset-x-0 top-0"
                  style={{ transform: `translateY(${virtualRow.start}px)` }}
                >
                  <LogRow
                    entry={entry}
                    lineNumber={virtualRow.index + 1}
                    query={normalizedQuery}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>

      <footer className="flex items-center justify-between gap-4 border-t border-border/70 px-4 py-2 md:px-6">
        <span className="shrink-0 whitespace-nowrap font-mono text-[11px] uppercase tracking-[0.18em] tabular-nums text-muted-foreground">
          {filteredEntries.length} / {entries.length}
        </span>
        <span className="min-w-0 truncate font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
          {selectedSource?.description ?? ""}
        </span>
      </footer>
    </section>
  );
}

function LogRow({
  entry,
  lineNumber,
  query,
}: {
  entry: LogEntry;
  lineNumber: number;
  query: string;
}) {
  const segments = logEntrySegments(entry);
  const level = normalizeLevel(entry.level);

  return (
    <div
      role="listitem"
      className={cn(
        "flex items-start gap-3 border-b border-border/40 px-3 py-1 font-mono text-xs leading-5 md:px-4",
        "hover:bg-muted/40",
        level === "error" && "bg-destructive/5",
      )}
    >
      <span className="w-10 shrink-0 select-none pt-px text-right tabular-nums text-muted-foreground/50">
        {lineNumber}
      </span>
      {segments.map((segment) =>
        segment.kind === "timestamp" ? (
          <span
            key="timestamp"
            className="hidden shrink-0 whitespace-nowrap text-muted-foreground/80 lg:inline"
          >
            {segment.text}
          </span>
        ) : segment.kind === "level" ? (
          <span
            key="level"
            className={cn(
              "w-[4.5rem] shrink-0 font-semibold tracking-wide",
              logLevelTextClass(level),
            )}
          >
            {segment.text}
          </span>
        ) : segment.kind === "target" ? (
          <span
            key="target"
            className="hidden max-w-[14rem] shrink-0 truncate text-foreground/60 md:inline"
            title={segment.text}
          >
            {segment.text}
          </span>
        ) : (
          <span
            key="message"
            className="min-w-0 flex-1 whitespace-pre-wrap break-words text-foreground"
          >
            {highlightText(segment.text, query)}
          </span>
        ),
      )}
    </div>
  );
}

/**
 * Ordered render segments for one log entry. Structured fields are preferred;
 * a line that was not parsed still exposes its raw text as a single message.
 */
export type LogSegment = {
  kind: "timestamp" | "level" | "target" | "message";
  text: string;
};

export function logEntrySegments(entry: LogEntry): LogSegment[] {
  if (entry.timestamp || entry.level || entry.target) {
    const segments: LogSegment[] = [];
    if (entry.timestamp) {
      segments.push({ kind: "timestamp", text: entry.timestamp });
    }
    if (entry.level) {
      segments.push({ kind: "level", text: displayLevel(entry.level) });
    }
    if (entry.target) {
      segments.push({ kind: "target", text: entry.target });
    }
    segments.push({ kind: "message", text: entry.message });
    return segments;
  }

  const parsed = entry.raw ? parseStructuredLogLine(entry.raw) : null;
  if (!parsed) {
    return [{ kind: "message", text: entry.message }];
  }

  const segments: LogSegment[] = [
    { kind: "timestamp", text: parsed.timestamp },
    { kind: "level", text: displayLevel(parsed.level) },
  ];
  if (parsed.target) {
    segments.push({ kind: "target", text: parsed.target });
  }
  segments.push({ kind: "message", text: parsed.message || entry.raw });
  return segments;
}

function highlightText(text: string, query: string) {
  if (!query) {
    return text;
  }

  const lowerText = text.toLowerCase();
  const parts: Array<string | React.ReactNode> = [];
  let cursor = 0;
  let matchIndex = lowerText.indexOf(query, cursor);
  while (matchIndex !== -1) {
    if (matchIndex > cursor) {
      parts.push(text.slice(cursor, matchIndex));
    }
    parts.push(
      <mark
        key={`${matchIndex}-${parts.length}`}
        className="rounded-sm bg-primary/25 text-foreground"
      >
        {text.slice(matchIndex, matchIndex + query.length)}
      </mark>,
    );
    cursor = matchIndex + query.length;
    matchIndex = lowerText.indexOf(query, cursor);
  }
  parts.push(text.slice(cursor));
  return parts;
}

function logLevelTextClass(level: string | null | undefined): string {
  switch (normalizeLevel(level)) {
    case "error":
      return "text-destructive";
    case "warn":
      return "text-amber-600 dark:text-amber-500";
    case "info":
      return "text-foreground/80";
    default:
      return "text-muted-foreground";
  }
}

function EmptyLogState({ message, title }: { message: string; title: string }) {
  return (
    <div className="flex h-full items-center justify-center px-4">
      <Empty className="max-w-md border border-dashed bg-card/60">
        <EmptyHeader>
          <EmptyTitle>{title}</EmptyTitle>
          <EmptyDescription>{message}</EmptyDescription>
        </EmptyHeader>
      </Empty>
    </div>
  );
}

function emptyStateMessage({
  sourceLoadState,
  sourceError,
  readLoadState,
  readError,
  selectedSource,
  entriesCount,
  filteredCount,
  levelFilter,
  query,
  t,
}: {
  sourceLoadState: LoadState;
  sourceError: string | null;
  readLoadState: LoadState;
  readError: string | null;
  selectedSource: LogSource | null;
  entriesCount: number;
  filteredCount: number;
  levelFilter: LogLevelFilter;
  query: string;
  t: TFunction;
}) {
  if (sourceLoadState === "error" && !selectedSource) {
    return sourceError ?? t("logs.sourceLoadFailed");
  }
  if (!selectedSource) {
    return sourceLoadState === "loading"
      ? t("logs.loadingSources")
      : t("logs.noSourceSelected");
  }
  if (readLoadState === "error" && entriesCount === 0) {
    return readError ?? t("logs.readFailed");
  }
  if (readLoadState === "loading" && entriesCount === 0) {
    return t("logs.loadingEntries");
  }
  if (entriesCount === 0) {
    return t("logs.noEntries");
  }
  if (!query.trim() && filteredCount === 0) {
    return t("logs.noLevelEntries", { level: displayLevel(levelFilter) });
  }
  if (query.trim() && filteredCount === 0) {
    return t("logs.noMatchingEntries");
  }
  return null;
}

type ParsedLogLine = {
  timestamp: string;
  level: string;
  target: string | null;
  message: string;
};

const LOG_LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "WARNING", "ERROR"] as const;

function isLogLevel(value: string) {
  return LOG_LEVELS.includes(value.toUpperCase() as (typeof LOG_LEVELS)[number]);
}

/**
 * Parse one log line into timestamp, level, target, and message.
 *
 * Python (`ts - LEVEL - target - message`) is tried first, then tracing
 * (`ts LEVEL [ThreadId(n)] [target:] message`). A line that matches neither
 * shape is not forced into those fields.
 */
function parseStructuredLogLine(raw: string): ParsedLogLine | null {
  return parsePythonLogLine(raw) ?? parseTracingLogLine(raw);
}

function parsePythonLogLine(raw: string): ParsedLogLine | null {
  const timestamp = readTimestamp(raw, "python");
  if (!timestamp) {
    return null;
  }
  let cursor = skipSpace(raw, timestamp.end);
  if (raw.slice(cursor, cursor + 1) !== "-") {
    return null;
  }
  cursor = skipSpace(raw, cursor + 1);
  const level = readLevel(raw, cursor);
  if (!level) {
    return null;
  }
  cursor = skipSpace(raw, level.end);
  if (raw.slice(cursor, cursor + 1) !== "-") {
    return null;
  }
  cursor = skipSpace(raw, cursor + 1);
  const separator = raw.indexOf(" - ", cursor);
  if (separator < 0) {
    return null;
  }
  const target = raw.slice(cursor, separator).trim();
  if (!target) {
    return null;
  }
  return {
    timestamp: timestamp.value,
    level: level.value,
    target,
    message: raw.slice(separator + 3),
  };
}

function parseTracingLogLine(raw: string): ParsedLogLine | null {
  const timestamp = readTimestamp(raw, "tracing");
  if (!timestamp) {
    return null;
  }
  let cursor = skipSpace(raw, timestamp.end);
  const level = readLevel(raw, cursor);
  if (!level) {
    return null;
  }
  cursor = skipSpace(raw, level.end);
  const thread = raw.slice(cursor).match(/^ThreadId\([^)]*\)/);
  if (thread) {
    cursor = skipSpace(raw, cursor + thread[0].length);
  }
  const rest = raw.slice(cursor);
  const targetMatch = rest.match(/^([^:\s][^:]*):\s*([\s\S]*)$/);
  return {
    timestamp: timestamp.value,
    level: level.value,
    target: targetMatch ? targetMatch[1].trim() || null : null,
    message: targetMatch ? targetMatch[2] : rest,
  };
}

function readTimestamp(raw: string, kind: "python" | "tracing") {
  const match =
    kind === "python"
      ? raw.match(/^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:[,.]\d+)?)/)
      : raw.match(/^(\d{4}-\d{2}-\d{2}[T ]\S+)/);
  if (!match) {
    return null;
  }
  return { value: match[1], end: match[1].length };
}

function readLevel(raw: string, start: number) {
  const match = raw.slice(start).match(/^([A-Za-z]+)/);
  if (!match || !isLogLevel(match[1])) {
    return null;
  }
  return { value: match[1], end: start + match[1].length };
}

function skipSpace(raw: string, start: number) {
  let cursor = start;
  while (cursor < raw.length && /\s/.test(raw[cursor])) {
    cursor += 1;
  }
  return cursor;
}

function parseLogEntry(line: LogLine, blankMessage: string): LogEntry {
  const raw = line.text.trimEnd();
  const fallback: LogEntry = {
    id: line.id,
    raw,
    timestamp: null,
    level: null,
    target: null,
    message: raw || blankMessage,
  };

  if (!raw) {
    return fallback;
  }

  const parsed = parseStructuredLogLine(raw);
  if (!parsed) {
    return fallback;
  }
  return {
    id: line.id,
    raw,
    timestamp: parsed.timestamp,
    level: normalizeLevel(parsed.level),
    target: parsed.target,
    message: parsed.message || raw,
  };
}

function normalizeLevel(level: string | null | undefined): string | null {
  if (!level) {
    return null;
  }
  const normalized = level.toLowerCase();
  if (normalized === "warning") {
    return "warn";
  }
  if (["trace", "debug", "info", "warn", "error"].includes(normalized)) {
    return normalized;
  }
  return normalized;
}

function displayLevel(level: string | null | undefined) {
  switch (normalizeLevel(level)) {
    case "trace":
      return "TRACE";
    case "debug":
      return "DEBUG";
    case "info":
      return "INFO";
    case "warn":
      return "WARNING";
    case "error":
      return "ERROR";
    default:
      return level?.trim() ? level.trim().toUpperCase() : "log";
  }
}

function entryMatchesLevelFilter(
  entry: LogEntry,
  levelFilter: LogLevelFilter,
) {
  const entryRank = logLevelRank(entry.level);
  if (entryRank === null) {
    return false;
  }
  return entryRank >= LOG_LEVEL_RANK[levelFilter];
}

function readStoredLevelFilter(): LogLevelFilter {
  if (typeof window === "undefined") {
    return "warn";
  }

  try {
    return (
      logLevelFilterFromValue(
        window.localStorage.getItem(LEVEL_FILTER_STORAGE_KEY),
      ) ?? "warn"
    );
  } catch {
    return "warn";
  }
}

function logLevelRank(level: string | null | undefined) {
  const normalizedLevel = logLevelFilterFromValue(level);
  return normalizedLevel ? LOG_LEVEL_RANK[normalizedLevel] : null;
}

function logLevelFilterFromValue(
  value: string | null | undefined,
): LogLevelFilter | null {
  switch (normalizeLevel(value)) {
    case "trace":
      return "trace";
    case "debug":
      return "debug";
    case "info":
      return "info";
    case "warn":
      return "warn";
    case "error":
      return "error";
    default:
      return null;
  }
}

function readMockLogSource({
  mockData,
  source,
  cursor,
  limit,
}: {
  mockData: LogsPageMockData;
  source: string;
  cursor?: number;
  limit: number;
}): LogReadResponse {
  const selectedSource = mockData.sources.find((entry) => entry.id === source);
  if (!selectedSource) {
    throw new Error(`Unknown mock log source: ${source}`);
  }

  const allLines = mockData.linesBySource[source] ?? [];
  const normalizedCursor =
    cursor === undefined ? null : Math.max(0, Math.trunc(cursor));
  const reset = normalizedCursor !== null && normalizedCursor > allLines.length;
  const startIndex =
    normalizedCursor === null || reset
      ? Math.max(allLines.length - limit, 0)
      : normalizedCursor;
  const lines = allLines.slice(startIndex, startIndex + limit);
  const nextCursor = startIndex + lines.length;

  return {
    source: selectedSource,
    lines,
    next_cursor: nextCursor,
    file_size_bytes: mockLogFileSizeBytes(allLines),
    truncated_start: startIndex > 0,
    has_more: nextCursor < allLines.length,
    reset,
  };
}

function mockLogFileSizeBytes(lines: string[]) {
  return lines.reduce((total, line) => total + line.length + 1, 0);
}

function toLogLines(rawLines: string[], responseCursor: number): LogLine[] {
  return rawLines.map((text, index) => ({
    id: `${responseCursor}-${index}-${text.length}`,
    text,
  }));
}

function trimLogLines(lines: LogLine[], maxLines: number) {
  return lines.length > maxLines ? lines.slice(lines.length - maxLines) : lines;
}
