import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type DragEvent,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
  type UIEvent,
} from "react";

import {
  AgentStatusAnimation,
  type AgentAnimationStatus,
} from "@/components/agent-status-animation";
import {
  ArrowDownIcon,
  CheckIcon,
  ClipboardIcon,
  GripVerticalIcon,
  ImagePlusIcon,
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
  fetchDashboardActivityHistory,
  getDashboardAttachmentUrl,
  runDashboardCommand,
  subscribeDashboardSnapshots,
  type DashboardActivityHistoryPage,
  type DashboardCommandAttachment,
  type DashboardContextCompositionSegment,
  type DashboardPendingAccessRequest,
  type DashboardSnapshot,
  type ActivityCellVariant,
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
const AGENT_CHAT_HISTORY_PAGE_LIMIT = 80;
const AGENT_CHAT_PREVIEW_MAX_VISIBLE_BUBBLES = 24;
const AGENT_CHAT_MESSAGE_LINE_LIMIT = 5;
const AGENT_CHAT_FOCUSED_MESSAGE_LINE_LIMIT = 12;
const AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT = Number.MAX_SAFE_INTEGER;
const AGENT_CHAT_CANONICAL_CELL_DIFF_LINE_LIMIT = 18;
const AGENT_CHAT_PLAN_STEP_LIMIT = 8;
const AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES = 4;
const AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES = 4;
const AGENT_CHAT_PATCH_FILE_LIMIT = 4;
const AGENT_CHAT_PATCH_DIFF_LINE_LIMIT = 18;
const AGENT_CHAT_TELEGRAM_DETAIL_LIMIT = 6;
const AGENT_CHAT_TELEGRAM_MESSAGE_LIMIT = 6;
const AGENT_CHAT_TERMINAL_WAIT_LINE_LIMIT = 6;
const AGENT_CHAT_ERROR_LINE_LIMIT = 12;
const AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX = 72;
const AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX = 160;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENTS = 4;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES = 10 * 1024 * 1024;
const AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX = 60;
const AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX = 16;
const AGENT_CHAT_PREVIEW_NOTICE_VISIBLE_MS = 3000;
const AGENT_CHAT_PREVIEW_NOTICE_FADE_MS = 300;
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
  const [chatComposerHeight, setChatComposerHeight] = useState(
    AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX,
  );
  const [isChatFocused, setIsChatFocused] = useState(false);
  const [chatPreviewNotice, setChatPreviewNotice] = useState<string | null>(null);
  const [isChatPreviewNoticeVisible, setIsChatPreviewNoticeVisible] = useState(false);
  const chatPreviewNoticeFrameRef = useRef<number | undefined>(undefined);
  const chatPreviewNoticeHideTimeoutRef = useRef<number | undefined>(undefined);
  const chatPreviewNoticeClearTimeoutRef = useRef<number | undefined>(undefined);
  const agentStatus = deriveAgentStatus({
    hasLoadError: Boolean(loadError),
    isLoading,
    snapshot,
  });
  const summaryText = derivePlanSummaryText(snapshot);
  const { isTyping, text: typedSummaryText } = useTypewriterText(summaryText);
  const clearChatPreviewNoticeSchedule = useCallback(() => {
    if (chatPreviewNoticeFrameRef.current !== undefined) {
      window.cancelAnimationFrame(chatPreviewNoticeFrameRef.current);
      chatPreviewNoticeFrameRef.current = undefined;
    }

    if (chatPreviewNoticeHideTimeoutRef.current !== undefined) {
      window.clearTimeout(chatPreviewNoticeHideTimeoutRef.current);
      chatPreviewNoticeHideTimeoutRef.current = undefined;
    }

    if (chatPreviewNoticeClearTimeoutRef.current !== undefined) {
      window.clearTimeout(chatPreviewNoticeClearTimeoutRef.current);
      chatPreviewNoticeClearTimeoutRef.current = undefined;
    }
  }, []);

  useEffect(() => {
    return clearChatPreviewNoticeSchedule;
  }, [clearChatPreviewNoticeSchedule]);

  const handleAgentChatSendResult = useCallback(
    (resultText: string) => {
      clearChatPreviewNoticeSchedule();
      setChatPreviewNotice(resultText);
      setIsChatPreviewNoticeVisible(false);

      chatPreviewNoticeFrameRef.current = window.requestAnimationFrame(() => {
        setIsChatPreviewNoticeVisible(true);
        chatPreviewNoticeFrameRef.current = undefined;
      });

      chatPreviewNoticeHideTimeoutRef.current = window.setTimeout(() => {
        setIsChatPreviewNoticeVisible(false);
        chatPreviewNoticeHideTimeoutRef.current = undefined;
        chatPreviewNoticeClearTimeoutRef.current = window.setTimeout(() => {
          setChatPreviewNotice(null);
          chatPreviewNoticeClearTimeoutRef.current = undefined;
        }, AGENT_CHAT_PREVIEW_NOTICE_FADE_MS);
      }, AGENT_CHAT_PREVIEW_NOTICE_VISIBLE_MS);
    },
    [clearChatPreviewNoticeSchedule],
  );

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
        composerHeight={chatComposerHeight}
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
            "relative z-20 grid min-h-6 max-w-[min(32rem,calc(100vw-3rem))] text-balance text-sm font-medium leading-6 text-muted-foreground transition-opacity duration-300 md:text-base",
            isChatFocused && "opacity-40",
          )}
        >
          <span
            aria-hidden={Boolean(chatPreviewNotice)}
            className={cn(
              "col-start-1 row-start-1 transition-opacity duration-300",
              chatPreviewNotice ? "opacity-0" : "opacity-100",
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
          </span>
          {chatPreviewNotice ? (
            <span
              className={cn(
                "col-start-1 row-start-1 transition-opacity duration-300",
                isChatPreviewNoticeVisible ? "opacity-100" : "opacity-0",
              )}
            >
              {chatPreviewNotice}
            </span>
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
        onHeightChange={setChatComposerHeight}
        onSendResult={handleAgentChatSendResult}
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
  planSteps: AgentChatPlanStep[];
  live?: boolean;
  toolName?: string;
  appName?: string;
  sourceLabel?: string;
  cell?: ActivityCellVariant | null;
};

type AgentChatPlanStepStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "unknown";

type AgentChatPlanStep = {
  status: AgentChatPlanStepStatus;
  text: string;
};

type AgentChatImageAttachmentData = {
  label: string;
  uri: string;
  mimeType: string;
};

type AgentChatPendingImageAttachment = {
  id: string;
  file: File;
  previewUrl: string;
};

type AgentChatActivityCellRender =
  | {
      kind: "text";
      marker: string;
      title: string;
      bodyLines: string[];
      imageAttachments?: AgentChatImageAttachmentData[];
      bodyLimit?: number;
      tone?: "default" | "error" | "muted";
    }
  | {
      kind: "browser";
      marker: string;
      title: string;
      detailLines: string[];
      detailLimit?: number;
    }
  | {
      kind: "plan";
      marker: string;
      title: string;
      steps: AgentChatPlanStep[];
    }
  | {
      kind: "workflow";
      marker: string;
      title: string;
      workflowId: string;
    }
  | {
      kind: "deepRecall";
      marker: string;
      title: string;
      memoryCount: number;
    }
  | {
      kind: "exec";
      marker: string;
      title: string;
      outputLines: string[];
      running?: boolean;
      exitCode?: number | null;
    }
  | {
      kind: "patch";
      marker: string;
      title: string;
      files: AgentChatDiffFile[];
    }
  | {
      kind: "messageActivity";
      marker: string;
      title: string;
      detailLines: string[];
      messageLines: string[];
      detailLimit: number;
      messageLimit: number;
    }
  | {
      kind: "reply";
      marker: string;
      title: string;
      messageLines: string[];
      disposition: string;
    };

type AgentChatActivityCellViewProps = {
  bubbleId: string;
  render: AgentChatActivityCellRender;
};

type AgentChatMarkdownBlockData =
  | { type: "paragraph"; text: string }
  | { type: "heading"; level: number; text: string }
  | { type: "list"; ordered: boolean; items: string[] }
  | { type: "blockquote"; lines: string[] }
  | { type: "rule" };

type AgentChatMarkdownInlineNode =
  | { type: "text"; text: string }
  | { type: "code"; text: string }
  | { type: "strong"; text: string }
  | { type: "em"; text: string }
  | { type: "link"; label: string; href: string };

type AgentChatMarkdownInlineToken = {
  start: number;
  end: number;
  node: AgentChatMarkdownInlineNode;
};

function AgentChatComposer({
  isFocused,
  onFocusChange,
  chatPanelRef,
  onHeightChange,
  onSendResult,
}: {
  isFocused: boolean;
  onFocusChange: (isFocused: boolean) => void;
  chatPanelRef: RefObject<HTMLDivElement | null>;
  onHeightChange: (height: number) => void;
  onSendResult: (resultText: string) => void;
}) {
  const formRef = useRef<HTMLFormElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [message, setMessage] = useState("");
  const [imageAttachments, setImageAttachments] = useState<
    AgentChatPendingImageAttachment[]
  >([]);
  const imageAttachmentsRef = useRef<AgentChatPendingImageAttachment[]>([]);
  const [isSending, setIsSending] = useState(false);
  const [isDraggingImage, setIsDraggingImage] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

  useEffect(() => {
    imageAttachmentsRef.current = imageAttachments;
  }, [imageAttachments]);

  useEffect(() => {
    return () => {
      for (const attachment of imageAttachmentsRef.current) {
        URL.revokeObjectURL(attachment.previewUrl);
      }
    };
  }, []);

  useEffect(() => {
    const form = formRef.current;
    if (!form) {
      return;
    }

    const updateHeight = () => {
      onHeightChange(Math.ceil(form.getBoundingClientRect().height));
    };

    updateHeight();

    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", updateHeight);
      return () => window.removeEventListener("resize", updateHeight);
    }

    const resizeObserver = new ResizeObserver(updateHeight);
    resizeObserver.observe(form);

    return () => resizeObserver.disconnect();
  }, [onHeightChange]);

  function handleFocus() {
    onFocusChange(true);
  }

  function handleCloseFocus() {
    onFocusChange(false);
  }

  function addImageFiles(files: Iterable<File>) {
    const nextFiles = Array.from(files).filter((file) =>
      file.type.startsWith("image/"),
    );
    if (nextFiles.length === 0) {
      return;
    }

    setImageAttachments((current) => {
      const remainingSlots = Math.max(
        0,
        AGENT_CHAT_MAX_IMAGE_ATTACHMENTS - current.length,
      );
      const accepted = nextFiles.slice(0, remainingSlots);
      const rejectedForCount = nextFiles.length - accepted.length;
      const valid = accepted.filter(
        (file) => file.size <= AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES,
      );
      const oversized = accepted.find(
        (file) => file.size > AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES,
      );

      if (rejectedForCount > 0) {
        setSendError(
          `最多只能附加 ${AGENT_CHAT_MAX_IMAGE_ATTACHMENTS} 张图片。`,
        );
      } else if (oversized) {
        setSendError(
          `${oversized.name} 太大了，单张图片上限是 ${formatFileSize(
            AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES,
          )}。`,
        );
      } else if (valid.length > 0) {
        setSendError(null);
      }

      return [
        ...current,
        ...valid.map((file) => ({
          id: `${file.name}-${file.size}-${file.lastModified}-${crypto.randomUUID()}`,
          file,
          previewUrl: URL.createObjectURL(file),
        })),
      ];
    });
  }

  function removeImageAttachment(id: string) {
    setImageAttachments((current) => {
      const removed = current.find((attachment) => attachment.id === id);
      if (removed) {
        URL.revokeObjectURL(removed.previewUrl);
      }
      return current.filter((attachment) => attachment.id !== id);
    });
  }

  async function commandAttachmentsFromPendingImages(): Promise<
    DashboardCommandAttachment[]
  > {
    return Promise.all(
      imageAttachments.map(async (attachment) => ({
        name: attachment.file.name || "image",
        media_type: attachment.file.type || "application/octet-stream",
        data_url: await readFileAsDataUrl(attachment.file),
      })),
    );
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = message.trim();

    if ((!trimmed && imageAttachments.length === 0) || isSending) {
      return;
    }

    setIsSending(true);
    setSendError(null);

    try {
      const attachments = await commandAttachmentsFromPendingImages();
      const output = await runDashboardCommand(trimmed, { attachments });
      const sendResultText = agentChatSendResultText(output);
      setMessage("");
      setImageAttachments((current) => {
        for (const attachment of current) {
          URL.revokeObjectURL(attachment.previewUrl);
        }
        return [];
      });
      if (sendResultText) {
        onSendResult(sendResultText);
      }
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

  function handlePaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(event.clipboardData.files).filter((file) =>
      file.type.startsWith("image/"),
    );
    if (files.length > 0) {
      addImageFiles(files);
    }
  }

  function handleDrop(event: DragEvent<HTMLFormElement>) {
    const files = Array.from(event.dataTransfer.files).filter((file) =>
      file.type.startsWith("image/"),
    );
    if (files.length === 0) {
      return;
    }
    event.preventDefault();
    setIsDraggingImage(false);
    addImageFiles(files);
  }

  return (
    <form
      ref={formRef}
      aria-label="Send message to agent"
      onSubmit={handleSubmit}
      onFocus={handleFocus}
      onDragEnter={(event) => {
        if (hasImageDragItems(event.dataTransfer)) {
          event.preventDefault();
          setIsDraggingImage(true);
        }
      }}
      onDragOver={(event) => {
        if (hasImageDragItems(event.dataTransfer)) {
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
          setIsDraggingImage(true);
        }
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setIsDraggingImage(false);
        }
      }}
      onDrop={handleDrop}
      className={cn(
        "fixed bottom-5 left-1/2 z-30 w-[min(42rem,calc(100vw-2rem))] -translate-x-1/2 rounded-[16px] border bg-background/85 p-2 shadow-2xl shadow-background/40 backdrop-blur-xl transition-all duration-300",
        isDraggingImage && "border-primary/70 ring-4 ring-primary/15",
        isFocused
          ? "border-primary/45 ring-4 ring-primary/10"
          : "border-border/70 hover:border-primary/30",
      )}
    >
      <input
        ref={fileInputRef}
        type="file"
        accept="image/*"
        multiple
        className="sr-only"
        aria-label="Attach images"
        onChange={(event) => {
          addImageFiles(event.target.files ?? []);
          event.currentTarget.value = "";
        }}
      />
      {imageAttachments.length > 0 ? (
        <div className="flex gap-2 overflow-x-auto px-2 pb-2">
          {imageAttachments.map((attachment) => (
            <div
              key={attachment.id}
              className="group relative h-16 w-16 shrink-0 overflow-hidden rounded-xl border border-border/70 bg-muted"
              title={`${attachment.file.name} · ${formatFileSize(attachment.file.size)}`}
            >
              <img
                src={attachment.previewUrl}
                alt={attachment.file.name || "Pending image attachment"}
                className="h-full w-full object-cover"
              />
              <button
                type="button"
                aria-label={`Remove ${attachment.file.name || "image"}`}
                onClick={() => removeImageAttachment(attachment.id)}
                className="absolute right-1 top-1 rounded-full bg-background/90 p-1 text-muted-foreground opacity-90 shadow-sm transition hover:text-foreground group-hover:opacity-100"
              >
                <XIcon className="size-3" />
              </button>
            </div>
          ))}
        </div>
      ) : null}
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="ghost"
          size="icon-lg"
          aria-label="Attach image"
          onClick={() => fileInputRef.current?.click()}
          className="rounded-full text-muted-foreground hover:text-foreground"
          disabled={
            isSending ||
            imageAttachments.length >= AGENT_CHAT_MAX_IMAGE_ATTACHMENTS
          }
        >
          <ImagePlusIcon className="size-4" />
        </Button>
        <textarea
          value={message}
          rows={1}
          placeholder="和 agent 说点什么，或粘贴/拖入图片…"
          aria-label="Message"
          onChange={(event) => {
            setMessage(event.target.value);
            setSendError(null);
          }}
          onPaste={handlePaste}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              event.currentTarget.form?.requestSubmit();
            }
          }}
          className="max-h-32 min-h-11 flex-1 resize-none bg-transparent px-4 py-3 text-sm leading-5 outline-none placeholder:text-muted-foreground/70"
        />
        {isFocused ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-lg"
            aria-label="Collapse agent chat"
            onClick={handleCloseFocus}
            className="rounded-full text-muted-foreground hover:text-foreground"
          >
            <XIcon className="size-4" />
          </Button>
        ) : null}
        <Button
          type="submit"
          size="icon-lg"
          disabled={
            (!message.trim() && imageAttachments.length === 0) || isSending
          }
          aria-label="Send message"
          className="rounded-full"
        >
          {isSending ? (
            <Loader2Icon className="size-4 animate-spin" />
          ) : (
            <SendHorizontalIcon className="size-4" />
          )}
        </Button>
      </div>
      {sendError ? (
        <p
          role="alert"
          className="px-4 pb-1 pt-0.5 text-xs text-destructive"
        >
          {sendError}
        </p>
      ) : null}
    </form>
  );
}

