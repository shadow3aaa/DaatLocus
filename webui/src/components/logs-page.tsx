import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ChevronDownIcon, SearchIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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

export function LogsPage() {
  const [sources, setSources] = useState<LogSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string | null>(null);
  const [sourceLoadState, setSourceLoadState] = useState<LoadState>("idle");
  const [sourceError, setSourceError] = useState<string | null>(null);
  const [readLoadState, setReadLoadState] = useState<LoadState>("idle");
  const [readError, setReadError] = useState<string | null>(null);
  const [lines, setLines] = useState<LogLine[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const [levelFilter, setLevelFilter] = useState<LogLevelFilter>(
    readStoredLevelFilter,
  );
  const [isSourceMenuOpen, setIsSourceMenuOpen] = useState(false);
  const [isLevelMenuOpen, setIsLevelMenuOpen] = useState(false);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const menuBarRef = useRef<HTMLDivElement | null>(null);

  const selectedSource =
    sources.find((source) => source.id === selectedSourceId) ?? null;

  const entries = useMemo(
    () => lines.map((line) => parseLogEntry(line)),
    [lines],
  );

  const filteredEntries = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();

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
  }, [entries, levelFilter, query]);

  const virtualizer = useVirtualizer({
    count: filteredEntries.length,
    getScrollElement: () => viewportRef.current,
    estimateSize: () => 76,
    overscan: 16,
  });

  useEffect(() => {
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
  }, []);

  useEffect(() => {
    setLines([]);
    setCursor(null);
    setReadError(null);
    setIsSourceMenuOpen(false);
    setIsLevelMenuOpen(false);
    if (!selectedSourceId) {
      return;
    }

    const controller = new AbortController();
    void loadInitialLog(selectedSourceId, controller.signal);

    return () => controller.abort();
  }, [selectedSourceId]);

  useEffect(() => {
    if (!selectedSourceId || cursor === null) {
      return;
    }

    const intervalId = window.setInterval(() => {
      void refreshLog({ onlyNew: true });
    }, FOLLOW_POLL_MS);

    return () => window.clearInterval(intervalId);
  }, [cursor, readLoadState, selectedSourceId]);

  useEffect(() => {
    if (query.trim() || filteredEntries.length === 0) {
      return;
    }

    requestAnimationFrame(() => {
      virtualizer.scrollToIndex(filteredEntries.length - 1, { align: "end" });
    });
  }, [filteredEntries.length, query, virtualizer]);

  useEffect(() => {
    if (!isSourceMenuOpen && !isLevelMenuOpen) {
      return;
    }

    function closeOnOutsidePointer(event: MouseEvent) {
      if (
        menuBarRef.current &&
        !menuBarRef.current.contains(event.target as Node)
      ) {
        setIsSourceMenuOpen(false);
        setIsLevelMenuOpen(false);
      }
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setIsSourceMenuOpen(false);
        setIsLevelMenuOpen(false);
      }
    }

    document.addEventListener("mousedown", closeOnOutsidePointer);
    document.addEventListener("keydown", closeOnEscape);

    return () => {
      document.removeEventListener("mousedown", closeOnOutsidePointer);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [isLevelMenuOpen, isSourceMenuOpen]);

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
      const response = await readLogSource({
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
      const response = await readLogSource({
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

  const visibleItems = virtualizer.getVirtualItems();
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
  });

  return (
    <section
      id="logs"
      aria-label="Logs"
      className="h-screen overflow-hidden bg-background pt-20"
    >
      <div
        ref={menuBarRef}
        className="fixed top-4 left-16 z-50 flex items-start gap-2 md:top-6 md:left-[calc(18rem+1.5rem)]"
      >
        <div className="relative">
          <Button
            type="button"
            variant="outline"
            aria-haspopup="menu"
            aria-expanded={isSourceMenuOpen}
            disabled={sourceLoadState === "loading" && sources.length === 0}
            onClick={() => {
              setIsSourceMenuOpen((open) => !open);
              setIsLevelMenuOpen(false);
            }}
            className="max-w-[36vw] rounded-full border-border/60 bg-background/70 px-3 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/55"
          >
            <span className="truncate">
              {selectedSource?.label ??
                (sourceLoadState === "loading" ? "Loading logs" : "Logs")}
            </span>
            <ChevronDownIcon
              className={cn(
                "size-4 transition-transform",
                isSourceMenuOpen && "rotate-180",
              )}
            />
          </Button>

          {isSourceMenuOpen ? (
            <div
              role="menu"
              className="absolute top-full left-0 mt-2 max-h-[min(24rem,70vh)] w-72 max-w-[calc(100vw-2rem)] overflow-auto rounded-xl border border-border/70 bg-popover p-1 text-popover-foreground shadow-xl"
            >
              {sourceLoadState === "error" ? (
                <div className="px-3 py-2 text-sm text-destructive">
                  {sourceError ?? "Unable to load log sources."}
                </div>
              ) : null}

              {sources.map((source) => (
                <button
                  key={source.id}
                  type="button"
                  role="menuitemradio"
                  aria-checked={source.id === selectedSourceId}
                  onClick={() => {
                    setSelectedSourceId(source.id);
                    setIsSourceMenuOpen(false);
                  }}
                  className={cn(
                    "flex w-full items-center justify-between gap-3 rounded-lg px-3 py-2 text-left text-sm transition hover:bg-muted",
                    source.id === selectedSourceId && "bg-muted",
                  )}
                >
                  <span className="min-w-0">
                    <span className="block truncate font-medium">
                      {source.label}
                    </span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {source.description}
                    </span>
                  </span>
                  <span
                    aria-label={source.exists ? "available" : "missing"}
                    className={cn(
                      "size-2 shrink-0 rounded-full",
                      source.exists
                        ? "bg-emerald-500"
                        : "bg-muted-foreground/35",
                    )}
                  />
                </button>
              ))}
            </div>
          ) : null}
        </div>

        <div className="relative">
          <Button
            type="button"
            variant="outline"
            aria-haspopup="menu"
            aria-expanded={isLevelMenuOpen}
            onClick={() => {
              setIsLevelMenuOpen((open) => !open);
              setIsSourceMenuOpen(false);
            }}
            className="rounded-full border-border/60 bg-background/70 px-3 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/55"
          >
            <span>{displayLevel(levelFilter)}</span>
            <ChevronDownIcon
              className={cn(
                "size-4 transition-transform",
                isLevelMenuOpen && "rotate-180",
              )}
            />
          </Button>

          {isLevelMenuOpen ? (
            <div
              role="menu"
              className="absolute top-full left-0 mt-2 w-40 rounded-xl border border-border/70 bg-popover p-1 text-popover-foreground shadow-xl"
            >
              {LOG_LEVEL_FILTERS.map((level) => (
                <button
                  key={level.value}
                  type="button"
                  role="menuitemradio"
                  aria-checked={level.value === levelFilter}
                  onClick={() => {
                    setLevelFilter(level.value);
                    setIsLevelMenuOpen(false);
                  }}
                  className={cn(
                    "flex w-full items-center justify-between rounded-lg px-3 py-2 text-left text-sm font-medium transition hover:bg-muted",
                    level.value === levelFilter && "bg-muted",
                  )}
                >
                  {level.label}
                  <span
                    className={cn(
                      "size-2 rounded-full",
                      level.value === levelFilter
                        ? "bg-primary"
                        : "bg-muted-foreground/30",
                    )}
                  />
                </button>
              ))}
            </div>
          ) : null}
        </div>
      </div>

      <div className="fixed top-4 right-4 z-50 md:top-6 md:right-6">
        <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search logs"
          aria-label="Search logs"
          className="h-10 w-[min(44vw,20rem)] rounded-full border-border/60 bg-background/70 pl-9 shadow-sm backdrop-blur supports-[backdrop-filter]:bg-background/55"
        />
      </div>

      <div ref={viewportRef} className="h-full overflow-auto px-3 pb-6 md:px-6">
        <div
          className="relative w-full"
          style={{
            height:
              filteredEntries.length > 0
                ? `${virtualizer.getTotalSize()}px`
                : "100%",
          }}
        >
          {emptyMessage ? <EmptyLogState message={emptyMessage} /> : null}

          {visibleItems.map((virtualItem) => {
            const entry = filteredEntries[virtualItem.index];
            if (!entry) {
              return null;
            }

            return (
              <div
                key={virtualItem.key}
                data-index={virtualItem.index}
                ref={virtualizer.measureElement}
                className="absolute top-0 left-0 w-full"
                style={{
                  transform: `translateY(${virtualItem.start}px)`,
                }}
              >
                <LogEntryRow entry={entry} query={query} />
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

function LogEntryRow({ entry, query }: { entry: LogEntry; query: string }) {
  return (
    <article className="grid gap-1 border-b border-border/60 px-1 py-3 transition hover:bg-muted/35 md:grid-cols-[9.5rem_5rem_minmax(8rem,16rem)_1fr] md:gap-3 md:px-0">
      <time className="min-w-0 truncate text-xs text-muted-foreground md:pt-1">
        {entry.timestamp ?? "—"}
      </time>
      <div className="md:pt-0.5">
        <span
          className={cn(
            "inline-flex rounded-full px-2 py-0.5 text-[0.68rem] font-semibold tracking-wide uppercase",
            levelClassName(entry.level),
          )}
        >
          {displayLevel(entry.level)}
        </span>
      </div>
      <div className="min-w-0 truncate text-xs text-muted-foreground md:pt-1">
        {entry.target ?? "—"}
      </div>
      <p className="min-w-0 whitespace-pre-wrap break-words text-sm leading-6">
        {highlightText(entry.message, query)}
      </p>
    </article>
  );
}

function EmptyLogState({ message }: { message: string }) {
  return (
    <div className="absolute inset-0 flex items-center justify-center text-sm text-muted-foreground">
      {message}
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
}) {
  if (sourceLoadState === "error" && !selectedSource) {
    return sourceError ?? "Unable to load log sources.";
  }
  if (!selectedSource) {
    return sourceLoadState === "loading"
      ? "Loading log sources…"
      : "No log source selected.";
  }
  if (readLoadState === "error" && entriesCount === 0) {
    return readError ?? "Unable to read this log.";
  }
  if (readLoadState === "loading" && entriesCount === 0) {
    return "Loading log entries…";
  }
  if (entriesCount === 0) {
    return "No log entries.";
  }
  if (!query.trim() && filteredCount === 0) {
    return `No ${displayLevel(levelFilter)} or higher log entries.`;
  }
  if (query.trim() && filteredCount === 0) {
    return "No matching log entries.";
  }
  return null;
}

function parseLogEntry(line: LogLine): LogEntry {
  const raw = line.text.trimEnd();
  const fallback: LogEntry = {
    id: line.id,
    raw,
    timestamp: null,
    level: inferLevel(raw),
    target: null,
    message: raw || "(blank)",
  };

  if (!raw) {
    return fallback;
  }

  const pythonMatch = raw.match(
    /^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:[,.]\d+)?)\s+-\s+([A-Z]+)\s+-\s+(.+?)\s+-\s+(.*)$/,
  );
  if (pythonMatch) {
    return {
      id: line.id,
      raw,
      timestamp: pythonMatch[1],
      level: normalizeLevel(pythonMatch[2]),
      target: pythonMatch[3],
      message: pythonMatch[4],
    };
  }

  const tracingMatch = raw.match(
    /^(\d{4}-\d{2}-\d{2}[T ][^\s]+)\s+([A-Z]+)\s+(?:ThreadId\([^)]+\)\s+)?(?:([^:]+):\s*)?(.*)$/,
  );
  if (tracingMatch) {
    return {
      id: line.id,
      raw,
      timestamp: tracingMatch[1],
      level: normalizeLevel(tracingMatch[2]),
      target: tracingMatch[3] ?? null,
      message: tracingMatch[4] || raw,
    };
  }

  return fallback;
}

function inferLevel(text: string): string | null {
  const match = text.match(/\b(TRACE|DEBUG|INFO|WARN|WARNING|ERROR)\b/i);
  return match ? normalizeLevel(match[1]) : null;
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

function levelClassName(level: string | null) {
  switch (normalizeLevel(level)) {
    case "error":
      return "bg-destructive/15 text-destructive";
    case "warn":
      return "bg-amber-500/15 text-amber-700 dark:text-amber-300";
    case "info":
      return "bg-sky-500/15 text-sky-700 dark:text-sky-300";
    case "debug":
      return "bg-violet-500/15 text-violet-700 dark:text-violet-300";
    case "trace":
      return "bg-muted text-muted-foreground";
    default:
      return "bg-secondary text-secondary-foreground";
  }
}

function highlightText(text: string, query: string): ReactNode {
  const needle = query.trim();
  if (!needle) {
    return text;
  }

  const lowerText = text.toLowerCase();
  const lowerNeedle = needle.toLowerCase();
  const parts: ReactNode[] = [];
  let cursor = 0;

  while (cursor < text.length) {
    const index = lowerText.indexOf(lowerNeedle, cursor);
    if (index === -1) {
      parts.push(text.slice(cursor));
      break;
    }

    if (index > cursor) {
      parts.push(text.slice(cursor, index));
    }

    parts.push(
      <mark
        key={`${index}-${lowerNeedle}`}
        className="rounded bg-primary/20 px-0.5 text-foreground"
      >
        {text.slice(index, index + needle.length)}
      </mark>,
    );
    cursor = index + needle.length;
  }

  return parts;
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
