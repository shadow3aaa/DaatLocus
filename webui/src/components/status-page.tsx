import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type DragEvent,
  type FormEvent,
  type RefObject,
  type UIEvent,
} from "react";

import { AgentStatusAnimation } from "@/components/agent-status-animation";
import {
  ArrowDownIcon,
  AlertTriangleIcon,
  CheckIcon,
  ClipboardIcon,
  CommandIcon,
  ImagePlusIcon,
  InfoIcon,
  Loader2Icon,
  SendHorizontalIcon,
  XIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  CollapsibleTrigger,
  useCollapsibleState,
} from "@/components/ui/collapsible";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  fetchDashboardActivityHistory,
  fetchSettingsSummary,
  getDashboardAttachmentUrl,
  runDashboardCommand,
  type DashboardActivityHistoryPage,
  type DashboardCommandAttachment,
  type DashboardPendingAccessRequest,
  type DashboardSnapshot,
  type ActivityCellVariant,
  type WebActivityBlock,
  type WebActivityItem,
} from "@/lib/daemon-api";
import {
  deriveAgentStatus,
  derivePlanSummaryText,
} from "@/lib/dashboard-view-model";
import { useDashboardSnapshot } from "@/hooks/use-dashboard-snapshot";
import { cn } from "@/lib/utils";
export { StatusPage } from "@/components/status-dashboard-page";