function hasImageDragItems(dataTransfer: DataTransfer) {
  return Array.from(dataTransfer.items).some((item) => {
    if (item.kind === "file") {
      return item.type.startsWith("image/");
    }
    return false;
  });
}

function readFileAsDataUrl(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error("读取图片失败。"));
      }
    });
    reader.addEventListener("error", () => {
      reject(reader.error ?? new Error("读取图片失败。"));
    });
    reader.readAsDataURL(file);
  });
}

function formatFileSize(bytes: number) {
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
  const maximumFractionDigits = value >= 10 || unitIndex === 0 ? 0 : 1;
  return `${value.toFixed(maximumFractionDigits)} ${units[unitIndex]}`;
}

function AgentChatBubbles({
  snapshot,
  isFocused,
  panelRef,
  composerHeight,
}: {
  snapshot: DashboardSnapshot | null;
  isFocused: boolean;
  panelRef: RefObject<HTMLDivElement | null>;
  composerHeight: number;
}) {
  const snapshotBubbles = useMemo(() => agentChatBubblesFromSnapshot(snapshot), [snapshot]);
  const [historyBubbles, setHistoryBubbles] = useState<AgentChatBubble[]>([]);
  const [oldestCursor, setOldestCursor] = useState<number | null>(null);
  const [hasMoreBefore, setHasMoreBefore] = useState(false);
  const [isLoadingHistory, setIsLoadingHistory] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const lastFocusedScrollTopRef = useRef(0);
  const hasFocusedScrollPositionRef = useRef(false);
  const shouldRestoreFocusScrollRef = useRef(false);
  const isFocusedNearBottomRef = useRef(true);
  const restoreAfterPrependRef = useRef<{
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const [showScrollToBottom, setShowScrollToBottom] = useState(false);
  const bubbles = useMemo(
    () => mergeAgentChatBubbles(historyBubbles, snapshotBubbles),
    [historyBubbles, snapshotBubbles],
  );
  const visibleBubbles = isFocused
    ? bubbles
    : bubbles.slice(-AGENT_CHAT_PREVIEW_MAX_VISIBLE_BUBBLES);

  function scrollToChatBottom(behavior: ScrollBehavior = "auto") {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }

    panel.scrollTo({
      top: panel.scrollHeight,
      behavior,
    });
  }

  function updateScrollButtonVisibility(panel: HTMLDivElement) {
    const distanceFromBottom =
      panel.scrollHeight - panel.clientHeight - panel.scrollTop;
    setShowScrollToBottom(
      isFocused && distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX,
    );
  }

  function handleScroll(event: UIEvent<HTMLDivElement>) {
    const panel = event.currentTarget;
    const distanceFromBottom =
      panel.scrollHeight - panel.clientHeight - panel.scrollTop;

    if (isFocused) {
      lastFocusedScrollTopRef.current = panel.scrollTop;
      hasFocusedScrollPositionRef.current = true;
      isFocusedNearBottomRef.current =
        distanceFromBottom <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX;
    }

    setShowScrollToBottom(
      isFocused && distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX,
    );
    if (
      isFocused &&
      panel.scrollTop <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX &&
      hasMoreBefore &&
      !isLoadingHistory
    ) {
      void loadOlderHistory();
    }
  }

  function handleScrollToBottomClick() {
    isFocusedNearBottomRef.current = true;
    scrollToChatBottom("smooth");
    setShowScrollToBottom(false);
  }

  const loadOlderHistory = useCallback(async () => {
    if (!isFocused || isLoadingHistory || !hasMoreBefore || oldestCursor === null) {
      return;
    }

    const panel = panelRef.current;
    if (panel) {
      restoreAfterPrependRef.current = {
        scrollHeight: panel.scrollHeight,
        scrollTop: panel.scrollTop,
      };
    }

    setIsLoadingHistory(true);
    setHistoryError(null);
    try {
      const page = await fetchDashboardActivityHistory({
        before: oldestCursor,
        limit: AGENT_CHAT_HISTORY_PAGE_LIMIT,
      });
      setHistoryBubbles((current) =>
        mergeAgentChatBubbles(agentChatBubblesFromHistoryPage(page), current),
      );
      setOldestCursor(page.oldest_cursor ?? oldestCursor);
      setHasMoreBefore(page.has_more_before);
    } catch (error) {
      restoreAfterPrependRef.current = null;
      setHistoryError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsLoadingHistory(false);
    }
  }, [hasMoreBefore, isFocused, isLoadingHistory, oldestCursor, panelRef]);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }

    if (isFocused) {
      if (!hasFocusedScrollPositionRef.current) {
        lastFocusedScrollTopRef.current = panel.scrollHeight;
        hasFocusedScrollPositionRef.current = true;
        isFocusedNearBottomRef.current = true;
      }
      shouldRestoreFocusScrollRef.current = true;
      return;
    }

    shouldRestoreFocusScrollRef.current = false;
    setShowScrollToBottom(false);
    window.requestAnimationFrame(() => {
      scrollToChatBottom();
    });
  }, [isFocused, panelRef]);

  useEffect(() => {
    const historyWindow = snapshot?.activity_history;
    setHistoryBubbles(agentChatCommittedBubblesFromSnapshot(snapshot));
    setOldestCursor(historyWindow?.oldest_cursor ?? null);
    setHasMoreBefore(Boolean(historyWindow?.has_more_before));
    setHistoryError(null);
    restoreAfterPrependRef.current = null;
  }, [snapshot?.activity_history?.newest_cursor]);

  useEffect(() => {
    if (!isFocused) {
      return;
    }

    const restore = restoreAfterPrependRef.current;
    if (!restore) {
      return;
    }

    window.requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (!panel) {
        restoreAfterPrependRef.current = null;
        return;
      }
      panel.scrollTop =
        panel.scrollHeight - restore.scrollHeight + restore.scrollTop;
      lastFocusedScrollTopRef.current = panel.scrollTop;
      updateScrollButtonVisibility(panel);
      restoreAfterPrependRef.current = null;
    });
  }, [historyBubbles.length, isFocused, panelRef]);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }

    if (!isFocused) {
      window.requestAnimationFrame(() => {
        scrollToChatBottom();
      });
      return;
    }

    if (shouldRestoreFocusScrollRef.current) {
      shouldRestoreFocusScrollRef.current = false;
      window.requestAnimationFrame(() => {
        const latestPanel = panelRef.current;
        if (!latestPanel) {
          return;
        }
        if (isFocusedNearBottomRef.current) {
          scrollToChatBottom();
        } else {
          latestPanel.scrollTop = Math.min(
            lastFocusedScrollTopRef.current,
            Math.max(0, latestPanel.scrollHeight - latestPanel.clientHeight),
          );
        }
        updateScrollButtonVisibility(latestPanel);
        isFocusedNearBottomRef.current =
          latestPanel.scrollHeight - latestPanel.clientHeight - latestPanel.scrollTop <=
          AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX;
      });
      return;
    }

    const distanceFromBottom =
      panel.scrollHeight - panel.clientHeight - panel.scrollTop;
    if (isFocusedNearBottomRef.current) {
      window.requestAnimationFrame(() => {
        scrollToChatBottom();
      });
    } else {
      setShowScrollToBottom(
        distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX,
      );
    }
  }, [bubbles.length, isFocused, panelRef]);

  useEffect(() => {
    const panel = panelRef.current;
    if (
      !panel ||
      !isFocused ||
      !hasMoreBefore ||
      isLoadingHistory ||
      restoreAfterPrependRef.current
    ) {
      return;
    }

    if (panel.scrollTop <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX) {
      void loadOlderHistory();
    }
  }, [hasMoreBefore, isFocused, isLoadingHistory, loadOlderHistory, panelRef]);

  return (
    <>
      <div
        ref={panelRef}
        aria-label="Agent chat preview"
        aria-hidden={!isFocused}
        onScroll={handleScroll}
        style={{
          paddingBottom: composerHeight + AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX,
        }}
        className={cn(
          "absolute inset-0 w-full overflow-y-auto pt-6 text-left [scrollbar-gutter:stable] transition-[filter,opacity] duration-300 ease-out",
          isFocused
            ? "pointer-events-auto z-20 opacity-100"
            : "pointer-events-none z-0 opacity-35 blur-[1px] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
        )}
      >
        <div className="relative z-10 flex min-h-full w-full flex-col justify-end">
          {visibleBubbles.length > 0 ? (
            <div
              className={cn(
                "me-auto w-full max-w-[min(48rem,94%)] space-y-3 py-1.5",
                !isFocused && "max-w-[min(42rem,88%)] space-y-2",
              )}
            >
              {isFocused && (hasMoreBefore || isLoadingHistory || historyError) ? (
                <div className="flex justify-center py-1">
                  {hasMoreBefore ? (
                    <Button
                      type="button"
                      variant="secondary"
                      size="sm"
                      disabled={isLoadingHistory}
                      onClick={() => {
                        void loadOlderHistory();
                      }}
                      className="rounded-full border border-border/70 bg-background/80 px-3 text-xs text-muted-foreground shadow-sm backdrop-blur-xl"
                    >
                      {isLoadingHistory ? "加载中…" : "加载更早记录"}
                    </Button>
                  ) : null}
                  {historyError ? (
                    <p role="alert" className="px-3 text-xs text-destructive">
                      {historyError}
                    </p>
                  ) : null}
                </div>
              ) : null}
              {visibleBubbles.map((bubble) => (
                <AgentChatBubbleItem
                  key={bubble.id}
                  bubble={bubble}
                  isFocused={isFocused}
                />
              ))}
            </div>
          ) : (
            <p className="mx-auto max-w-[min(32rem,calc(100vw-3rem))] px-4 py-3 text-center text-xs text-muted-foreground/70">
              聚焦底部输入框后，消息流会在整个屏幕上围绕 agent 浮动。
            </p>
          )}
        </div>
      </div>
      <Button
        type="button"
        variant="secondary"
        size="sm"
        aria-label="Scroll agent chat to bottom"
        onMouseDown={(event) => {
          event.preventDefault();
        }}
        onClick={handleScrollToBottomClick}
        className={cn(
          "fixed bottom-[calc(max(1.25rem,env(safe-area-inset-bottom))+6rem)] left-1/2 z-40 -translate-x-1/2 rounded-full border border-border/70 bg-background/90 px-3 shadow-lg shadow-background/30 backdrop-blur-xl transition-all duration-200",
          showScrollToBottom
            ? "pointer-events-auto translate-y-0 opacity-100"
            : "pointer-events-none translate-y-2 opacity-0",
        )}
      >
        <ArrowDownIcon className="size-3.5" />
        回到底部
      </Button>
    </>
  );
}

