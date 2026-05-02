import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type FocusEvent,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";

import {
  AgentStatusAnimation,
  type AgentAnimationStatus,
} from "@/components/agent-status-animation";
import {
  CheckIcon,
  GripVerticalIcon,
  Loader2Icon,
  SendHorizontalIcon,
  XIcon,
} from "lucide-react";
import {
  Bar,
  BarChart,
  Cell,
  Pie,
  PieChart,
  XAxis,
  YAxis,
} from "recharts";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  type ChartConfig,
} from "@/components/ui/chart";
import {
  runDashboardCommand,
  subscribeDashboardSnapshots,
  type DashboardContextCompositionSegment,
  type DashboardPendingAccessRequest,
  type DashboardSnapshot,
  type WebActivityBlock,
  type WebActivityItem,
  type TokenUsage,
  type TokenUsageInfo,
} from "@/lib/daemon-api";
import { cn } from "@/lib/utils";

const DASHBOARD_STREAM_RECONNECT_MS = 1500;
const SUMMARY_TYPE_INTERVAL_MS = 28;
const TOKEN_USAGE_MAX_VISIBLE_DAYS = 7;
const STATUS_CARD_ORDER_STORAGE_KEY = "daat-locus.status.card-order";
const CONTEXT_COMPOSITION_MAX_VISIBLE_SEGMENTS = 8;
const AGENT_CHAT_MAX_VISIBLE_BUBBLES = 24;
const AGENT_CHAT_MESSAGE_LINE_LIMIT = 5;
const AGENT_CHAT_FOCUSED_MESSAGE_LINE_LIMIT = 12;
const AGENT_CHAT_DETAIL_LINE_LIMIT = 8;
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
const CONTEXT_COMPOSITION_CHART_CONFIG = {
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
  empty: {
    label: "No data",
    color: "var(--muted)",
  },
} satisfies ChartConfig;
const RUNTIME_OPTIMIZATION_CHART_CONFIG = {
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
} satisfies ChartConfig;

const DEFAULT_STATUS_CARD_ORDER = [
  "telegram-approval",
  "runtime-optimization",
  "context-composition",
  "daily-token-usage",
  "workflow-optimization",
] as const;

type AgentStatusView = {
  animationStatus: AgentAnimationStatus;
  label: string;
};

type StatusCardId = (typeof DEFAULT_STATUS_CARD_ORDER)[number];
type StatusCardPlacement = "before" | "after";

type StatusCardDropIntent = {
  targetId: StatusCardId;
  placement: StatusCardPlacement;
};

type StatusCardContentProps = {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
};

type StatusCardDefinition = {
  label: string;
  render: (props: StatusCardContentProps) => ReactNode;
};

const STATUS_CARD_DEFINITIONS: Record<StatusCardId, StatusCardDefinition> = {
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
  "workflow-optimization": {
    label: "Workflow Optimization",
    render: (props) => <WorkflowOptimizationCard {...props} />,
  },
};

function useDashboardSnapshot() {
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

  return { isLoading, loadError, snapshot };
}

export function AgentPage() {
  const { isLoading, loadError, snapshot } = useDashboardSnapshot();
  const chatPanelRef = useRef<HTMLDivElement>(null);
  const [isChatFocused, setIsChatFocused] = useState(false);
  const agentStatus = deriveAgentStatus({
    hasLoadError: Boolean(loadError),
    isLoading,
    snapshot,
  });
  const summaryText = derivePlanSummaryText(snapshot);
  const { isTyping, text: typedSummaryText } = useTypewriterText(summaryText);

  return (
    <section
      id="agent"
      aria-label="Agent"
      className="relative flex min-h-screen w-full items-center justify-center overflow-hidden px-6 py-10"
    >
      <AgentChatBubbles
        snapshot={snapshot}
        isFocused={isChatFocused}
        panelRef={chatPanelRef}
      />
      <div className="relative z-10 flex flex-col items-center justify-center gap-5 text-center">
        <AgentStatusAnimation
          status={agentStatus.animationStatus}
          className={cn(
            "relative z-20 w-64 transition-[filter,opacity,transform] duration-300 md:w-80",
            isChatFocused && "scale-95 opacity-35 blur-[2px]",
          )}
        />
        <p
          aria-live="polite"
          className={cn(
            "relative z-20 min-h-6 max-w-[min(32rem,calc(100vw-3rem))] text-balance text-sm font-medium leading-6 text-muted-foreground transition-opacity duration-300 md:text-base",
            isChatFocused && "opacity-40",
          )}
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
      <AgentChatComposer
        isFocused={isChatFocused}
        onFocusChange={setIsChatFocused}
        chatPanelRef={chatPanelRef}
      />
    </section>
  );
}

type AgentChatBubbleRole = "assistant" | "user" | "tool" | "telegram" | "system";

type AgentChatBubble = {
  id: string;
  role: AgentChatBubbleRole;
  kind: string;
  status: string;
  title: string;
  blocks: WebActivityBlock[];
  detailBlocks: WebActivityBlock[];
  live?: boolean;
  toolName?: string;
  appName?: string;
  sourceLabel?: string;
  errorLines: string[];
  affectedFiles: string[];
};