const SUMMARY_TYPE_INTERVAL_MS = 28;
const AGENT_CHAT_HISTORY_PAGE_LIMIT = 80;
const AGENT_CHAT_PREVIEW_MAX_VISIBLE_BUBBLES = 24;
const AGENT_CHAT_MESSAGE_LINE_LIMIT = 5;
const AGENT_CHAT_ACTIVITY_BLOCK_LINE_LIMIT = 12;
const AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT = Number.MAX_SAFE_INTEGER;
const AGENT_CHAT_PLAN_STEP_LIMIT = 8;
const AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES = 4;
const AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES = 4;
const AGENT_CHAT_PATCH_FILE_LIMIT = 4;
const AGENT_CHAT_PATCH_DIFF_LINE_LIMIT = 18;
const AGENT_CHAT_TELEGRAM_DETAIL_LIMIT = 6;
const AGENT_CHAT_TELEGRAM_MESSAGE_LIMIT = 6;
const AGENT_CHAT_TERMINAL_WAIT_LINE_LIMIT = 6;
const AGENT_CHAT_ERROR_LINE_LIMIT = 12;
const AGENT_CHAT_THINKING_PREVIEW_LINE_LIMIT = 3;
const AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX = 72;
const AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX = 160;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENTS = 4;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES = 10 * 1024 * 1024;
const AGENT_CHAT_INLINE_PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
const AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX = 60;
const AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX = 16;
const AGENT_CHAT_PREVIEW_NOTICE_VISIBLE_MS = 3000;
const AGENT_CHAT_PREVIEW_NOTICE_FADE_MS = 300;
export function AgentPage({ sessionId }: { sessionId: string }) {
  const { isLoading, loadError, snapshot } = useDashboardSnapshot(sessionId);
  const chatPanelRef = useRef<HTMLDivElement>(null);
  const [chatComposerHeight, setChatComposerHeight] = useState(
    AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX,
  );
  const [isChatFocused, setIsChatFocused] = useState(false);
  const [chatPreviewNotice, setChatPreviewNotice] = useState<string | null>(
    null,
  );
  const [isChatPreviewNoticeVisible, setIsChatPreviewNoticeVisible] =
    useState(false);
  const chatPreviewNoticeFrameRef = useRef<number | undefined>(undefined);
  const chatPreviewNoticeHideTimeoutRef = useRef<number | undefined>(undefined);
  const chatPreviewNoticeClearTimeoutRef = useRef<number | undefined>(
    undefined,
  );
  const [supportsVision, setSupportsVision] = useState(true);

  useEffect(() => {
    const controller = new AbortController();
    void (async () => {
      try {
        const summary = await fetchSettingsSummary({
          signal: controller.signal,
        });
        const mainModel = summary.models.find((m) => m.is_main);
        if (mainModel) {
          setSupportsVision(mainModel.supports_vision);
        }
      } catch {
        // If settings fetch fails, keep default (true) so image button stays visible.
      }
    })();
    return () => controller.abort();
  }, []);

  const agentStatus = deriveAgentStatus({
    loadError,
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
        sessionId={sessionId}
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
        <span aria-live="polite" className="sr-only">
          {agentStatus.label}
        </span>
      </div>
      <AgentChatComposer
        sessionId={sessionId}
        snapshot={snapshot}
        agentName={snapshot?.agent_name}
        supportsVision={supportsVision}
        isFocused={isChatFocused}
        onFocusChange={setIsChatFocused}
        chatPanelRef={chatPanelRef}
        onHeightChange={setChatComposerHeight}
        onSendResult={handleAgentChatSendResult}
      />
    </section>
  );
}

type AgentChatBubbleRole =
  | "assistant"
  | "user"
  | "tool"
  | "telegram"
  | "system";

type AgentChatBubble = {
  id: string;
  role: AgentChatBubbleRole;
  kind: string;
  status: string;
  title: string;
  createdAt: number;
  updatedAt: number;
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
  previewUrl?: string;
};

type WebSlashCommandLevel = "info" | "warning" | "error";

type WebSlashCommandSuggestion = {
  display: string;
  completion: string;
  description: string;
};

type WebSlashCommandFeedback = {
  title: string;
  message: string;
  detail?: string;
  level: WebSlashCommandLevel;
  blocksSubmit?: boolean;
};

type WebSlashCommandResult = {
  command: string;
  title: string;
  output: string;
  message: string;
  detail?: string;
  level: WebSlashCommandLevel;
  presentation: "panel" | "compact";
};

type WebSlashTelegramPicker = {
  action: "approve" | "reject";
  requests: DashboardPendingAccessRequest[];
};

type WebSlashCommandDefinition = {
  name: string;
  usage: string;
  description: string;
  aliases?: string[];
  argumentKind?: "app";
  subcommands?: WebSlashSubcommandDefinition[];
};

type WebSlashSubcommandDefinition = {
  name: string;
  usage: string;
  description: string;
  aliases?: string[];
  argumentKind?: "telegram-request";
};

const WEB_SLASH_COMMANDS: WebSlashCommandDefinition[] = [
  {
    name: "status",
    usage: "status",
    description: "show overall status",
  },
  {
    name: "clear",
    usage: "clear",
    description: "clear runtime conversation, plan, events, and activity",
  },
  {
    name: "debug",
    usage: "debug",
    description: "debug outputs and internal runtime views",
    subcommands: [
      {
        name: "persona",
        usage: "persona",
        description: "show current prompt persona config",
      },
      {
        name: "system-prompt",
        usage: "system-prompt",
        description: "show current runtime system prompt",
        aliases: ["system_prompt"],
      },
      {
        name: "context",
        usage: "context",
        description: "show latest pre-turn runtime context",
        aliases: ["preturn-context", "preturn_context"],
      },
    ],
  },
  {
    name: "app-status",
    usage: "app-status <app>",
    description: "show current structured app state and llm-facing note",
    aliases: ["app_status"],
    argumentKind: "app",
  },
  {
    name: "restart",
    usage: "restart",
    description: "restart the daemon",
  },
  {
    name: "sleep",
    usage: "sleep",
    description: "sleep controls and status",
    subcommands: [
      {
        name: "status",
        usage: "status",
        description: "show sleep status",
      },
      {
        name: "run",
        usage: "run",
        description: "start a background sleep run",
      },
    ],
  },
  {
    name: "telegram",
    usage: "telegram",
    description: "telegram status and access controls",
    subcommands: [
      {
        name: "status",
        usage: "status",
        description: "show telegram details",
      },
      {
        name: "approve",
        usage: "approve [chat_id]",
        description: "approve a telegram access request",
        argumentKind: "telegram-request",
      },
      {
        name: "reject",
        usage: "reject [chat_id]",
        description: "reject a telegram access request",
        argumentKind: "telegram-request",
      },
    ],
  },
];

type AgentChatActivityCellRender =
  | {
      kind: "text";
      marker: string;
      title: string;
      bodyLines: string[];
      fullBody?: string | null;
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
      kind: "primitive";
      marker: string;
      title: string;
      primitiveId: string;
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
    }
  | {
      kind: "thinking";
      title: string;
      bodyLines: string[];
      fullBody?: string | null;
      bodyLimit: number;
    };

type AgentChatActivityCellViewProps = {
  bubbleId: string;
  render: AgentChatActivityCellRender;
};

type AgentChatDisplayItem =
  | {
      kind: "bubble";
      id: string;
      bubble: AgentChatBubble;
    }
  | {
      kind: "foldedActivityGroup";
      id: string;
      bubbles: AgentChatBubble[];
    };

function AgentChatComposer({
  sessionId,
  snapshot,
  agentName,
  supportsVision = true,
  isFocused,
  onFocusChange,
  chatPanelRef,
  onHeightChange,
  onSendResult,
}: {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
  agentName?: string;
  supportsVision?: boolean;
  isFocused: boolean;
  onFocusChange: (isFocused: boolean) => void;
  chatPanelRef: RefObject<HTMLDivElement | null>;
  onHeightChange: (height: number) => void;
  onSendResult: (resultText: string) => void;
}) {
  const chatPlaceholder = `Chat with ${agentName?.trim() || "Agent"}`;
  const formRef = useRef<HTMLFormElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [message, setMessage] = useState("");
  const [imageAttachments, setImageAttachments] = useState<
    AgentChatPendingImageAttachment[]
  >([]);
  const imageAttachmentsRef = useRef<AgentChatPendingImageAttachment[]>([]);
  const nextImageAttachmentIdRef = useRef(0);
  const [isSending, setIsSending] = useState(false);
  const [isDraggingImage, setIsDraggingImage] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [slashCommandSelection, setSlashCommandSelection] = useState(0);
  const [slashCommandResult, setSlashCommandResult] =
    useState<WebSlashCommandResult | null>(null);

  const slashCommandSuggestions = useMemo(
    () => webSlashCommandSuggestions(message, snapshot),
    [message, snapshot],
  );
  const slashCommandFeedback = useMemo(
    () => webSlashCommandFeedback(message, snapshot, imageAttachments.length),
    [imageAttachments.length, message, snapshot],
  );
  const slashTelegramPicker = useMemo(
    () => webSlashTelegramPicker(message, snapshot),
    [message, snapshot],
  );
  const selectedSlashSuggestion =
    slashCommandSuggestions[
      Math.min(slashCommandSelection, slashCommandSuggestions.length - 1)
    ];
  const slashCommandBlocksSubmit =
    Boolean(slashCommandFeedback?.blocksSubmit) ||
    (isWebSlashCommandInput(message) &&
      !parseWebSlashCommand(message)?.trimmed);

  useEffect(() => {
    setSlashCommandSelection((current) =>
      Math.min(current, Math.max(0, slashCommandSuggestions.length - 1)),
    );
  }, [slashCommandSuggestions.length]);

  useEffect(() => {
    imageAttachmentsRef.current = imageAttachments;
  }, [imageAttachments]);

  useEffect(() => {
    return () => {
      for (const attachment of imageAttachmentsRef.current) {
        revokeImagePreviewUrl(attachment);
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

  const updateMessageTextareaHeight = useCallback(() => {
    const textarea = textareaRef.current;
    if (!textarea || typeof window === "undefined") {
      return;
    }

    const maxHeight = window.innerHeight * 0.3;
    textarea.style.height = "auto";
    const nextHeight = Math.min(textarea.scrollHeight, maxHeight);
    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY =
      textarea.scrollHeight > maxHeight ? "auto" : "hidden";
  }, []);

  useEffect(() => {
    updateMessageTextareaHeight();
  }, [isFocused, message, updateMessageTextareaHeight]);

  useEffect(() => {
    window.addEventListener("resize", updateMessageTextareaHeight);
    return () =>
      window.removeEventListener("resize", updateMessageTextareaHeight);
  }, [updateMessageTextareaHeight]);

  function handleFocus() {
    onFocusChange(true);
  }

  function handleCloseFocus() {
    onFocusChange(false);
  }

  function createPendingImageAttachment(
    file: File,
  ): AgentChatPendingImageAttachment {
    const nextId = nextImageAttachmentIdRef.current;
    nextImageAttachmentIdRef.current += 1;
    return {
      id: `${file.name}-${file.size}-${file.lastModified}-${Date.now()}-${nextId}`,
      file,
      previewUrl: createImagePreviewUrl(file),
    };
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
          `You can attach up to ${AGENT_CHAT_MAX_IMAGE_ATTACHMENTS} images.`,
        );
      } else if (oversized) {
        setSendError(
          `${oversized.name} is too large. Each image must be ${formatFileSize(
            AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES,
          )} or smaller.`,
        );
      } else if (valid.length > 0) {
        setSendError(null);
      }

      return [
        ...current,
        ...valid.map((file) => createPendingImageAttachment(file)),
      ];
    });
  }

  function removeImageAttachment(id: string) {
    setImageAttachments((current) => {
      const removed = current.find((attachment) => attachment.id === id);
      if (removed) {
        revokeImagePreviewUrl(removed);
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

  async function submitComposerInput(rawInput: string) {
    const trimmed = rawInput.trim();
    const isSlashCommand = isWebSlashCommandInput(trimmed);
    const slashBodyMissing =
      isSlashCommand && !parseWebSlashCommand(trimmed)?.trimmed;
    if (
      (!trimmed && imageAttachments.length === 0) ||
      isSending ||
      slashBodyMissing ||
      (isSlashCommand &&
        webSlashCommandFeedback(trimmed, snapshot, imageAttachments.length)
          ?.blocksSubmit)
    ) {
      return;
    }

    setIsSending(true);
    setSendError(null);

    try {
      const attachments = isSlashCommand
        ? []
        : await commandAttachmentsFromPendingImages();
      const output = await runDashboardCommand(trimmed, {
        attachments,
        sessionId,
      });
      setMessage("");
      setImageAttachments((current) => {
        for (const attachment of current) {
          revokeImagePreviewUrl(attachment);
        }
        return [];
      });

      if (isSlashCommand) {
        setSlashCommandSelection(0);
        setSlashCommandResult(webSlashCommandResultForResponse(trimmed, output));
      } else {
        const sendResultText = agentChatSendResultText(output);
        if (sendResultText) {
          onSendResult(sendResultText);
        }
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

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await submitComposerInput(message);
  }

  function applySlashSuggestion(suggestion: WebSlashCommandSuggestion) {
    setMessage(suggestion.completion);
    setSendError(null);
    setSlashCommandSelection(0);
    window.requestAnimationFrame(() => {
      updateMessageTextareaHeight();
      textareaRef.current?.focus();
    });
  }

  function handlePaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (!supportsVision) {
      return;
    }
    const files = Array.from(event.clipboardData.files).filter((file) =>
      file.type.startsWith("image/"),
    );
    if (files.length > 0) {
      addImageFiles(files);
    }
  }

  function handleDrop(event: DragEvent<HTMLFormElement>) {
    if (!supportsVision) {
      return;
    }
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
        if (!supportsVision) {
          return;
        }
        if (hasImageDragItems(event.dataTransfer)) {
          event.preventDefault();
          setIsDraggingImage(true);
        }
      }}
      onDragOver={(event) => {
        if (!supportsVision) {
          return;
        }
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
        "fixed bottom-5 left-1/2 z-30 w-[min(42rem,calc(100vw-2rem))] -translate-x-1/2 rounded-[16px] border bg-background/85 p-2 shadow-2xl shadow-background/40 backdrop-blur-xl transition-all duration-300 md:left-[calc(18rem+(100vw-18rem)/2)] md:w-[min(42rem,calc(100vw-18rem-2rem))]",
        isDraggingImage && "border-primary/70 ring-4 ring-primary/15",
        isFocused
          ? "border-primary/45 ring-4 ring-primary/10"
          : "border-border/70 hover:border-primary/30",
      )}
    >
      <WebSlashCommandPanel
        feedback={slashCommandFeedback}
        suggestions={slashCommandSuggestions}
        selectedSuggestionIndex={slashCommandSelection}
        result={slashCommandResult}
        telegramPicker={slashTelegramPicker}
        isSending={isSending}
        onCloseResult={() => setSlashCommandResult(null)}
        onSelectSuggestion={applySlashSuggestion}
        onHoverSuggestion={setSlashCommandSelection}
        onRunTelegramRequest={(request) => {
          if (!slashTelegramPicker) {
            return;
          }
          void submitComposerInput(
            `/telegram ${slashTelegramPicker.action} ${request.chat_id}`,
          );
        }}
      />
      {supportsVision ? (
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
      ) : null}
      {imageAttachments.length > 0 ? (
        <div className="flex gap-2 overflow-x-auto px-2 pb-2">
          {imageAttachments.map((attachment) => (
            <div
              key={attachment.id}
              className="group relative h-16 w-16 shrink-0 overflow-hidden rounded-xl border border-border/70 bg-muted"
              title={`${attachment.file.name} · ${formatFileSize(attachment.file.size)}`}
            >
              {attachment.previewUrl ? (
                <img
                  src={attachment.previewUrl}
                  alt={attachment.file.name || "Pending image attachment"}
                  className="h-full w-full object-cover"
                />
              ) : (
                <div
                  aria-label={
                    attachment.file.name || "Pending image attachment"
                  }
                  className="flex h-full w-full flex-col items-center justify-center gap-1 p-1 text-center text-[10px] leading-tight text-muted-foreground"
                >
                  <ImagePlusIcon
                    className="size-4 shrink-0"
                    aria-hidden="true"
                  />
                  <span className="max-w-full truncate">Image selected</span>
                </div>
              )}
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
        <textarea
          ref={textareaRef}
          value={message}
          rows={1}
          placeholder={chatPlaceholder}
          aria-label="Message"
          onChange={(event) => {
            setMessage(event.target.value);
            setSendError(null);
            setSlashCommandSelection(0);
            setSlashCommandResult((current) =>
              current?.presentation === "compact" ? null : current,
            );
            updateMessageTextareaHeight();
          }}
          onPaste={handlePaste}
          onKeyDown={(event) => {
            if (
              isWebSlashCommandInput(message) &&
              slashCommandSuggestions.length > 0
            ) {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setSlashCommandSelection((current) =>
                  (current + 1) % slashCommandSuggestions.length,
                );
                return;
              }
              if (event.key === "ArrowUp") {
                event.preventDefault();
                setSlashCommandSelection(
                  (current) =>
                    (current - 1 + slashCommandSuggestions.length) %
                    slashCommandSuggestions.length,
                );
                return;
              }
              if (event.key === "Tab") {
                event.preventDefault();
                applySlashSuggestion(
                  selectedSlashSuggestion ?? slashCommandSuggestions[0],
                );
                return;
              }
            }
            if (
              event.key === "Enter" &&
              !event.shiftKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault();
              if (
                selectedSlashSuggestion &&
                selectedSlashSuggestion.completion !== message.trim()
              ) {
                applySlashSuggestion(selectedSlashSuggestion);
                return;
              }
              event.currentTarget.form?.requestSubmit();
            }
          }}
          className="max-h-[30vh] min-h-11 flex-1 resize-none overflow-y-hidden bg-transparent px-4 py-3 text-sm leading-5 outline-none placeholder:text-muted-foreground/70"
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
          type="button"
          variant="ghost"
          size="icon-lg"
          aria-label="Attach image"
          onClick={() => fileInputRef.current?.click()}
          className="rounded-full text-muted-foreground hover:text-foreground"
          disabled={
            !supportsVision ||
            isSending ||
            isWebSlashCommandInput(message) ||
            imageAttachments.length >= AGENT_CHAT_MAX_IMAGE_ATTACHMENTS
          }
        >
          <ImagePlusIcon className="size-4" />
        </Button>
        <Button
          type="submit"
          size="icon-lg"
          disabled={
            (!message.trim() && imageAttachments.length === 0) ||
            isSending ||
            slashCommandBlocksSubmit
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
        <p role="alert" className="px-4 pb-1 pt-0.5 text-xs text-destructive">
          {sendError}
        </p>
      ) : null}
    </form>
  );
}

function WebSlashCommandPanel({
  feedback,
  suggestions,
  selectedSuggestionIndex,
  result,
  telegramPicker,
  isSending,
  onCloseResult,
  onSelectSuggestion,
  onHoverSuggestion,
  onRunTelegramRequest,
}: {
  feedback: WebSlashCommandFeedback | null;
  suggestions: WebSlashCommandSuggestion[];
  selectedSuggestionIndex: number;
  result: WebSlashCommandResult | null;
  telegramPicker: WebSlashTelegramPicker | null;
  isSending: boolean;
  onCloseResult: () => void;
  onSelectSuggestion: (suggestion: WebSlashCommandSuggestion) => void;
  onHoverSuggestion: (index: number) => void;
  onRunTelegramRequest: (request: DashboardPendingAccessRequest) => void;
}) {
  const hasContent =
    Boolean(result) ||
    Boolean(feedback) ||
    suggestions.length > 0 ||
    Boolean(telegramPicker && telegramPicker.requests.length > 0);

  if (!hasContent) {
    return null;
  }

  return (
    <div className="mb-2 space-y-2 border-b border-border/70 px-2 pb-2">
      {result ? (
        <section
          aria-label={`${result.title} result`}
          className="space-y-2 text-sm"
        >
          <div className="flex min-w-0 items-center gap-2">
            <WebSlashCommandLevelIcon level={result.level} />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-foreground">
                {result.title}
              </p>
              <p className="truncate text-xs text-muted-foreground">
                {result.command}
              </p>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label="Close command result"
              onClick={onCloseResult}
              className="size-7 shrink-0 rounded-full text-muted-foreground hover:text-foreground"
            >
              <XIcon className="size-3.5" />
            </Button>
          </div>
          {result.presentation === "panel" ? (
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/45 p-3 font-mono text-xs leading-5 text-foreground/90">
              {result.output.trim() || result.message}
            </pre>
          ) : (
            <p className="break-words text-sm leading-5 text-muted-foreground">
              {result.message}
              {result.detail ? (
                <span className="ml-1 text-muted-foreground/70">
                  {result.detail}
                </span>
              ) : null}
            </p>
          )}
        </section>
      ) : null}

      {feedback ? <WebSlashCommandFeedbackView feedback={feedback} /> : null}

      {telegramPicker && telegramPicker.requests.length > 0 ? (
        <section aria-label="Telegram access requests" className="space-y-1">
          {telegramPicker.requests.slice(0, 5).map((request) => (
            <button
              key={`${telegramPicker.action}-${request.chat_id}`}
              type="button"
              disabled={isSending}
              onClick={() => onRunTelegramRequest(request)}
              className="flex w-full min-w-0 items-center gap-3 rounded-md px-2 py-1.5 text-left text-sm transition hover:bg-muted/70 disabled:cursor-not-allowed disabled:opacity-60"
            >
              <span className="shrink-0 font-mono text-xs text-muted-foreground">
                {request.chat_id}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-foreground">
                  {request.title || request.sender}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {request.sender} · {request.last_message_preview}
                </span>
              </span>
              <span className="shrink-0 text-xs font-medium text-primary">
                {telegramPicker.action}
              </span>
            </button>
          ))}
        </section>
      ) : null}

      {suggestions.length > 0 ? (
        <section aria-label="Command suggestions" className="space-y-1">
          {suggestions.slice(0, 6).map((suggestion, index) => {
            const selected = index === selectedSuggestionIndex;
            return (
              <button
                key={`${suggestion.completion}-${index}`}
                type="button"
                onMouseEnter={() => onHoverSuggestion(index)}
                onMouseDown={(event) => {
                  event.preventDefault();
                  onSelectSuggestion(suggestion);
                }}
                className={cn(
                  "flex w-full min-w-0 items-baseline gap-3 rounded-md px-2 py-1.5 text-left text-sm transition",
                  selected
                    ? "bg-muted text-foreground"
                    : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
                )}
              >
                <span className="shrink-0 font-mono text-xs">
                  {suggestion.display}
                </span>
                <span className="min-w-0 truncate text-xs text-muted-foreground/75">
                  {suggestion.description}
                </span>
              </button>
            );
          })}
        </section>
      ) : null}
    </div>
  );
}

function WebSlashCommandFeedbackView({
  feedback,
}: {
  feedback: WebSlashCommandFeedback;
}) {
  return (
    <div className="flex min-w-0 items-start gap-2 text-sm">
      <WebSlashCommandLevelIcon level={feedback.level} />
      <div className="min-w-0 flex-1">
        <p className="break-words text-sm font-medium leading-5 text-foreground">
          {feedback.message}
        </p>
        {feedback.detail ? (
          <p className="break-words text-xs leading-5 text-muted-foreground">
            {feedback.detail}
          </p>
        ) : null}
      </div>
    </div>
  );
}

function WebSlashCommandLevelIcon({ level }: { level: WebSlashCommandLevel }) {
  if (level === "error") {
    return (
      <AlertTriangleIcon
        className="mt-0.5 size-4 shrink-0 text-destructive"
        aria-hidden="true"
      />
    );
  }
  if (level === "warning") {
    return (
      <InfoIcon
        className="mt-0.5 size-4 shrink-0 text-amber-500"
        aria-hidden="true"
      />
    );
  }
  return (
    <CommandIcon
      className="mt-0.5 size-4 shrink-0 text-primary"
      aria-hidden="true"
    />
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

function shouldRenderInlineImagePreview(file: File) {
  return (
    file.size <= AGENT_CHAT_INLINE_PREVIEW_MAX_BYTES &&
    typeof URL !== "undefined" &&
    typeof URL.createObjectURL === "function"
  );
}

function createImagePreviewUrl(file: File) {
  if (!shouldRenderInlineImagePreview(file)) {
    return undefined;
  }
  try {
    return URL.createObjectURL(file);
  } catch {
    return undefined;
  }
}

function revokeImagePreviewUrl(attachment: AgentChatPendingImageAttachment) {
  if (
    !attachment.previewUrl ||
    typeof URL === "undefined" ||
    typeof URL.revokeObjectURL !== "function"
  ) {
    return;
  }
  try {
    URL.revokeObjectURL(attachment.previewUrl);
  } catch {
    // Some mobile WebViews throw while revoking blob URLs during teardown.
  }
}

function isWebSlashCommandInput(input: string) {
  return input.trimStart().startsWith("/");
}

function webSlashCommandBody(input: string) {
  const trimmedStart = input.trimStart();
  if (!trimmedStart.startsWith("/")) {
    return null;
  }
  return trimmedStart.slice(1);
}

function parseWebSlashCommand(input: string) {
  const body = webSlashCommandBody(input);
  if (body === null) {
    return null;
  }
  const trimmed = body.trim();
  return {
    body,
    trimmed,
    trailingSpace: body.endsWith(" "),
    parts: trimmed ? trimmed.split(/\s+/) : [],
  };
}

function webSlashCommandSuggestions(
  input: string,
  snapshot: DashboardSnapshot | null,
): WebSlashCommandSuggestion[] {
  const parsed = parseWebSlashCommand(input);
  if (!parsed) {
    return [];
  }
  if (!parsed.trimmed) {
    return WEB_SLASH_COMMANDS.map(webSlashRootSuggestion);
  }

  const [verb] = parsed.parts;
  const command = webSlashFindCommand(verb);
  if (!command) {
    return WEB_SLASH_COMMANDS.filter((candidate) =>
      candidate.name.startsWith(verb),
    ).map(webSlashRootSuggestion);
  }

  if (command.subcommands) {
    return webSlashSubcommandSuggestions(command, parsed, snapshot);
  }

  if (command.argumentKind === "app") {
    return webSlashAppSuggestions(command, parsed, snapshot);
  }

  return [];
}

function webSlashSubcommandSuggestions(
  command: WebSlashCommandDefinition,
  parsed: NonNullable<ReturnType<typeof parseWebSlashCommand>>,
  snapshot: DashboardSnapshot | null,
) {
  const subcommands = command.subcommands ?? [];
  if (parsed.parts.length === 1) {
    return subcommands.map((subcommand) =>
      webSlashSubcommandSuggestion(command, subcommand),
    );
  }

  const subcommandName = parsed.parts[1] ?? "";
  const subcommand = webSlashFindSubcommand(command, subcommandName);
  const inSubcommandWord =
    (parsed.trailingSpace && parsed.parts.length === 1) ||
    (!parsed.trailingSpace && parsed.parts.length === 2);

  if (inSubcommandWord) {
    if (subcommand && !parsed.trailingSpace) {
      return [];
    }
    return subcommands
      .filter((candidate) => webSlashSubcommandStartsWith(candidate, subcommandName))
      .map((candidate) => webSlashSubcommandSuggestion(command, candidate));
  }

  if (subcommand?.argumentKind === "telegram-request") {
    const prefix = parsed.parts[2] ?? "";
    return webSlashTelegramRequestSuggestions(
      subcommand,
      prefix,
      snapshot?.pending_access_requests ?? [],
    );
  }

  return [];
}

function webSlashAppSuggestions(
  command: WebSlashCommandDefinition,
  parsed: NonNullable<ReturnType<typeof parseWebSlashCommand>>,
  snapshot: DashboardSnapshot | null,
) {
  const apps = webSlashAppNames(snapshot);
  const prefix = parsed.parts[1] ?? "";
  if (parsed.parts.length > 2) {
    return [];
  }
  if (parsed.parts.length === 2 && apps.includes(prefix) && !parsed.trailingSpace) {
    return [];
  }
  return apps
    .filter((candidate) => candidate.startsWith(prefix))
    .map((candidate) => ({
      display: candidate,
      completion: `/${command.name} ${candidate}`,
      description: command.description,
    }));
}

function webSlashTelegramRequestSuggestions(
  subcommand: WebSlashSubcommandDefinition,
  prefix: string,
  requests: DashboardPendingAccessRequest[],
) {
  return requests
    .filter((request) => request.chat_id.toString().startsWith(prefix))
    .map((request) => ({
      display: `${request.chat_id} (${request.sender})`,
      completion: `/telegram ${subcommand.name} ${request.chat_id}`,
      description: request.title || request.last_message_preview,
    }));
}

function webSlashCommandFeedback(
  input: string,
  snapshot: DashboardSnapshot | null,
  attachmentCount: number,
): WebSlashCommandFeedback | null {
  const parsed = parseWebSlashCommand(input);
  if (!parsed || !parsed.trimmed) {
    return null;
  }
  if (attachmentCount > 0) {
    return {
      title: "COMMAND",
      message: "Commands cannot include image attachments.",
      detail: "Remove the image or send it as a normal agent message.",
      level: "error",
      blocksSubmit: true,
    };
  }

  const [verb] = parsed.parts;
  const command = webSlashFindCommand(verb);
  if (!command) {
    if (webSlashCommandSuggestions(input, snapshot).length > 0) {
      return null;
    }
    return {
      title: "UNKNOWN COMMAND",
      message: `No dashboard command named '${verb}'.`,
      detail: "Type / to browse available commands.",
      level: "error",
      blocksSubmit: true,
    };
  }

  if (command.subcommands) {
    const feedback = webSlashSubcommandFeedback(command, parsed, snapshot);
    if (feedback) {
      return feedback;
    }
  }

  const extraArgumentFeedback = webSlashExtraArgumentFeedback(parsed.parts);
  if (extraArgumentFeedback) {
    return extraArgumentFeedback;
  }

  if (command.argumentKind === "app") {
    return webSlashAppFeedback(command, parsed, snapshot);
  }

  return null;
}

function webSlashSubcommandFeedback(
  command: WebSlashCommandDefinition,
  parsed: NonNullable<ReturnType<typeof parseWebSlashCommand>>,
  snapshot: DashboardSnapshot | null,
): WebSlashCommandFeedback | null {
  if (parsed.parts.length === 1) {
    return {
      title: command.name.toUpperCase(),
      message: `Choose a subcommand for /${command.name}.`,
      detail: webSlashSubcommandChoiceText(command),
      level: "warning",
      blocksSubmit: true,
    };
  }

  const subcommandName = parsed.parts[1];
  const subcommand = webSlashFindSubcommand(command, subcommandName);
  if (!subcommand) {
    const possible = (command.subcommands ?? []).some((candidate) =>
      webSlashSubcommandStartsWith(candidate, subcommandName),
    );
    if (possible) {
      return null;
    }
    return {
      title: command.name.toUpperCase(),
      message: `Unknown ${command.name} subcommand '${subcommandName}'.`,
      detail: webSlashSubcommandChoiceText(command),
      level: "error",
      blocksSubmit: true,
    };
  }

  if (
    command.name === "telegram" &&
    subcommand.argumentKind === "telegram-request" &&
    parsed.parts.length === 2
  ) {
    const requests = snapshot?.pending_access_requests ?? [];
    if (requests.length === 0) {
      return {
        title: "TELEGRAM",
        message: `No pending Telegram requests to ${subcommand.name}.`,
        detail: "Use /telegram status to inspect Telegram state.",
        level: "info",
        blocksSubmit: true,
      };
    }
    return {
      title: "TELEGRAM",
      message: `Choose a request to ${subcommand.name}.`,
      detail: requests
        .slice(0, 4)
        .map((request) => `${request.chat_id} ${request.sender}`)
        .join(" · "),
      level: "info",
      blocksSubmit: true,
    };
  }

  if (
    command.name === "telegram" &&
    subcommand.argumentKind === "telegram-request" &&
    parsed.parts.length === 3 &&
    !/^-?\d+$/.test(parsed.parts[2]) &&
    webSlashTelegramRequestSuggestions(
      subcommand,
      parsed.parts[2],
      snapshot?.pending_access_requests ?? [],
    ).length === 0
  ) {
    return {
      title: "TELEGRAM",
      message: `Invalid chat_id '${parsed.parts[2]}'.`,
      detail: `Use /telegram ${subcommand.name} [chat_id].`,
      level: "error",
      blocksSubmit: true,
    };
  }

  return null;
}

function webSlashAppFeedback(
  command: WebSlashCommandDefinition,
  parsed: NonNullable<ReturnType<typeof parseWebSlashCommand>>,
  snapshot: DashboardSnapshot | null,
): WebSlashCommandFeedback | null {
  const apps = webSlashAppNames(snapshot);
  if (parsed.parts.length === 1) {
    return {
      title: "APP STATUS",
      message: "Choose an app for /app-status.",
      detail:
        apps.length > 0
          ? `available: ${apps.join(", ")}`
          : "No app state is currently available.",
      level: "warning",
      blocksSubmit: true,
    };
  }
  const target = parsed.parts[1];
  if (
    parsed.parts.length === 2 &&
    !apps.includes(target) &&
    !apps.some((candidate) => candidate.startsWith(target)) &&
    webSlashAppSuggestions(command, parsed, snapshot).length === 0
  ) {
    return {
      title: "APP STATUS",
      message: `Unknown app '${target}'.`,
      detail:
        apps.length > 0
          ? `available: ${apps.join(", ")}`
          : "No app state is currently available.",
      level: "error",
      blocksSubmit: true,
    };
  }
  return null;
}

function webSlashExtraArgumentFeedback(
  parts: string[],
): WebSlashCommandFeedback | null {
  const [verb] = parts;
  const rootNoArg = ["status", "clear", "restart"];
  if (rootNoArg.includes(verb) && parts.length > 1) {
    return {
      title: verb.toUpperCase(),
      message: `/${verb} does not take extra arguments.`,
      detail: `usage: /${verb}`,
      level: "error",
      blocksSubmit: true,
    };
  }

  if (parts[0] === "debug" && parts.length > 2) {
    return {
      title: "DEBUG",
      message: `/debug ${parts[1]} does not take extra arguments.`,
      detail: `usage: /debug ${parts[1]}`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (parts[0] === "sleep" && parts.length > 2) {
    return {
      title: "SLEEP",
      message: `/sleep ${parts[1]} does not take extra arguments.`,
      detail: `usage: /sleep ${parts[1]}`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (parts[0] === "telegram" && parts[1] === "status" && parts.length > 2) {
    return {
      title: "TELEGRAM",
      message: "/telegram status does not take extra arguments.",
      detail: "usage: /telegram status",
      level: "error",
      blocksSubmit: true,
    };
  }
  if (
    parts[0] === "telegram" &&
    (parts[1] === "approve" || parts[1] === "reject") &&
    parts.length > 3
  ) {
    return {
      title: "TELEGRAM",
      message: `/telegram ${parts[1]} accepts at most one chat_id.`,
      detail: `usage: /telegram ${parts[1]} [chat_id]`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (webSlashFindCommand(parts[0])?.argumentKind === "app" && parts.length > 2) {
    return {
      title: "APP STATUS",
      message: "/app-status accepts exactly one app name.",
      detail: "usage: /app-status <app>",
      level: "error",
      blocksSubmit: true,
    };
  }

  return null;
}

function webSlashTelegramPicker(
  input: string,
  snapshot: DashboardSnapshot | null,
): WebSlashTelegramPicker | null {
  const parsed = parseWebSlashCommand(input);
  if (!parsed || parsed.parts.length !== 2 || parsed.parts[0] !== "telegram") {
    return null;
  }
  const action = parsed.parts[1];
  if (action !== "approve" && action !== "reject") {
    return null;
  }
  const requests = snapshot?.pending_access_requests ?? [];
  return requests.length > 0 ? { action, requests } : null;
}

function webSlashCommandResultForResponse(
  input: string,
  output: string,
): WebSlashCommandResult | null {
  const level = webSlashCommandLevelForResponse(output);
  if (webSlashIsClearCommand(input) && level !== "error") {
    return null;
  }
  const message = webSlashCompactMessage(output);
  if (!message && !output.trim()) {
    return null;
  }
  const presentation =
    webSlashCommandUsesPanel(input) && level !== "error" ? "panel" : "compact";
  return {
    command: input.trim(),
    title: webSlashCommandTitle(input),
    output,
    message,
    detail: webSlashCommandDetail(output),
    level,
    presentation,
  };
}

function webSlashCommandUsesPanel(input: string) {
  const parsed = parseWebSlashCommand(input);
  if (!parsed) {
    return false;
  }
  const parts = parsed.parts;
  if (parts[0] === "status") {
    return true;
  }
  if (parts[0] === "debug" && parts.length >= 2) {
    return true;
  }
  if (parts[0] === "sleep" && parts[1] === "status") {
    return true;
  }
  if (parts[0] === "telegram" && parts[1] === "status") {
    return true;
  }
  return Boolean(webSlashFindCommand(parts[0])?.argumentKind === "app" && parts[1]);
}

function webSlashIsClearCommand(input: string) {
  const parsed = parseWebSlashCommand(input);
  return parsed?.parts[0] === "clear";
}

function webSlashCommandTitle(input: string) {
  return (
    parseWebSlashCommand(input)?.parts.join(" ").toUpperCase() || "COMMAND"
  );
}

function webSlashCommandLevelForResponse(output: string): WebSlashCommandLevel {
  const lower = output.toLowerCase();
  return lower.includes("failed") ||
    lower.includes("unknown") ||
    lower.includes("invalid") ||
    lower.includes("unavailable") ||
    lower.includes("required") ||
    lower.includes("cannot") ||
    lower.includes("error")
    ? "error"
    : "info";
}

function webSlashCompactMessage(output: string) {
  const first = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find(Boolean);
  return truncateText(first ?? "Done", 180);
}

function webSlashCommandDetail(output: string) {
  const lines = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  return lines.length > 1 ? truncateText(lines.slice(1).join("  "), 220) : undefined;
}

function webSlashFindCommand(verb: string) {
  return WEB_SLASH_COMMANDS.find(
    (command) => command.name === verb || command.aliases?.includes(verb),
  );
}

function webSlashFindSubcommand(
  command: WebSlashCommandDefinition,
  name: string,
) {
  return command.subcommands?.find(
    (subcommand) =>
      subcommand.name === name || subcommand.aliases?.includes(name),
  );
}

function webSlashSubcommandStartsWith(
  subcommand: WebSlashSubcommandDefinition,
  prefix: string,
) {
  return (
    subcommand.name.startsWith(prefix) ||
    Boolean(subcommand.aliases?.some((alias) => alias.startsWith(prefix)))
  );
}

function webSlashRootSuggestion(
  command: WebSlashCommandDefinition,
): WebSlashCommandSuggestion {
  return {
    display: command.usage,
    completion: `/${command.name}`,
    description: command.description,
  };
}

function webSlashSubcommandSuggestion(
  command: WebSlashCommandDefinition,
  subcommand: WebSlashSubcommandDefinition,
): WebSlashCommandSuggestion {
  return {
    display: subcommand.usage,
    completion: `/${command.name} ${subcommand.name}`,
    description: subcommand.description,
  };
}

function webSlashSubcommandChoiceText(command: WebSlashCommandDefinition) {
  return `available: ${(command.subcommands ?? [])
    .map((subcommand) => subcommand.usage)
    .join(" | ")}`;
}

function webSlashAppNames(snapshot: DashboardSnapshot | null) {
  return (snapshot?.app_status_outputs ?? [])
    .map(([name]) => name)
    .filter(Boolean)
    .sort();
}

function truncateText(text: string, maxLength: number) {
  return text.length > maxLength ? `${text.slice(0, maxLength)}...` : text;
}

function readFileAsDataUrl(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error("Failed to read image."));
      }
    });
    reader.addEventListener("error", () => {
      reject(reader.error ?? new Error("Failed to read image."));
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
  sessionId,
  snapshot,
  isFocused,
  panelRef,
  composerHeight,
}: {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
  isFocused: boolean;
  panelRef: RefObject<HTMLDivElement | null>;
  composerHeight: number;
}) {
  const snapshotBubbles = useMemo(
    () => agentChatBubblesFromSnapshot(snapshot),
    [snapshot],
  );
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
  const displayItems = useMemo(() => {
    const items = foldCompletedAgentChatActivity(bubbles);
    return isFocused
      ? items
      : items.slice(-AGENT_CHAT_PREVIEW_MAX_VISIBLE_BUBBLES);
  }, [bubbles, isFocused]);

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
    if (
      !isFocused ||
      isLoadingHistory ||
      !hasMoreBefore ||
      oldestCursor === null
    ) {
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
        sessionId,
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
  }, [
    hasMoreBefore,
    isFocused,
    isLoadingHistory,
    oldestCursor,
    panelRef,
    sessionId,
  ]);

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
  }, [sessionId, snapshot?.activity_history?.newest_cursor]);

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
          latestPanel.scrollHeight -
            latestPanel.clientHeight -
            latestPanel.scrollTop <=
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
          {displayItems.length > 0 ? (
            <div
              className={cn(
                "w-full space-y-3 px-6 py-1.5",
                !isFocused && "space-y-2",
              )}
            >
              {isFocused &&
              (hasMoreBefore || isLoadingHistory || historyError) ? (
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
                      {isLoadingHistory ? "Loading…" : "Load older messages"}
                    </Button>
                  ) : null}
                  {historyError ? (
                    <p role="alert" className="px-3 text-xs text-destructive">
                      {historyError}
                    </p>
                  ) : null}
                </div>
              ) : null}
              {displayItems.map((item) =>
                item.kind === "bubble" ? (
                  <AgentChatBubbleItem
                    key={item.id}
                    bubble={item.bubble}
                    isFocused={isFocused}
                  />
                ) : (
                  <AgentChatFoldedActivityGroup
                    key={item.id}
                    id={item.id}
                    bubbles={item.bubbles}
                    isFocused={isFocused}
                  />
                ),
              )}
            </div>
          ) : (
            <p className="mx-auto max-w-[min(32rem,calc(100vw-3rem))] px-4 py-3 text-center text-xs text-muted-foreground/70">
              Focus the bottom composer to float the message stream around the
              agent across the screen.
            </p>
          )}
        </div>
      </div>
      <Button
        type="button"
        variant="secondary"
        size="icon-lg"
        aria-label="Back to bottom"
        title="Back to bottom"
        onMouseDown={(event) => {
          event.preventDefault();
        }}
        onClick={handleScrollToBottomClick}
        style={{
          bottom: `calc(max(1.25rem, env(safe-area-inset-bottom)) + ${
            composerHeight + AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX
          }px)`,
        }}
        className={cn(
          "fixed left-1/2 z-40 -translate-x-1/2 rounded-full border border-border/70 bg-background/90 shadow-lg shadow-background/30 backdrop-blur-xl transition-all duration-200 md:left-[calc(18rem+(100vw-18rem)/2)]",
          showScrollToBottom
            ? "pointer-events-auto translate-y-0 opacity-100"
            : "pointer-events-none translate-y-2 opacity-0",
        )}
      >
        <ArrowDownIcon className="size-4" aria-hidden="true" />
      </Button>
    </>
  );
}

function AgentChatFoldedActivityGroup({
  id,
  bubbles,
  isFocused,
}: {
  id: string;
  bubbles: AgentChatBubble[];
  isFocused: boolean;
}) {
  const { open, toggle } = useCollapsibleState(false);
  const activityCount = bubbles.length;
  const workedDurationLabel = formatAgentChatWorkedDuration(bubbles);

  if (activityCount === 0) {
    return null;
  }

  return (
    <article
      className={cn(
        "w-full py-1 text-sm leading-6 text-muted-foreground",
        !isFocused && "select-none",
      )}
    >
      <div className="rounded-2xl border border-border/45 bg-background/50 p-4 shadow-sm backdrop-blur-xl">
        <div className="flex min-w-0 items-center justify-between gap-4">
          <p className="min-w-0 truncate font-semibold text-foreground/90">
            Worked For {workedDurationLabel}
          </p>
          {isFocused ? (
            <CollapsibleTrigger
              open={open}
              onToggle={toggle}
              className="ml-3 w-auto shrink-0 text-xs"
            >
              {open ? "Hide" : "Show"}
            </CollapsibleTrigger>
          ) : null}
        </div>
        {isFocused && open ? (
          <div className="mt-4 border-l border-border/60 pl-3">
            {bubbles.map((bubble) => (
              <AgentChatBubbleItem
                key={`${id}-${bubble.id}`}
                bubble={bubble}
                isFocused={isFocused}
              />
            ))}
          </div>
        ) : null}
      </div>
    </article>
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
  const visibleBlockLimit =
    isConversationMessage && isFocused
      ? primaryBlocks.length
      : isFocused
        ? 6
        : 3;
  const visibleBlocks = primaryBlocks.slice(0, visibleBlockLimit);

  return (
    <article
      className={cn(
        "w-full py-1.5",
        bubble.live || bubble.status === "running"
          ? "text-foreground"
          : "text-foreground/95",
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
              {isRunning ? (
                <Loader2Icon className="size-2.5 animate-spin" />
              ) : null}
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
        fullBody={render.fullBody}
        imageAttachments={render.imageAttachments}
        bodyLimit={render.bodyLimit}
        tone={render.tone}
      />
    );
  }

  if (render.kind === "thinking") {
    return (
      <AgentChatThinkingCollapsibleCell
        id={bubbleId}
        title={render.title}
        bodyLines={render.bodyLines}
        fullBody={render.fullBody}
        bodyLimit={render.bodyLimit}
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

  if (render.kind === "primitive") {
    return (
      <AgentChatStatusLineCell
        marker={render.marker}
        label={render.title}
        value={render.primitiveId}
        valueClassName="font-mono break-all"
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
        "inline-flex h-6 w-3 shrink-0 items-center justify-start font-mono text-sm font-semibold leading-none text-muted-foreground",
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
  fullBody,
  imageAttachments = [],
  bodyLimit,
  tone = "default",
}: {
  id: string;
  marker: string;
  title: string;
  bodyLines: string[];
  fullBody?: string | null;
  imageAttachments?: AgentChatImageAttachmentData[];
  bodyLimit?: number;
  tone?: "default" | "error" | "muted";
}) {
  const renderedText = fullBody
    ? fullBody.split("\n").slice(1).join("\n")
    : bodyLines.join("\n");
  const renderedLineCount = renderedText ? renderedText.split("\n").length : 0;

  return (
    <div
      className={cn(
        "space-y-1 text-sm leading-6 text-foreground/90",
        tone === "error" && "text-destructive",
        tone === "muted" && "text-muted-foreground",
      )}
    >
      <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-[16px] px-3">
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
      {renderedLineCount > 0 ? (
        <div
          className={cn(
            "space-y-0.5 px-3 text-muted-foreground",
            tone === "error" && "text-destructive/90",
            tone === "muted" && "text-muted-foreground",
          )}
        >
          <AgentChatMarkdownText
            text={renderedText}
            limit={bodyLimit ?? AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT}
            tone={tone === "error" ? "error" : "default"}
          />
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
    <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-[16px] px-3 text-sm leading-6 text-foreground/90">
      <AgentChatActivityMarker marker={marker} />
      <p className="min-w-0 break-words font-semibold text-foreground">
        {label}
        {value ? (
          <>
            {" "}
            <span className={cn("text-foreground/90", valueClassName)}>
              {value}
            </span>
            {suffix}
          </>
        ) : null}
      </p>
    </div>
  );
}

function AgentChatThinkingCollapsibleCell({
  id,
  title,
  bodyLines,
  fullBody,
  bodyLimit,
}: {
  id: string;
  title: string;
  bodyLines: string[];
  fullBody?: string | null;
  bodyLimit: number;
}) {
  const { open, toggle } = useCollapsibleState(false);
  const contentLines: string[] = fullBody ? fullBody.split("\n") : bodyLines;
  const isTruncatable = Boolean(fullBody) || bodyLines.length > bodyLimit;

  return (
    <div className="space-y-0.5 text-sm leading-6 text-foreground/90 border-l-2 border-muted pl-3 ml-3">
      <div className="flex items-center gap-1.5 min-w-0">
        <p className="min-w-0 break-words font-semibold text-foreground">
          <AgentChatMarkdownInline text={title} />
        </p>
        {isTruncatable ? (
          <CollapsibleTrigger
            open={open}
            onToggle={toggle}
            className="ml-auto shrink-0 w-auto text-xs"
          >
            {open ? "Hide" : "Expand"}
          </CollapsibleTrigger>
        ) : null}
      </div>
      {contentLines.length > 0 ? (
        <div
          className={`relative text-muted-foreground ${
            !open && isTruncatable ? "max-h-[4.5rem] overflow-hidden" : ""
          }`}
        >
          {contentLines.map((line, index) => (
            <p
              key={`${id}-thinking-line-${index}`}
              className="min-w-0 break-words"
            >
              <AgentChatMarkdownInline text={line} />
            </p>
          ))}
          {!open && isTruncatable ? (
            <div className="absolute bottom-0 left-0 right-0 h-8 bg-gradient-to-t from-background to-transparent pointer-events-none" />
          ) : null}
        </div>
      ) : null}
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
      <div className="flex min-w-0 items-start gap-x-[16px] px-3 leading-6">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
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

function AgentChatPlanStatusBadge({
  status,
}: {
  status: AgentChatPlanStepStatus;
}) {
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
  const renderedOutput =
    outputLines.length > 0
      ? truncateAgentChatLinesMiddle(
          outputLines,
          AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES,
          AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES,
        )
      : [mode === "running" ? "running..." : "(no output)"];
  const verb = mode === "running" ? "Running" : "Ran";

  return (
    <div className="space-y-1 text-sm">
      <div className="flex min-w-0 items-start gap-x-[16px] px-3 leading-6">
        {mode === "running" ? (
          <span className="inline-flex h-6 w-3 shrink-0 items-center justify-start text-muted-foreground">
            <Loader2Icon className="size-3 animate-spin" />
          </span>
        ) : (
          <AgentChatActivityMarker marker={marker} />
        )}
        <p
          className="min-w-0 flex-1 truncate font-semibold text-foreground"
          title={`${verb} ${title}`}
        >
          {verb}{" "}
          <span className="font-mono font-medium text-foreground/90">
            {title}
          </span>
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
          <span
            className={cn(line.startsWith("… +") && "text-muted-foreground/70")}
          >
            {line}
          </span>
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
      <div className="flex min-w-0 items-start gap-x-[16px] px-3 leading-6">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
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
            <p className="text-xs text-muted-foreground">
              … {hiddenFileCount} more file(s)
            </p>
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

  const oldLineNumber =
    typeof line.old_lineno === "number"
      ? String(line.old_lineno).padStart(oldWidth, " ")
      : "".padStart(oldWidth, " ");
  const newLineNumber =
    typeof line.new_lineno === "number"
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
      <span className="select-none text-right text-muted-foreground/65">
        {oldLineNumber}
      </span>
      <span className="select-none text-right text-muted-foreground/65">
        {newLineNumber}
      </span>
      <span className="select-none font-semibold text-muted-foreground">
        {gutter}
      </span>
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
      <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-[16px] px-3">
        <AgentChatActivityMarker marker={marker} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {visibleDetailLines.length > 0 || hiddenDetailCount > 0 ? (
        <div className="space-y-0.5 pl-10 pr-3 text-xs leading-5 text-muted-foreground">
          {visibleDetailLines.map((line, index) => (
            <p key={`${id}-detail-${index}`} className="break-words">
              {line}
            </p>
          ))}
          {hiddenDetailCount > 0 ? (
            <p>… {hiddenDetailCount} more line(s)</p>
          ) : null}
        </div>
      ) : null}
      {visibleMessageLines.length > 0 || hiddenMessageCount > 0 ? (
        <div className="space-y-0.5 px-3 text-foreground/90">
          {visibleMessageLines.map((line, index) => (
            <p key={`${id}-message-${index}`} className="min-w-0 break-words">
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
  marker,
  title,
  messageLines,
  disposition,
}: {
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
      <div className="flex min-w-0 items-start gap-x-[16px] px-3 leading-6">
        <AgentChatActivityMarker
          marker={marker}
          tone={disposition === "failed" ? "error" : "default"}
          className={
            disposition === "dismissed" ? "text-muted-foreground" : undefined
          }
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
  const lineLimit =
    messageMode
      ? isFocused
        ? AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT
        : AGENT_CHAT_MESSAGE_LINE_LIMIT
      : AGENT_CHAT_ACTIVITY_BLOCK_LINE_LIMIT;

  if (!record) {
    return null;
  }

  if (type === "text") {
    return (
      <AgentChatMarkdownText
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
      <AgentChatListItems blockId={blockId} items={items} limit={lineLimit} />
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
        <AgentChatImageAttachment label={label} uri={uri} mimeType={mimeType} />
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

function limitMarkdownInput(text: string, limit: number): string {
  if (limit >= Number.MAX_SAFE_INTEGER) return text;
  const lines = text.split("\n");
  if (lines.length <= limit) return text;
  return lines.slice(0, limit).join("\n");
}

const AgentChatMarkdownText = memo(function AgentChatMarkdownText({
  text,
  limit,
  tone = "default",
}: {
  text: string;
  limit: number;
  tone?: "default" | "error";
}) {
  const limitedText = limitMarkdownInput(text, limit);

  if (!limitedText.trim()) {
    return null;
  }

  return (
    <div
      className={cn(
        "space-y-2 text-sm leading-6 text-foreground/90",
        tone === "error" && "text-destructive",
      )}
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }: any) => (
            <h3 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h3>
          ),
          h2: ({ children }: any) => (
            <h4 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h4>
          ),
          h3: ({ children }: any) => (
            <h5 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h5>
          ),
          h4: ({ children }: any) => (
            <h5 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h5>
          ),
          h5: ({ children }: any) => (
            <h5 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h5>
          ),
          h6: ({ children }: any) => (
            <h5 className="mt-3 break-words text-base font-semibold leading-7 text-foreground first:mt-0">
              {children}
            </h5>
          ),
          ul: ({ children }: any) => (
            <ul className="list-disc space-y-1 pl-5 text-foreground/90">
              {children}
            </ul>
          ),
          ol: ({ children }: any) => (
            <ol className="list-decimal space-y-1 pl-5 text-foreground/90">
              {children}
            </ol>
          ),
          li: ({ children }: any) => (
            <li className="break-words pl-1">{children}</li>
          ),
          blockquote: ({ children }: any) => (
            <blockquote className="border-l-2 border-border/70 pl-3 text-muted-foreground">
              {children}
            </blockquote>
          ),
          hr: () => <hr className="border-border/70" />,
          p: ({ children }: any) => <p className="break-words">{children}</p>,
          code: (props: any) => {
            const { inline, className, children } = props;
            if (!inline && className) {
              const language = String(className).replace(/^language-/, "");
              return (
                <div className="space-y-1">
                  <div className="flex items-center gap-2 px-3">
                    <span className="font-mono text-[0.7rem] leading-none text-muted-foreground">
                      &lt;/&gt;
                    </span>
                    <span className="truncate text-sm font-semibold text-foreground/90">
                      {language || "Code"}
                    </span>
                  </div>
                  <pre className="max-h-72 overflow-auto whitespace-pre px-3 font-mono text-[0.82rem] leading-6 text-foreground/90 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
                    <code className={className}>{children}</code>
                  </pre>
                </div>
              );
            }

            return (
              <code className="font-mono text-[0.85em] text-foreground">
                {children}
              </code>
            );
          },
          a: (props: any) => {
            const { children, href } = props;
            return (
              <a
                href={href}
                target="_blank"
                rel="noreferrer"
                className="break-all text-sky-300 underline-offset-4 hover:underline"
              >
                {children}
              </a>
            );
          },
          strong: ({ children }: any) => (
            <strong className="font-semibold text-foreground">
              {children}
            </strong>
          ),
          em: ({ children }: any) => <em className="italic">{children}</em>,
        }}
      >
        {limitedText}
      </ReactMarkdown>
    </div>
  );
});
function AgentChatMarkdownInline({ text }: { text: string }) {
  if (!text) {
    return null;
  }

  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      disallowedElements={[
        "p",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "ul",
        "ol",
        "li",
        "blockquote",
        "pre",
        "hr",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
      ]}
      unwrapDisallowed
      components={{
        code: ({ children }: any) => (
          <code className="font-mono text-[0.85em] text-foreground">
            {children}
          </code>
        ),
        a: (props: any) => {
          const { children, href } = props;
          return (
            <a
              href={href}
              target="_blank"
              rel="noreferrer"
              className="break-all text-sky-300 underline-offset-4 hover:underline"
            >
              {children}
            </a>
          );
        },
        strong: ({ children }: any) => (
          <strong className="font-semibold text-foreground">{children}</strong>
        ),
        em: ({ children }: any) => <em className="italic">{children}</em>,
      }}
    >
      {text}
    </ReactMarkdown>
  );
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

const AgentChatCodeBlock = memo(function AgentChatCodeBlock({
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
  const canCopy =
    typeof navigator !== "undefined" && Boolean(navigator.clipboard);

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
            <span className="font-mono text-[0.7rem] leading-none">
              &lt;/&gt;
            </span>
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
});

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

  return (
    labels[normalized] ??
    `${normalized[0]?.toUpperCase() ?? ""}${normalized.slice(1)}`
  );
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
        <p className="font-sans text-xs text-muted-foreground">
          … +{hiddenFileCount} more file(s)
        </p>
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

function foldCompletedAgentChatActivity(
  bubbles: AgentChatBubble[],
): AgentChatDisplayItem[] {
  const items: AgentChatDisplayItem[] = [];
  let activeInput: AgentChatBubble | null = null;
  let pendingActivity: AgentChatBubble[] = [];

  function pushBubble(bubble: AgentChatBubble) {
    items.push({ kind: "bubble", id: bubble.id, bubble });
  }

  function flushPendingActivity() {
    for (const bubble of pendingActivity) {
      pushBubble(bubble);
    }
    pendingActivity = [];
  }

  function pushFoldedActivity(outputBubble: AgentChatBubble) {
    if (pendingActivity.length === 0) {
      return;
    }

    const first = pendingActivity[0];
    const last = pendingActivity[pendingActivity.length - 1];
    items.push({
      kind: "foldedActivityGroup",
      id: `folded-${activeInput?.id ?? "input"}-${outputBubble.id}-${first.id}-${last.id}`,
      bubbles: pendingActivity,
    });
    pendingActivity = [];
  }

  for (const bubble of bubbles) {
    if (agentChatBubbleIsUserInputBoundary(bubble)) {
      flushPendingActivity();
      pushBubble(bubble);
      activeInput = bubble;
      continue;
    }

    if (activeInput && agentChatBubbleIsOutputBoundary(bubble)) {
      pushFoldedActivity(bubble);
      pushBubble(bubble);
      activeInput = null;
      continue;
    }

    if (activeInput && agentChatBubbleCanFoldWithCompletedWork(bubble)) {
      pendingActivity.push(bubble);
      continue;
    }

    flushPendingActivity();
    pushBubble(bubble);
  }

  flushPendingActivity();
  return items;
}

function agentChatBubbleIsUserInputBoundary(bubble: AgentChatBubble) {
  return (
    agentChatBubbleIsConversationMessage(bubble) &&
    (bubble.role === "user" || bubble.role === "telegram")
  );
}

function agentChatBubbleIsOutputBoundary(bubble: AgentChatBubble) {
  return agentChatBubbleHasActivityCellVariant(bubble, "Reply");
}

function agentChatBubbleCanFoldWithCompletedWork(bubble: AgentChatBubble) {
  // A terminal tool result can describe a still-running session after the
  // tool call itself completed. Once it is committed history (not a live
  // snapshot row), keep it eligible for the completed-work fold.
  return !bubble.live;
}

function agentChatBubbleHasActivityCellVariant(
  bubble: AgentChatBubble,
  variant: string,
) {
  return Boolean(agentChatActivityCellPayload(bubble.cell, variant));
}

function formatAgentChatWorkedDuration(bubbles: AgentChatBubble[]) {
  const startTimes = bubbles
    .map((bubble) => bubble.createdAt)
    .filter((value) => value > 0);
  const endTimes = bubbles
    .map((bubble) => bubble.updatedAt)
    .filter((value) => value > 0);

  if (startTimes.length === 0 || endTimes.length === 0) {
    return "0s";
  }

  return formatAgentChatDuration(
    Math.max(0, Math.max(...endTimes) - Math.min(...startTimes)),
  );
}

function formatAgentChatDuration(durationMs: number) {
  const totalSeconds = Math.max(0, Math.round(durationMs / 1000));
  if (totalSeconds < 60) {
    return `${totalSeconds}s`;
  }

  const totalMinutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (totalMinutes < 60) {
    return seconds > 0 ? `${totalMinutes}m ${seconds}s` : `${totalMinutes}m`;
  }

  const totalHours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (totalHours < 24) {
    return minutes > 0 ? `${totalHours}h ${minutes}m` : `${totalHours}h`;
  }

  const days = Math.floor(totalHours / 24);
  const hours = totalHours % 24;
  return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
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
    createdAt: numberValue(record.created_at, 0),
    updatedAt: numberValue(record.updated_at, 0),
    blocks: webActivityBlocksValue(record.blocks),
    planSteps: agentChatPlanStepsFromMetadata(record.metadata),
    live,
    toolName: tool ? stringValue(tool.name, "") : undefined,
    appName: tool ? stringValue(tool.app, "") : undefined,
    sourceLabel: source
      ? stringValue(source.label, stringValue(source.source_type, ""))
      : undefined,
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

  if (
    actor === "telegram" ||
    stringValue(source?.source_type, "") === "telegram"
  ) {
    return "telegram";
  }

  if (["plan", "primitive", "memory"].includes(kind) || actor === "system") {
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

  if (kind === "primitive") {
    return "Primitive";
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
    if (
      bubble.appName === "Coding" ||
      bubble.toolName === "coding_tool_group"
    ) {
      return "◎";
    }
    return "⌁";
  }

  if (bubble.kind === "patch") {
    return "±";
  }

  if (bubble.kind === "primitive") {
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
    return "Running";
  }

  if (status === "failed") {
    return "Failed";
  }

  if (status === "dismissed") {
    return "Dismissed";
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
  const primaryLabel =
    bubble.appName === "Coding"
      ? null
      : bubble.appName || bubble.toolName || bubble.kind;

  return [primaryLabel, bubble.sourceLabel].filter(Boolean).join(" · ");
}

function agentChatBubbleIsConversationMessage(bubble: AgentChatBubble) {
  return (
    bubble.kind === "message" &&
    (bubble.role === "assistant" ||
      bubble.role === "user" ||
      bubble.role === "telegram")
  );
}

function agentChatDisplayBlocksForBubble(
  bubble: AgentChatBubble,
  blocks: WebActivityBlock[],
): WebActivityBlock[] {
  if (!agentChatBubbleIsConversationMessage(bubble)) {
    return blocks;
  }

  return blocks;
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

  const genericApp =
    agentChatActivityCellPayload(cell, "GenericApp") ??
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

  const createPrimitive = agentChatActivityCellPayload(
    cell,
    "CreatePrimitiveSpecResult",
  );
  if (createPrimitive) {
    return {
      kind: "primitive",
      marker: "⌘",
      title: "Created Primitive Spec:",
      primitiveId: stringValue(createPrimitive.primitive_id, "unknown"),
    };
  }

  const activatePrimitive = agentChatActivityCellPayload(
    cell,
    "ActivatePrimitiveResult",
  );
  if (activatePrimitive) {
    return {
      kind: "primitive",
      marker: "⌘",
      title: "Activated Primitive:",
      primitiveId: stringValue(activatePrimitive.primitive_id, "unknown"),
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
      title: agentChatReplyTitle(
        disposition,
        stringValue(reply.subject, "message"),
      ),
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

  const thinking = agentChatActivityCellPayload(cell, "Thinking");
  if (thinking) {
    return {
      kind: "thinking",
      title: stringValue(thinking.title, "Thinking"),
      bodyLines: stringArrayValue(thinking.body_lines),
      fullBody: nullableStringValue(thinking.full_body),
      bodyLimit: AGENT_CHAT_THINKING_PREVIEW_LINE_LIMIT,
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
    fullBody: nullableStringValue(cell.full_body),
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
  if (
    normalized === "resolved" ||
    normalized === "dismissed" ||
    normalized === "failed"
  ) {
    return normalized;
  }
  return "unknown";
}

function agentChatReplyTitle(disposition: string, subject: string) {
  if (disposition === "resolved") {
    return subject.toLowerCase() === "notice"
      ? "Resolved Notice"
      : "Resolved Message";
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

function agentChatActivityCellPayload(
  cell: ActivityCellVariant | null | undefined,
  variant: string,
): Record<string, unknown> | null {
  const record = asActivityCellVariant(cell) as Record<string, unknown> | null;
  return asRecord(record?.[variant]);
}

function normalizeCanonicalPlanStepStatus(
  value: unknown,
): AgentChatPlanStepStatus {
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
    .filter((attachment): attachment is Record<string, unknown> =>
      Boolean(attachment),
    )
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
  return /^queued (terminal|session) message as event\b/.test(output)
    ? "Sent to agent"
    : output;
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