function AgentChatBubbleItem({
  bubble,
  isFocused,
}: {
  bubble: AgentChatBubble;
  isFocused: boolean;
}) {
  const isConversationMessage = agentChatBubbleIsConversationMessage(bubble);
  const activityCellRender = agentChatActivityCellRenderForBubble(bubble);
  const useCanonicalActivityCell = Boolean(activityCellRender);
  const primaryBlocks = useCanonicalActivityCell
    ? []
    : agentChatDisplayBlocksForBubble(
        bubble,
        bubble.blocks.length > 0
          ? bubble.blocks
          : isConversationMessage
            ? ([{ type: "text", text: bubble.title }] as WebActivityBlock[])
            : [],
      );
  const visibleBlockLimit = isConversationMessage && isFocused
    ? primaryBlocks.length
    : isFocused
      ? 6
      : 3;
  const visibleBlocks = primaryBlocks.slice(0, visibleBlockLimit);

  return (
    <article
      className={cn(
        "w-full py-1.5",
        bubble.live || bubble.status === "running" ? "text-foreground" : "text-foreground/95",
        !isFocused && "select-none",
      )}
    >
      <div className="space-y-2 text-sm leading-6 text-foreground">
        {!isConversationMessage && !useCanonicalActivityCell ? (
          <AgentChatActivityHeader bubble={bubble} isFocused={isFocused} />
        ) : null}
        {activityCellRender ? (
          <AgentChatActivityCellView
            bubbleId={bubble.id}
            render={activityCellRender}
          />
        ) : (
          <div className="space-y-2 text-foreground/90">
            {visibleBlocks.map((block, index) => (
              <AgentChatBlock
                key={`${bubble.id}-block-${index}`}
                block={block}
                blockId={`${bubble.id}-block-${index}`}
                isFocused={isFocused}
                messageMode={isConversationMessage}
              />
            ))}
          </div>
        )}
      </div>
    </article>
  );
}

function AgentChatActivityHeader({
  bubble,
  isFocused,
}: {
  bubble: AgentChatBubble;
  isFocused: boolean;
}) {
  const isRunning = bubble.live || bubble.status === "running";
  const statusText = agentChatActivityStatusText(bubble.status, bubble.live);
  const subtitle = agentChatActivitySubtitle(bubble);

  return (
    <div
      className={cn(
        "flex min-w-0 items-start gap-2 text-foreground",
        !isFocused && "opacity-90",
      )}
    >
      <span
        aria-hidden="true"
        className={cn(
          "mt-0.5 inline-flex size-5 shrink-0 items-center justify-center font-mono text-[0.65rem] font-semibold leading-none",
          agentChatActivityIconClass(bubble),
          !isFocused && "size-4 text-[0.58rem]",
        )}
      >
        {agentChatActivityGlyph(bubble)}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <p
            className={cn(
              "min-w-0 break-words text-sm font-semibold leading-6 text-foreground",
              !isFocused && "text-xs leading-5",
            )}
          >
            {bubble.title}
          </p>
          {isRunning || bubble.status === "failed" ? (
            <span
              className={cn(
                "inline-flex shrink-0 items-center gap-1 text-[0.62rem] font-medium leading-none",
                agentChatActivityStatusClass(bubble.status, bubble.live),
              )}
            >
              {isRunning ? <Loader2Icon className="size-2.5 animate-spin" /> : null}
              {statusText}
            </span>
          ) : null}
        </div>
        {subtitle && isFocused ? (
          <p className="break-words text-xs leading-5 text-muted-foreground/80">
            {subtitle}
          </p>
        ) : null}
      </div>
    </div>
  );
}


