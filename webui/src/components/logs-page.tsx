import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  AlertTriangleIcon,
  CopyIcon,
  DownloadIcon,
  FileTextIcon,
  PauseIcon,
  PlayIcon,
  RefreshCwIcon,
  SearchIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  fetchLogSources,
  readLogSource,
  type LogReadResponse,
  type LogSource,
} from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

const LOG_READ_LIMIT = 700;
const FOLLOW_POLL_MS = 1500;
const MAX_RENDERED_LINES = 3_000;

type LogLine = {
  id: string;
  text: string;
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
  const [fileSizeBytes, setFileSizeBytes] = useState(0);
  const [truncatedStart, setTruncatedStart] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [followTail, setFollowTail] = useState(true);
  const [query, setQuery] = useState("");
  const [copied, setCopied] = useState(false);
  const viewportRef = useRef<HTMLDivElement | null>(null);

  const selectedSource =
    sources.find((source) => source.id === selectedSourceId) ?? null;

  const filteredLines = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    if (!normalizedQuery) {
      return lines;
    }

    return lines.filter((line) =>
      line.text.toLowerCase().includes(normalizedQuery),
    );
  }, [lines, query]);

  const virtualizer = useVirtualizer({
    count: filteredLines.length,
    getScrollElement: () => viewportRef.current,
    estimateSize: () => 24,
    overscan: 18,
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
    setFileSizeBytes(0);
    setTruncatedStart(false);
    setHasMore(false);
    setReadError(null);
    if (!selectedSourceId) {
      return;
    }

    const controller = new AbortController();
    void loadInitialLog(selectedSourceId, controller.signal);

    return () => controller.abort();
  }, [selectedSourceId]);

  useEffect(() => {
    if (!selectedSourceId || !followTail) {
      return;
    }

    const intervalId = window.setInterval(() => {
      void refreshLog({ onlyNew: true });
    }, FOLLOW_POLL_MS);

    return () => window.clearInterval(intervalId);
  }, [cursor, followTail, selectedSourceId]);

  useEffect(() => {
    if (!followTail || query.trim() || filteredLines.length === 0) {
      return;
    }

    requestAnimationFrame(() => {
      virtualizer.scrollToIndex(filteredLines.length - 1, { align: "end" });
    });
  }, [filteredLines.length, followTail, query, virtualizer]);

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
    if (!selectedSourceId) {
      return;
    }

    const nextCursor = onlyNew && cursor !== null ? cursor : undefined;
    if (readLoadState === "loading") {
      return;
    }

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
    setFileSizeBytes(response.file_size_bytes);
    setTruncatedStart(response.truncated_start);
    setHasMore(response.has_more);
  }

  async function copyVisibleLines() {
    const text = filteredLines.map((line) => line.text).join("\n");
    await navigator.clipboard.writeText(text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }

  function downloadVisibleLines() {
    const text = filteredLines.map((line) => line.text).join("\n");
    const blob = new Blob([text], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${selectedSource?.id ?? "logs"}.txt`;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  return (
    <section
      id="logs"
      aria-label="Logs"
      className="min-h-screen px-4 pt-20 pb-6 md:px-6 md:pt-24"
    >
      <div className="mx-auto flex w-full max-w-7xl flex-col gap-4">
        <header className="flex flex-col gap-3 md:flex-row md:items-end md:justify-between">
          <div>
            <p className="text-xs font-semibold tracking-[0.24em] text-muted-foreground uppercase">
              Daat Locus
            </p>
            <h1 className="mt-1 text-3xl font-semibold tracking-tight">Logs</h1>
            <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
              Inspect daemon, hindsight, and diagnostic journals from an
              allowlisted set of local log files.
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <Badge variant={selectedSource?.exists ? "secondary" : "outline"}>
              {selectedSource?.exists ? "available" : "missing"}
            </Badge>
            <span>{formatBytes(fileSizeBytes || selectedSource?.size_bytes || 0)}</span>
            {selectedSource?.modified_at_ms ? (
              <span>updated {formatTimestamp(selectedSource.modified_at_ms)}</span>
            ) : null}
          </div>
        </header>

        <div className="grid min-h-[calc(100vh-11rem)] gap-4 lg:grid-cols-[18rem_minmax(0,1fr)]">
          <Card className="min-h-0">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <FileTextIcon className="size-4" />
                Sources
              </CardTitle>
            </CardHeader>
            <CardContent className="min-h-0">
              <div className="grid gap-2">
                {sourceLoadState === "error" ? (
                  <StateMessage tone="error" message={sourceError} />
                ) : null}

                {sources.map((source) => (
                  <button
                    key={source.id}
                    type="button"
                    onClick={() => setSelectedSourceId(source.id)}
                    className={cn(
                      "rounded-lg border p-3 text-left transition hover:bg-muted/70",
                      selectedSourceId === source.id
                        ? "border-foreground/20 bg-muted"
                        : "border-border/60 bg-background",
                    )}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="truncate text-sm font-medium">
                        {source.label}
                      </span>
                      <span
                        className={cn(
                          "size-2 rounded-full",
                          source.exists ? "bg-emerald-500" : "bg-muted-foreground/35",
                        )}
                      />
                    </span>
                    <span className="mt-1 line-clamp-2 block text-xs text-muted-foreground">
                      {source.description}
                    </span>
                    <span className="mt-2 flex flex-wrap gap-1">
                      <Badge variant="outline">{source.category}</Badge>
                      <Badge variant="outline">{source.format}</Badge>
                      {source.sensitive ? (
                        <Badge variant="destructive">sensitive</Badge>
                      ) : null}
                    </span>
                  </button>
                ))}
              </div>
            </CardContent>
          </Card>

          <Card className="min-h-0">
            <CardHeader className="gap-3">
              <div className="flex flex-col gap-2 lg:flex-row lg:items-center lg:justify-between">
                <div className="min-w-0">
                  <CardTitle className="truncate">
                    {selectedSource?.label ?? "Select a log"}
                  </CardTitle>
                  <p className="mt-1 truncate text-xs text-muted-foreground">
                    {selectedSource?.path ?? "No source selected"}
                  </p>
                </div>

                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    type="button"
                    variant={followTail ? "default" : "outline"}
                    size="sm"
                    onClick={() => setFollowTail((value) => !value)}
                  >
                    {followTail ? (
                      <PauseIcon className="size-3.5" />
                    ) : (
                      <PlayIcon className="size-3.5" />
                    )}
                    {followTail ? "Following" : "Paused"}
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={!selectedSourceId || readLoadState === "loading"}
                    onClick={() => void refreshLog({ onlyNew: false })}
                  >
                    <RefreshCwIcon className="size-3.5" />
                    Refresh
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={filteredLines.length === 0}
                    onClick={() => void copyVisibleLines()}
                  >
                    <CopyIcon className="size-3.5" />
                    {copied ? "Copied" : "Copy"}
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={filteredLines.length === 0}
                    onClick={downloadVisibleLines}
                  >
                    <DownloadIcon className="size-3.5" />
                    Download
                  </Button>
                </div>
              </div>

              <div className="flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
                <label className="relative block flex-1">
                  <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    placeholder="Search visible lines"
                    className="pl-8"
                  />
                </label>

                <div className="flex flex-wrap gap-2 text-xs text-muted-foreground">
                  <span>{filteredLines.length.toLocaleString()} shown</span>
                  <span>{lines.length.toLocaleString()} loaded</span>
                  {truncatedStart ? <span>tail truncated</span> : null}
                  {hasMore ? <span>more pending</span> : null}
                  {readLoadState === "loading" ? <span>loading…</span> : null}
                </div>
              </div>
            </CardHeader>

            <CardContent className="min-h-0">
              {selectedSource?.sensitive ? (
                <div className="mb-3 flex items-start gap-2 rounded-lg border border-destructive/25 bg-destructive/5 p-3 text-xs text-destructive">
                  <AlertTriangleIcon className="mt-0.5 size-4 shrink-0" />
                  <span>
                    This source may contain raw prompts, responses, or diagnostic
                    payloads. Avoid copying it into external channels unless needed.
                  </span>
                </div>
              ) : null}

              {readLoadState === "error" ? (
                <StateMessage tone="error" message={readError} />
              ) : null}

              <div
                ref={viewportRef}
                className="h-[calc(100vh-23rem)] min-h-[28rem] overflow-auto rounded-xl border border-border/70 bg-zinc-950 text-zinc-100 shadow-inner"
              >
                {filteredLines.length === 0 ? (
                  <div className="flex h-full items-center justify-center p-6 text-center text-sm text-zinc-400">
                    {readLoadState === "loading"
                      ? "Loading log lines…"
                      : query.trim()
                        ? "No visible lines match the search."
                        : "No log lines to display."}
                  </div>
                ) : (
                  <div
                    className="relative w-full"
                    style={{ height: `${virtualizer.getTotalSize()}px` }}
                  >
                    {virtualizer.getVirtualItems().map((virtualRow) => {
                      const line = filteredLines[virtualRow.index];
                      return (
                        <div
                          key={line.id}
                          ref={virtualizer.measureElement}
                          data-index={virtualRow.index}
                          className="absolute top-0 left-0 grid w-full grid-cols-[4.5rem_minmax(0,1fr)] gap-3 border-b border-white/5 px-3 py-1.5 font-mono text-[11px] leading-5"
                          style={{
                            transform: `translateY(${virtualRow.start}px)`,
                          }}
                        >
                          <span className="select-none text-right text-zinc-500">
                            {(virtualRow.index + 1).toLocaleString()}
                          </span>
                          <HighlightedLogLine line={line.text} query={query} />
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </section>
  );
}

function StateMessage({
  message,
  tone,
}: {
  message: string | null;
  tone: "error";
}) {
  return (
    <div
      className={cn(
        "rounded-lg border p-3 text-sm",
        tone === "error" &&
          "border-destructive/25 bg-destructive/5 text-destructive",
      )}
    >
      {message ?? "Something went wrong."}
    </div>
  );
}

function HighlightedLogLine({
  line,
  query,
}: {
  line: string;
  query: string;
}) {
  const normalizedQuery = query.trim();
  if (!normalizedQuery) {
    return <span className="whitespace-pre-wrap break-words">{line}</span>;
  }

  const lowerLine = line.toLowerCase();
  const lowerQuery = normalizedQuery.toLowerCase();
  const parts: Array<{ text: string; match: boolean }> = [];
  let cursor = 0;

  while (cursor < line.length) {
    const matchIndex = lowerLine.indexOf(lowerQuery, cursor);
    if (matchIndex === -1) {
      parts.push({ text: line.slice(cursor), match: false });
      break;
    }
    if (matchIndex > cursor) {
      parts.push({ text: line.slice(cursor, matchIndex), match: false });
    }
    parts.push({
      text: line.slice(matchIndex, matchIndex + normalizedQuery.length),
      match: true,
    });
    cursor = matchIndex + normalizedQuery.length;
  }

  return (
    <span className="whitespace-pre-wrap break-words">
      {parts.map((part, index) =>
        part.match ? (
          <mark
            key={`${part.text}-${index}`}
            className="rounded-sm bg-yellow-300/25 px-0.5 text-yellow-100"
          >
            {part.text}
          </mark>
        ) : (
          <span key={`${part.text}-${index}`}>{part.text}</span>
        ),
      )}
    </span>
  );
}

function toLogLines(rawLines: string[], cursor: number): LogLine[] {
  return rawLines.map((text, index) => ({
    id: `${cursor}-${index}-${text.length}`,
    text,
  }));
}

function trimLogLines(lines: LogLine[], maxLines: number) {
  if (lines.length <= maxLines) {
    return lines;
  }
  return lines.slice(lines.length - maxLines);
}

function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }

  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 10 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

function formatTimestamp(timestampMs: number) {
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestampMs));
}