function AgentChatComposer({
  isFocused,
  onFocusChange,
  chatPanelRef,
}: {
  isFocused: boolean;
  onFocusChange: (isFocused: boolean) => void;
  chatPanelRef: RefObject<HTMLDivElement | null>;
}) {
  const [message, setMessage] = useState("");
  const [isSending, setIsSending] = useState(false);
  const [sendResult, setSendResult] = useState<string | null>(null);
  const [sendError, setSendError] = useState<string | null>(null);

  function handleFocus() {
    onFocusChange(true);
  }

  function handleBlur(event: FocusEvent<HTMLFormElement>) {
    if (event.currentTarget.contains(event.relatedTarget)) {
      return;
    }
    onFocusChange(false);
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = message.trim();

    if (!trimmed || isSending) {
      return;
    }

    setIsSending(true);
    setSendResult(null);
    setSendError(null);

    try {
      const output = await runDashboardCommand(trimmed);
      setMessage("");
      setSendResult(agentChatSendResultText(output));
      onFocusChange(true);
      window.requestAnimationFrame(() => {
        if (chatPanelRef.current) {
          chatPanelRef.current.scrollTop = chatPanelRef.current.scrollHeight;
        }
      });
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsSending(false);
    }
  }

  return (
    <form
      aria-label="Send message to agent"
      onSubmit={handleSubmit}
      onFocus={handleFocus}
      onBlur={handleBlur}
      className={cn(
        "fixed bottom-5 left-1/2 z-30 w-[min(42rem,calc(100vw-2rem))] -translate-x-1/2 rounded-[2rem] border bg-background/85 p-2 shadow-2xl shadow-background/40 backdrop-blur-xl transition-all duration-300",
        isFocused
          ? "border-primary/45 ring-4 ring-primary/10"
          : "border-border/70 hover:border-primary/30",
      )}
    >
      <div className="flex items-end gap-2">
        <textarea
          value={message}
          rows={1}
          placeholder="和 agent 说点什么…"
          aria-label="Message"
          onChange={(event) => {
            setMessage(event.target.value);
            setSendError(null);
            setSendResult(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              event.currentTarget.form?.requestSubmit();
            }
          }}
          className="max-h-32 min-h-11 flex-1 resize-none bg-transparent px-4 py-3 text-sm leading-5 outline-none placeholder:text-muted-foreground/70"
        />
        <Button
          type="submit"
          size="icon-lg"
          disabled={!message.trim() || isSending}
          aria-label="Send message"
          className="mb-0.5 rounded-full"
        >
          {isSending ? (
            <Loader2Icon className="size-4 animate-spin" />
          ) : (
            <SendHorizontalIcon className="size-4" />
          )}
        </Button>
      </div>
      {sendError || sendResult ? (
        <p
          role={sendError ? "alert" : "status"}
          className={cn(
            "px-4 pb-1 pt-0.5 text-xs",
            sendError ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {sendError ?? sendResult}
        </p>
      ) : null}
    </form>
  );
}

function AgentChatBubbles({
  snapshot,
  isFocused,
  panelRef,
}: {
  snapshot: DashboardSnapshot | null;
  isFocused: boolean;
  panelRef: RefObject<HTMLDivElement | null>;
}) {
  const bubbles = useMemo(() => agentChatBubblesFromSnapshot(snapshot), [snapshot]);

  useEffect(() => {
    if (!isFocused || !panelRef.current) {
      return;
    }
    panelRef.current.scrollTop = panelRef.current.scrollHeight;
  }, [bubbles.length, isFocused, panelRef]);

  return (
    <div
      ref={panelRef}
      aria-label="Agent chat preview"
      aria-hidden={!isFocused}
      className={cn(
        "absolute inset-0 w-full text-left transition-[filter,opacity] duration-300 ease-out",
        isFocused
          ? "pointer-events-auto z-20 overflow-y-auto px-4 pb-[calc(50vh+9rem)] pt-6 opacity-100 [scrollbar-gutter:stable] md:px-8"
          : "pointer-events-none z-0 overflow-hidden px-4 pb-24 pt-[calc(50vh+8rem)] opacity-35 blur-[1px] md:px-8",
      )}
    >
      <div
        aria-hidden="true"
        className={cn(
          "pointer-events-none absolute left-1/2 top-1/2 z-0 h-[min(34rem,72vw)] w-[min(34rem,72vw)] -translate-x-1/2 -translate-y-1/2 rounded-full bg-background/10 opacity-0 backdrop-blur-0 transition-[backdrop-filter,opacity] duration-300",
          isFocused && "opacity-100 backdrop-blur-md",
        )}
      />
      <div
        className={cn(
          "relative z-10 flex min-h-full w-full flex-col gap-3 transition-transform duration-300 ease-out",
          isFocused ? "justify-end" : "justify-start translate-y-8",
        )}
      >
        {bubbles.length > 0 ? (
          bubbles.map((bubble) => (
            <AgentChatBubbleItem
              key={bubble.id}
              bubble={bubble}
              isFocused={isFocused}
            />
          ))
        ) : (
          <p className="mx-auto max-w-[min(32rem,calc(100vw-3rem))] px-4 py-3 text-center text-xs text-muted-foreground/70">
            聚焦底部输入框后，消息流会在整个屏幕上围绕 agent 浮动。
          </p>
        )}
      </div>
    </div>
  );
}

function AgentChatBubbleItem({
  bubble,
  isFocused,
}: {
  bubble: AgentChatBubble;
  isFocused: boolean;
}) {
  const isUser = bubble.role === "user";
  const isAssistant = bubble.role === "assistant";
  const primaryBlocks = bubble.blocks.length > 0
    ? bubble.blocks
    : ([{ type: "text", text: bubble.title }] as WebActivityBlock[]);
  const visibleBlockLimit = isFocused ? 6 : 3;
  const visibleBlocks = primaryBlocks.slice(0, visibleBlockLimit);
  const hiddenBlocks = primaryBlocks.slice(visibleBlockLimit);
  const hasDetails =
    bubble.detailBlocks.length > 0 ||
    hiddenBlocks.length > 0 ||
    bubble.errorLines.length > 0 ||
    bubble.affectedFiles.length > 0 ||
    Boolean(bubble.toolName || bubble.appName || bubble.sourceLabel);

  return (
    <article
      className={cn(
        "w-full",
        !isFocused && "select-none",
      )}
    >
      <div className="max-w-[min(44rem,88%)] px-1 py-2 text-sm leading-5 text-foreground">
        <div
          className={cn(
            "mb-1 flex flex-wrap items-center gap-2 text-[0.68rem] font-semibold uppercase tracking-[0.16em]",
            isUser
              ? "text-primary"
              : isAssistant
                ? "text-cyan-300"
                : bubble.role === "telegram"
                  ? "text-sky-300"
                  : bubble.role === "system"
                    ? "text-violet-300"
                    : "text-muted-foreground",
          )}
        >
          {bubble.live || bubble.status === "running" ? (
            <span className="size-1.5 rounded-full bg-emerald-400 shadow-[0_0_0_3px_rgba(52,211,153,0.18)]" />
          ) : null}
          <span>{agentChatBubbleLabel(bubble)}</span>
          {bubble.status && !["completed", "unknown"].includes(bubble.status) ? (
            <span className="text-muted-foreground/70">· {agentChatStatusLabel(bubble.status)}</span>
          ) : null}
          {bubble.toolName ? (
            <span className="text-muted-foreground/70">· {bubble.toolName}</span>
          ) : null}
        </div>
        <div className="space-y-2 text-foreground/90">
          {visibleBlocks.map((block, index) => (
            <AgentChatBlock
              key={`${bubble.id}-block-${index}`}
              block={block}
              blockId={`${bubble.id}-block-${index}`}
              isFocused={isFocused}
            />
          ))}
        </div>
        {hasDetails ? (
          <details className="mt-2 text-xs text-muted-foreground">
            <summary className="cursor-pointer list-none select-none hover:text-foreground/80">
              详情
            </summary>
            <div className="mt-1 space-y-2 border-l border-border/50 pl-3">
              <AgentChatMetadata bubble={bubble} />
              {bubble.errorLines.length > 0 ? (
                <AgentChatTextLines
                  id={`${bubble.id}-error`}
                  lines={bubble.errorLines}
                  limit={AGENT_CHAT_DETAIL_LINE_LIMIT}
                  tone="error"
                />
              ) : null}
              {hiddenBlocks.map((block, index) => (
                <AgentChatBlock
                  key={`${bubble.id}-hidden-${index}`}
                  block={block}
                  blockId={`${bubble.id}-hidden-${index}`}
                  isFocused={true}
                />
              ))}
              {bubble.detailBlocks.map((block, index) => (
                <AgentChatBlock
                  key={`${bubble.id}-detail-block-${index}`}
                  block={block}
                  blockId={`${bubble.id}-detail-block-${index}`}
                  isFocused={true}
                  detail
                />
              ))}
            </div>
          </details>
        ) : null}
      </div>
    </article>
  );
}

function AgentChatMetadata({ bubble }: { bubble: AgentChatBubble }) {
  const lines = [
    bubble.appName ? `app: ${bubble.appName}` : null,
    bubble.sourceLabel ? `source: ${bubble.sourceLabel}` : null,
    bubble.affectedFiles.length > 0
      ? `files: ${bubble.affectedFiles.slice(0, 5).join(", ")}${bubble.affectedFiles.length > 5 ? " …" : ""}`
      : null,
  ].filter((line): line is string => Boolean(line));

  if (lines.length === 0) {
    return null;
  }

  return (
    <div className="space-y-0.5">
      {lines.map((line) => (
        <p key={line} className="break-words">
          {line}
        </p>
      ))}
    </div>
  );
}

function AgentChatBlock({
  block,
  blockId,
  isFocused,
  detail = false,
}: {
  block: WebActivityBlock;
  blockId: string;
  isFocused: boolean;
  detail?: boolean;
}) {
  const record = asRecord(block);
  const type = typeof record?.type === "string" ? record.type : "unknown";
  const lineLimit = detail
    ? AGENT_CHAT_DETAIL_LINE_LIMIT
    : isFocused
      ? AGENT_CHAT_FOCUSED_MESSAGE_LINE_LIMIT
      : AGENT_CHAT_MESSAGE_LINE_LIMIT;

  if (!record) {
    return null;
  }

  if (type === "text") {
    return (
      <AgentChatTextLines
        id={blockId}
        lines={splitDisplayLines(stringValue(record.text, ""))}
        limit={lineLimit}
      />
    );
  }

  if (type === "code") {
    return (
      <AgentChatCodeBlock
        id={blockId}
        code={stringValue(record.code, "")}
        language={stringValue(record.language, "")}
        limit={lineLimit}
      />
    );
  }

  if (type === "kv") {
    const entries = kvEntriesValue(record.entries);
    return entries.length > 0 ? (
      <dl className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 text-xs text-muted-foreground">
        {entries.map((entry, index) => (
          <FragmentPair
            key={`${blockId}-kv-${index}`}
            left={entry.key}
            right={entry.value}
          />
        ))}
      </dl>
    ) : null;
  }

  if (type === "list") {
    const items = stringArrayValue(record.items);
    return items.length > 0 ? (
      <ul className="space-y-1 text-sm text-foreground/90">
        {items.slice(0, lineLimit).map((item, index) => (
          <li key={`${blockId}-item-${index}`} className="flex gap-2 break-words">
            <span className="text-muted-foreground">•</span>
            <span>{item}</span>
          </li>
        ))}
        {items.length > lineLimit ? (
          <li className="text-xs text-muted-foreground">… +{items.length - lineLimit} more</li>
        ) : null}
      </ul>
    ) : null;
  }

  if (type === "diff") {
    return (
      <AgentChatDiffBlock
        id={blockId}
        files={diffFilesValue(record.files)}
        limit={lineLimit}
      />
    );
  }

  if (type === "link") {
    const url = stringValue(record.url, "");
    const label = stringValue(record.label, url);
    return url ? (
      <a
        href={url}
        target="_blank"
        rel="noreferrer"
        className="break-all text-sky-300 underline-offset-4 hover:underline"
      >
        {label}
      </a>
    ) : null;
  }

  if (type === "artifact") {
    const label = stringValue(record.label, "Artifact");
    const uri = stringValue(record.uri, "");
    return uri ? (
      <a
        href={uri}
        target="_blank"
        rel="noreferrer"
        className="break-all text-sky-300 underline-offset-4 hover:underline"
      >
        {label}
      </a>
    ) : (
      <p className="break-words text-muted-foreground">{label}</p>
    );
  }

  return (
    <p className="break-words text-xs text-muted-foreground">
      Unsupported activity block: {safeJsonPreview(record)}
    </p>
  );
}

function FragmentPair({ left, right }: { left: string; right: string }) {
  return (
    <>
      <dt className="font-medium text-muted-foreground/80">{left}</dt>
      <dd className="min-w-0 break-words text-foreground/80">{right}</dd>
    </>
  );
}

function AgentChatTextLines({
  id,
  lines,
  limit,
  tone = "default",
}: {
  id: string;
  lines: string[];
  limit: number;
  tone?: "default" | "error";
}) {
  const visibleLines = lines.slice(0, limit);
  const hiddenLines = lines.slice(limit);

  if (lines.length === 0) {
    return null;
  }

  return (
    <div className={cn("space-y-1", tone === "error" && "text-destructive")}>
      {visibleLines.map((line, index) => (
        <p key={`${id}-line-${index}`} className="break-words">
          {line}
        </p>
      ))}
      {hiddenLines.length > 0 ? (
        <details className="text-xs text-muted-foreground">
          <summary className="cursor-pointer list-none hover:text-foreground/80">
            展开剩余 {hiddenLines.length} 行
          </summary>
          <div className="mt-1 space-y-1">
            {hiddenLines.map((line, index) => (
              <p key={`${id}-hidden-line-${index}`} className="break-words">
                {line}
              </p>
            ))}
          </div>
        </details>
      ) : null}
    </div>
  );
}

function AgentChatCodeBlock({
  id,
  code,
  language,
  limit,
}: {
  id: string;
  code: string;
  language: string;
  limit: number;
}) {
  const lines = code.split(/\r?\n/);
  const visibleLines = lines.slice(0, limit);
  const hiddenLines = lines.slice(limit);

  if (!code.trim()) {
    return <p className="text-muted-foreground">(no output)</p>;
  }

  return (
    <div className="space-y-1">
      {language ? (
        <p className="text-[0.68rem] uppercase tracking-[0.14em] text-muted-foreground">
          {language}
        </p>
      ) : null}
      <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/35 px-3 py-2 font-mono text-xs leading-5 text-foreground/85">
        {visibleLines.join("\n")}
      </pre>
      {hiddenLines.length > 0 ? (
        <details className="text-xs text-muted-foreground">
          <summary className="cursor-pointer list-none hover:text-foreground/80">
            展开完整输出（+{hiddenLines.length} 行）
          </summary>
          <pre className="mt-1 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/35 px-3 py-2 font-mono text-xs leading-5 text-foreground/85">
            {code}
          </pre>
        </details>
      ) : null}
    </div>
  );
}

function AgentChatDiffBlock({
  id,
  files,
  limit,
}: {
  id: string;
  files: AgentChatDiffFile[];
  limit: number;
}) {
  if (files.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2 font-mono text-xs">
      {files.slice(0, 3).map((file, fileIndex) => {
        const visibleLines = file.lines.slice(0, limit);
        const hiddenLines = file.lines.slice(limit);
        return (
          <div key={`${id}-file-${fileIndex}`} className="space-y-1">
            <p className="break-all text-foreground/85">
              {file.path} <span className="text-emerald-300">+{file.added_lines}</span>{" "}
              <span className="text-red-300">-{file.removed_lines}</span>
            </p>
            <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/30 px-3 py-2 leading-5">
              {visibleLines.map((line) => renderDiffLine(line)).join("\n")}
            </pre>
            {hiddenLines.length > 0 ? (
              <details className="font-sans text-xs text-muted-foreground">
                <summary className="cursor-pointer list-none hover:text-foreground/80">
                  展开剩余 {hiddenLines.length} 行 diff
                </summary>
                <pre className="mt-1 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/30 px-3 py-2 font-mono leading-5">
                  {file.lines.map((line) => renderDiffLine(line)).join("\n")}
                </pre>
              </details>
            ) : null}
          </div>
        );
      })}
      {files.length > 3 ? (
        <p className="font-sans text-xs text-muted-foreground">… +{files.length - 3} more file(s)</p>
      ) : null}
    </div>
  );
}

function agentChatBubblesFromSnapshot(
  snapshot: DashboardSnapshot | null,
): AgentChatBubble[] {
  if (!snapshot) {
    return [];
  }

  const committed = (snapshot.web_activity_items ?? [])
    .map((item, index) => agentChatBubbleFromWebActivityItem(item, `activity-${index}`))
    .filter((bubble): bubble is AgentChatBubble => Boolean(bubble));
  const live = (snapshot.live_web_activity_items ?? [])
    .map((entry, index) =>
      agentChatBubbleFromWebActivityItem(
        entry.item,
        `live-${entry.key || index}`,
        true,
      ),
    )
    .filter((bubble): bubble is AgentChatBubble => Boolean(bubble));

  return [...committed, ...live].slice(-AGENT_CHAT_MAX_VISIBLE_BUBBLES);
}

function agentChatBubbleFromWebActivityItem(
  item: WebActivityItem | unknown,
  fallbackId: string,
  live = false,
): AgentChatBubble | null {
  const record = asRecord(item);

  if (!record) {
    return null;
  }

  const id = stringValue(record.id, fallbackId);
  const kind = stringValue(record.kind, "unknown");
  const status = live ? "running" : stringValue(record.status, "unknown");
  const actor = stringValue(record.actor, "");
  const title = stringValue(record.title, agentChatFallbackTitle(actor, kind));
  const tool = asRecord(record.tool);
  const source = asRecord(record.source);
  const error = asRecord(record.error);

  return {
    id,
    role: agentChatRoleFromWebActivity(actor, kind, source),
    kind,
    status,
    title,
    blocks: webActivityBlocksValue(record.blocks),
    detailBlocks: webActivityBlocksValue(record.detail_blocks),
    live,
    toolName: tool ? stringValue(tool.name, "") : undefined,
    appName: tool ? stringValue(tool.app, "") : undefined,
    sourceLabel: source ? stringValue(source.label, stringValue(source.source_type, "")) : undefined,
    errorLines: error
      ? [stringValue(error.message, ""), ...stringArrayValue(error.details)].filter(Boolean)
      : [],
    affectedFiles: tool ? stringArrayValue(tool.affected_files) : [],
  };
}

function agentChatRoleFromWebActivity(
  actor: string,
  kind: string,
  source: Record<string, unknown> | null,
): AgentChatBubbleRole {
  if (actor === "assistant") {
    return "assistant";
  }

  if (actor === "user") {
    return "user";
  }

  if (actor === "telegram" || stringValue(source?.source_type, "") === "telegram") {
    return "telegram";
  }

  if (["plan", "workflow", "memory"].includes(kind) || actor === "system") {
    return "system";
  }

  return "tool";
}

function agentChatFallbackTitle(actor: string, kind: string) {
  if (actor === "assistant") {
    return "Agent";
  }

  if (actor === "user") {
    return "You";
  }

  if (actor === "telegram") {
    return "Telegram";
  }

  if (kind === "plan") {
    return "Plan";
  }

  if (kind === "workflow") {
    return "Workflow";
  }

  return "Activity";
}

function agentChatBubbleLabel(bubble: AgentChatBubble) {
  if (bubble.role === "assistant") {
    return bubble.live || bubble.status === "running" ? "Agent · streaming" : "Agent";
  }

  if (bubble.role === "user") {
    return "You";
  }

  if (bubble.role === "telegram") {
    return "Telegram";
  }

  if (bubble.role === "system") {
    return bubble.kind === "workflow" ? "Workflow" : bubble.kind === "memory" ? "Memory" : "System";
  }

  return bubble.live || bubble.status === "running" ? "Tool · running" : "Tool";
}

function agentChatStatusLabel(status: string) {
  switch (status) {
    case "pending":
      return "pending";
    case "running":
      return "running";
    case "failed":
      return "failed";
    case "dismissed":
      return "dismissed";
    default:
      return status;
  }
}

type AgentChatDiffFile = {
  path: string;
  operation: string;
  added_lines: number;
  removed_lines: number;
  lines: AgentChatDiffLine[];
};

type AgentChatDiffLine = {
  kind: string;
  text: string;
  old_lineno?: number | null;
  new_lineno?: number | null;
};

function webActivityBlocksValue(value: unknown): WebActivityBlock[] {
  if (!Array.isArray(value)) {
    return [];
  }

  return value.filter((block): block is WebActivityBlock => {
    const record = asRecord(block);
    return Boolean(record && typeof record.type === "string");
  });
}

function kvEntriesValue(value: unknown) {
  if (!Array.isArray(value)) {
    return [];
  }

  return value
    .map(asRecord)
    .filter((entry): entry is Record<string, unknown> => Boolean(entry))
    .map((entry) => ({
      key: stringValue(entry.key, ""),
      value: stringValue(entry.value, ""),
    }))
    .filter((entry) => entry.key || entry.value);
}

function diffFilesValue(value: unknown): AgentChatDiffFile[] {
  if (!Array.isArray(value)) {
    return [];
  }

  return value
    .map(asRecord)
    .filter((file): file is Record<string, unknown> => Boolean(file))
    .map((file) => ({
      path: stringValue(file.path, "unknown"),
      operation: stringValue(file.operation, "update"),
      added_lines: numberValue(file.added_lines, 0),
      removed_lines: numberValue(file.removed_lines, 0),
      lines: diffLinesValue(file.lines),
    }));
}

function diffLinesValue(value: unknown): AgentChatDiffLine[] {
  if (!Array.isArray(value)) {
    return [];
  }

  return value
    .map(asRecord)
    .filter((line): line is Record<string, unknown> => Boolean(line))
    .map((line) => ({
      kind: stringValue(line.kind, "context"),
      text: stringValue(line.text, ""),
      old_lineno: nullableNumberValue(line.old_lineno),
      new_lineno: nullableNumberValue(line.new_lineno),
    }));
}

function renderDiffLine(line: AgentChatDiffLine) {
  const prefix = line.kind === "add" ? "+" : line.kind === "delete" ? "-" : " ";
  return `${prefix} ${line.text}`;
}

function splitDisplayLines(text: string) {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }

  return value as Record<string, unknown>;
}

function stringValue(value: unknown, fallback: string) {
  return typeof value === "string" && value.trim() ? value : fallback;
}

function numberValue(value: unknown, fallback: number) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function nullableNumberValue(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function stringArrayValue(value: unknown) {
  if (!Array.isArray(value)) {
    return [];
  }

  return value
    .filter((line): line is string => typeof line === "string")
    .map((line) => line.trim())
    .filter(Boolean);
}

function safeJsonPreview(value: unknown) {
  try {
    const text = JSON.stringify(value);
    if (!text) {
      return "unknown";
    }
    return text.length > 160 ? `${text.slice(0, 160)}…` : text;
  } catch {
    return "unknown";
  }
}

function agentChatSendResultText(output: string) {
  return /^queued terminal message as event\b/.test(output)
    ? "已发送给 agent"
    : output;
}

export function StatusPage() {
  const { snapshot } = useDashboardSnapshot();
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
      className="min-h-screen w-full px-6 pb-10 pt-20 md:pb-12 md:pt-24"
    >
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
                    snapshot,
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
      <GripVerticalIcon className="size-4" />
    </Button>
  );
}

function TelegramApprovalCard({
  snapshot,
  dragHandle,
}: {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
}) {
  const requests = snapshot?.pending_access_requests ?? [];
  const [busyChatId, setBusyChatId] = useState<number | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  async function handleRequestAction(
    request: DashboardPendingAccessRequest,
    action: "approve" | "reject",
  ) {
    setBusyChatId(request.chat_id);
    setActionError(null);

    try {
      await runDashboardCommand(`/telegram ${action} ${request.chat_id}`);
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
          <div className="space-y-3">
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
                      variant="outline"
                      size="icon-sm"
                      aria-label={`Approve ${label}`}
                      disabled={busyChatId !== null}
                      onClick={() => handleRequestAction(request, "approve")}
                      className="text-emerald-600 hover:bg-emerald-500/10 hover:text-emerald-600"
                    >
                      <CheckIcon className="size-4" />
                    </Button>
                    <Button
                      type="button"
                      variant="destructive"
                      size="icon-sm"
                      aria-label={`Reject ${label}`}
                      disabled={busyChatId !== null}
                      onClick={() => handleRequestAction(request, "reject")}
                    >
                      <XIcon className="size-4" />
                    </Button>
                  </div>
                  {isBusy ? <span className="sr-only">Processing</span> : null}
                </div>
              );
            })}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No pending Telegram approvals.
          </p>
        )}
        {actionError ? (
          <p className="mt-3 text-xs text-destructive">{actionError}</p>
        ) : null}
      </CardContent>
    </Card>
  );
}