function AgentChatActivityCellView({
  bubbleId,
  render,
}: AgentChatActivityCellViewProps) {
  if (render.kind === "text") {
    return (
      <AgentChatActivityTextCell
        id={bubbleId}
        marker={render.marker}
        title={render.title}
        bodyLines={render.bodyLines}
        imageAttachments={render.imageAttachments}
        bodyLimit={render.bodyLimit}
        tone={render.tone}
      />
    );
  }

  if (render.kind === "browser") {
    return (
      <AgentChatActivityTextCell
        id={bubbleId}
        marker={render.marker}
        title={render.title}
        bodyLines={render.detailLines}
        bodyLimit={render.detailLimit}
        tone="muted"
      />
    );
  }

  if (render.kind === "plan") {
    return render.steps.length > 0 ? (
      <AgentChatPlanActivityPanel
        marker={render.marker}
        title={render.title}
        steps={render.steps}
      />
    ) : null;
  }

  if (render.kind === "workflow") {
    return (
      <AgentChatStatusLineCell
        marker={render.marker}
        label={render.title}
        value={render.workflowId}
        valueClassName="font-mono break-all"
      />
    );
  }

  if (render.kind === "deepRecall") {
    return (
      <AgentChatStatusLineCell
        marker={render.marker}
        label={render.title}
        value={String(render.memoryCount)}
        suffix=" Memories"
        valueClassName="tabular-nums"
      />
    );
  }

  if (render.kind === "exec") {
    return (
      <AgentChatCommandExecutionPanel
        mode={render.running ? "running" : "completed"}
        marker={render.marker}
        title={render.title}
        outputLines={render.outputLines}
        exitCode={render.exitCode}
      />
    );
  }

  if (render.kind === "patch") {
    return (
      <AgentChatPatchActivityPanel
        marker={render.marker}
        title={render.title}
        files={render.files}
      />
    );
  }

  if (render.kind === "messageActivity") {
    return (
      <AgentChatMessageActivityLine
        id={bubbleId}
        marker={render.marker}
        title={render.title}
        detailLines={render.detailLines}
        messageLines={render.messageLines}
        detailLimit={render.detailLimit}
        messageLimit={render.messageLimit}
      />
    );
  }

  return (
    <AgentChatReplyActivityLine
      id={bubbleId}
      marker={render.marker}
      title={render.title}
      messageLines={render.messageLines}
      disposition={render.disposition}
    />
  );
}

function AgentChatActivityMarker({
  marker,
  tone = "default",
  className,
}: {
  marker: string;
  tone?: "default" | "error";
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-flex h-6 w-6 shrink-0 items-center justify-center font-mono text-sm font-semibold leading-none text-muted-foreground",
        tone === "error" && "text-destructive",
        className,
      )}
    >
      {marker}
    </span>
  );
}

