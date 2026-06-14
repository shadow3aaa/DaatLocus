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
  type ReactNode,
  type RefObject,
  type UIEvent,
} from "react";

import {
  ArrowDownIcon,
  AlertTriangleIcon,
  CheckIcon,
  ChevronRightIcon,
  ClipboardIcon,
  CommandIcon,
  ImagePlusIcon,
  InfoIcon,
  SendHorizontalIcon,
  XIcon,
} from "lucide-react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  CollapsibleTrigger,
  useCollapsibleState,
} from "@/components/ui/collapsible";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupTextarea,
} from "@/components/ui/input-group";
import { Separator } from "@/components/ui/separator";
import { Spinner } from "@/components/ui/spinner";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  fetchDashboardActivityHistory,
  fetchSettingsSummary,
  getDashboardAttachmentUrl,
  runDashboardAction,
  runDashboardCommand,
  type DashboardAction,
  type DashboardActionResult,
  type DashboardActivityHistoryPage,
  type DashboardCommandAttachment,
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

const AGENT_CHAT_HISTORY_PAGE_LIMIT = 80;
const AGENT_CHAT_MESSAGE_LINE_LIMIT = 5;
const AGENT_CHAT_ACTIVITY_BLOCK_LINE_LIMIT = 12;
const AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT = Number.MAX_SAFE_INTEGER;
const AGENT_CHAT_PLAN_STEP_LIMIT = 8;
const AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES = 4;
const AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES = 4;
const AGENT_CHAT_EXPLORED_CALL_LIMIT = 12;
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
export function AgentPage({
  sessionId,
  mockSnapshot,
}: {
  sessionId: string;
  mockSnapshot?: DashboardSnapshot;
}) {
  const { isLoading, loadError, snapshot } = useDashboardSnapshot(sessionId, {
    disabled: Boolean(mockSnapshot),
    initialSnapshot: mockSnapshot ?? null,
  });
  const chatPanelRef = useRef<HTMLDivElement>(null);
  const [chatComposerHeight, setChatComposerHeight] = useState(
    AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX,
  );
  const [supportsVision, setSupportsVision] = useState(true);

  useEffect(() => {
    if (mockSnapshot) {
      setSupportsVision(true);
      return;
    }

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
  }, [mockSnapshot]);

  const agentStatus = deriveAgentStatus({
    loadError,
    isLoading,
    snapshot,
  });
  const summaryText = derivePlanSummaryText(snapshot);

  return (
    <section
      id="agent"
      aria-label="Agent"
      className="relative flex h-screen min-h-screen w-full max-w-full flex-col overflow-hidden bg-background"
    >
      <AgentChatBubbles
        sessionId={sessionId}
        snapshot={snapshot}
        panelRef={chatPanelRef}
        composerHeight={chatComposerHeight}
      />
      <AgentChatComposer
        sessionId={sessionId}
        snapshot={snapshot}
        agentName={snapshot?.agent_name}
        agentStatusLabel={agentStatus.label}
        summaryText={summaryText}
        supportsVision={supportsVision}
        chatPanelRef={chatPanelRef}
        onHeightChange={setChatComposerHeight}
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
  uiHint?: string | null;
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

type AgentChatActivityMarkerKind =
  | "activity"
  | "error"
  | "user";

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
  dismissible?: boolean;
};

type WebSlashPanel =
  | {
      kind: "selection";
      panel: "debug" | "sleep" | "telegram" | "skills" | "app-status";
    }
  | {
      kind: "detail";
      title: string;
      text: string;
    }
  | {
      kind: "status";
    }
  | {
      kind: "sleep-status";
    }
  | {
      kind: "telegram-status";
    }
  | {
      kind: "skills-list";
      search: string;
    }
  | {
      kind: "skills-toggle";
      search: string;
      feedback?: WebSlashCommandFeedback | null;
    }
  | {
      kind: "telegram-access";
      action: "approve" | "reject";
    };

type WebSlashSelectionItem = {
  id: string;
  name: string;
  description: string;
  disabled?: boolean;
};

type WebSlashActionFeedback = WebSlashCommandFeedback & {
  command?: string;
};

type WebSlashCommandDefinition = {
  name: string;
  description: string;
  aliases?: string[];
};

const WEB_SLASH_COMMANDS: WebSlashCommandDefinition[] = [
  {
    name: "status",
    description: "show overall status",
  },
  {
    name: "clear",
    description: "clear runtime conversation, plan, events, and activity",
  },
  {
    name: "debug",
    description: "debug outputs and internal runtime views",
  },
  {
    name: "app-status",
    description: "show current structured app state and llm-facing note",
    aliases: ["app_status"],
  },
  {
    name: "restart",
    description: "restart the daemon",
  },
  {
    name: "sleep",
    description: "sleep controls and status",
  },
  {
    name: "skills",
    description: "list and manage OpenSkills automatic use",
  },
  {
    name: "telegram",
    description: "telegram status and access controls",
  },
];

type AgentChatActivityCellRender =
  | {
      kind: "text";
      icon: AgentChatActivityMarkerKind;
      title: string;
      bodyLines: string[];
      fullBody?: string | null;
      imageAttachments?: AgentChatImageAttachmentData[];
      bodyLimit?: number;
      tone?: "default" | "error" | "muted";
    }
  | {
      kind: "browser";
      icon: AgentChatActivityMarkerKind;
      title: string;
      detailLines: string[];
      detailLimit?: number;
    }
  | {
      kind: "plan";
      icon: AgentChatActivityMarkerKind;
      title: string;
      steps: AgentChatPlanStep[];
    }
  | {
      kind: "primitive";
      icon: AgentChatActivityMarkerKind;
      title: string;
      primitiveId: string;
    }
  | {
      kind: "exec";
      icon: AgentChatActivityMarkerKind;
      title: string;
      outputLines: string[];
      running?: boolean;
      exitCode?: number | null;
    }
  | {
      kind: "explored";
      icon: AgentChatActivityMarkerKind;
      title: string;
      calls: AgentChatExploredCall[];
    }
  | {
      kind: "patch";
      icon: AgentChatActivityMarkerKind;
      title: string;
      files: AgentChatDiffFile[];
    }
  | {
      kind: "messageActivity";
      icon: AgentChatActivityMarkerKind;
      title: string;
      detailLines: string[];
      messageLines: string[];
      detailLimit: number;
      messageLimit: number;
    }
  | {
      kind: "reply";
      icon: AgentChatActivityMarkerKind;
      title: string;
      messageLines: string[];
      disposition: string;
      subject: string;
    }
  | {
      kind: "thinking";
      title: string;
      bodyLines: string[];
      fullBody?: string | null;
      bodyLimit: number;
    };

type AgentChatExploredCallAction = "read" | "list" | "search" | "run" | "unknown";

type AgentChatExploredCall = {
  toolName: string;
  action: AgentChatExploredCallAction;
  target: string | null;
  secondaryTarget: string | null;
  summary: string;
  detailLines: string[];
  detailTitle: string | null;
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
  agentStatusLabel,
  summaryText,
  supportsVision = true,
  chatPanelRef,
  onHeightChange,
}: {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
  agentName?: string;
  agentStatusLabel: string;
  summaryText: string;
  supportsVision?: boolean;
  chatPanelRef: RefObject<HTMLDivElement | null>;
  onHeightChange: (height: number) => void;
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
  const [slashPanel, setSlashPanel] = useState<WebSlashPanel | null>(null);
  const [slashActionFeedback, setSlashActionFeedback] =
    useState<WebSlashActionFeedback | null>(null);

  const slashCommandSuggestions = useMemo(
    () => webSlashCommandSuggestions(message, snapshot),
    [message, snapshot],
  );
  const slashCommandFeedback = useMemo(
    () => webSlashCommandFeedback(message, snapshot, imageAttachments.length),
    [imageAttachments.length, message, snapshot],
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
  }, [message, updateMessageTextareaHeight]);

  useEffect(() => {
    window.addEventListener("resize", updateMessageTextareaHeight);
    return () =>
      window.removeEventListener("resize", updateMessageTextareaHeight);
  }, [updateMessageTextareaHeight]);

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
    const slashFeedback = isSlashCommand
      ? webSlashCommandFeedback(trimmed, snapshot, imageAttachments.length)
      : null;
    if (
      (!trimmed && imageAttachments.length === 0) ||
      isSending ||
      slashBodyMissing ||
      Boolean(slashFeedback?.blocksSubmit)
    ) {
      return;
    }

    if (isSlashCommand) {
      const panel = webSlashPanelForInput(trimmed, snapshot);
      if (panel) {
        setSlashPanel(panel);
        setSlashActionFeedback(null);
        setMessage("");
        setSlashCommandSelection(0);
        window.requestAnimationFrame(() => {
          updateMessageTextareaHeight();
          textareaRef.current?.focus();
        });
        return;
      }
      const action = webSlashActionForInput(trimmed, snapshot);
      if (action) {
        await runSlashDashboardAction(action, trimmed);
        return;
      }
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
        setSlashActionFeedback(webSlashActionFeedbackForResponse(trimmed, output));
      }
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
    setSlashActionFeedback(null);
    window.requestAnimationFrame(() => {
      updateMessageTextareaHeight();
      textareaRef.current?.focus();
    });
  }

  async function runSlashAction(command: string, detailTitle?: string) {
    if (!detailTitle) {
      await submitComposerInput(command);
      return;
    }

    setIsSending(true);
    setSendError(null);
    setSlashActionFeedback(null);
    try {
      const output = await runDashboardCommand(command, {
        attachments: [],
        sessionId,
      });
      setSlashPanel({
        kind: "detail",
        title: detailTitle,
        text: fallbackOutput(output),
      });
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsSending(false);
    }
  }

  async function runSlashDashboardAction(
    action: DashboardAction,
    commandLabel?: string,
  ) {
    setIsSending(true);
    setSendError(null);
    setSlashActionFeedback(null);
    try {
      const result = await runDashboardAction(action, { sessionId });
      setMessage("");
      setImageAttachments((current) => {
        for (const attachment of current) {
          revokeImagePreviewUrl(attachment);
        }
        return [];
      });
      setSlashCommandSelection(0);
      setSlashActionFeedback(
        webSlashActionFeedbackForResult(commandLabel ?? action.kind, result),
      );
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsSending(false);
    }
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
        "fixed inset-x-4 bottom-4 z-30 rounded-xl border bg-background/92 p-2 shadow-xl shadow-background/30 backdrop-blur-xl transition-all duration-300 md:right-auto md:left-[calc(18rem+(100vw-18rem)/2)] md:w-[min(56rem,calc(100vw-18rem-2rem))] md:-translate-x-1/2",
        isDraggingImage && "border-primary/70 ring-4 ring-primary/15",
        "border-border/70 focus-within:border-primary/45 focus-within:ring-4 focus-within:ring-primary/10 hover:border-primary/30",
      )}
    >
      <WebSlashCommandPanel
        panel={slashPanel}
        snapshot={snapshot}
        feedback={slashCommandFeedback}
        actionFeedback={slashActionFeedback}
        suggestions={slashCommandSuggestions}
        selectedSuggestionIndex={slashCommandSelection}
        isSending={isSending}
        onClosePanel={() => setSlashPanel(null)}
        onSetPanel={setSlashPanel}
        onCloseActionFeedback={() => setSlashActionFeedback(null)}
        onSelectSuggestion={applySlashSuggestion}
        onHoverSuggestion={setSlashCommandSelection}
        onRunAction={(command) => void runSlashAction(command)}
        onRunDashboardAction={(action) => void runSlashDashboardAction(action)}
      />
      <AgentComposerStatusLine
        statusLabel={agentStatusLabel}
        runtimeActive={Boolean(snapshot?.runtime_activity?.active_runtime_turn)}
        summaryText={summaryText}
        footerContext={snapshot?.footer_context}
      />
      {supportsVision ? (
        <Input
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
      <Separator className="mb-2" />
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
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Remove ${attachment.file.name || "image"}`}
                onClick={() => removeImageAttachment(attachment.id)}
                className="absolute right-1 top-1 rounded-full bg-background/90 text-muted-foreground opacity-90 shadow-sm hover:text-foreground group-hover:opacity-100"
              >
                <XIcon data-icon="inline-start" aria-hidden="true" />
              </Button>
            </div>
          ))}
        </div>
      ) : null}
      <InputGroup className="h-auto min-h-11 items-end border-0 bg-transparent shadow-none has-[[data-slot=input-group-control]:focus-visible]:border-transparent has-[[data-slot=input-group-control]:focus-visible]:ring-0 dark:bg-transparent">
        <InputGroupTextarea
          ref={textareaRef}
          value={message}
          rows={1}
          placeholder={chatPlaceholder}
          aria-label="Message"
          onChange={(event) => {
            setMessage(event.target.value);
            setSendError(null);
            setSlashCommandSelection(0);
            setSlashActionFeedback(null);
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
          className="max-h-[30vh] min-h-11 overflow-y-hidden px-3 py-2.5 text-sm leading-5 placeholder:text-muted-foreground/70"
        />
        <InputGroupAddon
          align="inline-end"
          className="self-end gap-1 pb-1.5 pr-1.5 has-[>button]:mr-0"
        >
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
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
            <ImagePlusIcon data-icon="inline-start" aria-hidden="true" />
          </Button>
          <Button
            type="submit"
            size="icon-sm"
            disabled={
              (!message.trim() && imageAttachments.length === 0) ||
              isSending ||
              slashCommandBlocksSubmit
            }
            aria-label="Send message"
            className="rounded-full"
          >
            {isSending ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <SendHorizontalIcon data-icon="inline-start" aria-hidden="true" />
            )}
          </Button>
        </InputGroupAddon>
      </InputGroup>
      {sendError ? (
        <Alert variant="destructive" className="mx-2 px-2 py-1">
          <AlertDescription className="text-xs">{sendError}</AlertDescription>
        </Alert>
      ) : null}
    </form>
  );
}

function AgentComposerStatusLine({
  statusLabel,
  runtimeActive,
  summaryText,
  footerContext,
}: {
  statusLabel: string;
  runtimeActive: boolean;
  summaryText: string;
  footerContext?: string;
}) {
  const detail = summaryText || footerContext?.trim() || "Enter send";

  return (
    <div className="flex min-w-0 items-center gap-2 px-2 pb-2 text-xs text-muted-foreground">
      <span
        aria-hidden="true"
        className={cn(
          "size-1.5 rounded-full",
          runtimeActive ? "bg-primary" : "bg-muted-foreground/45",
        )}
      />
      <span className="shrink-0 font-medium text-foreground">
        {runtimeActive ? "Working" : statusLabel}
      </span>
      <span className="min-w-0 truncate">{detail}</span>
    </div>
  );
}

function WebSlashCommandPanel({
  panel,
  snapshot,
  feedback,
  actionFeedback,
  suggestions,
  selectedSuggestionIndex,
  isSending,
  onClosePanel,
  onSetPanel,
  onCloseActionFeedback,
  onSelectSuggestion,
  onHoverSuggestion,
  onRunAction,
  onRunDashboardAction,
}: {
  panel: WebSlashPanel | null;
  snapshot: DashboardSnapshot | null;
  feedback: WebSlashCommandFeedback | null;
  actionFeedback: WebSlashActionFeedback | null;
  suggestions: WebSlashCommandSuggestion[];
  selectedSuggestionIndex: number;
  isSending: boolean;
  onClosePanel: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
  onCloseActionFeedback: () => void;
  onSelectSuggestion: (suggestion: WebSlashCommandSuggestion) => void;
  onHoverSuggestion: (index: number) => void;
  onRunAction: (command: string, detailTitle?: string) => void;
  onRunDashboardAction: (action: DashboardAction) => void;
}) {
  const hasContent =
    Boolean(panel) ||
    Boolean(actionFeedback) ||
    Boolean(feedback) ||
    suggestions.length > 0;

  if (!hasContent) {
    return null;
  }

  return (
    <div className="mb-2 flex flex-col gap-2 border-b border-border/70 px-2 pb-2">
      {panel ? (
        <WebSlashPanelView
          panel={panel}
          snapshot={snapshot}
          isSending={isSending}
          onClose={onClosePanel}
          onSetPanel={onSetPanel}
          onRunAction={onRunAction}
          onRunDashboardAction={onRunDashboardAction}
        />
      ) : null}

      {actionFeedback ? (
        <WebSlashCommandFeedbackView
          feedback={actionFeedback}
          onClose={actionFeedback.dismissible ? onCloseActionFeedback : undefined}
        />
      ) : null}

      {!panel && feedback ? (
        <WebSlashCommandFeedbackView feedback={feedback} />
      ) : null}

      {!panel && suggestions.length > 0 ? (
        <section
          aria-label="Command suggestions"
          className="flex flex-col gap-1"
        >
          {suggestions.slice(0, 6).map((suggestion, index) => {
            const selected = index === selectedSuggestionIndex;
            return (
              <Button
                key={`${suggestion.completion}-${index}`}
                type="button"
                variant="ghost"
                onMouseEnter={() => onHoverSuggestion(index)}
                onMouseDown={(event) => {
                  event.preventDefault();
                  onSelectSuggestion(suggestion);
                }}
                className={cn(
                  "h-auto w-full min-w-0 justify-start gap-3 px-2 py-1.5 text-left text-sm",
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
              </Button>
            );
          })}
        </section>
      ) : null}
    </div>
  );
}

function WebSlashPanelView({
  panel,
  snapshot,
  isSending,
  onClose,
  onSetPanel,
  onRunAction,
  onRunDashboardAction,
}: {
  panel: WebSlashPanel;
  snapshot: DashboardSnapshot | null;
  isSending: boolean;
  onClose: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
  onRunAction: (command: string, detailTitle?: string) => void;
  onRunDashboardAction: (action: DashboardAction) => void;
}) {
  switch (panel.kind) {
    case "selection":
      return (
        <WebSlashSelectionPanel
          panel={panel.panel}
          snapshot={snapshot}
          isSending={isSending}
          onClose={onClose}
          onSetPanel={onSetPanel}
          onRunAction={onRunAction}
          onRunDashboardAction={onRunDashboardAction}
        />
      );
    case "status":
      return <WebSlashStatusPanel snapshot={snapshot} onClose={onClose} />;
    case "sleep-status":
      return <WebSlashSleepStatusPanel snapshot={snapshot} onClose={onClose} />;
    case "telegram-status":
      return <WebSlashTelegramStatusPanel snapshot={snapshot} onClose={onClose} />;
    case "detail":
      return (
        <WebSlashDetailPanel
          title={panel.title}
          text={panel.text}
          onClose={onClose}
        />
      );
    case "skills-list":
      return (
        <WebSlashSkillsListPanel
          panel={panel}
          snapshot={snapshot}
          onClose={onClose}
          onSetPanel={onSetPanel}
        />
      );
    case "skills-toggle":
      return (
        <WebSlashSkillsTogglePanel
          panel={panel}
          snapshot={snapshot}
          isSending={isSending}
          onClose={onClose}
          onSetPanel={onSetPanel}
          onRunDashboardAction={onRunDashboardAction}
        />
      );
    case "telegram-access":
      return (
        <WebSlashTelegramAccessPanel
          panel={panel}
          snapshot={snapshot}
          isSending={isSending}
          onClose={onClose}
          onRunDashboardAction={onRunDashboardAction}
        />
      );
  }
}

function WebSlashPanelShell({
  title,
  subtitle,
  children,
  onClose,
}: {
  title: string;
  subtitle?: string;
  children: ReactNode;
  onClose: () => void;
}) {
  return (
    <section aria-label={title} className="flex flex-col gap-2 text-sm">
      <div className="flex min-w-0 items-start gap-2">
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium text-foreground">{title}</p>
          {subtitle ? (
            <p className="truncate text-xs text-muted-foreground">{subtitle}</p>
          ) : null}
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label={`Close ${title}`}
          onClick={onClose}
          className="shrink-0 rounded-full text-muted-foreground hover:text-foreground"
        >
          <XIcon data-icon="inline-start" aria-hidden="true" />
        </Button>
      </div>
      {children}
    </section>
  );
}

function WebSlashSelectionPanel({
  panel,
  snapshot,
  isSending,
  onClose,
  onSetPanel,
  onRunAction,
  onRunDashboardAction,
}: {
  panel: Extract<WebSlashPanel, { kind: "selection" }>["panel"];
  snapshot: DashboardSnapshot | null;
  isSending: boolean;
  onClose: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
  onRunAction: (command: string, detailTitle?: string) => void;
  onRunDashboardAction: (action: DashboardAction) => void;
}) {
  const meta = webSlashSelectionMeta(panel, snapshot);
  const items = webSlashSelectionItems(panel, snapshot);

  return (
    <WebSlashPanelShell
      title={meta.title}
      subtitle={meta.subtitle}
      onClose={onClose}
    >
      <div className="flex max-h-64 flex-col gap-1 overflow-auto">
        {items.length > 0 ? (
          items.map((item, index) => (
            <Button
              key={item.id}
              type="button"
              variant="ghost"
              disabled={isSending || item.disabled}
              onClick={() =>
                webSlashRunSelectionItem(
                  item.id,
                  snapshot,
                  onSetPanel,
                  onRunAction,
                  onRunDashboardAction,
                )
              }
              className={cn(
                "group h-auto w-full min-w-0 justify-start gap-2 px-2 py-1.5 text-left text-sm disabled:cursor-not-allowed disabled:opacity-45",
                index === 0 && !item.disabled && "bg-muted text-foreground",
              )}
            >
              <span
                aria-hidden="true"
                className={cn(
                  "w-3 shrink-0 text-primary opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100",
                  index === 0 && !item.disabled && "opacity-100",
                )}
              >
                ›
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium text-foreground">
                  {item.name}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {item.description}
                </span>
              </span>
            </Button>
          ))
        ) : (
          <p className="px-2 py-3 text-sm text-muted-foreground">No items.</p>
        )}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashStatusPanel({
  snapshot,
  onClose,
}: {
  snapshot: DashboardSnapshot | null;
  onClose: () => void;
}) {
  const rows = webSlashStatusRows(snapshot);

  return (
    <WebSlashPanelShell
      title="STATUS"
      subtitle="Current session runtime facts."
      onClose={onClose}
    >
      <WebSlashKeyValueRows rows={rows} />
    </WebSlashPanelShell>
  );
}

function WebSlashSleepStatusPanel({
  snapshot,
  onClose,
}: {
  snapshot: DashboardSnapshot | null;
  onClose: () => void;
}) {
  return (
    <WebSlashPanelShell
      title="SLEEP STATUS"
      subtitle="Background optimization state."
      onClose={onClose}
    >
      <div className="flex max-h-72 flex-col gap-3 overflow-auto">
        <div className="flex flex-col gap-1">
          <p className="text-xs font-medium text-foreground">
            Runtime optimization
          </p>
          <WebSlashKeyValueRows rows={webSlashRuntimeOptimizationRows(snapshot)} />
        </div>
        <div className="flex flex-col gap-1">
          <p className="text-xs font-medium text-foreground">
            Primitive optimization
          </p>
          <WebSlashKeyValueRows rows={webSlashPrimitiveOptimizationRows(snapshot)} />
        </div>
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashTelegramStatusPanel({
  snapshot,
  onClose,
}: {
  snapshot: DashboardSnapshot | null;
  onClose: () => void;
}) {
  const requests = snapshot?.pending_access_requests ?? [];

  return (
    <WebSlashPanelShell
      title="TELEGRAM STATUS"
      subtitle="Transport access request state."
      onClose={onClose}
    >
      <div className="flex max-h-72 flex-col gap-2 overflow-auto">
        <WebSlashKeyValueRows rows={[["Pending requests", requests.length]]} />
        {requests.length > 0 ? (
          <div className="flex flex-col gap-1">
            {requests.map((request) => (
              <div
                key={request.chat_id}
                className="flex min-w-0 gap-2 px-2 py-1 text-sm"
              >
                <span className="shrink-0 font-mono text-xs text-muted-foreground">
                  {request.chat_id}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium text-foreground">
                    {request.title || request.sender}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {request.sender}  {request.last_message_preview}
                  </span>
                </span>
              </div>
            ))}
          </div>
        ) : (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            No pending Telegram access requests.
          </p>
        )}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashDetailPanel({
  title,
  text,
  onClose,
}: {
  title: string;
  text: string;
  onClose: () => void;
}) {
  return (
    <WebSlashPanelShell title={title} onClose={onClose}>
      <div className="max-h-72 overflow-auto rounded-md bg-muted/35 p-2 font-mono text-xs leading-5 text-foreground/90">
        {renderWebSlashDetailLines(text).map((line, index) => (
          <div
            key={`${index}-${line.text}`}
            className={cn(
              "min-h-5 whitespace-pre-wrap break-words",
              line.kind === "header" && "font-semibold text-foreground",
              line.kind === "bullet" && "pl-2 text-muted-foreground",
              line.kind === "label" && "text-muted-foreground",
            )}
          >
            {line.text}
          </div>
        ))}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashSkillsListPanel({
  panel,
  snapshot,
  onClose,
  onSetPanel,
}: {
  panel: Extract<WebSlashPanel, { kind: "skills-list" }>;
  snapshot: DashboardSnapshot | null;
  onClose: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
}) {
  const skills = webSlashFilteredSkills(snapshot, panel.search);
  const errors = snapshot?.skill_errors ?? [];

  return (
    <WebSlashPanelShell
      title="Skills"
      subtitle={
        (snapshot?.skills ?? []).length > 0
          ? `${snapshot?.skills?.length ?? 0} loaded. Choose a skill to inspect.`
          : "No skills loaded."
      }
      onClose={onClose}
    >
      <Input
        value={panel.search}
        onChange={(event) =>
          onSetPanel({ kind: "skills-list", search: event.target.value })
        }
        placeholder="Type to search skills"
        className="h-8"
      />
      {errors.length > 0 ? (
        <div className="flex flex-col gap-1 text-xs">
          {errors.slice(0, 2).map((error) => (
            <div
              key={`${error.path}-${error.message}`}
              className="flex min-w-0 gap-2 text-muted-foreground"
            >
              <span className="shrink-0 text-primary">!</span>
              <span className="min-w-0 truncate">{error.path}</span>
              <span className="min-w-0 flex-1 truncate text-muted-foreground/75">
                {error.message}
              </span>
            </div>
          ))}
        </div>
      ) : null}
      <div className="flex max-h-64 flex-col gap-1 overflow-auto">
        {skills.length > 0 ? (
          skills.map((skill, index) => (
            <Button
              key={skill.path}
              type="button"
              variant="ghost"
              onClick={() =>
                onSetPanel({
                  kind: "detail",
                  title: `SKILL ${skill.name}`,
                  text: webSlashSkillDetailText(skill),
                })
              }
              className={cn(
                "group h-auto w-full min-w-0 justify-start gap-2 px-2 py-1.5 text-left text-sm",
                index === 0 && "bg-muted text-foreground",
              )}
            >
              <span
                aria-hidden="true"
                className={cn(
                  "w-3 shrink-0 text-primary opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100",
                  index === 0 && "opacity-100",
                )}
              >
                ›
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium text-foreground">
                  {skill.name}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {webSlashSkillStatusDescription(skill)}  {skill.description}
                </span>
              </span>
            </Button>
          ))
        ) : (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            {panel.search.trim() ? "No matches." : "No skills loaded."}
          </p>
        )}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashSkillsTogglePanel({
  panel,
  snapshot,
  isSending,
  onClose,
  onSetPanel,
  onRunDashboardAction,
}: {
  panel: Extract<WebSlashPanel, { kind: "skills-toggle" }>;
  snapshot: DashboardSnapshot | null;
  isSending: boolean;
  onClose: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
  onRunDashboardAction: (action: DashboardAction) => void;
}) {
  const skills = webSlashFilteredSkills(snapshot, panel.search);

  return (
    <WebSlashPanelShell
      title="Skills"
      subtitle={
        (snapshot?.skills ?? []).length > 0
          ? "Toggle automatic use for loaded skills."
          : "No skills loaded."
      }
      onClose={onClose}
    >
      <Input
        value={panel.search}
        onChange={(event) =>
          onSetPanel({
            kind: "skills-toggle",
            search: event.target.value,
            feedback: panel.feedback ?? null,
          })
        }
        placeholder="Type to search skills"
        className="h-8"
      />
      {panel.feedback ? (
        <WebSlashCommandFeedbackView feedback={panel.feedback} />
      ) : null}
      <div className="flex max-h-64 flex-col gap-1 overflow-auto">
        {skills.length > 0 ? (
          skills.map((skill, index) => (
            <Button
              key={skill.path}
                type="button"
                variant="ghost"
                disabled={isSending}
                onClick={() =>
                  onRunDashboardAction({
                    kind: "set_skill_auto_use",
                    path: skill.path,
                    enabled: !skill.auto_use_enabled,
                  })
                }
                className={cn(
                  "group h-auto w-full min-w-0 justify-start gap-2 px-2 py-1.5 text-left text-sm disabled:cursor-not-allowed disabled:opacity-60",
                  index === 0 && "bg-muted text-foreground",
                )}
              >
                <span
                  aria-hidden="true"
                  className={cn(
                    "w-3 shrink-0 text-primary opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100",
                    index === 0 && "opacity-100",
                  )}
                >
                  ›
                </span>
                <span className="shrink-0 font-mono text-xs text-muted-foreground">
                  {skill.auto_use_enabled ? "[x]" : "[ ]"}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium text-foreground">
                    {skill.name}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {skill.scope} - {webSlashSkillStatusDescription(skill)}
                  </span>
                </span>
            </Button>
          ))
        ) : (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            {panel.search.trim() ? "No matches." : "No skills loaded."}
          </p>
        )}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashTelegramAccessPanel({
  panel,
  snapshot,
  isSending,
  onClose,
  onRunDashboardAction,
}: {
  panel: Extract<WebSlashPanel, { kind: "telegram-access" }>;
  snapshot: DashboardSnapshot | null;
  isSending: boolean;
  onClose: () => void;
  onRunDashboardAction: (action: DashboardAction) => void;
}) {
  const requests = snapshot?.pending_access_requests ?? [];

  return (
    <WebSlashPanelShell
      title={panel.action === "approve" ? "TELEGRAM APPROVE" : "TELEGRAM REJECT"}
      subtitle={`Select a pending request to ${panel.action}.`}
      onClose={onClose}
    >
      <div className="flex max-h-64 flex-col gap-1 overflow-auto">
        {requests.length > 0 ? (
          requests.map((request, index) => (
            <Button
              key={`${panel.action}-${request.chat_id}`}
              type="button"
              variant="ghost"
              disabled={isSending}
              onClick={() =>
                onRunDashboardAction({
                  kind:
                    panel.action === "approve"
                      ? "approve_telegram_access"
                      : "reject_telegram_access",
                  chat_id: request.chat_id,
                })
              }
              className={cn(
                "group h-auto w-full min-w-0 justify-start gap-2 px-2 py-1.5 text-left text-sm disabled:cursor-not-allowed disabled:opacity-60",
                index === 0 && "bg-muted text-foreground",
              )}
            >
              <span
                aria-hidden="true"
                className={cn(
                  "w-3 shrink-0 text-primary opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100",
                  index === 0 && "opacity-100",
                )}
              >
                ›
              </span>
              <span className="shrink-0 font-mono text-xs text-muted-foreground">
                {request.chat_id}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium text-foreground">
                  {request.title || request.sender}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {request.sender}  {request.last_message_preview}
                </span>
              </span>
            </Button>
          ))
        ) : (
          <p className="px-2 py-3 text-sm text-muted-foreground">
            No pending Telegram access requests.
          </p>
        )}
      </div>
    </WebSlashPanelShell>
  );
}

function WebSlashKeyValueRows({
  rows,
}: {
  rows: Array<[string, string | number | null | undefined]>;
}) {
  return (
    <div className="grid max-h-72 grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1 overflow-auto text-sm">
      {rows.map(([label, value]) => (
        <Fragment key={label}>
          <span className="truncate font-medium text-foreground">{label}</span>
          <span className="min-w-0 truncate text-muted-foreground">
            {formatWebSlashValue(value)}
          </span>
        </Fragment>
      ))}
    </div>
  );
}

function WebSlashCommandFeedbackView({
  feedback,
  onClose,
}: {
  feedback: WebSlashCommandFeedback;
  onClose?: () => void;
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
      {onClose ? (
        <Button
          type="button"
          variant="ghost"
          size="icon"
          aria-label="Dismiss command feedback"
          onClick={onClose}
          className="shrink-0 rounded-full text-muted-foreground hover:text-foreground"
        >
          <XIcon data-icon="inline-start" aria-hidden="true" />
        </Button>
      ) : null}
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
        className="mt-0.5 size-4 shrink-0 text-muted-foreground"
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
  _snapshot: DashboardSnapshot | null,
): WebSlashCommandSuggestion[] {
  const parsed = parseWebSlashCommand(input);
  if (!parsed) {
    return [];
  }
  if (!parsed.trimmed) {
    return WEB_SLASH_COMMANDS.map(webSlashRootSuggestion);
  }

  const [verb] = parsed.parts;
  if (parsed.parts.length > 1 || parsed.body.endsWith(" ")) {
    return [];
  }
  return WEB_SLASH_COMMANDS.filter((candidate) =>
    candidate.name.startsWith(verb),
  ).map(webSlashRootSuggestion);
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

  const extraArgumentFeedback = webSlashExtraArgumentFeedback(parsed.parts);
  if (extraArgumentFeedback) {
    return extraArgumentFeedback;
  }

  if (verb === "debug" && parsed.parts.length > 1) {
    const view = parsed.parts[1];
    if (!["persona", "system-prompt", "system_prompt", "context", "preturn-context", "preturn_context"].includes(view)) {
      return {
        title: "DEBUG",
        message: `Unknown debug view '${view}'.`,
        detail: "Use /debug to choose a view.",
        level: "error",
        blocksSubmit: true,
      };
    }
  }

  if (
    webSlashCommandAccepts("app-status", verb) &&
    parsed.parts.length === 2 &&
    !webSlashAppNames(snapshot).includes(parsed.parts[1].toLowerCase())
  ) {
    return {
      title: "APP STATUS",
      message: `Unknown app '${parsed.parts[1]}'.`,
      detail: webSlashAvailableAppsText(snapshot),
      level: "error",
      blocksSubmit: true,
    };
  }

  if (verb === "skills") {
    const action = parsed.parts[1];
    if (["show", "enable", "disable"].includes(action) && parsed.parts.length === 3) {
      const target = parsed.parts[2];
      if (!webSlashResolveSkillTarget(snapshot, target)) {
        return {
          title: "SKILLS",
          message: `Unknown skill '${target}'.`,
          detail: "Use /skills to browse loaded skills.",
          level: "error",
          blocksSubmit: true,
        };
      }
    } else if (action && !["list", "show", "enable", "disable", "reload"].includes(action)) {
      return {
        title: "SKILLS",
        message: `Unknown skills action '${action}'.`,
        detail: "Use /skills to choose an action.",
        level: "error",
        blocksSubmit: true,
      };
    }
  }

  if (verb === "sleep") {
    const action = parsed.parts[1];
    if (action && !["run", "status"].includes(action)) {
      return {
        title: "SLEEP",
        message: `Unknown sleep action '${action}'.`,
        detail: "Use /sleep to choose an action.",
        level: "error",
        blocksSubmit: true,
      };
    }
  }

  if (verb === "telegram") {
    const action = parsed.parts[1];
    if (action && !["status", "approve", "reject"].includes(action)) {
      return {
        title: "TELEGRAM",
        message: `Unknown Telegram action '${action}'.`,
        detail: "Use /telegram to choose an action.",
        level: "error",
        blocksSubmit: true,
      };
    }
    if (
      ["approve", "reject"].includes(action) &&
      parsed.parts.length === 3 &&
      !/^-?\d+$/.test(parsed.parts[2])
    ) {
      return {
        title: "TELEGRAM",
        message: `Invalid chat_id '${parsed.parts[2]}'.`,
        level: "error",
        blocksSubmit: true,
      };
    }
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
      message: `debug ${parts[1]} does not take extra arguments.`,
      detail: `usage: /debug ${parts[1]}`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (parts[0] === "sleep" && parts.length > 2) {
    return {
      title: "SLEEP",
      message: `sleep ${parts[1]} does not take extra arguments.`,
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
    parts[0] === "skills" &&
    (parts[1] === "list" || parts[1] === "reload") &&
    parts.length > 2
  ) {
    return {
      title: "SKILLS",
      message: `skills ${parts[1]} does not take extra arguments.`,
      detail: `usage: /skills ${parts[1]}`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (
    parts[0] === "skills" &&
    (parts[1] === "show" || parts[1] === "enable" || parts[1] === "disable") &&
    parts.length === 2
  ) {
    return {
      title: "SKILLS",
      message: `skills ${parts[1]} needs a skill name.`,
      detail: `usage: /skills ${parts[1]} <skill>`,
      level: "warning",
      blocksSubmit: true,
    };
  }
  if (
    parts[0] === "skills" &&
    (parts[1] === "show" || parts[1] === "enable" || parts[1] === "disable") &&
    parts.length > 3
  ) {
    return {
      title: "SKILLS",
      message: `skills ${parts[1]} accepts exactly one skill name.`,
      detail: `usage: /skills ${parts[1]} <skill>`,
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
      message: `telegram ${parts[1]} accepts at most one chat_id.`,
      detail: `usage: /telegram ${parts[1]} [chat_id]`,
      level: "error",
      blocksSubmit: true,
    };
  }
  if (webSlashCommandAccepts("app-status", parts[0]) && parts.length > 2) {
    return {
      title: "APP STATUS",
      message: "app-status accepts exactly one app name.",
      detail: "usage: /app-status <app>",
      level: "error",
      blocksSubmit: true,
    };
  }

  return null;
}

function webSlashPanelForInput(
  input: string,
  snapshot: DashboardSnapshot | null,
): WebSlashPanel | null {
  const parsed = parseWebSlashCommand(input);
  if (!parsed) {
    return null;
  }
  const parts = parsed.parts;
  const [verb] = parts;
  if (parts.length === 1) {
    if (verb === "status") {
      return { kind: "status" };
    }
    if (["debug", "sleep", "telegram", "skills"].includes(verb)) {
      return { kind: "selection", panel: verb as Extract<WebSlashPanel, { kind: "selection" }>["panel"] };
    }
    if (webSlashCommandAccepts("app-status", verb)) {
      const apps = webSlashAppNames(snapshot);
      if (apps.length === 0) {
        return {
          kind: "detail",
          title: "APP STATUS",
          text: "No app state is currently available.",
        };
      }
      return { kind: "selection", panel: "app-status" };
    }
    return null;
  }
  if (verb === "debug") {
    if (parts[1] === "system-prompt" || parts[1] === "system_prompt") {
      return {
        kind: "detail",
        title: "DEBUG SYSTEM PROMPT",
        text: fallbackOutput(snapshot?.system_prompt_output),
      };
    }
    if (
      parts[1] === "context" ||
      parts[1] === "preturn-context" ||
      parts[1] === "preturn_context"
    ) {
      return {
        kind: "detail",
        title: "DEBUG CONTEXT",
        text: fallbackOutput(snapshot?.preturn_context_output),
      };
    }
  }
  if (verb === "sleep" && parts[1] === "status") {
    return { kind: "sleep-status" };
  }
  if (verb === "telegram") {
    if (parts[1] === "status") {
      return { kind: "telegram-status" };
    }
    if (parts[1] === "approve" || parts[1] === "reject") {
      return { kind: "telegram-access", action: parts[1] };
    }
  }
  if (webSlashCommandAccepts("app-status", verb) && parts.length === 2) {
    const target = parts[1].toLowerCase();
    const app = (snapshot?.app_status_outputs ?? []).find(([name]) => name === target);
    if (app) {
      return {
        kind: "detail",
        title: `APP STATUS ${target.toUpperCase()}`,
        text: fallbackOutput(app[1]),
      };
    }
  }
  if (verb === "skills") {
    if (parts[1] === "list" || (parts[1] === "show" && parts.length === 2)) {
      return { kind: "skills-list", search: "" };
    }
    if (parts[1] === "show" && parts.length === 3) {
      const skill = webSlashResolveSkillTarget(snapshot, parts[2]);
      if (skill) {
        return {
          kind: "detail",
          title: `SKILL ${skill.name}`,
          text: webSlashSkillDetailText(skill),
        };
      }
    }
  }
  return null;
}

function webSlashActionForInput(
  input: string,
  snapshot: DashboardSnapshot | null,
): DashboardAction | null {
  const parsed = parseWebSlashCommand(input);
  if (!parsed) {
    return null;
  }
  const parts = parsed.parts;
  if (parts.length === 1) {
    if (parts[0] === "clear") {
      return { kind: "clear_conversation" };
    }
    if (parts[0] === "restart") {
      return { kind: "restart_daemon" };
    }
    return null;
  }
  if (parts[0] === "sleep" && parts[1] === "run") {
    return { kind: "run_sleep" };
  }
  if (parts[0] === "skills" && parts[1] === "reload") {
    return { kind: "reload_skills" };
  }
  if (
    parts[0] === "skills" &&
    (parts[1] === "enable" || parts[1] === "disable") &&
    parts.length === 3
  ) {
    const skill = webSlashResolveSkillTarget(snapshot, parts[2]);
    if (!skill) {
      return null;
    }
    return {
      kind: "set_skill_auto_use",
      path: skill.path,
      enabled: parts[1] === "enable",
    };
  }
  if (
    parts[0] === "telegram" &&
    (parts[1] === "approve" || parts[1] === "reject") &&
    parts.length === 3
  ) {
    const chatId = Number(parts[2]);
    if (!Number.isSafeInteger(chatId)) {
      return null;
    }
    return {
      kind:
        parts[1] === "approve"
          ? "approve_telegram_access"
          : "reject_telegram_access",
      chat_id: chatId,
    };
  }
  return null;
}

function webSlashActionFeedbackForResponse(
  input: string,
  output: string,
): WebSlashActionFeedback | null {
  if (webSlashIsClearCommand(input)) {
    return null;
  }
  const message = webSlashCompactMessage(output);
  if (!message && !output.trim()) {
    return null;
  }
  return {
    command: input.trim(),
    title: webSlashCommandTitle(input),
    message,
    detail: webSlashCommandDetail(output),
    level: "info",
    dismissible: true,
  };
}

function webSlashActionFeedbackForResult(
  commandLabel: string,
  result: DashboardActionResult,
): WebSlashActionFeedback | null {
  if (commandLabel.trim() === "/clear" && result.success) {
    return null;
  }
  return {
    command: commandLabel,
    title: webSlashCommandTitle(commandLabel),
    message: result.message,
    detail: result.detail ?? undefined,
    level: result.success ? "info" : "error",
    dismissible: true,
  };
}

function webSlashSelectionMeta(
  panel: Extract<WebSlashPanel, { kind: "selection" }>["panel"],
  snapshot: DashboardSnapshot | null,
) {
  if (panel === "skills") {
    const skills = snapshot?.skills ?? [];
    const autoCount = skills.filter((skill) => skill.auto_use_enabled).length;
    const manualCount = skills.length - autoCount;
    return {
      title: "Skills",
      subtitle: `${skills.length} loaded, ${autoCount} auto-use, ${manualCount} manual-only`,
    };
  }
  if (panel === "debug") {
    return {
      title: "Debug",
      subtitle: "Inspect internal runtime views.",
    };
  }
  if (panel === "sleep") {
    return {
      title: "Sleep",
      subtitle: "Inspect sleep state or start a background sleep run.",
    };
  }
  if (panel === "telegram") {
    return {
      title: "Telegram",
      subtitle: "Inspect transport state or handle access requests.",
    };
  }
  return {
    title: "App Status",
    subtitle: "Choose an app to inspect.",
  };
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

function webSlashCommandAccepts(primaryVerb: string, verb: string) {
  const command = WEB_SLASH_COMMANDS.find((candidate) => candidate.name === primaryVerb);
  return command?.name === verb || Boolean(command?.aliases?.includes(verb));
}

function webSlashRootSuggestion(
  command: WebSlashCommandDefinition,
): WebSlashCommandSuggestion {
  return {
    display: command.name,
    completion: `/${command.name}`,
    description: command.description,
  };
}

function webSlashSelectionItems(
  panel: Extract<WebSlashPanel, { kind: "selection" }>["panel"],
  snapshot: DashboardSnapshot | null,
): WebSlashSelectionItem[] {
  if (panel === "debug") {
    return [
      {
        id: "debug-persona",
        name: "Prompt persona",
        description: "show current prompt persona config",
      },
      {
        id: "debug-system-prompt",
        name: "System prompt",
        description: "show current runtime system prompt",
      },
      {
        id: "debug-context",
        name: "Runtime context",
        description: "show latest pre-turn runtime context",
      },
    ];
  }
  if (panel === "sleep") {
    return [
      {
        id: "sleep-status",
        name: "Status",
        description: "show sleep status",
      },
      {
        id: "sleep-run",
        name: "Start sleep run",
        description: "start a background sleep run",
      },
    ];
  }
  if (panel === "telegram") {
    const pending = snapshot?.pending_access_requests.length ?? 0;
    return [
      {
        id: "telegram-status",
        name: "Status",
        description: "show Telegram transport details",
      },
      {
        id: "telegram-approve",
        name: "Approve access request",
        description: `approve one of ${pending} pending requests`,
        disabled: pending === 0,
      },
      {
        id: "telegram-reject",
        name: "Reject access request",
        description: `reject one of ${pending} pending requests`,
        disabled: pending === 0,
      },
    ];
  }
  if (panel === "skills") {
    const skills = snapshot?.skills ?? [];
    return [
      {
        id: "skills-list",
        name: "List skills",
        description: "show loaded skills and load errors",
      },
      {
        id: "skills-toggle",
        name: "Enable/Disable Skills",
        description: "toggle whether skills may be selected automatically",
        disabled: skills.length === 0,
      },
    ];
  }
  return (snapshot?.app_status_outputs ?? []).map(([name, output]) => ({
    id: `app-status:${name}`,
    name,
    description: truncateText(
      output
        .split(/\r?\n/)
        .map((line) => line.trim())
        .find(Boolean) ?? "app state",
      120,
    ),
  }));
}

function webSlashRunSelectionItem(
  itemId: string,
  snapshot: DashboardSnapshot | null,
  onSetPanel: (panel: WebSlashPanel | null) => void,
  onRunAction: (command: string, detailTitle?: string) => void,
  onRunDashboardAction: (action: DashboardAction) => void,
) {
  if (itemId === "debug-persona") {
    onRunAction("/debug persona", "DEBUG PERSONA");
  } else if (itemId === "debug-system-prompt") {
    onSetPanel({
      kind: "detail",
      title: "DEBUG SYSTEM PROMPT",
      text: fallbackOutput(snapshot?.system_prompt_output),
    });
  } else if (itemId === "debug-context") {
    onSetPanel({
      kind: "detail",
      title: "DEBUG CONTEXT",
      text: fallbackOutput(snapshot?.preturn_context_output),
    });
  } else if (itemId === "sleep-status") {
    onSetPanel({ kind: "sleep-status" });
  } else if (itemId === "sleep-run") {
    onRunDashboardAction({ kind: "run_sleep" });
  } else if (itemId === "telegram-status") {
    onSetPanel({ kind: "telegram-status" });
  } else if (itemId === "telegram-approve") {
    onSetPanel({ kind: "telegram-access", action: "approve" });
  } else if (itemId === "telegram-reject") {
    onSetPanel({ kind: "telegram-access", action: "reject" });
  } else if (itemId === "skills-list") {
    onSetPanel({ kind: "skills-list", search: "" });
  } else if (itemId === "skills-toggle") {
    onSetPanel({ kind: "skills-toggle", search: "", feedback: null });
  } else if (itemId.startsWith("app-status:")) {
    const appName = itemId.slice("app-status:".length);
    const app = (snapshot?.app_status_outputs ?? []).find(([name]) => name === appName);
    onSetPanel({
      kind: "detail",
      title: `APP STATUS ${appName.toUpperCase()}`,
      text: fallbackOutput(app?.[1]),
    });
  }
}

function webSlashAppNames(snapshot: DashboardSnapshot | null) {
  return (snapshot?.app_status_outputs ?? [])
    .map(([name]) => name)
    .filter(Boolean)
    .sort();
}

function webSlashAvailableAppsText(snapshot: DashboardSnapshot | null) {
  const apps = webSlashAppNames(snapshot);
  return apps.length > 0
    ? `available: ${apps.join(", ")}`
    : "No app state is currently available.";
}

function webSlashStatusRows(
  snapshot: DashboardSnapshot | null,
): Array<[string, string | number | null | undefined]> {
  const runtimeActivity = snapshot?.runtime_activity;
  const tokenUsage = snapshot?.token_usage;
  const context = snapshot?.context_composition;
  const skills = snapshot?.skills ?? [];
  return [
    ["Runtime", snapshot?.runtime_status || runtimeActivity?.label || "Idle"],
    ["Active turn", runtimeActivity?.active_runtime_turn ? "yes" : "no"],
    ["Phase", runtimeActivity?.active_runtime_phase],
    ["Current plan", snapshot?.current_plan_step?.step],
    ["Last cycle", formatWebSlashDuration(snapshot?.last_cycle_elapsed_ms)],
    ["Input tokens", snapshot?.footer_estimated_input_tokens],
    ["Main model", tokenUsage?.main_model],
    ["Efficient model", tokenUsage?.efficient_model],
    ["Context model", context?.model],
    ["Context tokens", context?.total_estimated_tokens],
    ["Skills", `${skills.length} loaded`],
    ["Telegram pending", snapshot?.pending_access_requests.length ?? 0],
  ];
}

function webSlashRuntimeOptimizationRows(
  snapshot: DashboardSnapshot | null,
): Array<[string, string | number | null | undefined]> {
  const runtime = snapshot?.runtime_optimization;
  return [
    ["Running", runtime?.running ? "yes" : "no"],
    ["Trigger", runtime?.current_trigger],
    ["Last result", runtime?.last_result],
    ["Last completed", formatWebSlashTimestamp(runtime?.last_completed_at_ms)],
    ["Unread error backlog", runtime?.unread_runtime_error_backlog],
    ["Error cases consumed", runtime?.total_runtime_error_cases_consumed],
    ["Runtime cases", runtime?.total_runtime_error_cases],
    ["Reflections", runtime?.total_runtime_error_reflections],
    ["Contract candidates", runtime?.total_runtime_contract_candidates],
    ["Candidate evaluations", runtime?.total_runtime_contract_candidate_evaluations],
    ["System additions", runtime?.total_runtime_contract_system_additions],
    ["Contract updates", runtime?.total_runtime_contract_updates],
  ];
}

function webSlashPrimitiveOptimizationRows(
  snapshot: DashboardSnapshot | null,
): Array<[string, string | number | null | undefined]> {
  const primitive = snapshot?.primitive_optimization;
  return [
    ["Running", primitive?.running ? "yes" : "no"],
    ["Trigger", primitive?.current_trigger],
    ["Last result", primitive?.last_result],
    ["Last completed", formatWebSlashTimestamp(primitive?.last_completed_at_ms)],
    ["Evidence records", primitive?.primitive_evidence_records],
    ["Evidence run records", primitive?.total_primitive_evidence_run_records],
    ["Reflections", primitive?.total_primitive_reflections],
    ["Patch candidates", primitive?.total_primitive_patch_candidates],
    ["Merge candidates", primitive?.total_primitive_merge_candidates],
    ["Candidate evaluations", primitive?.total_primitive_candidate_evaluations],
    ["Frontier entries", primitive?.total_primitive_frontier_entries],
    ["Frontier root entries", primitive?.latest_primitive_frontier_root_entries],
    ["Frontier branched entries", primitive?.latest_primitive_frontier_branched_entries],
    ["Frontier max generation", primitive?.latest_primitive_frontier_max_generation],
    ["Patch applied", primitive?.total_primitive_patch_applied],
    ["Merge applied", primitive?.total_primitive_merge_applied],
    ["Rollbacks", primitive?.total_primitive_update_rollbacks],
    ["Optimization rounds", primitive?.total_primitive_optimization_rounds],
  ];
}

function webSlashFilteredSkills(
  snapshot: DashboardSnapshot | null,
  query: string,
) {
  const normalized = query.trim().toLowerCase();
  const skills = snapshot?.skills ?? [];
  if (!normalized) {
    return skills;
  }
  return skills.filter((skill) =>
    [skill.name, skill.description, skill.path, skill.scope].some((value) =>
      value.toLowerCase().includes(normalized),
    ),
  );
}

function webSlashResolveSkillTarget(
  snapshot: DashboardSnapshot | null,
  target: string,
) {
  const skills = snapshot?.skills ?? [];
  return (
    skills.find((skill) => skill.path === target) ??
    skills.find((skill) => skill.name === target) ??
    null
  );
}

function webSlashSkillDetailText(
  skill: NonNullable<DashboardSnapshot["skills"]>[number],
) {
  return [
    `Name: ${skill.name}`,
    `Status: ${webSlashSkillStatusDescription(skill)}`,
    `Scope: ${skill.scope}`,
    `Path: ${skill.path}`,
    `Description: ${skill.description}`,
  ].join("\n");
}

function webSlashSkillStatusDescription(
  skill: NonNullable<DashboardSnapshot["skills"]>[number],
) {
  if (skill.auto_use_enabled) {
    return "auto-use enabled";
  }
  if (skill.user_disabled) {
    return "manual-only: disabled by /skills";
  }
  if (!skill.allow_implicit_invocation) {
    return "manual-only: policy disallows implicit invocation";
  }
  return "manual-only";
}

function fallbackOutput(output: string | null | undefined) {
  return output?.trim() ? output : "no data";
}

function renderWebSlashDetailLines(text: string) {
  const lines = fallbackOutput(text).split(/\r?\n/);
  let previousBlank = true;
  return lines.map((rawLine) => {
    const line = rawLine.trimEnd();
    let kind: "blank" | "header" | "bullet" | "label" | "text" = "text";
    if (!line.trim()) {
      previousBlank = true;
      return { kind: "blank", text: "" };
    }
    if (previousBlank && line.length < 72 && !line.includes(":")) {
      kind = "header";
    } else if (line.startsWith("• ")) {
      kind = "bullet";
    } else if (line.includes(":")) {
      kind = "label";
    }
    previousBlank = false;
    return { kind, text: line };
  });
}

function formatWebSlashValue(value: string | number | null | undefined) {
  if (value === null || value === undefined || value === "") {
    return "none";
  }
  return String(value);
}

function formatWebSlashDuration(durationMs: number | null | undefined) {
  if (durationMs === null || durationMs === undefined) {
    return null;
  }
  if (durationMs < 1000) {
    return `${durationMs}ms`;
  }
  const totalSeconds = Math.floor(durationMs / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}

function formatWebSlashTimestamp(timestampMs: number | null | undefined) {
  if (timestampMs === null || timestampMs === undefined) {
    return null;
  }
  try {
    return new Date(timestampMs).toLocaleString();
  } catch {
    return String(timestampMs);
  }
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
  panelRef,
  composerHeight,
}: {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
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
  const displayItems = useMemo(
    () => foldCompletedAgentChatActivity(bubbles),
    [bubbles],
  );

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
    setShowScrollToBottom(distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX);
  }

  function handleScroll(event: UIEvent<HTMLDivElement>) {
    const panel = event.currentTarget;
    const distanceFromBottom =
      panel.scrollHeight - panel.clientHeight - panel.scrollTop;

    lastFocusedScrollTopRef.current = panel.scrollTop;
    hasFocusedScrollPositionRef.current = true;
    isFocusedNearBottomRef.current =
      distanceFromBottom <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX;

    setShowScrollToBottom(distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX);
    if (
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

    if (!hasFocusedScrollPositionRef.current) {
      lastFocusedScrollTopRef.current = panel.scrollHeight;
      hasFocusedScrollPositionRef.current = true;
      isFocusedNearBottomRef.current = true;
    }
    shouldRestoreFocusScrollRef.current = true;
  }, [panelRef]);

  useEffect(() => {
    const historyWindow = snapshot?.activity_history;
    setHistoryBubbles(agentChatCommittedBubblesFromSnapshot(snapshot));
    setOldestCursor(historyWindow?.oldest_cursor ?? null);
    setHasMoreBefore(Boolean(historyWindow?.has_more_before));
    setHistoryError(null);
    restoreAfterPrependRef.current = null;
  }, [sessionId, snapshot?.activity_history?.newest_cursor]);

  useEffect(() => {
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
  }, [historyBubbles.length, panelRef]);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) {
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
  }, [bubbles.length, panelRef]);

  useEffect(() => {
    const panel = panelRef.current;
    if (
      !panel ||
      !hasMoreBefore ||
      isLoadingHistory ||
      restoreAfterPrependRef.current
    ) {
      return;
    }

    if (panel.scrollTop <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX) {
      void loadOlderHistory();
    }
  }, [hasMoreBefore, isLoadingHistory, loadOlderHistory, panelRef]);

  return (
    <>
      <div
        ref={panelRef}
        aria-label="Agent activity"
        onScroll={handleScroll}
        style={{
          paddingBottom: composerHeight + AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX,
        }}
        className={cn(
          "relative z-10 min-h-0 w-full max-w-full flex-1 overflow-x-hidden overflow-y-auto text-left [scrollbar-gutter:stable] [scrollbar-width:thin]",
        )}
      >
        <div className="relative z-10 flex min-h-full w-full min-w-0 max-w-full flex-col justify-end">
          {displayItems.length > 0 ? (
            <div
              className={cn(
                "mx-auto flex w-full min-w-0 max-w-5xl flex-col gap-3 overflow-x-hidden px-2 py-4 sm:px-4 md:px-6",
              )}
            >
              {hasMoreBefore || isLoadingHistory || historyError ? (
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
                    <Alert variant="destructive" className="max-w-xl px-2 py-1">
                      <AlertDescription className="text-xs">
                        {historyError}
                      </AlertDescription>
                    </Alert>
                  ) : null}
                </div>
              ) : null}
              {displayItems.map((item) =>
                item.kind === "bubble" ? (
                  <AgentChatBubbleItem
                    key={item.id}
                    bubble={item.bubble}
                  />
                ) : (
                  <AgentChatFoldedActivityGroup
                    key={item.id}
                    id={item.id}
                    bubbles={item.bubbles}
                  />
                ),
              )}
            </div>
          ) : (
            <div className="mx-auto flex min-h-[40vh] w-full min-w-0 max-w-3xl items-center justify-center px-6 text-center">
              <Empty className="border border-dashed bg-card/60">
                <EmptyHeader>
                  <EmptyTitle>No activity yet</EmptyTitle>
                  <EmptyDescription>
                    Messages and tool activity will appear here as the session
                    starts working.
                  </EmptyDescription>
                </EmptyHeader>
              </Empty>
            </div>
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
        <ArrowDownIcon data-icon="inline-start" aria-hidden="true" />
      </Button>
    </>
  );
}

function AgentChatFoldedActivityGroup({
  id,
  bubbles,
  isFocused = true,
}: {
  id: string;
  bubbles: AgentChatBubble[];
  isFocused?: boolean;
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
        "w-full min-w-0 max-w-full text-sm leading-6 text-muted-foreground",
        !isFocused && "select-none",
      )}
    >
      <div className="min-w-0 max-w-full">
        <AgentChatWorkedDivider
          label={`Worked for ${workedDurationLabel}`}
          open={open}
          onToggle={isFocused ? toggle : undefined}
        />
        {isFocused && open ? (
          <div className="min-w-0 pt-2">
            {bubbles.map((bubble) => (
              <AgentChatBubbleItem
                key={`${id}-${bubble.id}`}
                bubble={bubble}
                isFocused={isFocused}
                compact
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
  isFocused = true,
  compact = false,
}: {
  bubble: AgentChatBubble;
  isFocused?: boolean;
  compact?: boolean;
}) {
  if (bubble.uiHint === "final-message-separator") {
    return <AgentWorkedSeparator bubble={bubble} compact={compact} />;
  }

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

  const content = (
    <div
      className={cn(
        "w-full min-w-0 max-w-full overflow-hidden [overflow-wrap:anywhere]",
        bubble.live || bubble.status === "running"
          ? "text-foreground"
          : "text-foreground/95",
        !isFocused && "select-none",
      )}
    >
      <div className="flex min-w-0 max-w-full flex-col gap-2 text-sm leading-6 text-foreground">
        {!isConversationMessage && !useCanonicalActivityCell ? (
          <AgentChatActivityHeader bubble={bubble} isFocused={isFocused} />
        ) : null}
        {activityCellRender ? (
          <AgentChatActivityCellView
            bubbleId={bubble.id}
            render={activityCellRender}
          />
        ) : (
          <div className="flex min-w-0 max-w-full flex-col gap-2 text-foreground/90">
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
    </div>
  );

  if (compact) {
    return <div className="min-w-0 max-w-full py-1.5">{content}</div>;
  }

  return (
    <article
      aria-label={bubble.title || "Activity"}
      className={cn(
        "w-full min-w-0 max-w-full overflow-x-clip border-l border-transparent px-1 py-1.5 [overflow-wrap:anywhere]",
        bubble.status === "failed" || bubble.kind === "error"
          ? "border-destructive/45"
          : "",
      )}
    >
      {content}
    </article>
  );
}

function AgentWorkedSeparator({
  bubble,
  compact,
}: {
  bubble: AgentChatBubble;
  compact: boolean;
}) {
  const label = normalizeAgentChatWorkedLabel(bubble.title.trim() || "Worked");

  return (
    <AgentChatWorkedDivider label={label} compact={compact} />
  );
}

function AgentChatWorkedDivider({
  label,
  compact = false,
  open,
  onToggle,
}: {
  label: string;
  compact?: boolean;
  open?: boolean;
  onToggle?: () => void;
}) {
  const interactive = Boolean(onToggle);
  const className = cn(
    "group flex w-full min-w-0 items-center gap-1 border-b border-border/70 px-2 text-left text-sm text-muted-foreground transition-colors",
    compact ? "h-8" : "h-10",
    interactive && "cursor-pointer hover:text-foreground",
  );
  const content = (
    <>
      <span className="min-w-0 shrink-0 truncate">{label}</span>
      {interactive ? (
        <ChevronRightIcon
          aria-hidden="true"
          className={cn(
            "size-4 shrink-0 transition-transform duration-150",
            open && "rotate-90",
          )}
        />
      ) : null}
    </>
  );

  if (interactive) {
    return (
      <button
        type="button"
        aria-expanded={open}
        onClick={onToggle}
        className={className}
      >
        {content}
      </button>
    );
  }

  return <div className={className}>{content}</div>;
}

function normalizeAgentChatWorkedLabel(label: string) {
  return label.replace(/^Worked For\b/, "Worked for");
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
  const icon = agentChatActivityIconForBubble(bubble);

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
          "mt-0.5 inline-flex size-5 shrink-0 items-center justify-center",
          agentChatActivityIconClass(bubble),
          !isFocused && "size-4",
        )}
      >
        {isRunning ? (
          <span className="font-mono text-sm font-semibold leading-none motion-safe:animate-pulse">
            •
          </span>
        ) : (
          <span
            className={cn(
              "font-mono text-sm font-semibold leading-none",
              !isFocused && "text-xs",
            )}
          >
            {icon === "error" ? "■" : "•"}
          </span>
        )}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <p
            className={cn(
              "min-w-0 break-words text-sm font-semibold leading-6 text-foreground [overflow-wrap:anywhere]",
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
                <Spinner className="size-2.5" />
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
    if (render.icon === "user") {
      return (
        <AgentChatPromptCell
          id={bubbleId}
          title={render.title}
          bodyLines={render.bodyLines}
          fullBody={render.fullBody}
          imageAttachments={render.imageAttachments}
          markdown={false}
        />
      );
    }

    return (
      <AgentChatActivityTextCell
        id={bubbleId}
        icon={render.icon}
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
        icon={render.icon}
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
        icon={render.icon}
        title={render.title}
        steps={render.steps}
      />
    ) : null;
  }

  if (render.kind === "primitive") {
    return (
      <AgentChatStatusLineCell
        icon={render.icon}
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
        icon={render.icon}
        title={render.title}
        outputLines={render.outputLines}
        exitCode={render.exitCode}
      />
    );
  }

  if (render.kind === "explored") {
    return (
      <AgentChatExploredActivityPanel
        icon={render.icon}
        title={render.title}
        calls={render.calls}
      />
    );
  }

  if (render.kind === "patch") {
    return (
      <AgentChatPatchActivityPanel
        icon={render.icon}
        title={render.title}
        files={render.files}
      />
    );
  }

  if (render.kind === "messageActivity") {
    return (
      <AgentChatMessageActivityLine
        id={bubbleId}
        icon={render.icon}
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
      icon={render.icon}
      title={render.title}
      messageLines={render.messageLines}
      disposition={render.disposition}
      subject={render.subject}
    />
  );
}

function AgentChatActivityMarker({
  icon,
  tone = "default",
  className,
}: {
  icon: AgentChatActivityMarkerKind;
  tone?: "default" | "error";
  className?: string;
}) {
  const marker = icon === "error" || tone === "error" ? "■" : "•";

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

function AgentChatPromptCell({
  id,
  title,
  bodyLines,
  fullBody,
  imageAttachments = [],
  markdown,
}: {
  id: string;
  title: string;
  bodyLines: string[];
  fullBody?: string | null;
  imageAttachments?: AgentChatImageAttachmentData[];
  markdown: boolean;
}) {
  const text = fullBody?.trimEnd() || [title, ...bodyLines].join("\n").trimEnd();
  const lines = text ? text.split("\n") : [];

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1 py-1 text-sm leading-6 text-foreground [overflow-wrap:anywhere]">
      {lines.length > 0 ? (
        <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-2 px-2 sm:px-3">
          <span
            aria-hidden="true"
            className="inline-flex h-6 w-3 shrink-0 items-center justify-start font-mono text-sm font-semibold leading-none text-muted-foreground"
          >
            ›
          </span>
          <div className="min-w-0">
            {markdown ? (
              <AgentChatMarkdownText
                text={text}
                limit={AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT}
              />
            ) : (
              lines.map((line, index) => (
                <p
                  key={`${id}-prompt-line-${index}`}
                  className="min-w-0 whitespace-pre-wrap break-words"
                >
                  {line || "\u00a0"}
                </p>
              ))
            )}
          </div>
        </div>
      ) : null}
      {imageAttachments.length > 0 ? (
        <div className="flex min-w-0 max-w-full flex-col gap-2 pl-7 pr-2 sm:pl-8 sm:pr-3">
          {imageAttachments.map((attachment, index) => (
            <AgentChatImageAttachment
              key={`${id}-prompt-image-${index}`}
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

function AgentChatDetailRows({
  rows,
  className,
}: {
  rows: ReactNode[];
  className?: string;
}) {
  if (rows.length === 0) {
    return null;
  }

  return (
    <div
      className={cn(
        "grid min-w-0 grid-cols-[1.75rem_minmax(0,1fr)] px-2 text-sm leading-6 sm:px-3",
        className,
      )}
    >
      {rows.map((row, index) => (
        <Fragment key={`detail-row-${index}`}>
          <span className="select-none font-mono text-muted-foreground">
            {index === 0 ? "└" : ""}
          </span>
          <div className="min-w-0 break-words text-muted-foreground">
            {row}
          </div>
        </Fragment>
      ))}
    </div>
  );
}

function AgentChatActivityTextCell({
  id,
  icon,
  title,
  bodyLines,
  fullBody,
  imageAttachments = [],
  bodyLimit,
  tone = "default",
}: {
  id: string;
  icon: AgentChatActivityMarkerKind;
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
        "flex min-w-0 max-w-full flex-col gap-1 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]",
        tone === "error" && "text-destructive",
        tone === "muted" && "text-muted-foreground",
      )}
    >
      <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-3 px-2 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker
          icon={icon}
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
            "flex min-w-0 max-w-full flex-col gap-0.5 pl-7 pr-2 text-muted-foreground sm:pl-8 sm:pr-3",
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
        <div className="flex min-w-0 max-w-full flex-col gap-2 px-2 sm:px-3">
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
  icon,
  label,
  value,
  suffix = "",
  valueClassName,
}: {
  icon: AgentChatActivityMarkerKind;
  label: string;
  value?: string;
  suffix?: string;
  valueClassName?: string;
}) {
  return (
    <div className="grid min-w-0 max-w-full grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-3 px-2 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere] sm:gap-x-[16px] sm:px-3">
      <AgentChatActivityMarker icon={icon} />
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
    <div className="ml-3 flex min-w-0 max-w-full flex-col gap-0.5 border-l-2 border-muted pl-3 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]">
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
          className={cn(
            "relative min-w-0 max-w-full text-muted-foreground",
            !open && isTruncatable && "max-h-[4.5rem] overflow-hidden",
          )}
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
  icon,
  title,
  steps,
}: {
  icon: AgentChatActivityMarkerKind;
  title: string;
  steps: AgentChatPlanStep[];
}) {
  const visibleSteps = steps.slice(0, AGENT_CHAT_PLAN_STEP_LIMIT);
  const rows =
    visibleSteps.length > 0
      ? visibleSteps.map((step) => (
          <AgentChatPlanStepLine key={`${step.status}-${step.text}`} step={step} />
        ))
      : [<span className="text-muted-foreground/75">No active plan.</span>];

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1 text-sm [overflow-wrap:anywhere]">
      <div className="flex min-w-0 items-start gap-x-3 px-2 leading-6 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground [overflow-wrap:anywhere]">
          {title}
        </p>
      </div>
      <AgentChatDetailRows rows={rows} />
    </div>
  );
}

function AgentChatPlanStepLine({
  step,
}: {
  step: AgentChatPlanStep;
}) {
  const marker = step.status === "completed" ? "✔" : "□";
  const isCurrent = step.status === "in_progress";

  return (
    <p
      className={cn(
        "min-w-0 break-words text-muted-foreground",
        isCurrent && "font-semibold text-primary",
        step.status === "completed" && "text-muted-foreground/65 line-through",
      )}
    >
      <span className="mr-1 font-mono">{marker}</span>
      {step.text}
    </p>
  );
}

function AgentChatCommandExecutionPanel({
  mode,
  icon,
  title,
  outputLines,
}: {
  mode: "running" | "completed";
  icon: AgentChatActivityMarkerKind;
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
    <div className="flex min-w-0 max-w-full flex-col gap-1 text-sm [overflow-wrap:anywhere]">
      <div className="flex min-w-0 items-start gap-x-3 px-2 leading-6 sm:gap-x-[16px] sm:px-3">
        {mode === "running" ? (
          <AgentChatActivityMarker
            icon={icon}
            className="motion-safe:animate-pulse"
          />
        ) : (
          <AgentChatActivityMarker icon={icon} />
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
      </div>
      <AgentChatTerminalOutputBlock lines={renderedOutput} />
    </div>
  );
}

function AgentChatTerminalOutputBlock({ lines }: { lines: string[] }) {
  const rows = lines.map((line, index) => (
    <pre
      key={`terminal-output-${index}`}
      className={cn(
        "min-w-0 whitespace-pre-wrap break-words font-mono text-xs leading-5 text-muted-foreground",
        line.startsWith("… +") && "text-muted-foreground/70",
      )}
    >
      {line}
    </pre>
  ));

  return (
    <AgentChatDetailRows rows={rows} />
  );
}

function AgentChatExploredActivityPanel({
  icon,
  title,
  calls,
}: {
  icon: AgentChatActivityMarkerKind;
  title: string;
  calls: AgentChatExploredCall[];
}) {
  const rows = agentChatExploredDetailRows(calls);
  const hiddenCallCount = Math.max(0, calls.length - AGENT_CHAT_EXPLORED_CALL_LIMIT);
  if (hiddenCallCount > 0) {
    rows.push(
      <span className="text-muted-foreground/75">
        … +{hiddenCallCount} more calls
      </span>,
    );
  }

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1 text-sm [overflow-wrap:anywhere]">
      <div className="flex min-w-0 items-start gap-x-3 px-2 leading-6 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {rows.length > 0 ? (
        <AgentChatDetailRows rows={rows} />
      ) : (
        <p className="px-2 text-xs text-muted-foreground sm:px-3">
          No explored tool calls
        </p>
      )}
    </div>
  );
}

function AgentChatPatchActivityPanel({
  icon,
  title,
  files,
}: {
  icon: AgentChatActivityMarkerKind;
  title: string;
  files: AgentChatDiffFile[];
}) {
  const visibleFiles = files.slice(0, AGENT_CHAT_PATCH_FILE_LIMIT);
  const hiddenFileCount = files.length - visibleFiles.length;
  const rows = visibleFiles.map((file, index) => (
    <AgentChatPatchFileBlock
      key={`${file.path}-${index}`}
      file={file}
      hideHeader={files.length === 1}
    />
  ));
  if (hiddenFileCount > 0) {
    rows.push(
      <p className="text-xs text-muted-foreground">
        … {hiddenFileCount} more files
      </p>,
    );
  }

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1.5 text-sm [overflow-wrap:anywhere]">
      <div className="flex min-w-0 items-start gap-x-3 px-2 leading-6 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {visibleFiles.length > 0 ? (
        <AgentChatDetailRows rows={rows} />
      ) : (
        <p className="px-2 text-xs text-muted-foreground sm:px-3">No file changes</p>
      )}
    </div>
  );
}

function AgentChatPatchFileBlock({
  file,
  hideHeader = false,
}: {
  file: AgentChatDiffFile;
  hideHeader?: boolean;
}) {
  const visibleLines = file.lines.slice(0, AGENT_CHAT_PATCH_DIFF_LINE_LIMIT);
  const hiddenLineCount = file.lines.length - visibleLines.length;
  const oldWidth = agentChatDiffLineNumberWidth(visibleLines, "old_lineno");
  const newWidth = agentChatDiffLineNumberWidth(visibleLines, "new_lineno");

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1">
      {!hideHeader ? (
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
      ) : null}
      {visibleLines.length > 0 ? (
        <div className="min-w-0 max-w-full overflow-x-auto font-mono text-xs leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
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
      <div className="grid min-w-0 grid-cols-[var(--old-width)_var(--new-width)_1rem_minmax(0,1fr)] gap-1 px-2 py-0.5 text-muted-foreground/70 [--new-width:2.5rem] [--old-width:2.5rem] sm:min-w-max sm:gap-2 sm:px-3">
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
        "grid min-w-0 grid-cols-[var(--old-width)_var(--new-width)_1rem_minmax(0,1fr)] gap-1 px-2 py-0.5 sm:min-w-max sm:gap-2 sm:px-3",
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
      <span className="whitespace-pre-wrap break-words text-foreground/85 sm:whitespace-pre">
        {line.text}
      </span>
    </div>
  );
}

function AgentChatMessageActivityLine({
  id,
  icon,
  title,
  detailLines,
  messageLines,
  detailLimit,
  messageLimit,
}: {
  id: string;
  icon: AgentChatActivityMarkerKind;
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
    <div className="flex min-w-0 max-w-full flex-col gap-1 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]">
      <div className="grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-3 px-2 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {visibleDetailLines.length > 0 || hiddenDetailCount > 0 ? (
        <div className="flex min-w-0 max-w-full flex-col gap-0.5 pl-7 pr-2 text-xs leading-5 text-muted-foreground sm:pl-10 sm:pr-3">
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
        <div className="flex min-w-0 max-w-full flex-col gap-0.5 px-2 text-foreground/90 sm:px-3">
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
  id,
  icon,
  title,
  messageLines,
  disposition,
  subject,
}: {
  id: string;
  icon: AgentChatActivityMarkerKind;
  title: string;
  messageLines: string[];
  disposition: string;
  subject: string;
}) {
  if (disposition === "resolved" && subject === "message") {
    const agentMessage = agentChatAgentMessageFromLines(messageLines);
    if (!agentMessage) {
      return null;
    }
    return (
      <AgentChatActivityTextCell
        id={id}
        icon="activity"
        title={agentMessage.title}
        bodyLines={agentMessage.bodyLines}
        fullBody={agentMessage.fullBody}
      />
    );
  }

  return (
    <div
      className={cn(
        "flex min-w-0 max-w-full flex-col gap-1 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]",
        disposition === "failed" && "text-destructive",
        disposition === "dismissed" && "text-muted-foreground",
      )}
    >
      <div className="flex min-w-0 items-start gap-x-3 px-2 leading-6 sm:gap-x-[16px] sm:px-3">
        <AgentChatActivityMarker
          icon={icon}
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
        <div className="px-2 text-foreground/90 sm:px-3">
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
        className="break-all text-primary underline-offset-4 hover:underline"
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
        className="break-all text-primary underline-offset-4 hover:underline"
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
        "flex min-w-0 max-w-full flex-col gap-2 text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]",
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
            <ul className="flex list-disc flex-col gap-1 pl-5 text-foreground/90">
              {children}
            </ul>
          ),
          ol: ({ children }: any) => (
            <ol className="flex list-decimal flex-col gap-1 pl-5 text-foreground/90">
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
          hr: () => <Separator />,
          p: ({ children }: any) => <p className="break-words">{children}</p>,
          code: (props: any) => {
            const { inline, className, children } = props;
            if (!inline && className) {
              const language = String(className).replace(/^language-/, "");
              return (
                <div className="flex min-w-0 max-w-full flex-col gap-1">
                  <div className="flex items-center gap-2 px-2 sm:px-3">
                    <span className="font-mono text-[0.7rem] leading-none text-muted-foreground">
                      &lt;/&gt;
                    </span>
                    <span className="truncate text-sm font-semibold text-foreground/90">
                      {language || "Code"}
                    </span>
                  </div>
                  <pre className="max-h-72 min-w-0 max-w-full overflow-auto whitespace-pre-wrap px-2 font-mono text-[0.82rem] leading-6 text-foreground/90 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin] sm:whitespace-pre sm:px-3">
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
                className="break-all text-primary underline-offset-4 hover:underline"
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
              className="break-all text-primary underline-offset-4 hover:underline"
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
        "flex flex-col gap-1 text-foreground/90",
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
    <figure className="min-w-0 max-w-[min(28rem,100%)] overflow-hidden rounded-lg border border-border/60 bg-muted/20">
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
    <div className="flex min-w-0 max-w-full flex-col gap-1">
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
              <CheckIcon data-icon="inline-start" aria-hidden="true" />
            ) : (
              <ClipboardIcon data-icon="inline-start" aria-hidden="true" />
            )}
          </Button>
        ) : null}
      </div>
      <div className="relative">
        <pre
          className="max-h-72 min-w-0 max-w-full overflow-auto whitespace-pre px-3 font-mono text-[0.82rem] leading-6 text-foreground/90 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]"
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
    <div className="flex min-w-0 max-w-full flex-col gap-2 font-mono text-xs">
      {visibleFiles.map((file, fileIndex) => {
        const visibleLines = file.lines.slice(0, limit);
        const hiddenLines = file.lines.slice(limit);
        return (
          <div
            key={`${id}-file-${fileIndex}`}
            className="flex min-w-0 max-w-full flex-col gap-1"
          >
            <div className="flex items-center justify-between gap-3 px-2 sm:px-3">
              <p className="min-w-0 truncate text-foreground/85">{file.path}</p>
              <span className="shrink-0 font-sans text-[0.68rem] text-muted-foreground">
                <span className="text-primary">+{file.added_lines}</span>{" "}
                <span className="text-destructive">-{file.removed_lines}</span>
              </span>
            </div>
            <pre className="max-h-72 min-w-0 max-w-full overflow-auto whitespace-pre-wrap px-2 leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin] sm:whitespace-pre sm:px-3">
              {visibleLines.map((line) => renderDiffLine(line)).join("\n")}
            </pre>
            {hiddenLines.length > 0 ? (
              <p className="px-2 font-sans text-xs text-muted-foreground sm:px-3">
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
    uiHint: nullableStringValue(record.ui_hint),
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

function agentChatActivityIconForBubble(
  bubble: AgentChatBubble,
): AgentChatActivityMarkerKind {
  if (bubble.kind === "error" || bubble.status === "failed") {
    return "error";
  }

  return "activity";
}

function agentChatActivityIconClass(bubble: AgentChatBubble) {
  if (bubble.status === "failed" || bubble.kind === "error") {
    return "text-destructive";
  }

  if (bubble.live || bubble.status === "running") {
    return "text-primary";
  }

  if (bubble.kind === "patch") {
    return "text-primary";
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
    bubble.appName === "Coding" || bubble.toolName === "explored"
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
    return agentChatTextActivityRender("activity", assistant, "Activity");
  }

  const user = agentChatActivityCellPayload(cell, "User");
  if (user) {
    const render = agentChatTextActivityRender("user", user, "user");
    render.imageAttachments = imageAttachmentsValue(user.image_attachments);
    return render;
  }

  const browser = agentChatActivityCellPayload(cell, "Browser");
  if (browser) {
    return {
      kind: "browser",
      icon: "activity",
      title: `Captured URL: ${compactAgentChatBrowserUrl(nullableStringValue(browser.url) ?? "unknown")}`,
      detailLines: agentChatBrowserStatsLines(browser),
    };
  }

  const liveBrowser = agentChatActivityCellPayload(cell, "LiveBrowser");
  if (liveBrowser) {
    const url = nullableStringValue(liveBrowser.url);
    return {
      kind: "browser",
      icon: "activity",
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
      icon: "activity",
      title: `App: ${stringValue(genericApp.title, "Tool")}`,
      bodyLines: [],
    };
  }

  const plan = agentChatActivityCellPayload(cell, "PlanResult");
  if (plan) {
    return {
      kind: "plan",
      icon: "activity",
      title: agentChatPlanTitleFromActivityCell(plan),
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
      icon: "activity",
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
      icon: "activity",
      title: "Activated Primitive:",
      primitiveId: stringValue(activatePrimitive.primitive_id, "unknown"),
    };
  }

  const explored = agentChatActivityCellPayload(cell, "Explored");
  if (explored) {
    return {
      kind: "explored",
      icon: "activity",
      title: stringValue(explored.title, "Explored"),
      calls: agentChatExploredCallsFromActivityCell(explored),
    };
  }

  const execResult = agentChatActivityCellPayload(cell, "ExecResult");
  if (execResult) {
    return {
      kind: "exec",
      icon: "activity",
      title: stringValue(execResult.title, "Command"),
      outputLines: stringArrayValuePreserveWhitespace(execResult.output_lines),
      exitCode: parseAgentChatExitCode(nullableStringValue(execResult.meta)),
    };
  }

  const liveExec = agentChatActivityCellPayload(cell, "LiveExec");
  if (liveExec) {
    return {
      kind: "exec",
      icon: "activity",
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
      icon: "activity",
      title: agentChatPatchTitle(files),
      files,
    };
  }

  const telegram = agentChatActivityCellPayload(cell, "Telegram");
  if (telegram) {
    return {
      kind: "messageActivity",
      icon: "activity",
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
      icon: "activity",
      title: agentChatReplyTitle(
        disposition,
        stringValue(reply.subject, "message"),
      ),
      messageLines: stringArrayValue(reply.message_lines),
      disposition,
      subject: stringValue(reply.subject, "message").toLowerCase(),
    };
  }

  const terminalWait = agentChatActivityCellPayload(cell, "TerminalWait");
  if (terminalWait) {
    return {
      kind: "text",
      icon: "activity",
      title: stringValue(terminalWait.title, "Terminal wait"),
      bodyLines: stringArrayValue(terminalWait.body_lines),
      bodyLimit: AGENT_CHAT_TERMINAL_WAIT_LINE_LIMIT,
    };
  }

  const error = agentChatActivityCellPayload(cell, "Error");
  if (error) {
    return {
      kind: "text",
      icon: "error",
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
  icon: AgentChatActivityMarkerKind,
  cell: Record<string, unknown>,
  fallbackTitle: string,
): Extract<AgentChatActivityCellRender, { kind: "text" }> {
  return {
    kind: "text",
    icon,
    title: stringValue(cell.title, fallbackTitle),
    bodyLines: stringArrayValue(cell.body_lines),
    fullBody: nullableStringValue(cell.full_body),
  };
}

function agentChatAgentMessageFromLines(messageLines: string[]) {
  if (messageLines.length === 0) {
    return null;
  }

  const fullBody = messageLines.join("\n").replace(/[\r\n]+$/, "");
  const lines = fullBody.split("\n");
  const title = lines[0] ?? "";
  return {
    title,
    bodyLines: lines.slice(1),
    fullBody,
  };
}

function agentChatExploredCallsFromActivityCell(
  cell: Record<string, unknown>,
): AgentChatExploredCall[] {
  return arrayValue(cell.calls)
    .map(asRecord)
    .filter((call): call is Record<string, unknown> => Boolean(call))
    .map((call) => ({
      toolName: stringValue(call.tool_name, "tool"),
      action: normalizeAgentChatExploredAction(call.action),
      target: nullableStringValue(call.target),
      secondaryTarget: nullableStringValue(call.secondary_target),
      summary: stringValue(call.summary, ""),
      detailLines: stringArrayValue(call.detail_lines),
      detailTitle: nullableStringValue(call.detail_title),
    }));
}

function normalizeAgentChatExploredAction(
  value: unknown,
): AgentChatExploredCallAction {
  if (typeof value !== "string") {
    return "unknown";
  }

  const normalized = value.toLowerCase();
  if (
    normalized === "read" ||
    normalized === "list" ||
    normalized === "search" ||
    normalized === "run"
  ) {
    return normalized;
  }

  return "unknown";
}

function agentChatExploredActionLabel(action: AgentChatExploredCallAction) {
  if (action === "read") {
    return "Read";
  }
  if (action === "list") {
    return "List";
  }
  if (action === "search") {
    return "Search";
  }
  if (action === "run") {
    return "Run";
  }
  return "Tool";
}

function agentChatExploredDetailRows(calls: AgentChatExploredCall[]) {
  const rows: ReactNode[] = [];
  const visibleCalls = calls.slice(0, AGENT_CHAT_EXPLORED_CALL_LIMIT);
  let index = 0;

  while (index < visibleCalls.length) {
    const call = visibleCalls[index];
    if (call.action === "read") {
      const names = [agentChatExploredReadTarget(call)];
      index += 1;
      while (index < visibleCalls.length && visibleCalls[index].action === "read") {
        names.push(agentChatExploredReadTarget(visibleCalls[index]));
        index += 1;
      }
      rows.push(
        <AgentChatExploredActionLine
          action="Read"
          detail={dedupeStrings(names).join(", ")}
        />,
      );
      continue;
    }

    rows.push(
      <AgentChatExploredActionLine
        action={agentChatExploredActionLabel(call.action)}
        detail={agentChatExploredCallDetail(call)}
      />,
    );
    index += 1;
  }

  return rows;
}

function AgentChatExploredActionLine({
  action,
  detail,
}: {
  action: string;
  detail: string;
}) {
  return (
    <p className="min-w-0 break-words text-foreground/90">
      <span className="font-medium text-primary">{action}</span>
      {detail ? <span> {detail}</span> : null}
    </p>
  );
}

function agentChatExploredCallDetail(call: AgentChatExploredCall) {
  if (call.action === "search") {
    if (call.target) {
      return call.secondaryTarget
        ? `${call.target.trim()} in ${compactAgentChatCodingSummaryPath(call.secondaryTarget)}`
        : call.target.trim();
    }
    return call.summary;
  }

  if (call.action === "list") {
    return call.target
      ? compactAgentChatCodingSummaryPath(call.target)
      : call.summary;
  }

  if (call.action === "run") {
    return call.target?.trim() || call.summary;
  }

  return call.summary;
}

function agentChatExploredReadTarget(call: AgentChatExploredCall) {
  return compactAgentChatCodingSummaryPath(call.target || call.summary);
}

function compactAgentChatCodingSummaryPath(value: string) {
  const target = value
    .split(" -> ")[0]
    .split(":L")[0]
    .split("#")[0]
    .trim();
  const normalized = target.replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  return parts.at(-1) ?? target;
}

function dedupeStrings(values: string[]) {
  return values.filter((value, index) => value && values.indexOf(value) === index);
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
  if (files.length === 1) {
    const file = files[0];
    return `Edited ${file.path} (+${file.added_lines} -${file.removed_lines})`;
  }

  const fileNoun = files.length === 1 ? "File" : "Files";
  return `Edited ${files.length} ${fileNoun}`;
}

function agentChatPlanTitleFromActivityCell(cell: Record<string, unknown>) {
  return stringValue(cell.kind, "updated").toLowerCase() === "proposed"
    ? "Proposed Plan"
    : "Updated Plan";
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