function DailyTokenUsageCard({
  snapshot,
  dragHandle,
}: {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
}) {
  const chartData = useMemo(() => dailyTokenUsageChartData(snapshot), [snapshot]);
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
          <p className="mt-2 text-xs text-muted-foreground">
            No token usage recorded yet.
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function ModelContextCompositionCard({
  snapshot,
  dragHandle,
}: {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
}) {
  const composition = snapshot?.context_composition;
  const compositionData = useMemo(
    () => contextCompositionCardData(snapshot),
    [snapshot],
  );

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>Model Context Composition</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent className="space-y-4">
        {composition ? (
          <>
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
                label="Stable prefix"
                value={formatPercent(compositionData.stablePrefixRatio)}
                detail={formatCompactNumber(composition.stable_prefix_tokens)}
              />
            </div>

            <div className="space-y-2">
              <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
                <span>Prefix reuse vs changed/new request tail</span>
                <span className="font-mono tabular-nums">
                  {composition.previous_request_hash ? "vs previous" : "first snapshot"}
                </span>
              </div>
              <ChartContainer
                config={CONTEXT_COMPOSITION_CHART_CONFIG}
                className="h-12 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
              >
                <BarChart
                  accessibilityLayer
                  data={compositionData.prefixSummaryData}
                  layout="vertical"
                  margin={{ top: 8, right: 0, left: 0, bottom: 8 }}
                  stackOffset="expand"
                >
                  <XAxis
                    type="number"
                    hide
                    domain={[0, 1]}
                  />
                  <YAxis
                    type="category"
                    dataKey="label"
                    hide
                  />
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
              <div className="grid grid-cols-3 gap-2 text-xs">
                {compositionData.prefixLegend.map((bar) => (
                  <div
                    key={bar.key}
                    className="flex min-w-0 items-center gap-1.5 text-muted-foreground"
                  >
                    <span
                      className="size-2 shrink-0 rounded-[2px]"
                      style={{
                        backgroundColor: `var(--color-${bar.colorKey})`,
                      }}
                    />
                    <span className="truncate">{bar.shortLabel}</span>
                    <span className="ml-auto font-mono tabular-nums text-foreground">
                      {formatPercent(bar.ratio)}
                    </span>
                  </div>
                ))}
              </div>
            </div>

            <ChartContainer
              config={CONTEXT_COMPOSITION_CHART_CONFIG}
              className="h-64 w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
            >
              <BarChart
                accessibilityLayer
                data={compositionData.segmentChartData}
                layout="vertical"
                margin={{ top: 8, right: 12, left: 8, bottom: 0 }}
                barCategoryGap="24%"
              >
                <XAxis
                  type="number"
                  hide
                  domain={[0, compositionData.maxSegmentTokens]}
                />
                <YAxis
                  type="category"
                  dataKey="shortLabel"
                  width={118}
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                />
                <ChartTooltip
                  cursor={{ fill: "var(--muted)" }}
                  wrapperStyle={{ zIndex: 50 }}
                  content={<ContextCompositionTooltip />}
                />
                <Bar
                  dataKey="tokens"
                  fill="var(--color-tokens)"
                  radius={[0, 4, 4, 0]}
                  isAnimationActive={false}
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
              <ContextCompositionDetailRow
                label="Bytes"
                value={formatCompactNumber(composition.total_bytes)}
              />
              <ContextCompositionDetailRow
                label="Model"
                value={composition.model ?? "unknown"}
              />
            </div>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            Waiting for the next model request to capture context composition.
          </p>
        )}
      </CardContent>
    </Card>
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

function WorkflowOptimizationCard({
  snapshot,
  dragHandle,
}: {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
}) {
  const progressData = useMemo(
    () => workflowOptimizationProgressData(snapshot),
    [snapshot],
  );
  const chartData = useMemo(
    () => workflowOptimizationDonutData(progressData),
    [progressData],
  );
  const total = progressData.reduce((sum, item) => sum + item.value, 0);

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>Workflow Optimization</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        <div className="relative mx-auto h-48 w-full max-w-48">
          <ChartContainer
            config={WORKFLOW_OPTIMIZATION_CHART_CONFIG}
            className="h-full w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
          >
            <PieChart accessibilityLayer>
              <ChartTooltip
                cursor={false}
                wrapperStyle={{ zIndex: 50 }}
                content={<WorkflowOptimizationTooltip />}
              />
              <Pie
                data={chartData}
                dataKey="chartValue"
                nameKey="label"
                innerRadius={44}
                outerRadius={66}
                paddingAngle={2}
                strokeWidth={0}
                isAnimationActive={false}
              >
                {chartData.map((item) => (
                  <Cell
                    key={item.key}
                    fill={`var(--color-${item.colorKey})`}
                  />
                ))}
              </Pie>
            </PieChart>
          </ChartContainer>
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
            <div className="text-center">
              <div className="font-mono text-2xl font-medium tabular-nums text-foreground">
                {formatCompactNumber(total)}
              </div>
              <div className="text-xs text-muted-foreground">
                {total > 0 ? "Events" : "No data"}
              </div>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function RuntimeOptimizationCard({
  snapshot,
  dragHandle,
}: {
  snapshot: DashboardSnapshot | null;
  dragHandle: ReactNode;
}) {
  const progressData = useMemo(
    () => runtimeOptimizationProgressData(snapshot),
    [snapshot],
  );
  const chartData = useMemo(
    () => runtimeOptimizationDonutData(progressData),
    [progressData],
  );
  const total = progressData.reduce((sum, item) => sum + item.value, 0);

  return (
    <Card className="w-full overflow-visible">
      <CardHeader>
        <CardTitle>Runtime Optimization</CardTitle>
        <CardAction>{dragHandle}</CardAction>
      </CardHeader>
      <CardContent>
        <div className="relative mx-auto h-48 w-full max-w-48">
          <ChartContainer
            config={RUNTIME_OPTIMIZATION_CHART_CONFIG}
            className="h-full w-full overflow-visible [&_.recharts-wrapper]:overflow-visible"
          >
            <PieChart accessibilityLayer>
              <ChartTooltip
                cursor={false}
                wrapperStyle={{ zIndex: 50 }}
                content={<RuntimeOptimizationTooltip />}
              />
              <Pie
                data={chartData}
                dataKey="chartValue"
                nameKey="label"
                innerRadius={44}
                outerRadius={66}
                paddingAngle={2}
                strokeWidth={0}
                isAnimationActive={false}
              >
                {chartData.map((item) => (
                  <Cell
                    key={item.key}
                    fill={`var(--color-${item.colorKey})`}
                  />
                ))}
              </Pie>
            </PieChart>
          </ChartContainer>
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
            <div className="text-center">
              <div className="font-mono text-2xl font-medium tabular-nums text-foreground">
                {formatCompactNumber(total)}
              </div>
              <div className="text-xs text-muted-foreground">
                {total > 0 ? "Events" : "No data"}
              </div>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  );
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

type ContextCompositionSegmentChartDatum = {
  key: string;
  label: string;
  shortLabel: string;
  source: string;
  tokens: number;
  bytes: number;
  percent: number;
  cacheRole: string;
};

type ContextCompositionPrefixBar = {
  key: string;
  label: string;
  shortLabel: string;
  tokens: number;
  ratio: number;
  colorKey: keyof typeof CONTEXT_COMPOSITION_CHART_CONFIG;
};

type ContextCompositionPrefixSummaryDatum = {
  label: string;
  stable: number;
  changed: number;
  new: number;
  unknown: number;
  bars: ContextCompositionPrefixBar[];
};

type ContextCompositionCardData = {
  segmentChartData: ContextCompositionSegmentChartDatum[];
  maxSegmentTokens: number;
  stablePrefixRatio: number;
  newSuffixRatio: number;
  prefixSummaryData: ContextCompositionPrefixSummaryDatum[];
  prefixLegend: ContextCompositionPrefixBar[];
};

type ContextCompositionTooltipPayloadItem = {
  payload?: ContextCompositionSegmentChartDatum;
};

type ContextPrefixTooltipPayloadItem = {
  payload?: ContextCompositionPrefixSummaryDatum;
};

type WorkflowOptimizationChartDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: keyof typeof WORKFLOW_OPTIMIZATION_CHART_CONFIG;
  detail: string;
};

type WorkflowOptimizationDonutDatum = WorkflowOptimizationChartDatum & {
  chartValue: number;
};

type WorkflowOptimizationTooltipPayloadItem = {
  payload?: WorkflowOptimizationDonutDatum;
};

type RuntimeOptimizationChartDatum = {
  key: string;
  label: string;
  value: number;
  colorKey: keyof typeof RUNTIME_OPTIMIZATION_CHART_CONFIG;
  detail: string;
};

type RuntimeOptimizationDonutDatum = RuntimeOptimizationChartDatum & {
  chartValue: number;
};

type RuntimeOptimizationTooltipPayloadItem = {
  payload?: RuntimeOptimizationDonutDatum;
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

function contextCompositionCardData(
  snapshot: DashboardSnapshot | null,
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

function workflowOptimizationDonutData(
  progressData: WorkflowOptimizationChartDatum[],
): WorkflowOptimizationDonutDatum[] {
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
      detail: "No workflow optimization activity yet",
    },
  ];
}

function runtimeOptimizationProgressData(
  snapshot: DashboardSnapshot | null,
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

function runtimeOptimizationDonutData(
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

function ContextCompositionTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: ContextCompositionTooltipPayloadItem[];
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
      <div className="flex items-center gap-2 font-medium text-foreground">
        <span
          className="size-2 shrink-0 rounded-[2px]"
          style={{ backgroundColor: "var(--color-tokens)" }}
        />
        <span className="min-w-0 flex-1 truncate">{datum.label}</span>
        <span className="font-mono tabular-nums">
          {formatCompactNumber(datum.tokens)}
        </span>
      </div>
      <div className="grid gap-1 text-muted-foreground">
        <ContextCompositionTooltipRow
          label="Share"
          value={formatPercent(datum.percent / 100)}
        />
        <ContextCompositionTooltipRow
          label="Bytes"
          value={formatCompactNumber(datum.bytes)}
        />
        <ContextCompositionTooltipRow
          label="Source"
          value={datum.source}
        />
        <ContextCompositionTooltipRow
          label="Cache role"
          value={datum.cacheRole}
        />
      </div>
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
          <div
            key={bar.key}
            className="flex min-w-0 items-center gap-2"
          >
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

function ContextCompositionTooltipRow({
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

function RuntimeOptimizationTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: RuntimeOptimizationTooltipPayloadItem[];
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
  const [, , month, day] = date.match(/^(\d{4})-(\d{2})-(\d{2})$/) ?? [];

  if (!month || !day) {
    return date;
  }

  return `${Number(month)}月${Number(day)}日`;
}

function formatPercentAxisTick(value: number) {
  return `${Math.round(value * 100)}%`;
}

function formatPercent(value: number) {
  if (!Number.isFinite(value)) {
    return "0%";
  }

  return `${Math.round(Math.max(0, Math.min(1, value)) * 100)}%`;
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

function telegramApprovalDisplayName(request: DashboardPendingAccessRequest) {
  return request.sender || request.title || "Unknown";
}

function telegramApprovalInitials(value: string) {
  const characters = Array.from(value.trim());
  return characters.slice(0, 2).join("").toUpperCase() || "TG";
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

  if (!snapshot) {
    return { animationStatus: "idle", label: "空闲" };
  }

  const runtimeStatus = snapshot.runtime_status?.toLowerCase() ?? "";
  const dashboardText = [snapshot.runtime_status, snapshot.status_output]
    .join(" ")
    .toLowerCase();
  const hasRunningTurn = /\bruntime turn:\s*running\b/.test(dashboardText);

  if (!runtimeStatus && !hasRunningTurn) {
    return { animationStatus: "idle", label: "空闲" };
  }

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