function AgentChatActivityTextCell({
  id,
  marker,
  title,
  bodyLines,
  imageAttachments = [],
  bodyLimit,
  tone = "default",
}: {
  id: string;
  marker: string;
  title: string;
  bodyLines: string[];
  imageAttachments?: AgentChatImageAttachmentData[];
  bodyLimit?: number;
  tone?: "default" | "error" | "muted";
}) {
  const visibleLines = typeof bodyLimit === "number"
    ? bodyLines.slice(0, bodyLimit)
    : bodyLines;
  const hiddenLineCount = typeof bodyLimit === "number"
    ? Math.max(0, bodyLines.length - visibleLines.length)
    : 0;

  return (
    <div
      className={cn(
        "space-y-1 text-sm leading-6 text-foreground/90",
        tone === "error" && "text-destructive",
        tone === "muted" && "text-muted-foreground",
      )}
    >
      <div className="grid min-w-0 grid-cols-[1.5rem_minmax(0,1fr)] items-start gap-2 px-3">
        <AgentChatActivityMarker
          marker={marker}
          tone={tone === "error" ? "error" : "default"}
        />
        <p
          className={cn(
            "min-w-0 break-words font-semibold text-foreground",
            tone === "error" && "text-destructive",
            tone === "muted" && "text-foreground/90",
          )}
        >
          <AgentChatMarkdownInline text={title} />
        </p>
      </div>
      {visibleLines.length > 0 ? (
        <div
          className={cn(
            "space-y-0.5 px-3 text-muted-foreground",
            tone === "error" && "text-destructive/90",
            tone === "muted" && "text-muted-foreground",
          )}
        >
          {visibleLines.map((line, index) => (
            <p
              key={`${id}-activity-line-${index}`}
              className="min-w-0 break-words"
            >
              <AgentChatMarkdownInline text={line} />
            </p>
          ))}
          {hiddenLineCount > 0 ? (
            <p className="text-xs text-muted-foreground">
              … {hiddenLineCount} more line(s)
            </p>
          ) : null}
        </div>
      ) : null}
      {imageAttachments.length > 0 ? (
        <div className="space-y-2 px-3">
          {imageAttachments.map((attachment, index) => (
            <AgentChatImageAttachment
              key={`${id}-activity-image-${index}`}
              label={attachment.label}
              uri={attachment.uri}
              mimeType={attachment.mimeType}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function AgentChatStatusLineCell({
  marker,
  label,
  value,
  suffix = "",
  valueClassName,
}: {
  marker: string;
  label: string;
  value?: string;
  suffix?: string;
  valueClassName?: string;
}) {
  return (
    <div className="grid min-w-0 grid-cols-[1.5rem_minmax(0,1fr)] items-start gap-2 px-3 text-sm leading-6 text-foreground/90">
      <AgentChatActivityMarker marker={marker} />
      <p className="min-w-0 break-words font-semibold text-foreground">
        {label}
        {value ? (
          <>
            {" "}
            <span className={cn("text-foreground/90", valueClassName)}>{value}</span>
            {suffix}
          </>
        ) : null}
      </p>
    </div>
  );
}

function AgentChatPlanActivityPanel({
  marker,
  title,
  steps,
}: {
  marker: string;
  title: string;
  steps: AgentChatPlanStep[];
}) {
  const visibleSteps = steps.slice(0, AGENT_CHAT_PLAN_STEP_LIMIT);

  return (
    <div className="space-y-1.5 text-sm">
      <div className="flex min-w-0 items-start gap-2 px-3 leading-6">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">{title}</p>
      </div>
      {visibleSteps.length > 0 ? (
        <div role="table" aria-label="Plan" className="space-y-1">
          <div
            role="row"
            className="grid grid-cols-[8.5rem_1fr] gap-3 px-3 py-0.5 text-[0.68rem] font-semibold uppercase tracking-wide text-muted-foreground"
          >
            <span role="columnheader">Status</span>
            <span role="columnheader">Step</span>
          </div>
          {visibleSteps.map((step, index) => {
            const isCurrent = step.status === "in_progress";
            return (
              <div
                key={`plan-step-${index}`}
                role="row"
                aria-current={isCurrent ? "step" : undefined}
                className={cn(
                  "grid grid-cols-[8.5rem_1fr] gap-3 px-3 py-0.5 text-sm",
                  isCurrent && "font-medium",
                )}
              >
                <span role="cell">
                  <AgentChatPlanStatusBadge status={step.status} />
                </span>
                <span
                  role="cell"
                  className={cn(
                    "min-w-0 break-words text-foreground/90",
                    isCurrent && "font-medium text-foreground",
                    step.status === "pending" && "text-muted-foreground",
                  )}
                >
                  {step.text}
                </span>
              </div>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}

function AgentChatPlanStatusBadge({ status }: { status: AgentChatPlanStepStatus }) {
  const label = agentChatPlanStatusLabel(status);
  const marker = status === "pending" ? "○" : "●";

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground",
        status === "in_progress" && "text-foreground",
      )}
    >
      <span aria-hidden="true" className="font-mono text-[0.65rem]">
        {marker}
      </span>
      {label}
    </span>
  );
}

function AgentChatCommandExecutionPanel({
  mode,
  marker,
  title,
  outputLines,
  exitCode,
}: {
  mode: "running" | "completed";
  marker: string;
  title: string;
  outputLines: string[];
  exitCode?: number | null;
}) {
  const renderedOutput = outputLines.length > 0
    ? truncateAgentChatLinesMiddle(
        outputLines,
        AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES,
        AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES,
      )
    : [mode === "running" ? "running..." : "(no output)"];
  const verb = mode === "running" ? "Running" : "Ran";

  return (
    <div className="space-y-1 text-sm">
      <div className="flex min-w-0 items-start gap-x-2 px-3 leading-6">
        {mode === "running" ? (
          <span className="inline-flex h-6 w-6 shrink-0 items-center justify-center text-muted-foreground">
            <Loader2Icon className="size-3.5 animate-spin" />
          </span>
        ) : (
          <AgentChatActivityMarker marker={marker} />
        )}
        <p className="min-w-0 flex-1 truncate font-semibold text-foreground" title={`${verb} ${title}`}>
          {verb}{" "}
          <span className="font-mono font-medium text-foreground/90">{title}</span>
        </p>
        {typeof exitCode === "number" ? (
          <span className="shrink-0 text-[0.68rem] font-medium leading-none text-muted-foreground">
            exit {exitCode}
          </span>
        ) : null}
      </div>
      <AgentChatTerminalOutputBlock lines={renderedOutput} />
    </div>
  );
}

function AgentChatTerminalOutputBlock({ lines }: { lines: string[] }) {
  return (
    <pre className="overflow-x-auto whitespace-pre px-3 font-mono text-xs leading-5 text-muted-foreground [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
      {lines.map((line, index) => (
        <Fragment key={`terminal-output-${index}`}>
          <span className={cn(line.startsWith("… +") && "text-muted-foreground/70")}>{line}</span>
          {index + 1 < lines.length ? "\n" : null}
        </Fragment>
      ))}
    </pre>
  );
}

function AgentChatPatchActivityPanel({
  marker,
  title,
  files,
}: {
  marker: string;
  title: string;
  files: AgentChatDiffFile[];
}) {
  const visibleFiles = files.slice(0, AGENT_CHAT_PATCH_FILE_LIMIT);
  const hiddenFileCount = files.length - visibleFiles.length;

  return (
    <div className="space-y-1.5 text-sm">
      <div className="flex min-w-0 items-start gap-2 px-3 leading-6">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">{title}</p>
      </div>
      {visibleFiles.length > 0 ? (
        <div className="space-y-2 px-3">
          {visibleFiles.map((file, index) => (
            <AgentChatPatchFileBlock
              key={`${file.path}-${index}`}
              file={file}
            />
          ))}
          {hiddenFileCount > 0 ? (
            <p className="text-xs text-muted-foreground">… {hiddenFileCount} more file(s)</p>
          ) : null}
        </div>
      ) : (
        <p className="px-3 text-xs text-muted-foreground">No file changes</p>
      )}
    </div>
  );
}

function AgentChatPatchFileBlock({ file }: { file: AgentChatDiffFile }) {
  const visibleLines = file.lines.slice(0, AGENT_CHAT_PATCH_DIFF_LINE_LIMIT);
  const hiddenLineCount = file.lines.length - visibleLines.length;
  const oldWidth = agentChatDiffLineNumberWidth(visibleLines, "old_lineno");
  const newWidth = agentChatDiffLineNumberWidth(visibleLines, "new_lineno");

  return (
    <div className="space-y-1">
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        <p className="min-w-0 break-all font-mono text-xs font-semibold text-foreground/90">
          {file.path}
        </p>
        <span className="text-[0.68rem] font-medium leading-none text-muted-foreground">
          {file.operation}
        </span>
        <span className="font-mono text-[0.7rem] text-muted-foreground">
          +{file.added_lines} -{file.removed_lines}
        </span>
      </div>
      {visibleLines.length > 0 ? (
        <div className="overflow-x-auto font-mono text-xs leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
          {visibleLines.map((line, index) => (
            <AgentChatPatchDiffRow
              key={`patch-line-${index}`}
              line={line}
              oldWidth={oldWidth}
              newWidth={newWidth}
            />
          ))}
        </div>
      ) : null}
      {hiddenLineCount > 0 ? (
        <p className="text-xs text-muted-foreground">
          … {hiddenLineCount} more line(s)
        </p>
      ) : null}
    </div>
  );
}

function AgentChatPatchDiffRow({
  line,
  oldWidth,
  newWidth,
}: {
  line: AgentChatDiffLine;
  oldWidth: number;
  newWidth: number;
}) {
  if (line.kind === "hunk_break") {
    return (
      <div className="grid min-w-max grid-cols-[var(--old-width)_var(--new-width)_1rem_minmax(0,1fr)] gap-2 px-3 py-0.5 text-muted-foreground/70 [--new-width:2.5rem] [--old-width:2.5rem]">
        <span>{"".padStart(oldWidth, " ")}</span>
        <span>{"".padStart(newWidth, " ")}</span>
        <span>⋮</span>
        <span />
      </div>
    );
  }

  const oldLineNumber = typeof line.old_lineno === "number"
    ? String(line.old_lineno).padStart(oldWidth, " ")
    : "".padStart(oldWidth, " ");
  const newLineNumber = typeof line.new_lineno === "number"
    ? String(line.new_lineno).padStart(newWidth, " ")
    : "".padStart(newWidth, " ");
  const gutter = line.kind === "add" ? "+" : line.kind === "delete" ? "-" : " ";

  return (
    <div
      className={cn(
        "grid min-w-max grid-cols-[var(--old-width)_var(--new-width)_1rem_minmax(0,1fr)] gap-2 px-3 py-0.5",
        "[--new-width:2.5rem] [--old-width:2.5rem]",
        line.kind === "add" && "bg-muted/30",
        line.kind === "delete" && "bg-muted/20",
      )}
    >
      <span className="select-none text-right text-muted-foreground/65">{oldLineNumber}</span>
      <span className="select-none text-right text-muted-foreground/65">{newLineNumber}</span>
      <span className="select-none font-semibold text-muted-foreground">{gutter}</span>
      <span className="whitespace-pre text-foreground/85">{line.text}</span>
    </div>
  );
}

function AgentChatMessageActivityLine({
  id,
  marker,
  title,
  detailLines,
  messageLines,
  detailLimit,
  messageLimit,
}: {
  id: string;
  marker: string;
  title: string;
  detailLines: string[];
  messageLines: string[];
  detailLimit: number;
  messageLimit: number;
}) {
  const visibleDetailLines = detailLines.slice(0, detailLimit);
  const hiddenDetailCount = detailLines.length - visibleDetailLines.length;
  const visibleMessageLines = messageLines.slice(0, messageLimit);
  const hiddenMessageCount = messageLines.length - visibleMessageLines.length;

  return (
    <div className="space-y-1 text-sm leading-6 text-foreground/90">
      <div className="grid min-w-0 grid-cols-[1.5rem_minmax(0,1fr)] items-start gap-2 px-3">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">{title}</p>
      </div>
      {visibleDetailLines.length > 0 || hiddenDetailCount > 0 ? (
        <div className="space-y-0.5 pl-11 pr-3 text-xs leading-5 text-muted-foreground">
          {visibleDetailLines.map((line, index) => (
            <p key={`${id}-detail-${index}`} className="break-words">
              {line}
            </p>
          ))}
          {hiddenDetailCount > 0 ? <p>… {hiddenDetailCount} more line(s)</p> : null}
        </div>
      ) : null}
      {visibleMessageLines.length > 0 || hiddenMessageCount > 0 ? (
        <div className="space-y-0.5 px-3 text-foreground/90">
          {visibleMessageLines.map((line, index) => (
            <p
              key={`${id}-message-${index}`}
              className="min-w-0 break-words"
            >
              <AgentChatMarkdownInline text={line} />
            </p>
          ))}
          {hiddenMessageCount > 0 ? (
            <p className="text-xs text-muted-foreground">
              … {hiddenMessageCount} more line(s)
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function AgentChatReplyActivityLine({
  id,
  marker,
  title,
  messageLines,
  disposition,
}: {
  id: string;
  marker: string;
  title: string;
  messageLines: string[];
  disposition: string;
}) {
  return (
    <div
      className={cn(
        "space-y-1 text-sm leading-6 text-foreground/90",
        disposition === "failed" && "text-destructive",
        disposition === "dismissed" && "text-muted-foreground",
      )}
    >
      <div className="flex min-w-0 items-start gap-2 px-3 leading-6">
        <AgentChatActivityMarker
          marker={marker}
          tone={disposition === "failed" ? "error" : "default"}
          className={disposition === "dismissed" ? "text-muted-foreground" : undefined}
        />
        <p
          className={cn(
            "min-w-0 break-words font-semibold text-foreground",
            disposition === "failed" && "text-destructive",
            disposition === "dismissed" && "text-muted-foreground",
          )}
        >
          {title}
        </p>
      </div>
      {messageLines.length > 0 ? (
        <div className="px-3 text-foreground/90">
          <AgentChatMarkdownText
            id={`${id}-reply`}
            text={messageLines.join("\n")}
            limit={AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT}
            tone={disposition === "failed" ? "error" : "default"}
          />
        </div>
      ) : null}
    </div>
  );
}

function AgentChatBlock({
  block,
  blockId,
  isFocused,
  messageMode = false,
}: {
  block: WebActivityBlock;
  blockId: string;
  isFocused: boolean;
  messageMode?: boolean;
}) {
  const record = asRecord(block);
  const type = typeof record?.type === "string" ? record.type : "unknown";
  const lineLimit = messageMode && isFocused
      ? AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT
    : isFocused
      ? AGENT_CHAT_FOCUSED_MESSAGE_LINE_LIMIT
      : AGENT_CHAT_MESSAGE_LINE_LIMIT;

  if (!record) {
    return null;
  }

  if (type === "text") {
    return (
      <AgentChatMarkdownText
        id={blockId}
        text={stringValue(record.text, "")}
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
      <AgentChatListItems
        blockId={blockId}
        items={items}
        limit={lineLimit}
      />
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

  if (type === "image" || type === "artifact") {
    const label = stringValue(record.label, "Artifact");
    const uri = stringValue(record.uri, "");
    const mimeType = stringValue(record.mime_type, "");
    if (uri && (type === "image" || mimeType.startsWith("image/"))) {
      return (
        <AgentChatImageAttachment
          label={label}
          uri={uri}
          mimeType={mimeType}
        />
      );
    }
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


function AgentChatMarkdownText({
  id,
  text,
  limit,
  tone = "default",
}: {
  id: string;
  text: string;
  limit: number;
  tone?: "default" | "error";
}) {
  const blocks = parseAgentChatMarkdownBlocks(text);
  const visibleBlocks = blocks.slice(0, limit);

  if (blocks.length === 0) {
    return null;
  }

  return (
    <div
      className={cn(
        "space-y-2 text-sm leading-6 text-foreground/90",
        tone === "error" && "text-destructive",
      )}
    >
      {visibleBlocks.map((block, index) => (
        <AgentChatMarkdownBlock
          key={`${id}-markdown-${index}`}
          block={block}
          blockId={`${id}-markdown-${index}`}
        />
      ))}
      {blocks.length > visibleBlocks.length ? (
        <p className="text-xs text-muted-foreground">
          … +{blocks.length - visibleBlocks.length} more block(s)
        </p>
      ) : null}
    </div>
  );
}

function AgentChatMarkdownBlock({
  block,
  blockId,
}: {
  block: AgentChatMarkdownBlockData;
  blockId: string;
}) {
  if (block.type === "heading") {
    const content = <AgentChatMarkdownInline text={block.text} />;
    const className = "mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0";

    if (block.level <= 1) {
      return <h3 className={className}>{content}</h3>;
    }

    if (block.level === 2) {
      return <h4 className={className}>{content}</h4>;
    }

    return <h5 className={className}>{content}</h5>;
  }

  if (block.type === "list") {
    return block.ordered ? (
      <ol
        className="list-decimal space-y-1 pl-5 text-foreground/90"
      >
        {block.items.map((item, index) => (
          <li key={`${blockId}-item-${index}`} className="break-words pl-1">
            <AgentChatMarkdownInline text={item} />
          </li>
        ))}
      </ol>
    ) : (
      <ul
        className="list-disc space-y-1 pl-5 text-foreground/90"
      >
        {block.items.map((item, index) => (
          <li key={`${blockId}-item-${index}`} className="break-words pl-1">
            <AgentChatMarkdownInline text={item} />
          </li>
        ))}
      </ul>
    );
  }

  if (block.type === "blockquote") {
    return (
      <blockquote className="border-l-2 border-border/70 pl-3 text-muted-foreground">
        {block.lines.map((line, index) => (
          <p key={`${blockId}-quote-${index}`} className="break-words">
            <AgentChatMarkdownInline text={line} />
          </p>
        ))}
      </blockquote>
    );
  }

  if (block.type === "rule") {
    return <hr className="border-border/70" />;
  }

  return (
    <p className="break-words">
      <AgentChatMarkdownInline text={block.text} />
    </p>
  );
}

function AgentChatMarkdownInline({ text }: { text: string }) {
  return <>{parseAgentChatMarkdownInline(text).map(renderAgentChatMarkdownInlineNode)}</>;
}

function AgentChatListItems({
  blockId,
  items,
  limit,
  dense = false,
}: {
  blockId: string;
  items: string[];
  limit: number;
  dense?: boolean;
}) {
  const visibleItems = items.slice(0, limit);
  const hiddenItems = items.slice(limit);

  return (
    <ul
      className={cn(
        "space-y-1 text-foreground/90",
        dense ? "text-xs" : "text-sm",
      )}
    >
      {visibleItems.map((item, index) => (
        <li key={`${blockId}-item-${index}`} className="flex gap-2 break-words">
          <span className="text-muted-foreground">•</span>
          <span>{item}</span>
        </li>
      ))}
      {hiddenItems.length > 0 ? (
        <li className="text-xs text-muted-foreground">
          … +{hiddenItems.length} more
        </li>
      ) : null}
    </ul>
  );
}

function AgentChatImageAttachment({
  label,
  uri,
  mimeType,
}: {
  label: string;
  uri: string;
  mimeType: string;
}) {
  const imageUrl = getDashboardAttachmentUrl(uri);
  const title = [label, mimeType].filter(Boolean).join(" · ");

  return (
    <figure className="max-w-[min(28rem,100%)] overflow-hidden rounded-2xl border border-border/60 bg-muted/20">
      <a href={imageUrl} target="_blank" rel="noreferrer" className="block">
        <img
          src={imageUrl}
          alt={label}
          title={title || label}
          loading="lazy"
          className="max-h-[22rem] w-full object-contain"
        />
      </a>
      <figcaption className="flex items-center justify-between gap-3 border-t border-border/50 px-3 py-1.5 text-xs text-muted-foreground">
        <span className="min-w-0 truncate">{label}</span>
        {mimeType ? <span className="shrink-0">{mimeType}</span> : null}
      </figcaption>
    </figure>
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
      {lines.length > visibleLines.length ? (
        <p className="text-xs text-muted-foreground">
          … +{lines.length - visibleLines.length} more line(s)
        </p>
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
  const [hasCopied, setHasCopied] = useState(false);
  const lines = code.split(/\r?\n/);
  const visibleLines = lines.slice(0, limit);
  const hiddenLines = lines.slice(limit);
  const label = agentChatCodeLanguageLabel(language);
  const canCopy = typeof navigator !== "undefined" && Boolean(navigator.clipboard);

  async function handleCopyCode() {
    if (!canCopy) {
      return;
    }

    try {
      await navigator.clipboard.writeText(code);
      setHasCopied(true);
      window.setTimeout(() => setHasCopied(false), 1600);
    } catch {
      setHasCopied(false);
    }
  }

  if (!code.trim()) {
    return <p className="text-muted-foreground">(no output)</p>;
  }

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-3 px-3">
        <div className="flex min-w-0 items-center gap-2">
          <span
            aria-hidden="true"
            className="inline-flex size-5 shrink-0 items-center justify-center text-muted-foreground"
          >
            <span className="font-mono text-[0.7rem] leading-none">&lt;/&gt;</span>
          </span>
          <span className="truncate text-sm font-semibold text-foreground/90">
            {label}
          </span>
        </div>
        {canCopy ? (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={`Copy ${label} code`}
            onClick={handleCopyCode}
            className="rounded-full text-muted-foreground hover:text-foreground"
          >
            {hasCopied ? (
              <CheckIcon className="size-3.5" />
            ) : (
              <ClipboardIcon className="size-3.5" />
            )}
          </Button>
        ) : null}
      </div>
      <div className="relative">
        <pre
          className="max-h-72 overflow-auto whitespace-pre px-3 font-mono text-[0.82rem] leading-6 text-foreground/90 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]"
          data-code-block-id={id}
        >
          {visibleLines.join("\n")}
        </pre>
        {hiddenLines.length > 0 ? (
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-background/95 to-transparent"
          />
        ) : null}
      </div>
      {hiddenLines.length > 0 ? (
        <p className="px-3 text-xs text-muted-foreground">
          … +{hiddenLines.length} more line(s)
        </p>
      ) : null}
    </div>
  );
}

function agentChatCodeLanguageLabel(language: string) {
  const normalized = language.trim().toLowerCase();

  if (!normalized) {
    return "Code";
  }

  const labels: Record<string, string> = {
    bash: "Bash",
    css: "CSS",
    html: "HTML",
    js: "JavaScript",
    json: "JSON",
    jsx: "JSX",
    md: "Markdown",
    py: "Python",
    python: "Python",
    rs: "Rust",
    rust: "Rust",
    sh: "Shell",
    shell: "Shell",
    ts: "TypeScript",
    tsx: "TSX",
    zsh: "Zsh",
  };

  return labels[normalized] ?? `${normalized[0]?.toUpperCase() ?? ""}${normalized.slice(1)}`;
}

function AgentChatDiffBlock({
  id,
  files,
  limit,
  fileLimit = 3,
}: {
  id: string;
  files: AgentChatDiffFile[];
  limit: number;
  fileLimit?: number;
}) {
  if (files.length === 0) {
    return null;
  }

  const visibleFiles = files.slice(0, fileLimit);
  const hiddenFileCount = files.length - visibleFiles.length;

  return (
    <div className="space-y-2 font-mono text-xs">
      {visibleFiles.map((file, fileIndex) => {
        const visibleLines = file.lines.slice(0, limit);
        const hiddenLines = file.lines.slice(limit);
        return (
          <div key={`${id}-file-${fileIndex}`} className="space-y-1">
            <div className="flex items-center justify-between gap-3 px-3">
              <p className="min-w-0 truncate text-foreground/85">{file.path}</p>
              <span className="shrink-0 font-sans text-[0.68rem] text-muted-foreground">
                <span className="text-emerald-300">+{file.added_lines}</span>{" "}
                <span className="text-red-300">-{file.removed_lines}</span>
              </span>
            </div>
            <pre className="max-h-72 overflow-auto whitespace-pre px-3 leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
              {visibleLines.map((line) => renderDiffLine(line)).join("\n")}
            </pre>
            {hiddenLines.length > 0 ? (
              <p className="px-3 font-sans text-xs text-muted-foreground">
                … +{hiddenLines.length} more diff line(s)
              </p>
            ) : null}
          </div>
        );
      })}
      {hiddenFileCount > 0 ? (
        <p className="font-sans text-xs text-muted-foreground">… +{hiddenFileCount} more file(s)</p>
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

  const committed = agentChatCommittedBubblesFromSnapshot(snapshot);
  const live = (snapshot.live_web_activity_items ?? [])
    .map((entry, index) =>
      agentChatBubbleFromWebActivityItem(
        entry.item,
        `live-${entry.key || index}`,
        true,
      ),
    )
    .filter((bubble): bubble is AgentChatBubble => Boolean(bubble));

  return mergeAgentChatBubbles(committed, live);
}

function agentChatCommittedBubblesFromSnapshot(
  snapshot: DashboardSnapshot | null,
): AgentChatBubble[] {
  if (!snapshot) {
    return [];
  }

  return agentChatBubblesFromWebActivityItems(
    snapshot.activity_history?.items ?? snapshot.web_activity_items ?? [],
    "activity",
  );
}

function agentChatBubblesFromHistoryPage(
  page: DashboardActivityHistoryPage,
): AgentChatBubble[] {
  return agentChatBubblesFromWebActivityItems(page.items ?? [], "history-page");
}

function agentChatBubblesFromWebActivityItems(
  items: WebActivityItem[],
  fallbackPrefix: string,
): AgentChatBubble[] {
  return items
    .map((item, index) =>
      agentChatBubbleFromWebActivityItem(item, `${fallbackPrefix}-${index}`),
    )
    .filter((bubble): bubble is AgentChatBubble => Boolean(bubble));
}

function mergeAgentChatBubbles(
  ...groups: AgentChatBubble[][]
): AgentChatBubble[] {
  const merged = new Map<string, AgentChatBubble>();
  for (const bubble of groups.flat()) {
    merged.set(bubble.id, bubble);
  }
  return Array.from(merged.values());
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

  return {
    id,
    role: agentChatRoleFromWebActivity(actor, kind, source),
    kind,
    status,
    title,
    blocks: webActivityBlocksValue(record.blocks),
    planSteps: agentChatPlanStepsFromMetadata(record.metadata),
    live,
    toolName: tool ? stringValue(tool.name, "") : undefined,
    appName: tool ? stringValue(tool.app, "") : undefined,
    sourceLabel: source ? stringValue(source.label, stringValue(source.source_type, "")) : undefined,
    cell: asActivityCellVariant(record.cell),
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

function agentChatActivityGlyph(bubble: AgentChatBubble) {
  if (bubble.kind === "tool") {
    if (bubble.toolName === "terminal") {
      return "$";
    }
    if (bubble.toolName === "browser") {
      return "↗";
    }
    return "⌁";
  }

  if (bubble.kind === "patch") {
    return "±";
  }

  if (bubble.kind === "workflow") {
    return "◇";
  }

  if (bubble.kind === "memory") {
    return "◌";
  }

  if (bubble.kind === "error" || bubble.status === "failed") {
    return "!";
  }

  return "·";
}

function agentChatActivityIconClass(bubble: AgentChatBubble) {
  if (bubble.status === "failed" || bubble.kind === "error") {
    return "text-destructive";
  }

  if (bubble.live || bubble.status === "running") {
    return "text-primary";
  }

  if (bubble.kind === "patch") {
    return "text-emerald-400";
  }

  return "text-muted-foreground";
}

function agentChatActivityStatusText(status: string, live?: boolean) {
  if (live || status === "running") {
    return "进行中";
  }

  if (status === "failed") {
    return "失败";
  }

  if (status === "dismissed") {
    return "已忽略";
  }

  return status || "activity";
}

function agentChatActivityStatusClass(status: string, live?: boolean) {
  if (live || status === "running") {
    return "text-primary";
  }

  if (status === "failed") {
    return "text-destructive";
  }

  return "text-muted-foreground";
}

function agentChatActivitySubtitle(bubble: AgentChatBubble) {
  return [
    bubble.appName || bubble.toolName || bubble.kind,
    bubble.sourceLabel,
  ]
    .filter(Boolean)
    .join(" · ");
}

function agentChatBubbleIsConversationMessage(bubble: AgentChatBubble) {
  return (
    bubble.kind === "message" &&
    (bubble.role === "assistant" || bubble.role === "user" || bubble.role === "telegram")
  );
}

function agentChatDisplayBlocksForBubble(
  bubble: AgentChatBubble,
  blocks: WebActivityBlock[],
): WebActivityBlock[] {
  if (!agentChatBubbleIsConversationMessage(bubble)) {
    return blocks;
  }

  return blocks.flatMap((block) => agentChatSplitMarkdownCodeFences(block));
}


function agentChatActivityCellRenderForBubble(
  bubble: AgentChatBubble,
): AgentChatActivityCellRender | null {
  const cell = bubble.cell;

  const assistant = agentChatActivityCellPayload(cell, "Assistant");
  if (assistant) {
    return agentChatTextActivityRender("›", assistant, "Activity");
  }

  const user = agentChatActivityCellPayload(cell, "User");
  if (user) {
    const render = agentChatTextActivityRender("•", user, "user");
    render.imageAttachments = imageAttachmentsValue(user.image_attachments);
    return render;
  }

  const appAttention = agentChatActivityCellPayload(cell, "AppAttention");
  if (appAttention) {
    return agentChatTextActivityRender("◉", appAttention, "Focused App");
  }

  const browser = agentChatActivityCellPayload(cell, "Browser");
  if (browser) {
    return {
      kind: "browser",
      marker: "↗",
      title: `Captured URL: ${compactAgentChatBrowserUrl(nullableStringValue(browser.url) ?? "unknown")}`,
      detailLines: agentChatBrowserStatsLines(browser),
    };
  }

  const liveBrowser = agentChatActivityCellPayload(cell, "LiveBrowser");
  if (liveBrowser) {
    const url = nullableStringValue(liveBrowser.url);
    return {
      kind: "browser",
      marker: "↗",
      title: url
        ? `Opening URL: ${compactAgentChatBrowserUrl(url)}`
        : stringValue(liveBrowser.title, "Browser action"),
      detailLines: stringArrayValue(liveBrowser.body_lines),
      detailLimit: 1,
    };
  }

  const genericApp = agentChatActivityCellPayload(cell, "GenericApp") ??
    agentChatActivityCellPayload(cell, "ToolResult");
  if (genericApp) {
    return {
      kind: "text",
      marker: "•",
      title: `App: ${stringValue(genericApp.title, "Tool")}`,
      bodyLines: [],
    };
  }

  const plan = agentChatActivityCellPayload(cell, "PlanResult");
  if (plan) {
    return {
      kind: "plan",
      marker: "∷",
      title: "Plan",
      steps: agentChatPlanStepsFromActivityCell(cell),
    };
  }

  const createWorkflow = agentChatActivityCellPayload(cell, "CreateWorkflowResult");
  if (createWorkflow) {
    return {
      kind: "workflow",
      marker: "⌘",
      title: "Created Workflow:",
      workflowId: stringValue(createWorkflow.workflow_id, "unknown"),
    };
  }

  const activateWorkflow = agentChatActivityCellPayload(cell, "ActivateWorkflowResult");
  if (activateWorkflow) {
    return {
      kind: "workflow",
      marker: "⌘",
      title: "Activated Workflow:",
      workflowId: stringValue(activateWorkflow.workflow_id, "unknown"),
    };
  }

  const deepRecall = agentChatActivityCellPayload(cell, "DeepRecallResult");
  if (deepRecall) {
    return {
      kind: "deepRecall",
      marker: "⟲",
      title: "Recalled",
      memoryCount: numberValue(deepRecall.memory_count, 0),
    };
  }

  const execResult = agentChatActivityCellPayload(cell, "ExecResult");
  if (execResult) {
    return {
      kind: "exec",
      marker: "•",
      title: stringValue(execResult.title, "Command"),
      outputLines: stringArrayValuePreserveWhitespace(execResult.output_lines),
      exitCode: parseAgentChatExitCode(nullableStringValue(execResult.meta)),
    };
  }

  const liveExec = agentChatActivityCellPayload(cell, "LiveExec");
  if (liveExec) {
    return {
      kind: "exec",
      marker: "•",
      title: stringValue(liveExec.title, "Tool running"),
      outputLines: stringArrayValuePreserveWhitespace(liveExec.output_lines),
      running: true,
      exitCode: null,
    };
  }

  const patch = agentChatActivityCellPayload(cell, "Patch");
  if (patch) {
    const files = agentChatPatchFilesFromActivityCell(patch);
    return {
      kind: "patch",
      marker: "∂",
      title: agentChatPatchTitle(files),
      files,
    };
  }

  const telegram = agentChatActivityCellPayload(cell, "Telegram");
  if (telegram) {
    return {
      kind: "messageActivity",
      marker: "◦",
      title: stringValue(telegram.title, "Telegram"),
      detailLines: stringArrayValue(telegram.detail_lines),
      messageLines: stringArrayValue(telegram.message_lines),
      detailLimit: AGENT_CHAT_TELEGRAM_DETAIL_LIMIT,
      messageLimit: AGENT_CHAT_TELEGRAM_MESSAGE_LIMIT,
    };
  }

  const reply = agentChatActivityCellPayload(cell, "Reply");
  if (reply) {
    const disposition = normalizeAgentChatReplyDisposition(reply.disposition);
    return {
      kind: "reply",
      marker: "✣",
      title: agentChatReplyTitle(disposition, stringValue(reply.subject, "message")),
      messageLines: stringArrayValue(reply.message_lines),
      disposition,
    };
  }

  const terminalWait = agentChatActivityCellPayload(cell, "TerminalWait");
  if (terminalWait) {
    return {
      kind: "text",
      marker: "•",
      title: stringValue(terminalWait.title, "Terminal wait"),
      bodyLines: stringArrayValue(terminalWait.body_lines),
      bodyLimit: AGENT_CHAT_TERMINAL_WAIT_LINE_LIMIT,
    };
  }

  const error = agentChatActivityCellPayload(cell, "Error");
  if (error) {
    return {
      kind: "text",
      marker: "!",
      title: stringValue(error.title, "Error"),
      bodyLines: stringArrayValue(error.body_lines),
      bodyLimit: AGENT_CHAT_ERROR_LINE_LIMIT,
      tone: "error",
    };
  }

  return null;
}

function agentChatTextActivityRender(
  marker: string,
  cell: Record<string, unknown>,
  fallbackTitle: string,
): Extract<AgentChatActivityCellRender, { kind: "text" }> {
  return {
    kind: "text",
    marker,
    title: stringValue(cell.title, fallbackTitle),
    bodyLines: stringArrayValue(cell.body_lines),
  };
}

function agentChatBrowserStatsLines(cell: Record<string, unknown>): string[] {
  const lineCount = nullableNumberValue(cell.line_count);
  const refCount = nullableNumberValue(cell.ref_count);
  const stats = [
    lineCount !== null ? `${lineCount} lines` : null,
    refCount !== null ? `${refCount} refs` : null,
  ].filter((line): line is string => Boolean(line));

  return stats.length > 0 ? [stats.join(" · ")] : [];
}

function compactAgentChatBrowserUrl(value: string) {
  const compact = value.replace(/\s+/g, " ").trim();
  return compact.length > 88 ? `${compact.slice(0, 85)}...` : compact;
}

function agentChatPatchFilesFromActivityCell(
  cell: Record<string, unknown>,
): AgentChatDiffFile[] {
  return diffFilesValue(
    arrayValue(cell.files).map((file) => {
      const record = asRecord(file);
      return record
        ? {
            ...record,
            lines: record.diff_lines,
          }
        : file;
    }),
  );
}

function agentChatPatchTitle(files: AgentChatDiffFile[]) {
  const fileNoun = files.length === 1 ? "File" : "Files";
  return `Edited ${files.length} ${fileNoun}`;
}

function agentChatPlanStatusLabel(status: AgentChatPlanStepStatus) {
  if (status === "in_progress") {
    return "In progress";
  }

  if (status === "completed") {
    return "Completed";
  }

  if (status === "pending") {
    return "Pending";
  }

  return "Unknown";
}

function normalizeAgentChatReplyDisposition(value: unknown) {
  const normalized = typeof value === "string" ? value.toLowerCase() : "";
  if (normalized === "resolved" || normalized === "dismissed" || normalized === "failed") {
    return normalized;
  }
  return "unknown";
}

function agentChatReplyTitle(disposition: string, subject: string) {
  if (disposition === "resolved") {
    return subject.toLowerCase() === "notice" ? "Resolved Notice" : "Resolved Message";
  }

  if (disposition === "dismissed") {
    return "Dismissed";
  }

  if (disposition === "failed") {
    return "Failed";
  }

  return "Reply";
}

function parseAgentChatExitCode(meta: string | null) {
  const match = meta?.match(/exit=(-?\d+)/);
  return match ? Number(match[1]) : null;
}

function truncateAgentChatLinesMiddle(
  lines: string[],
  headCount: number,
  tailCount: number,
) {
  if (lines.length <= headCount + tailCount) {
    return lines;
  }

  const hiddenCount = lines.length - headCount - tailCount;
  return [
    ...lines.slice(0, headCount),
    `… +${hiddenCount} more line(s)`,
    ...lines.slice(lines.length - tailCount),
  ];
}

function agentChatDiffLineNumberWidth(
  lines: AgentChatDiffLine[],
  key: "old_lineno" | "new_lineno",
) {
  return Math.max(
    1,
    ...lines.map((line) =>
      typeof line[key] === "number" ? String(line[key]).length : 0,
    ),
  );
}

function agentChatPlanStepsFromActivityCell(
  cell: ActivityCellVariant | null | undefined,
): AgentChatPlanStep[] {
  const plan = agentChatActivityCellPayload(cell, "PlanResult");

  if (!plan) {
    return [];
  }

  return arrayValue(plan.steps)
    .map(asRecord)
    .filter((step): step is Record<string, unknown> => Boolean(step))
    .map((step) => {
      const text = stringValue(step.text, "");
      if (!text) {
        return null;
      }
      return {
        status: normalizeCanonicalPlanStepStatus(step.status),
        text,
      } satisfies AgentChatPlanStep;
    })
    .filter((step): step is AgentChatPlanStep => Boolean(step));
}

function agentChatCanonicalCellVariantName(
  cell: ActivityCellVariant | null | undefined,
): string | null {
  const record = asActivityCellVariant(cell);

  if (!record) {
    return null;
  }

  return Object.keys(record)[0] ?? null;
}

function agentChatActivityCellPayload(
  cell: ActivityCellVariant | null | undefined,
  variant: string,
): Record<string, unknown> | null {
  const record = asActivityCellVariant(cell) as Record<string, unknown> | null;
  return asRecord(record?.[variant]);
}

function normalizeCanonicalPlanStepStatus(value: unknown): AgentChatPlanStepStatus {
  if (value === "Pending" || value === "pending") {
    return "pending";
  }

  if (value === "InProgress" || value === "in_progress") {
    return "in_progress";
  }

  if (value === "Completed" || value === "completed") {
    return "completed";
  }

  return "unknown";
}

function canonicalPlanStepMarker(status: AgentChatPlanStepStatus) {
  if (status === "pending") {
    return "○";
  }

  if (status === "in_progress" || status === "completed") {
    return "●";
  }

  return "•";
}

function agentChatPlanStepsFromMetadata(value: unknown): AgentChatPlanStep[] {
  const metadata = asRecord(value);
  const steps = Array.isArray(metadata?.steps) ? metadata.steps : [];

  return steps
    .map((entry) => {
      const record = asRecord(entry);
      if (!record) {
        return null;
      }
      const text = stringValue(record.text, "");
      if (!text) {
        return null;
      }
      return {
        status: normalizeAgentChatPlanStepStatus(record.status) ?? "unknown",
        text,
      } satisfies AgentChatPlanStep;
    })
    .filter((step): step is AgentChatPlanStep => Boolean(step));
}

function normalizeAgentChatPlanStepStatus(
  value: unknown,
): AgentChatPlanStepStatus | null {
  return value === "pending" ||
    value === "in_progress" ||
    value === "completed" ||
    value === "unknown"
    ? value
    : null;
}

function agentChatSplitMarkdownCodeFences(block: WebActivityBlock): WebActivityBlock[] {
  const record = asRecord(block);

  if (!record || record.type !== "text") {
    return [block];
  }

  const text = stringValue(record.text, "");

  if (!text.includes("```")) {
    return [block];
  }

  const blocks: WebActivityBlock[] = [];
  const lines = text.split(/\r?\n/);
  let textLines: string[] = [];
  let codeLines: string[] | null = null;
  let codeLanguage = "";

  function flushText() {
    const content = textLines.join("\n").trim();
    textLines = [];

    if (content) {
      blocks.push({ type: "text", text: content });
    }
  }

  function flushCode() {
    if (!codeLines) {
      return;
    }

    blocks.push({
      type: "code",
      code: codeLines.join("\n"),
      language: codeLanguage || undefined,
    });
    codeLines = null;
    codeLanguage = "";
  }

  for (const line of lines) {
    const fenceMatch = line.match(/^\s*```\s*([A-Za-z0-9_+.-]*)\s*$/);

    if (fenceMatch) {
      if (codeLines) {
        flushCode();
      } else {
        flushText();
        codeLines = [];
        codeLanguage = fenceMatch[1] ?? "";
      }
      continue;
    }

    if (codeLines) {
      codeLines.push(line);
    } else {
      textLines.push(line);
    }
  }

  if (codeLines) {
    flushCode();
  }
  flushText();

  return blocks.length > 0 ? blocks : [block];
}

function parseAgentChatMarkdownBlocks(text: string): AgentChatMarkdownBlockData[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: AgentChatMarkdownBlockData[] = [];
  let paragraphLines: string[] = [];

  function flushParagraph() {
    const content = paragraphLines.join(" ").replace(/\s+/g, " ").trim();
    paragraphLines = [];

    if (content) {
      blocks.push({ type: "paragraph", text: content });
    }
  }

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index] ?? "";
    const trimmed = line.trim();

    if (!trimmed) {
      flushParagraph();
      continue;
    }

    const headingMatch = trimmed.match(/^(#{1,6})\s+(.+)$/);
    if (headingMatch) {
      flushParagraph();
      blocks.push({
        type: "heading",
        level: headingMatch[1]?.length ?? 1,
        text: headingMatch[2]?.trim() ?? "",
      });
      continue;
    }

    if (/^([-*_]\s*){3,}$/.test(trimmed)) {
      flushParagraph();
      blocks.push({ type: "rule" });
      continue;
    }

    const unorderedMatch = trimmed.match(/^[-*+]\s+(.+)$/);
    const orderedMatch = trimmed.match(/^\d+[.)]\s+(.+)$/);
    if (unorderedMatch || orderedMatch) {
      const ordered = Boolean(orderedMatch);
      const items: string[] = [];
      let cursor = index;

      while (cursor < lines.length) {
        const candidate = (lines[cursor] ?? "").trim();
        const candidateUnorderedMatch = candidate.match(/^[-*+]\s+(.+)$/);
        const candidateOrderedMatch = candidate.match(/^\d+[.)]\s+(.+)$/);
        const candidateIsOrdered = Boolean(candidateOrderedMatch);

        if (
          !candidate ||
          (ordered ? !candidateOrderedMatch : !candidateUnorderedMatch) ||
          candidateIsOrdered !== ordered
        ) {
          break;
        }

        items.push((candidateOrderedMatch?.[1] ?? candidateUnorderedMatch?.[1] ?? "").trim());
        cursor += 1;
      }

      flushParagraph();
      blocks.push({ type: "list", ordered, items });
      index = cursor - 1;
      continue;
    }

    const quoteMatch = trimmed.match(/^>\s?(.*)$/);
    if (quoteMatch) {
      const quoteLines: string[] = [];
      let cursor = index;

      while (cursor < lines.length) {
        const candidate = (lines[cursor] ?? "").trim();
        const candidateMatch = candidate.match(/^>\s?(.*)$/);
        if (!candidateMatch) {
          break;
        }
        quoteLines.push(candidateMatch[1]?.trim() ?? "");
        cursor += 1;
      }

      flushParagraph();
      blocks.push({
        type: "blockquote",
        lines: quoteLines.filter(Boolean),
      });
      index = cursor - 1;
      continue;
    }

    paragraphLines.push(trimmed);
  }

  flushParagraph();

  return blocks;
}

function parseAgentChatMarkdownInline(text: string): AgentChatMarkdownInlineNode[] {
  const tokenPatterns: Array<{
    pattern: RegExp;
    toNode: (match: RegExpExecArray) => AgentChatMarkdownInlineNode | null;
  }> = [
    {
      pattern: /`([^`]+)`/g,
      toNode: (match) => ({ type: "code", text: match[1] ?? "" }),
    },
    {
      pattern: /\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
      toNode: (match) => ({
        type: "link",
        label: match[1] ?? "",
        href: match[2] ?? "",
      }),
    },
    {
      pattern: /\*\*([^*]+)\*\*/g,
      toNode: (match) => ({ type: "strong", text: match[1] ?? "" }),
    },
    {
      pattern: /__([^_]+)__/g,
      toNode: (match) => ({ type: "strong", text: match[1] ?? "" }),
    },
    {
      pattern: /(?<!\*)\*([^*\s][^*]*?)\*(?!\*)/g,
      toNode: (match) => ({ type: "em", text: match[1] ?? "" }),
    },
    {
      pattern: /(?<!_)_([^_\s][^_]*?)_(?!_)/g,
      toNode: (match) => ({ type: "em", text: match[1] ?? "" }),
    },
  ];
  const tokens: AgentChatMarkdownInlineToken[] = [];

  for (const { pattern, toNode } of tokenPatterns) {
    pattern.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = pattern.exec(text)) !== null) {
      const node = toNode(match);
      if (!node) {
        continue;
      }
      tokens.push({
        start: match.index,
        end: match.index + match[0].length,
        node,
      });
    }
  }

  const acceptedTokens = tokens
    .sort((left, right) =>
      left.start === right.start
        ? right.end - right.start - (left.end - left.start)
        : left.start - right.start,
    )
    .reduce<AgentChatMarkdownInlineToken[]>((accepted, token) => {
      if (token.start < (accepted[accepted.length - 1]?.end ?? 0)) {
        return accepted;
      }
      accepted.push(token);
      return accepted;
    }, []);

  const nodes: AgentChatMarkdownInlineNode[] = [];
  let cursor = 0;

  for (const token of acceptedTokens) {
    if (token.start > cursor) {
      nodes.push({ type: "text", text: text.slice(cursor, token.start) });
    }
    nodes.push(token.node);
    cursor = token.end;
  }

  if (cursor < text.length) {
    nodes.push({ type: "text", text: text.slice(cursor) });
  }

  return nodes.filter((node) => {
    if (node.type === "link") {
      return Boolean(node.href && node.label);
    }
    return Boolean(node.text);
  });
}

function renderAgentChatMarkdownInlineNode(
  node: AgentChatMarkdownInlineNode,
  index: number,
) {
  if (node.type === "code") {
    return (
      <code
        key={`inline-code-${index}`}
        className="font-mono text-[0.85em] text-foreground"
      >
        {node.text}
      </code>
    );
  }

  if (node.type === "strong") {
    return (
      <strong key={`inline-strong-${index}`} className="font-semibold text-foreground">
        {node.text}
      </strong>
    );
  }

  if (node.type === "em") {
    return (
      <em key={`inline-em-${index}`} className="italic">
        {node.text}
      </em>
    );
  }

  if (node.type === "link") {
    return (
      <a
        key={`inline-link-${index}`}
        href={node.href}
        target="_blank"
        rel="noreferrer"
        className="break-all text-sky-300 underline-offset-4 hover:underline"
      >
        {node.label}
      </a>
    );
  }

  return node.text;
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

function asActivityCellVariant(value: unknown): ActivityCellVariant | null {
  const record = asRecord(value);

  if (!record) {
    return null;
  }

  const keys = Object.keys(record);

  if (keys.length !== 1) {
    return null;
  }

  return record as ActivityCellVariant;
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function imageAttachmentsValue(value: unknown): AgentChatImageAttachmentData[] {
  return arrayValue(value)
    .map(asRecord)
    .filter((attachment): attachment is Record<string, unknown> => Boolean(attachment))
    .map((attachment) => ({
      label: stringValue(attachment.label, "Image"),
      uri: stringValue(attachment.uri, ""),
      mimeType: stringValue(attachment.mime_type, ""),
    }))
    .filter((attachment) => attachment.uri);
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
  if (line.kind === "hunk_break") {
    return `  ${line.text}`;
  }

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

function stringArrayValuePreserveWhitespace(value: unknown) {
  if (!Array.isArray(value)) {
    return [];
  }

  return value.filter((line): line is string => typeof line === "string");
}

function nullableStringValue(value: unknown) {
  return typeof value === "string" && value.trim() ? value : null;
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
