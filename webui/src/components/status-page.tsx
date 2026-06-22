import {
  Fragment,
  isValidElement,
  memo,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type CSSProperties,
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
  CornerDownLeftIcon,
  GripVerticalIcon,
  ImagePlusIcon,
  InfoIcon,
  MoreHorizontalIcon,
  PencilIcon,
  SendHorizontalIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  CollapsibleTrigger,
  useCollapsibleState,
} from "@/components/ui/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupTextarea,
} from "@/components/ui/input-group";
import { Separator } from "@/components/ui/separator";
import { Spinner } from "@/components/ui/spinner";
import { Textarea } from "@/components/ui/textarea";
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
  type DashboardPendingUserInput,
  type DashboardSnapshot,
  type DashboardPlanStep,
  type TokenUsageInfo,
  type ActivityCellVariant,
  type WebActivityBlock,
  type WebActivityItem,
} from "@/lib/daemon-api";
import {
  foldCompletedAgentChatActivity,
  type AgentChatFoldDisplayItem,
} from "@/lib/agent-chat-folding";
import {
  highlightCodeWithShiki,
  type ShikiColorScheme,
  type ShikiHighlightedCode,
  type ShikiHighlightToken,
} from "@/lib/shiki-highlight";
import { useDashboardSnapshot } from "@/hooks/use-dashboard-snapshot";
import { cn } from "@/lib/utils";
export { StatusPage } from "@/components/status-dashboard-page";

const AGENT_CHAT_HISTORY_PAGE_LIMIT = 80;
const AGENT_CHAT_NAV_HISTORY_PAGE_LIMIT = 40;
const AGENT_CHAT_QUICK_NAV_MAX_ITEMS = 120;
const AGENT_CHAT_MESSAGE_LINE_LIMIT = 5;
const AGENT_CHAT_ACTIVITY_BLOCK_LINE_LIMIT = 12;
const AGENT_CHAT_FULL_MESSAGE_LINE_LIMIT = Number.MAX_SAFE_INTEGER;
const AGENT_CHAT_PLAN_STEP_LIMIT = 8;
const AGENT_CHAT_TERMINAL_OUTPUT_HEAD_LINES = 4;
const AGENT_CHAT_TERMINAL_OUTPUT_TAIL_LINES = 4;
const AGENT_CHAT_TELEGRAM_DETAIL_LIMIT = 6;
const AGENT_CHAT_TELEGRAM_MESSAGE_LIMIT = 6;
const AGENT_CHAT_TERMINAL_WAIT_LINE_LIMIT = 6;
const AGENT_CHAT_ERROR_LINE_LIMIT = 12;
const AGENT_CHAT_THINKING_PREVIEW_LINE_LIMIT = 3;
const AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX = 72;
const AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX = 160;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENTS = 4;
const AGENT_CHAT_MAX_IMAGE_ATTACHMENT_BYTES = 10 * 1024 * 1024;
const AGENT_CHAT_RUNTIME_SHIMMER_MS = 2_000;
const AGENT_CHAT_RUNTIME_SHIMMER_STAGGER_MS = 120;
const AGENT_CHAT_INLINE_PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
const AGENT_CHAT_COMPOSER_DEFAULT_HEIGHT_PX = 60;
const AGENT_CHAT_COMPOSER_BOTTOM_GAP_PX = 16;
const AGENT_CHAT_PENDING_INPUT_VISIBLE_DELAY_MS = 200;
const AGENT_CHAT_MAX_QUEUED_INPUTS = 5;
const AGENT_CHAT_QUICK_NAV_COLLAPSED_ITEM_LIMIT = 14;
const AGENT_CHAT_QUICK_NAV_SCROLL_OFFSET_PX = 16;
export function AgentPage({
  sessionId,
  mockSnapshot,
}: {
  sessionId: string;
  mockSnapshot?: DashboardSnapshot;
}) {
  const { snapshot } = useDashboardSnapshot(sessionId, {
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

type AgentChatQuickNavItem = {
  id: string;
  label: string;
  order: number;
};

type AgentChatQuickNavDisplayTarget = {
  id: string;
  quickNavItemId: string;
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

type WebInputSuggestion = {
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
    }
  | {
      kind: "runtimeStatus";
      icon: AgentChatActivityMarkerKind;
      title: string;
      detail?: string | null;
      startedAtMs?: number | null;
      reducedMotion?: string | null;
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

function AgentChatComposer({
  sessionId,
  snapshot,
  agentName,
  supportsVision = true,
  chatPanelRef,
  onHeightChange,
}: {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
  agentName?: string;
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
  const [pendingUserInputActionId, setPendingUserInputActionId] = useState<
    string | null
  >(null);
  const pendingUserInputs = snapshot?.pending_user_inputs ?? [];
  const visiblePendingUserInputs = useDelayedPendingUserInputs(
    pendingUserInputs,
    AGENT_CHAT_PENDING_INPUT_VISIBLE_DELAY_MS,
  );
  const queuedInputLimitReached =
    pendingUserInputs.length >= AGENT_CHAT_MAX_QUEUED_INPUTS;
  const queuedInputLimitMessage = `You can queue up to ${AGENT_CHAT_MAX_QUEUED_INPUTS} inputs. Wait for the agent to handle one or clear the queue.`;

  const inputSuggestions = useMemo(
    () => webInputSuggestions(message, snapshot),
    [message, snapshot],
  );
  const slashCommandFeedback = useMemo(
    () => webSlashCommandFeedback(message, snapshot, imageAttachments.length),
    [imageAttachments.length, message, snapshot],
  );
  const selectedInputSuggestion =
    inputSuggestions[Math.min(slashCommandSelection, inputSuggestions.length - 1)];
  const slashCommandBlocksSubmit =
    Boolean(slashCommandFeedback?.blocksSubmit) ||
    (isWebSlashCommandInput(message) &&
      !parseWebSlashCommand(message)?.trimmed);
  const composerHasPayload = message.trim().length > 0 || imageAttachments.length > 0;
  const composerQueuesUserInput = !isWebSlashCommandInput(message);
  const queuedInputLimitBlocksSubmit =
    queuedInputLimitReached && composerHasPayload && composerQueuesUserInput;
  const isRuntimeInterruptible =
    snapshot?.runtime_activity?.active_runtime_turn ?? false;
  const sendButtonInterruptsRuntime =
    isRuntimeInterruptible && !composerHasPayload;
  const sendButtonDisabled =
    isSending ||
    (sendButtonInterruptsRuntime
      ? false
      : !composerHasPayload ||
        slashCommandBlocksSubmit ||
        queuedInputLimitBlocksSubmit);

  useEffect(() => {
    setSlashCommandSelection((current) =>
      Math.min(current, Math.max(0, inputSuggestions.length - 1)),
    );
  }, [inputSuggestions.length]);

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

    if (
      !isSlashCommand &&
      pendingUserInputs.length >= AGENT_CHAT_MAX_QUEUED_INPUTS
    ) {
      setSendError(queuedInputLimitMessage);
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

  async function interruptRuntimeTurn(): Promise<boolean> {
    if (isSending || !isRuntimeInterruptible) {
      return false;
    }

    setIsSending(true);
    setSendError(null);
    setSlashActionFeedback(null);
    try {
      const result = await runDashboardAction(
        { kind: "interrupt_runtime" },
        { sessionId },
      );
      if (!result.success) {
        setSendError(
          result.detail ? `${result.message}: ${result.detail}` : result.message,
        );
        return false;
      }
      return true;
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
      return false;
    } finally {
      setIsSending(false);
    }
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (sendButtonInterruptsRuntime) {
      await interruptRuntimeTurn();
      return;
    }
    await submitComposerInput(message);
  }

  function applyInputSuggestion(suggestion: WebInputSuggestion) {
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

  async function runPendingUserInputAction(
    action: DashboardAction,
    actionId: string,
  ) {
    if (pendingUserInputActionId) {
      return;
    }

    setPendingUserInputActionId(actionId);
    setSendError(null);
    setSlashActionFeedback(null);
    try {
      const result = await runDashboardAction(action, { sessionId });
      if (!result.success) {
        setSendError(
          result.detail ? `${result.message}: ${result.detail}` : result.message,
        );
      }
    } catch (error) {
      setSendError(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingUserInputActionId(null);
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
        suggestions={inputSuggestions}
        selectedSuggestionIndex={slashCommandSelection}
        isSending={isSending}
        onClosePanel={() => setSlashPanel(null)}
        onSetPanel={setSlashPanel}
        onCloseActionFeedback={() => setSlashActionFeedback(null)}
        onSelectSuggestion={applyInputSuggestion}
        onHoverSuggestion={setSlashCommandSelection}
        onRunAction={(command) => void runSlashAction(command)}
        onRunDashboardAction={(action) => void runSlashDashboardAction(action)}
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
      <PendingUserInputQueue
        inputs={visiblePendingUserInputs}
        busyActionId={pendingUserInputActionId}
        onPreempt={(input) =>
          void runPendingUserInputAction(
            { kind: "preempt_pending_user_input", event_id: input.event_id },
            pendingUserInputPreemptActionId(input),
          )
        }
        onDismiss={(input) =>
          void runPendingUserInputAction(
            { kind: "dismiss_pending_user_input", event_id: input.event_id },
            pendingUserInputDismissActionId(input),
          )
        }
        onClear={() =>
          void runPendingUserInputAction(
            { kind: "clear_pending_user_inputs" },
            pendingUserInputClearActionId(),
          )
        }
        onEdit={(input, incomingText) =>
          void runPendingUserInputAction(
            {
              kind: "update_pending_user_input",
              event_id: input.event_id,
              incoming_text: incomingText,
            },
            pendingUserInputEditActionId(input),
          )
        }
        onMoveToPosition={(input, targetPosition) =>
          void runPendingUserInputAction(
            {
              kind: "move_pending_user_input_to_position",
              event_id: input.event_id,
              target_position: targetPosition,
            },
            pendingUserInputMoveToPositionActionId(input, targetPosition),
          )
        }
      />
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
            if (inputSuggestions.length > 0) {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setSlashCommandSelection((current) =>
                  (current + 1) % inputSuggestions.length,
                );
                return;
              }
              if (event.key === "ArrowUp") {
                event.preventDefault();
                setSlashCommandSelection(
                  (current) =>
                    (current - 1 + inputSuggestions.length) % inputSuggestions.length,
                );
                return;
              }
              if (event.key === "Tab") {
                event.preventDefault();
                applyInputSuggestion(
                  selectedInputSuggestion ?? inputSuggestions[0],
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
                selectedInputSuggestion &&
                selectedInputSuggestion.completion !==
                  (isWebSlashCommandInput(message) ? message.trim() : message)
              ) {
                applyInputSuggestion(selectedInputSuggestion);
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
            type={sendButtonInterruptsRuntime ? "button" : "submit"}
            variant={sendButtonInterruptsRuntime ? "destructive" : "default"}
            size="icon-sm"
            disabled={sendButtonDisabled}
            aria-label={
              sendButtonInterruptsRuntime
                ? "Interrupt agent"
                : queuedInputLimitBlocksSubmit
                  ? "Queued input limit reached"
                  : "Send message"
            }
            title={
              sendButtonInterruptsRuntime
                ? "Interrupt agent"
                : queuedInputLimitBlocksSubmit
                  ? queuedInputLimitMessage
                  : "Send message"
            }
            onClick={
              sendButtonInterruptsRuntime
                ? () => void interruptRuntimeTurn()
                : undefined
            }
            className="rounded-full"
          >
            {isSending ? (
              <Spinner data-icon="inline-start" />
            ) : sendButtonInterruptsRuntime ? (
              <XIcon data-icon="inline-start" aria-hidden="true" />
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

function useDelayedPendingUserInputs(
  inputs: DashboardPendingUserInput[],
  delayMs: number,
): DashboardPendingUserInput[] {
  const [visibleInputTick, setVisibleInputTick] = useState(0);

  useEffect(() => {
    if (inputs.length === 0) {
      return;
    }

    const now = Date.now();
    const nextDelay = inputs.reduce<number | null>((currentDelay, input) => {
      const ageMs = Math.max(0, now - input.arrived_at_ms);
      const remainingMs = delayMs - ageMs;

      if (remainingMs <= 0) {
        return currentDelay;
      }

      return currentDelay === null
        ? remainingMs
        : Math.min(currentDelay, remainingMs);
    }, null);

    if (nextDelay === null) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setVisibleInputTick((current) => current + 1);
    }, nextDelay);

    return () => window.clearTimeout(timeoutId);
  }, [delayMs, inputs, visibleInputTick]);

  const now = Date.now();
  return inputs.filter(
    (input) => Math.max(0, now - input.arrived_at_ms) >= delayMs,
  );
}

function PendingUserInputQueue({
  inputs,
  busyActionId,
  onPreempt,
  onDismiss,
  onClear,
  onEdit,
  onMoveToPosition,
}: {
  inputs: DashboardPendingUserInput[];
  busyActionId: string | null;
  onPreempt: (input: DashboardPendingUserInput) => void;
  onDismiss: (input: DashboardPendingUserInput) => void;
  onClear: () => void;
  onEdit: (input: DashboardPendingUserInput, incomingText: string) => void;
  onMoveToPosition: (
    input: DashboardPendingUserInput,
    targetPosition: number,
  ) => void;
}) {
  const [draggingEventId, setDraggingEventId] = useState<string | null>(null);
  const [dragOverEventId, setDragOverEventId] = useState<string | null>(null);
  const [editingInput, setEditingInput] =
    useState<DashboardPendingUserInput | null>(null);
  const [editText, setEditText] = useState("");
  const editTextareaId = useId();
  const queueBusy = Boolean(busyActionId);
  const editBusy = editingInput
    ? busyActionId === pendingUserInputEditActionId(editingInput)
    : false;

  useEffect(() => {
    if (
      editingInput &&
      !inputs.some((input) => input.event_id === editingInput.event_id)
    ) {
      setEditingInput(null);
      setEditText("");
    }
  }, [editingInput, inputs]);

  if (inputs.length === 0) {
    return null;
  }

  function openEditDialog(input: DashboardPendingUserInput) {
    setEditingInput(input);
    setEditText(input.incoming_text);
  }

  function closeEditDialog() {
    if (editBusy) {
      return;
    }
    setEditingInput(null);
    setEditText("");
  }

  function handleEditSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    event.stopPropagation();
    if (!editingInput || editBusy) {
      return;
    }
    onEdit(editingInput, editText);
    setEditingInput(null);
    setEditText("");
  }

  function handleDragStart(
    event: DragEvent<HTMLButtonElement>,
    input: DashboardPendingUserInput,
  ) {
    if (queueBusy || inputs.length <= 1) {
      event.preventDefault();
      return;
    }
    event.stopPropagation();
    setDraggingEventId(input.event_id);
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", input.event_id);
  }

  function handleDragOver(
    event: DragEvent<HTMLDivElement>,
    input: DashboardPendingUserInput,
  ) {
    if (!draggingEventId || draggingEventId === input.event_id) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "move";
    setDragOverEventId(input.event_id);
  }

  function handleDrop(
    event: DragEvent<HTMLDivElement>,
    targetInput: DashboardPendingUserInput,
  ) {
    const sourceEventId =
      draggingEventId || event.dataTransfer.getData("text/plain");
    setDraggingEventId(null);
    setDragOverEventId(null);
    if (!sourceEventId || sourceEventId === targetInput.event_id) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const sourceInput = inputs.find((input) => input.event_id === sourceEventId);
    const targetPosition = inputs.findIndex(
      (input) => input.event_id === targetInput.event_id,
    );
    if (!sourceInput || targetPosition < 0) {
      return;
    }
    onMoveToPosition(sourceInput, targetPosition);
  }

  function resetDragState() {
    setDraggingEventId(null);
    setDragOverEventId(null);
  }

  return (
    <>
      <section
        role="region"
        aria-label="Pending user inputs"
        aria-busy={queueBusy || undefined}
        className="mb-2 border-b border-border/60 pb-2"
      >
        <div role="list" className="max-h-40 overflow-auto">
          {inputs.map((input, index) => {
            const position = index + 1;
            const dismissBusy =
              busyActionId === pendingUserInputDismissActionId(input);
            const preemptBusy =
              busyActionId === pendingUserInputPreemptActionId(input);
            const moveBusy = Boolean(
              busyActionId?.startsWith(`${input.event_id}:move-to:`),
            );
            const preview = pendingUserInputPreview(input);
            const canReorder = inputs.length > 1;
            const dragOver =
              dragOverEventId === input.event_id &&
              draggingEventId !== input.event_id;

            return (
              <div
                key={input.event_id}
                role="listitem"
                onDragOver={(event) => handleDragOver(event, input)}
                onDrop={(event) => handleDrop(event, input)}
                onDragEnd={resetDragState}
                className={cn(
                  "grid min-w-0 grid-cols-[1.5rem_minmax(0,1fr)_auto_auto] items-start gap-x-2 px-2 py-1 text-sm leading-5 text-foreground/90",
                  dragOver && "rounded-md bg-accent/60",
                )}
              >
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  draggable={canReorder && !queueBusy}
                  aria-label={`Drag pending input ${position}`}
                  title={canReorder ? "Drag to reorder" : "Only one queued input"}
                  disabled={queueBusy || !canReorder}
                  onDragStart={(event) => handleDragStart(event, input)}
                  onDragEnd={resetDragState}
                  className="rounded-full text-muted-foreground hover:text-foreground disabled:opacity-60 enabled:cursor-grab enabled:active:cursor-grabbing"
                >
                  {moveBusy ? (
                    <Spinner data-icon="inline-start" />
                  ) : (
                    <GripVerticalIcon data-icon="inline-start" aria-hidden="true" />
                  )}
                </Button>
                <p className="min-w-0 truncate" title={preview}>
                  {preview}
                </p>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`Run pending input ${position} now`}
                  title="Run this queued input now"
                  disabled={queueBusy}
                  onClick={() => onPreempt(input)}
                  className="rounded-full text-muted-foreground hover:text-foreground"
                >
                  {preemptBusy ? (
                    <Spinner data-icon="inline-start" />
                  ) : (
                    <CornerDownLeftIcon data-icon="inline-start" aria-hidden="true" />
                  )}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      aria-label={`More actions for pending input ${position}`}
                      title="More actions"
                      disabled={queueBusy}
                      className="rounded-full text-muted-foreground hover:text-foreground"
                    >
                      {dismissBusy ? (
                        <Spinner data-icon="inline-start" />
                      ) : (
                        <MoreHorizontalIcon
                          data-icon="inline-start"
                          aria-hidden="true"
                        />
                      )}
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="w-44">
                    <DropdownMenuGroup>
                      <DropdownMenuItem
                        disabled={queueBusy}
                        onSelect={() => openEditDialog(input)}
                      >
                        <PencilIcon />
                        <span>Edit message</span>
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        disabled={queueBusy}
                        variant="destructive"
                        onSelect={() => onDismiss(input)}
                      >
                        <Trash2Icon />
                        <span>Dismiss message</span>
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        disabled={queueBusy}
                        variant="destructive"
                        onSelect={onClear}
                      >
                        <Trash2Icon />
                        <span>Clear queue</span>
                      </DropdownMenuItem>
                    </DropdownMenuGroup>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            );
          })}
        </div>
      </section>

      <Dialog
        open={editingInput !== null}
        onOpenChange={(open) => {
          if (!open) {
            closeEditDialog();
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Edit queued message</DialogTitle>
            <DialogDescription>
              Update this pending input before the agent handles it.
            </DialogDescription>
          </DialogHeader>
          <form onSubmit={handleEditSubmit} className="flex flex-col gap-4">
            <FieldGroup className="gap-3">
              <Field>
                <FieldLabel htmlFor={editTextareaId}>Message</FieldLabel>
                <Textarea
                  id={editTextareaId}
                  value={editText}
                  rows={5}
                  disabled={editBusy}
                  onChange={(event) => setEditText(event.target.value)}
                />
              </Field>
            </FieldGroup>
            {editingInput && editingInput.attachment_count > 0 ? (
              <p className="text-xs text-muted-foreground">
                Attachments stay queued with this message.
              </p>
            ) : null}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={editBusy}
                onClick={closeEditDialog}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={editBusy}>
                {editBusy ? <Spinner data-icon="inline-start" /> : null}
                Save
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  );
}

function pendingUserInputMoveToPositionActionId(
  input: DashboardPendingUserInput,
  targetPosition: number,
) {
  return `${input.event_id}:move-to:${targetPosition}`;
}

function pendingUserInputEditActionId(input: DashboardPendingUserInput) {
  return `${input.event_id}:edit`;
}

function pendingUserInputClearActionId() {
  return "pending-user-inputs:clear";
}

function pendingUserInputPreemptActionId(input: DashboardPendingUserInput) {
  return `${input.event_id}:preempt`;
}

function pendingUserInputDismissActionId(input: DashboardPendingUserInput) {
  return `${input.event_id}:dismiss`;
}

function pendingUserInputPreview(input: DashboardPendingUserInput) {
  const text = input.incoming_text.trim();
  if (text) {
    return text;
  }
  return input.attachment_count > 0 ? "Attachment-only input" : "Empty input";
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
  suggestions: WebInputSuggestion[];
  selectedSuggestionIndex: number;
  isSending: boolean;
  onClosePanel: () => void;
  onSetPanel: (panel: WebSlashPanel | null) => void;
  onCloseActionFeedback: () => void;
  onSelectSuggestion: (suggestion: WebInputSuggestion) => void;
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
    <section aria-label={title} className="flex min-h-0 flex-col gap-2 text-sm">
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
      <div className="-mx-2 min-h-0 max-h-72 overflow-y-auto overflow-x-hidden px-2 [scrollbar-gutter:stable]">
        <div className="flex min-w-0 flex-col gap-2">{children}</div>
      </div>
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
      <div className="flex min-w-0 flex-col gap-1">
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
  const status = webSlashStatusSnapshot(snapshot);
  const tokenUsageSections = webSlashStatusTokenUsageSections(snapshot);

  return (
    <WebSlashPanelShell
      title="STATUS"
      subtitle="Current session runtime facts."
      onClose={onClose}
    >
      <div className="flex min-w-0 flex-col gap-3">
        {!snapshot ? (
          <Alert className="px-2 py-1">
            <InfoIcon className="size-4" aria-hidden="true" />
            <AlertDescription className="text-xs">
              Status snapshot is still loading; showing placeholder values.
            </AlertDescription>
          </Alert>
        ) : null}


        <section className="flex flex-col gap-2" aria-label="Model usage">
          <p className="text-sm font-medium text-foreground">Model usage</p>
          <div className="flex flex-col gap-3">
            {tokenUsageSections.length > 0 ? (
              tokenUsageSections.map((section, index) => (
                <Fragment key={section.role}>
                  <WebSlashStatusTokenUsageSection section={section} />
                  {index < tokenUsageSections.length - 1 ? <Separator /> : null}
                </Fragment>
              ))
            ) : (
              <Empty className="py-3">
                <EmptyHeader>
                  <EmptyTitle>No token usage recorded yet.</EmptyTitle>
                </EmptyHeader>
              </Empty>
            )}
          </div>
        </section>

        <Separator />

        <section className="flex flex-col gap-2" aria-label="Plan">
          <p className="text-sm font-medium text-foreground">Plan</p>
          {status.planSteps.length > 0 ? (
            <ul className="flex flex-col gap-1">
              {status.planSteps.map((step, index) => (
                <li
                  key={`${index}-${step.step}`}
                  className="flex min-w-0 items-start gap-2 text-sm"
                >
                  <span className="shrink-0 text-muted-foreground" aria-hidden="true">
                    •
                  </span>
                  <span className="min-w-0 flex-1 break-words text-foreground">
                    {step.step}
                  </span>
                  <Badge
                    variant={webSlashPlanStatusBadgeVariant(step.status)}
                    className="shrink-0"
                  >
                    {step.status}
                  </Badge>
                </li>
              ))}
            </ul>
          ) : (
            <Empty className="py-3">
              <EmptyHeader>
                <EmptyTitle>No active plan items.</EmptyTitle>
              </EmptyHeader>
            </Empty>
          )}
        </section>
      </div>
    </WebSlashPanelShell>
  );
}

type WebSlashBadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "ghost";

type WebSlashStatusSnapshotView = {
  planSteps: DashboardPlanStep[];
};

type WebSlashStatusTokenUsageSectionData = {
  role: string;
  model: string;
  context: {
    percent: number;
    text: string;
  } | null;
  lastTurnParts: string[];
  totalText: string;
  cachedText: string | null;
};


function WebSlashStatusTokenUsageSection({
  section,
}: {
  section: WebSlashStatusTokenUsageSectionData;
}) {
  return (
    <section className="flex flex-col gap-2" aria-label={`${section.role} model usage`}>
      <div className="flex min-w-0 items-center gap-2">
        <Badge variant="outline" className="shrink-0">
          {section.role}
        </Badge>
        <span className="min-w-0 truncate text-sm font-medium text-foreground">
          {section.model}
        </span>
      </div>
      {section.context ? (
        <div className="flex flex-col gap-1">
          <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
            <span>Context</span>
            <span>{section.context.text}</span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-primary"
              style={{
                width: `${Math.min(Math.max(section.context.percent, 0), 1) * 100}%`,
              }}
            />
          </div>
        </div>
      ) : null}
      {section.lastTurnParts.length > 0 ? (
        <p className="text-xs text-muted-foreground">
          Last turn: {section.lastTurnParts.join(" · ")}
        </p>
      ) : null}
      <p className="text-xs text-muted-foreground">
        Total: {section.totalText}
        {section.cachedText ? ` · ${section.cachedText}` : ""}
      </p>
    </section>
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
      <div className="flex min-w-0 flex-col gap-3">
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
      <div className="flex min-w-0 flex-col gap-2">
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
      <div className="min-w-0 rounded-md bg-muted/35 p-2 font-mono text-xs leading-5 text-foreground/90">
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
      <div className="flex min-w-0 flex-col gap-1">
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
      <div className="flex min-w-0 flex-col gap-1">
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
      <div className="flex min-w-0 flex-col gap-1">
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
    <div className="grid min-w-0 grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1 text-sm">
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

function webInputSuggestions(
  input: string,
  snapshot: DashboardSnapshot | null,
): WebInputSuggestion[] {
  if (isWebSlashCommandInput(input)) {
    return webSlashCommandSuggestions(input, snapshot);
  }
  return webSkillMentionSuggestions(input, snapshot);
}

function webSkillMentionSuggestions(
  input: string,
  snapshot: DashboardSnapshot | null,
): WebInputSuggestion[] {
  const target = webSkillCompletionTarget(input);
  if (!target) {
    return [];
  }
  return (snapshot?.skills ?? [])
    .filter((skill) => skill.name.startsWith(target.prefix))
    .filter((skill) => webSkillNameIsUnique(snapshot, skill.name))
    .map((skill) => ({
      display: `$${skill.name}`,
      completion: `${input.slice(0, target.mentionStart)}$${skill.name}`,
      description: webSkillSuggestionDescription(skill),
    }));
}

function webSkillCompletionTarget(input: string) {
  const mentionStart = input.lastIndexOf("$");
  if (mentionStart < 0) {
    return null;
  }
  const prefix = input.slice(mentionStart + 1);
  if (!/^[A-Za-z0-9_:-]*$/.test(prefix)) {
    return null;
  }
  return { mentionStart, prefix };
}

function webSkillNameIsUnique(
  snapshot: DashboardSnapshot | null,
  name: string,
) {
  return (snapshot?.skills ?? []).filter((skill) => skill.name === name).length === 1;
}

function webSkillSuggestionDescription(
  skill: NonNullable<DashboardSnapshot["skills"]>[number],
) {
  const status = webSlashSkillStatusDescription(skill);
  return skill.description ? `${skill.description} — ${status}` : status;
}

function webSlashCommandSuggestions(
  input: string,
  _snapshot: DashboardSnapshot | null,
): WebInputSuggestion[] {
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
): WebInputSuggestion {
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

function webSlashStatusSnapshot(
  snapshot: DashboardSnapshot | null,
): WebSlashStatusSnapshotView {
  const structured = snapshot?.status_command;

  return {
    planSteps:
      structured?.plan_steps ??
      (snapshot?.current_plan_step ? [snapshot.current_plan_step] : []),
  };
}

function webSlashStatusTokenUsageSections(
  snapshot: DashboardSnapshot | null,
): WebSlashStatusTokenUsageSectionData[] {
  const tokenUsage = snapshot?.token_usage;
  return [
    webSlashStatusTokenUsageSection("main", tokenUsage?.main_model, tokenUsage?.main),
    webSlashStatusTokenUsageSection("judge", tokenUsage?.judge_model, tokenUsage?.judge),
  ].filter((section): section is WebSlashStatusTokenUsageSectionData => Boolean(section));
}

function webSlashStatusTokenUsageSection(
  role: string,
  model: string | null | undefined,
  info: TokenUsageInfo | null | undefined,
): WebSlashStatusTokenUsageSectionData | null {
  if (!info || webSlashTokenUsageIsZero(info.total_token_usage)) {
    return null;
  }

  const used = Math.max(0, info.last_token_usage.input_tokens);
  const window = info.model_context_window;
  const context =
    typeof window === "number" && window > 0
      ? {
          percent: used / window,
          text: `${formatWebSlashCompactNumber(used)} of ${formatWebSlashCompactNumber(window)}`,
        }
      : null;
  const lastTurnParts = webSlashTokenUsageParts(info.last_token_usage);
  const totalTokens = Math.max(0, info.total_token_usage.total_tokens);
  const cachedText =
    info.total_token_usage.input_tokens > 0
      ? `${Math.round(
          (info.total_token_usage.cached_input_tokens /
            info.total_token_usage.input_tokens) *
            100,
        )}% cached`
      : null;

  return {
    role,
    model: model?.trim() || "<unknown>",
    context,
    lastTurnParts,
    totalText: `${formatWebSlashCompactNumber(totalTokens)} Used`,
    cachedText,
  };
}

function webSlashTokenUsageParts(usage: {
  input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
}) {
  if (webSlashTokenUsageIsZero(usage)) {
    return [];
  }

  const parts = [
    `${formatWebSlashNumber(Math.max(0, usage.input_tokens))} in`,
    `${formatWebSlashNumber(Math.max(0, usage.output_tokens))} out`,
  ];
  if (usage.reasoning_output_tokens > 0) {
    parts.push(
      `${formatWebSlashNumber(Math.max(0, usage.reasoning_output_tokens))} reasoning`,
    );
  }
  return parts;
}

function webSlashTokenUsageIsZero(usage: { total_tokens: number }) {
  return Math.max(0, usage.total_tokens) === 0;
}


function webSlashPlanStatusBadgeVariant(
  status: DashboardPlanStep["status"],
): WebSlashBadgeVariant {
  if (status === "in_progress") {
    return "default";
  }
  if (status === "completed") {
    return "secondary";
  }
  return "outline";
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
function formatWebSlashCompactNumber(value: number) {
  return new Intl.NumberFormat("en", {
    compactDisplay: "short",
    maximumFractionDigits: value >= 1000 ? 1 : 0,
    notation: "compact",
  }).format(value);
}

function formatWebSlashNumber(value: number) {
  return new Intl.NumberFormat("en").format(value);
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
  const [navHistoryBubbles, setNavHistoryBubbles] = useState<AgentChatBubble[]>([]);
  const [navOldestCursor, setNavOldestCursor] = useState<number | null>(null);
  const [hasMoreNavBefore, setHasMoreNavBefore] = useState(false);
  const [isLoadingNavHistory, setIsLoadingNavHistory] = useState(false);
  const [navHistoryError, setNavHistoryError] = useState<string | null>(null);
  const navHistoryAbortRef = useRef<AbortController | null>(null);
  const lastFocusedScrollTopRef = useRef(0);
  const hasFocusedScrollPositionRef = useRef(false);
  const shouldRestoreFocusScrollRef = useRef(false);
  const isFocusedNearBottomRef = useRef(true);
  const restoreAfterPrependRef = useRef<{
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const historySessionIdRef = useRef<string | null>(null);
  const loadedOlderHistoryRef = useRef(false);
  const navHistorySessionIdRef = useRef<string | null>(null);
  const navHistoryInitializedRef = useRef(false);
  const [showScrollToBottom, setShowScrollToBottom] = useState(false);
  const bubbles = useMemo(
    () => mergeAgentChatBubbles(historyBubbles, snapshotBubbles),
    [historyBubbles, snapshotBubbles],
  );
  const displayItems = useMemo(
    () =>
      foldCompletedAgentChatActivity(bubbles, {
        isOutputBoundary: agentChatBubbleIsOutputBoundary,
      }),
    [bubbles],
  );
  const [openFoldedActivityGroups, setOpenFoldedActivityGroups] = useState<
    Record<string, boolean>
  >({});
  const [activeQuickNavItemId, setActiveQuickNavItemId] = useState<
    string | null
  >(null);
  const quickNavItems = useMemo(() => {
    const allNavBubbles = mergeAgentChatBubbles(navHistoryBubbles, bubbles);
    return allNavBubbles
      .flatMap((bubble): AgentChatQuickNavItem[] => {
        const label = agentChatQuickNavLabelForBubble(bubble);
        return label
          ? [
              {
                id: bubble.id,
                label,
                order: agentChatQuickNavOrderForBubble(bubble),
              },
            ]
          : [];
      })
      .sort(agentChatQuickNavItemCompare);
  }, [bubbles, navHistoryBubbles]);
  const visibleQuickNavItems = useMemo(
    () => quickNavItems.slice(-AGENT_CHAT_QUICK_NAV_MAX_ITEMS),
    [quickNavItems],
  );
  const displayQuickNavTargets = useMemo(() => {
    const visibleQuickNavItemIds = new Set(
      visibleQuickNavItems.map((item) => item.id),
    );
    return displayItems
      .map((item): AgentChatQuickNavDisplayTarget | null => {
        const quickNavItemId = agentChatFoldDisplayItemQuickNavTargetId(item);
        if (!quickNavItemId || !visibleQuickNavItemIds.has(quickNavItemId)) {
          return null;
        }
        return { id: item.id, quickNavItemId };
      })
      .filter((item): item is AgentChatQuickNavDisplayTarget => Boolean(item));
  }, [displayItems, visibleQuickNavItems]);
  const navReachedMax = quickNavItems.length >= AGENT_CHAT_QUICK_NAV_MAX_ITEMS;
  const displayItemElementsRef = useRef(new Map<string, HTMLDivElement>());

  const [pendingQuickNavTargetId, setPendingQuickNavTargetId] = useState<
    string | null
  >(null);
  const pendingFoldCollapseAnchorRef = useRef<{
    id: string;
    top: number;
  } | null>(null);
  const registerDisplayItemElement = useCallback(
    (id: string, node: HTMLDivElement | null) => {
      if (node) {
        displayItemElementsRef.current.set(id, node);
      } else {
        displayItemElementsRef.current.delete(id);
      }
    },
    [],
  );

  const updateQuickNavActiveItem = useCallback(
    (panel: HTMLDivElement) => {
      if (visibleQuickNavItems.length === 0) {
        setActiveQuickNavItemId((current) => (current === null ? current : null));
        return;
      }

      const panelRect = panel.getBoundingClientRect();
      const targetY = panelRect.top + Math.min(panelRect.height * 0.38, 240);
      let nextActiveId: string | null = null;
      let bestDistance = Number.POSITIVE_INFINITY;

      for (const target of displayQuickNavTargets) {
        const element = displayItemElementsRef.current.get(target.id);
        if (!element) {
          continue;
        }

        const rect = element.getBoundingClientRect();
        if (rect.bottom < panelRect.top || rect.top > panelRect.bottom) {
          continue;
        }

        const distance = Math.abs(rect.top - targetY);
        if (distance < bestDistance) {
          bestDistance = distance;
          nextActiveId = target.quickNavItemId;
        }
      }

      nextActiveId ??= visibleQuickNavItems[0]?.id ?? null;
      setActiveQuickNavItemId((current) =>
        current === nextActiveId ? current : nextActiveId,
      );
    },
    [displayQuickNavTargets, visibleQuickNavItems],
  );
  const handleFoldedActivityGroupOpenChange = useCallback(
    (id: string, nextOpen: boolean) => {
      if (!nextOpen) {
        const panel = panelRef.current;
        const element = displayItemElementsRef.current.get(id);
        const header = element?.querySelector<HTMLElement>(
          "[data-agent-chat-fold-header='true']",
        );
        if (panel && element) {
          pendingFoldCollapseAnchorRef.current = {
            id,
            top: (header ?? element).getBoundingClientRect().top,
          };
        }
      }

      setOpenFoldedActivityGroups((current) => {
        if (Boolean(current[id]) === nextOpen) {
          return current;
        }
        if (!nextOpen) {
          const next = { ...current };
          delete next[id];
          return next;
        }
        return { ...current, [id]: true };
      });
    },
    [panelRef],
  );

  const scrollToChatBottom = useCallback(
    (behavior: ScrollBehavior = "auto") => {
      const panel = panelRef.current;
      if (!panel) {
        return;
      }

      panel.scrollTo({
        top: panel.scrollHeight,
        behavior,
      });
    },
    [panelRef],
  );

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
    updateQuickNavActiveItem(panel);
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

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }

    updateQuickNavActiveItem(panel);
  }, [visibleQuickNavItems, panelRef, updateQuickNavActiveItem]);

  const scrollToQuickNavTarget = useCallback(
    (id: string, behavior: ScrollBehavior = "smooth") => {
      const panel = panelRef.current;
      const element = displayItemElementsRef.current.get(id);
      if (!panel || !element) {
        return false;
      }

      const panelRect = panel.getBoundingClientRect();
      const elementRect = element.getBoundingClientRect();
      const top = Math.max(
        0,
        panel.scrollTop +
          elementRect.top -
          panelRect.top -
          AGENT_CHAT_QUICK_NAV_SCROLL_OFFSET_PX,
      );
      const distanceFromBottom = panel.scrollHeight - panel.clientHeight - top;

      isFocusedNearBottomRef.current =
        distanceFromBottom <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX;
      lastFocusedScrollTopRef.current = top;
      setActiveQuickNavItemId(id);
      setShowScrollToBottom(
        distanceFromBottom > AGENT_CHAT_SCROLL_BUTTON_THRESHOLD_PX,
      );
      panel.scrollTo({ top, behavior });
      return true;
    },
    [panelRef],
  );

  const handleQuickNavSelect = useCallback(
    (id: string) => {
      setActiveQuickNavItemId(id);
      if (scrollToQuickNavTarget(id)) {
        setPendingQuickNavTargetId(null);
        return;
      }
      setPendingQuickNavTargetId(id);
    },
    [scrollToQuickNavTarget],
  );

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
      const olderBubbles = agentChatBubblesFromHistoryPage(page);
      if (olderBubbles.length > 0) {
        loadedOlderHistoryRef.current = true;
      }
      setHistoryBubbles((current) =>
        mergeAgentChatBubbles(olderBubbles, current),
      );
      setNavHistoryBubbles((current) =>
        mergeAgentChatBubbles(olderBubbles, current),
      );
      setOldestCursor(page.oldest_cursor ?? oldestCursor);
      setNavOldestCursor((current) => {
        const nextCursor = page.oldest_cursor ?? current;
        if (nextCursor === null) {
          return null;
        }
        return current === null ? nextCursor : Math.min(current, nextCursor);
      });
      setHasMoreBefore(page.has_more_before);
      setHasMoreNavBefore(page.has_more_before);
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
    if (!pendingQuickNavTargetId) {
      return;
    }

    if (scrollToQuickNavTarget(pendingQuickNavTargetId)) {
      setPendingQuickNavTargetId(null);
      return;
    }

    if (!hasMoreBefore || isLoadingHistory || oldestCursor === null) {
      if (!hasMoreBefore) {
        setPendingQuickNavTargetId(null);
      }
      return;
    }

    void loadOlderHistory();
  }, [
    displayItems.length,
    hasMoreBefore,
    isLoadingHistory,
    loadOlderHistory,
    oldestCursor,
    pendingQuickNavTargetId,
    scrollToQuickNavTarget,
  ]);

  const loadOlderNavHistory = useCallback(async () => {
    if (
      isLoadingNavHistory ||
      !hasMoreNavBefore ||
      navOldestCursor === null ||
      navReachedMax
    ) {
      return;
    }

    navHistoryAbortRef.current?.abort();
    const controller = new AbortController();
    navHistoryAbortRef.current = controller;
    setIsLoadingNavHistory(true);
    setNavHistoryError(null);
    try {
      const page = await fetchDashboardActivityHistory({
        before: navOldestCursor,
        limit: AGENT_CHAT_NAV_HISTORY_PAGE_LIMIT,
        sessionId,
        signal: controller.signal,
      });
      const olderBubbles = agentChatBubblesFromHistoryPage(page);
      setNavHistoryBubbles((current) =>
        mergeAgentChatBubbles(olderBubbles, current),
      );
      setNavOldestCursor(page.oldest_cursor ?? navOldestCursor);
      setHasMoreNavBefore(page.has_more_before);
    } catch (error) {
      if (!controller.signal.aborted) {
        setNavHistoryError(error instanceof Error ? error.message : String(error));
      }
    } finally {
      if (navHistoryAbortRef.current === controller) {
        navHistoryAbortRef.current = null;
      }
      if (!controller.signal.aborted) {
        setIsLoadingNavHistory(false);
      }
    }
  }, [
    hasMoreNavBefore,
    isLoadingNavHistory,
    navOldestCursor,
    navReachedMax,
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
    navHistoryAbortRef.current?.abort();
    navHistoryAbortRef.current = null;
    navHistorySessionIdRef.current = null;
    navHistoryInitializedRef.current = false;
    setPendingQuickNavTargetId(null);
    setNavHistoryBubbles([]);
    setNavOldestCursor(null);
    setHasMoreNavBefore(false);
    setIsLoadingNavHistory(false);
    setNavHistoryError(null);
  }, [sessionId]);

  useEffect(() => {
    const historyWindow = snapshot?.activity_history;
    const committedBubbles = agentChatCommittedBubblesFromSnapshot(snapshot);
    const snapshotOldestCursor = historyWindow?.oldest_cursor ?? null;
    const snapshotNewestCursor = historyWindow?.newest_cursor ?? null;
    const sessionChanged = historySessionIdRef.current !== sessionId;
    const historyCleared =
      committedBubbles.length === 0 && snapshotNewestCursor === null;

    historySessionIdRef.current = sessionId;

    if (sessionChanged || historyCleared || !loadedOlderHistoryRef.current) {
      loadedOlderHistoryRef.current = false;
      setHistoryBubbles(committedBubbles);
      setOldestCursor(snapshotOldestCursor);
      setHasMoreBefore(Boolean(historyWindow?.has_more_before));
    } else {
      setHistoryBubbles((current) =>
        mergeAgentChatBubbles(current, committedBubbles),
      );
      setOldestCursor((current) => {
        if (snapshotOldestCursor === null) {
          return current;
        }
        if (current === null) {
          return snapshotOldestCursor;
        }
        return Math.min(current, snapshotOldestCursor);
      });
    }

    const navSessionChanged = navHistorySessionIdRef.current !== sessionId;
    navHistorySessionIdRef.current = sessionId;
    if (navSessionChanged || historyCleared || !navHistoryInitializedRef.current) {
      navHistoryInitializedRef.current = true;
      setNavHistoryBubbles(committedBubbles);
      setNavOldestCursor(snapshotOldestCursor);
      setHasMoreNavBefore(Boolean(historyWindow?.has_more_before));
    } else {
      setNavHistoryBubbles((current) =>
        mergeAgentChatBubbles(current, committedBubbles),
      );
      setNavOldestCursor((current) => {
        if (snapshotOldestCursor === null) {
          return current;
        }
        if (current === null) {
          return snapshotOldestCursor;
        }
        return Math.min(current, snapshotOldestCursor);
      });
      setHasMoreNavBefore((current) =>
        current || Boolean(historyWindow?.has_more_before),
      );
    }

    setHistoryError(null);
    setNavHistoryError(null);
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
      updateQuickNavActiveItem(panel);
      restoreAfterPrependRef.current = null;
    });
  }, [historyBubbles.length, panelRef, updateQuickNavActiveItem]);

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
        updateQuickNavActiveItem(latestPanel);
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
  }, [bubbles.length, panelRef, scrollToChatBottom, updateQuickNavActiveItem]);

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
  useEffect(() => {
    const foldedIds = new Set(
      displayItems
        .filter((item) => item.kind === "foldedActivityGroup")
        .map((item) => item.id),
    );

    setOpenFoldedActivityGroups((current) => {
      let changed = false;
      const next: Record<string, boolean> = {};
      for (const [id, open] of Object.entries(current)) {
        if (foldedIds.has(id)) {
          next[id] = open;
        } else {
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [displayItems]);

  useLayoutEffect(() => {
    const anchor = pendingFoldCollapseAnchorRef.current;
    if (!anchor) {
      return;
    }
    pendingFoldCollapseAnchorRef.current = null;

    const panel = panelRef.current;
    const element = displayItemElementsRef.current.get(anchor.id);
    if (!panel || !element) {
      return;
    }

    const nextTop = element.getBoundingClientRect().top;
    panel.scrollTop += nextTop - anchor.top;
    lastFocusedScrollTopRef.current = panel.scrollTop;
    updateScrollButtonVisibility(panel);
    updateQuickNavActiveItem(panel);
  }, [openFoldedActivityGroups, panelRef, updateQuickNavActiveItem]);


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
              {isLoadingHistory || historyError ? (
                <div className="flex justify-center py-1">
                  {isLoadingHistory ? (
                    <div className="flex items-center gap-2 rounded-full border border-border/70 bg-background/80 px-3 py-1 text-xs text-muted-foreground shadow-sm backdrop-blur-xl">
                      <Spinner data-icon="inline-start" />
                      Loading older…
                    </div>
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
              {displayItems.map((item) => (
                <div
                  key={item.id}
                  ref={(node) => registerDisplayItemElement(item.id, node)}
                  className="min-w-0 max-w-full"
                >
                  {item.kind === "bubble" ? (
                    <AgentChatBubbleItem bubble={item.bubble} />
                  ) : (
                    <AgentChatFoldedActivityGroup
                      id={item.id}
                      bubbles={item.bubbles}
                      open={Boolean(openFoldedActivityGroups[item.id])}
                      onOpenChange={(nextOpen) =>
                        handleFoldedActivityGroupOpenChange(item.id, nextOpen)
                      }
                    />
                  )}
                </div>
              ))}
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
      <AgentChatQuickNavigation
        items={visibleQuickNavItems}
        activeItemId={activeQuickNavItemId}
        hasMoreBefore={hasMoreNavBefore && !navReachedMax}
        isLoadingHistory={isLoadingNavHistory}
        historyError={navHistoryError}
        onNearTop={() => {
          void loadOlderNavHistory();
        }}
        onSelect={handleQuickNavSelect}
      />
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

function AgentChatQuickNavigation({
  items,
  activeItemId,
  hasMoreBefore,
  isLoadingHistory,
  historyError,
  onNearTop,
  onSelect,
}: {
  items: AgentChatQuickNavItem[];
  activeItemId: string | null;
  hasMoreBefore: boolean;
  isLoadingHistory: boolean;
  historyError: string | null;
  onNearTop: () => void;
  onSelect: (id: string) => void;
}) {
  const navListRef = useRef<HTMLDivElement>(null);
  const collapsedItems = useMemo(
    () => agentChatQuickNavCollapsedItems(items, activeItemId),
    [activeItemId, items],
  );

  const handleNavScroll = useCallback(() => {
    const list = navListRef.current;
    if (!list || isLoadingHistory || !hasMoreBefore) {
      return;
    }

    if (list.scrollTop <= AGENT_CHAT_STICKY_BOTTOM_THRESHOLD_PX) {
      onNearTop();
    }
  }, [hasMoreBefore, isLoadingHistory, onNearTop]);

  useEffect(() => {
    handleNavScroll();
  }, [handleNavScroll, items.length]);

  if (items.length === 0 && !hasMoreBefore && !isLoadingHistory && !historyError) {
    return null;
  }

  return (
    <nav
      aria-label="User message quick navigation"
      className="group fixed top-1/2 right-4 z-50 flex h-[min(26rem,calc(100vh-1rem))] -translate-y-1/2 items-center justify-end"
    >
      {items.length > 0 || hasMoreBefore || isLoadingHistory ? (
        <div
          aria-hidden="true"
          className="flex h-full max-h-[calc(100vh-1rem)] w-10 flex-col items-end justify-center gap-2.5 rounded-full px-2 py-2.5 transition-opacity duration-150 group-hover:opacity-0 group-focus-within:opacity-0"
        >
          {collapsedItems.length > 0 ? (
            collapsedItems.map((item) => (
              <span
                key={item.id}
                className={cn(
                  "h-[3px] w-6 rounded-full bg-muted-foreground/45 transition-colors",
                  item.id === activeItemId && "bg-foreground",
                )}
              />
            ))
          ) : (
            <span className="h-[3px] w-6 rounded-full bg-muted-foreground/45" />
          )}
        </div>
      ) : null}
      <div className="pointer-events-none absolute top-1/2 right-0 w-[min(17rem,calc(100vw-1rem))] -translate-y-1/2 overflow-hidden rounded-lg border border-border/70 bg-background opacity-0 shadow-lg shadow-background/20 transition-opacity duration-150 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100">
        <div
          ref={navListRef}
          onScroll={handleNavScroll}
          className="flex max-h-[min(26rem,calc(100vh-1rem))] flex-col gap-1 overflow-y-auto p-1.5"
        >
          {isLoadingHistory ? (
            <div className="flex min-h-9 items-center gap-2 rounded-md px-2.5 py-1.5 text-sm leading-5 text-muted-foreground">
              <Spinner data-icon="inline-start" />
              Loading older…
            </div>
          ) : null}
          {historyError ? (
            <div className="rounded-md px-2.5 py-1.5 text-sm text-destructive">
              {historyError}
            </div>
          ) : null}
          {items.map((item) => {
            const active = item.id === activeItemId;

            return (
              <button
                key={item.id}
                type="button"
                aria-current={active ? "location" : undefined}
                title={item.label}
                onClick={() => onSelect(item.id)}
                className={cn(
                  "min-h-8 w-full rounded-md px-2.5 py-1.5 text-left text-sm leading-5 text-foreground/90 transition-colors hover:bg-muted/70 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
                  active && "bg-muted text-foreground",
                )}
              >
                <span className="block truncate">{item.label}</span>
              </button>
            );
          })}
        </div>
      </div>
    </nav>
  );
}

function AgentChatFoldedActivityGroup({
  id,
  bubbles,
  open,
  onOpenChange,
  isFocused = true,
}: {
  id: string;
  bubbles: AgentChatBubble[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  isFocused?: boolean;
}) {
  const { toggle } = useCollapsibleState(false, open, onOpenChange);
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
        <div
          data-agent-chat-fold-header="true"
          className={cn(
            "min-w-0 max-w-full",
            open &&
              "sticky top-2 z-20 rounded-md bg-background/95 shadow-sm shadow-background/30 backdrop-blur-xl supports-[backdrop-filter]:bg-background/80",
          )}
        >
          <AgentChatWorkedDivider
            label={`Worked for ${workedDurationLabel}`}
            open={open}
            onToggle={isFocused ? toggle : undefined}
          />
        </div>
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
        AGENT_CHAT_ACTIVITY_ROW_CLASS,
        "text-foreground",
        !isFocused && "opacity-90",
      )}
    >
      <AgentChatActivityMarker
        icon={icon}
        tone={icon === "error" ? "error" : "default"}
        className={cn(
          agentChatActivityIconClass(bubble),
          isRunning && "motion-safe:animate-pulse",
          !isFocused && "text-xs",
        )}
      />
      <div className="min-w-0">
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
              {isRunning ? <Spinner className="size-2.5" /> : null}
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

  if (render.kind === "runtimeStatus") {
    return (
      <AgentChatRuntimeStatusCell
        icon={render.icon}
        title={render.title}
        detail={render.detail}
        startedAtMs={render.startedAtMs}
        reducedMotion={render.reducedMotion}
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

const AGENT_CHAT_ACTIVITY_ROW_CLASS =
  "grid min-w-0 grid-cols-[0.75rem_minmax(0,1fr)] items-start gap-x-3 px-2 sm:gap-x-[16px] sm:px-3";
const AGENT_CHAT_ACTIVITY_DETAIL_ROWS_CLASS =
  "grid min-w-0 grid-cols-[1.5rem_minmax(0,1fr)] px-2 text-sm leading-6 sm:grid-cols-[1.75rem_minmax(0,1fr)] sm:px-3";

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
        AGENT_CHAT_ACTIVITY_DETAIL_ROWS_CLASS,
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

function AgentChatRuntimeStatusCell({
  icon,
  title,
  detail,
  startedAtMs,
  reducedMotion,
}: {
  icon: AgentChatActivityMarkerKind;
  title: string;
  detail?: string | null;
  startedAtMs?: number | null;
  reducedMotion?: string | null;
}) {
  const [nowMs, setNowMs] = useState(() => Date.now());
  const elapsedText = formatAgentChatDuration(
    startedAtMs ? Math.max(0, nowMs - startedAtMs) : 0,
  );
  const detailText = detail?.trim();
  const shouldAnimate = reducedMotion !== "Reduced";

  useEffect(() => {
    if (!startedAtMs) {
      return;
    }

    const interval = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, [startedAtMs]);

  return (
    <div
      className={cn(
        AGENT_CHAT_ACTIVITY_ROW_CLASS,
        "text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]",
      )}
    >
      <AgentChatActivityMarker
        icon={icon}
        className={cn(
          shouldAnimate && "agent-chat-runtime-marker-flash text-primary",
        )}
      />
      <p className="min-w-0 break-words">
        <span className="font-semibold text-foreground">
          {shouldAnimate ? (
            <AgentChatRuntimeShimmerText
              text={title}
              startedAtMs={startedAtMs}
            />
          ) : (
            <AgentChatMarkdownInline text={title} />
          )}
        </span>{" "}
        <span className="text-muted-foreground">({elapsedText})</span>
        {detailText ? (
          <>
            <span className="text-muted-foreground"> — </span>
            <span className="text-muted-foreground">{detailText}</span>
          </>
        ) : null}
      </p>
    </div>
  );
}

function AgentChatRuntimeShimmerText({
  text,
  startedAtMs,
}: {
  text: string;
  startedAtMs?: number | null;
}) {
  const baseDelayMs = useMemo(() => {
    if (!startedAtMs) {
      return 0;
    }

    return -(
      Math.max(0, Date.now() - startedAtMs) % AGENT_CHAT_RUNTIME_SHIMMER_MS
    );
  }, [startedAtMs]);

  return (
    <span className="agent-chat-runtime-shimmer" aria-label={text}>
      {Array.from(text).map((char, index) => (
        <span
          key={`${index}-${char}`}
          aria-hidden="true"
          className="agent-chat-runtime-shimmer-letter"
          style={{
            animationDelay: `${
              baseDelayMs + index * AGENT_CHAT_RUNTIME_SHIMMER_STAGGER_MS
            }ms`,
          }}
        >
          {char}
        </span>
      ))}
    </span>
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
      <div className={AGENT_CHAT_ACTIVITY_ROW_CLASS}>
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
            "flex min-w-0 max-w-full flex-col gap-0.5 pl-8 pr-2 text-muted-foreground sm:pl-10 sm:pr-3",
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
    <div
      className={cn(
        AGENT_CHAT_ACTIVITY_ROW_CLASS,
        "max-w-full text-sm leading-6 text-foreground/90 [overflow-wrap:anywhere]",
      )}
    >
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
      <div className={cn(AGENT_CHAT_ACTIVITY_ROW_CLASS, "leading-6")}>
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
      <div className={cn(AGENT_CHAT_ACTIVITY_ROW_CLASS, "leading-6")}>
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

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1 text-sm [overflow-wrap:anywhere]">
      <div className={cn(AGENT_CHAT_ACTIVITY_ROW_CLASS, "leading-6")}>
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
  const rows = files.map((file, index) => (
    <AgentChatPatchFileBlock
      key={`${file.path}-${index}`}
      file={file}
      hideHeader={files.length === 1}
    />
  ));

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1.5 text-sm [overflow-wrap:anywhere]">
      <div className={cn(AGENT_CHAT_ACTIVITY_ROW_CLASS, "leading-6")}>
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {files.length > 0 ? (
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
  const oldWidth = agentChatDiffLineNumberWidth(file.lines, "old_lineno");
  const newWidth = agentChatDiffLineNumberWidth(file.lines, "new_lineno");
  const highlighted = useShikiHighlightedCode(
    agentChatDiffHighlightSource(file.lines),
    file.path,
  );

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
      {file.lines.length > 0 ? (
        <div className="min-w-0 max-w-full overflow-x-auto font-mono text-xs leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin]">
          {file.lines.map((line, index) => (
            <AgentChatPatchDiffRow
              key={`patch-line-${index}`}
              line={line}
              oldWidth={oldWidth}
              newWidth={newWidth}
              highlightedLine={highlighted?.lines[index]}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function AgentChatPatchDiffRow({
  line,
  oldWidth,
  newWidth,
  highlightedLine,
}: {
  line: AgentChatDiffLine;
  oldWidth: number;
  newWidth: number;
  highlightedLine?: ShikiHighlightToken[];
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
        agentChatDiffRowToneClassName(line.kind),
      )}
    >
      <span className="select-none text-right text-muted-foreground/65">
        {oldLineNumber}
      </span>
      <span className="select-none text-right text-muted-foreground/65">
        {newLineNumber}
      </span>
      <span
        className={cn(
          "select-none font-semibold text-muted-foreground",
          agentChatDiffGutterToneClassName(line.kind),
        )}
      >
        {gutter}
      </span>
      <span className="whitespace-pre-wrap break-words text-foreground/85 sm:whitespace-pre">
        <AgentChatHighlightedInline
          tokens={highlightedLine}
          fallback={line.text}
        />
      </span>
    </div>
  );
}

function agentChatDiffHighlightSource(lines: AgentChatDiffLine[]) {
  return lines
    .map((line) => (line.kind === "hunk_break" ? "" : line.text))
    .join("\n");
}

function agentChatDiffLinePrefix(line: AgentChatDiffLine) {
  if (line.kind === "hunk_break") {
    return "  ";
  }
  return line.kind === "add" ? "+ " : line.kind === "delete" ? "- " : "  ";
}

function agentChatDiffRowToneClassName(kind: string) {
  if (kind === "add") {
    return "bg-emerald-50/90 dark:bg-emerald-500/10";
  }
  if (kind === "delete") {
    return "bg-red-50/90 dark:bg-red-500/10";
  }
  return "";
}

function agentChatDiffGutterToneClassName(kind: string) {
  if (kind === "add") {
    return "text-emerald-700 dark:text-emerald-300/90";
  }
  if (kind === "delete") {
    return "text-red-700 dark:text-red-300/90";
  }
  return "";
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
      <div className={AGENT_CHAT_ACTIVITY_ROW_CLASS}>
        <AgentChatActivityMarker icon={icon} />
        <p className="min-w-0 break-words font-semibold text-foreground">
          {title}
        </p>
      </div>
      {visibleDetailLines.length > 0 || hiddenDetailCount > 0 ? (
        <div className="flex min-w-0 max-w-full flex-col gap-0.5 pl-8 pr-2 text-xs leading-5 text-muted-foreground sm:pl-10 sm:pr-3">
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
      <div className={cn(AGENT_CHAT_ACTIVITY_ROW_CLASS, "leading-6")}>
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
      <AgentChatDiffBlock id={blockId} files={diffFilesValue(record.files)} />
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

type MarkdownCodeElementProps = {
  className?: string;
  children?: ReactNode;
};

function markdownNodeText(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") {
    return "";
  }
  if (
    typeof node === "string" ||
    typeof node === "number" ||
    typeof node === "bigint"
  ) {
    return String(node);
  }
  if (Array.isArray(node)) {
    return node.map(markdownNodeText).join("");
  }
  if (isValidElement(node)) {
    return markdownNodeText(
      (node.props as { children?: ReactNode }).children,
    );
  }
  return "";
}

function markdownCodeLanguage(className: unknown): string {
  return (
    String(className ?? "")
      .split(/\s+/)
      .find((name) => name.startsWith("language-"))
      ?.replace(/^language-/, "") ?? ""
  );
}

function markdownPreCodeProps(children: ReactNode) {
  const child = Array.isArray(children)
    ? children.find((item) => isValidElement(item))
    : children;
  if (!isValidElement(child)) {
    return null;
  }
  return child.props as MarkdownCodeElementProps;
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
  const markdownId = useId();

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
          pre: ({ children }: { children?: ReactNode }) => {
            const codeProps = markdownPreCodeProps(children);
            if (codeProps) {
              const language = markdownCodeLanguage(codeProps.className);
              const code = markdownNodeText(codeProps.children).replace(/\n$/, "");
              return (
                <AgentChatCodeBlock
                  id={`${markdownId}-code-${language || "plain"}`}
                  code={code}
                  language={language}
                  limit={limit}
                />
              );
            }

            return (
              <pre className="max-w-full overflow-auto whitespace-pre-wrap rounded-md bg-muted/45 px-3 py-2 font-mono text-xs leading-5 text-foreground/90">
                {children}
              </pre>
            );
          },
          code: ({ children }: { children?: ReactNode }) => {
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

function useShikiHighlightedCode(code: string, languageOrPath: string) {
  const colorScheme = useAgentChatCodeColorScheme();
  const [highlighted, setHighlighted] = useState<ShikiHighlightedCode | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    setHighlighted(null);
    void highlightCodeWithShiki(code, languageOrPath, colorScheme).then(
      (nextHighlighted) => {
        if (!cancelled) {
          setHighlighted(nextHighlighted);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [code, colorScheme, languageOrPath]);

  return highlighted;
}

function useAgentChatCodeColorScheme(): ShikiColorScheme {
  const [colorScheme, setColorScheme] = useState(agentChatCodeColorScheme);

  useEffect(() => {
    if (typeof document === "undefined") {
      return;
    }

    const root = document.documentElement;
    const update = () => setColorScheme(agentChatCodeColorScheme());
    const observer = new MutationObserver(update);
    observer.observe(root, { attributes: true, attributeFilter: ["class"] });

    return () => observer.disconnect();
  }, []);

  return colorScheme;
}

function agentChatCodeColorScheme(): ShikiColorScheme {
  if (
    typeof document !== "undefined" &&
    document.documentElement.classList.contains("dark")
  ) {
    return "dark";
  }
  return "light";
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
  const highlighted = useShikiHighlightedCode(code, language);
  const visibleHighlightedLines = highlighted?.lines.slice(0, limit) ?? null;

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
          {visibleHighlightedLines ? (
            <AgentChatHighlightedCodeLines
              lines={visibleHighlightedLines}
              lineKeyPrefix={`${id}-line`}
            />
          ) : (
            visibleLines.join("\n")
          )}
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

function AgentChatHighlightedCodeLines({
  lines,
  lineKeyPrefix,
}: {
  lines: ShikiHighlightToken[][];
  lineKeyPrefix: string;
}) {
  return (
    <>
      {lines.map((line, lineIndex) => (
        <Fragment key={`${lineKeyPrefix}-${lineIndex}`}>
          <AgentChatHighlightedInline
            tokens={line}
            fallback={line.length === 0 ? " " : ""}
          />
          {lineIndex < lines.length - 1 ? "\n" : null}
        </Fragment>
      ))}
    </>
  );
}

function AgentChatHighlightedInline({
  tokens,
  fallback,
}: {
  tokens: ShikiHighlightToken[] | null | undefined;
  fallback: string;
}) {
  if (!tokens || tokens.length === 0) {
    return <>{fallback}</>;
  }

  return (
    <>
      {tokens.map((token, index) => (
        <span
          key={`${index}-${token.content}`}
          style={agentChatShikiTokenStyle(token)}
        >
          {token.content}
        </span>
      ))}
    </>
  );
}

function agentChatShikiTokenStyle(token: ShikiHighlightToken): CSSProperties {
  const style: CSSProperties = {};
  if (token.color) {
    style.color = token.color;
  }
  if (typeof token.fontStyle === "number") {
    if (token.fontStyle & 1) {
      style.fontStyle = "italic";
    }
    if (token.fontStyle & 2) {
      style.fontWeight = 700;
    }
    if (token.fontStyle & 4) {
      style.textDecorationLine = "underline";
    }
  }
  return style;
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

  return (
    labels[normalized] ??
    `${normalized[0]?.toUpperCase() ?? ""}${normalized.slice(1)}`
  );
}

function AgentChatDiffBlock({
  id,
  files,
}: {
  id: string;
  files: AgentChatDiffFile[];
}) {
  if (files.length === 0) {
    return null;
  }

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-2 font-mono text-xs">
      {files.map((file, fileIndex) => (
        <AgentChatDiffBlockFile key={`${id}-file-${fileIndex}`} file={file} />
      ))}
    </div>
  );
}

function AgentChatDiffBlockFile({
  file,
}: {
  file: AgentChatDiffFile;
}) {
  const highlighted = useShikiHighlightedCode(
    agentChatDiffHighlightSource(file.lines),
    file.path,
  );

  return (
    <div className="flex min-w-0 max-w-full flex-col gap-1">
      <div className="flex items-center justify-between gap-3 px-2 sm:px-3">
        <p className="min-w-0 truncate text-foreground/85">{file.path}</p>
        <span className="shrink-0 font-sans text-[0.68rem] text-muted-foreground">
          <span className="text-primary">+{file.added_lines}</span>{" "}
          <span className="text-destructive">-{file.removed_lines}</span>
        </span>
      </div>
      <pre className="max-h-72 min-w-0 max-w-full overflow-auto whitespace-pre-wrap px-2 leading-5 [scrollbar-color:hsl(var(--muted-foreground)/0.35)_transparent] [scrollbar-width:thin] sm:whitespace-pre sm:px-3">
        {file.lines.map((line, lineIndex) => (
          <Fragment key={`${file.path}-legacy-diff-${lineIndex}`}>
            <span
              className={cn(
                "inline-block min-w-full",
                agentChatDiffRowToneClassName(line.kind),
              )}
            >
              <span
                className={cn(
                  "text-muted-foreground",
                  agentChatDiffGutterToneClassName(line.kind),
                )}
              >
                {agentChatDiffLinePrefix(line)}
              </span>
              <AgentChatHighlightedInline
                tokens={highlighted?.lines[lineIndex]}
                fallback={line.text}
              />
            </span>
            {lineIndex < file.lines.length - 1 ? "\n" : null}
          </Fragment>
        ))}
      </pre>
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

function agentChatBubbleIsOutputBoundary(bubble: AgentChatBubble) {
  return agentChatBubbleHasActivityCellVariant(bubble, "Reply");
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

function agentChatBubbleIsUserInput(bubble: AgentChatBubble) {
  return (
    bubble.kind === "message" &&
    (bubble.role === "user" || bubble.role === "telegram") &&
    !agentChatBubbleHasActivityCellVariant(bubble, "Assistant") &&
    !agentChatBubbleHasActivityCellVariant(bubble, "Reply") &&
    !agentChatBubbleHasActivityCellVariant(bubble, "Thinking")
  );
}

function agentChatFoldDisplayItemQuickNavTargetId(
  item: AgentChatFoldDisplayItem<AgentChatBubble>,
) {
  if (item.inputBoundaryId) {
    return item.inputBoundaryId;
  }

  if (
    item.kind === "bubble" &&
    item.bubble.uiHint !== "final-message-separator" &&
    agentChatBubbleIsUserInput(item.bubble)
  ) {
    return item.id;
  }

  return null;
}

function agentChatQuickNavLabelForBubble(bubble: AgentChatBubble) {
  if (
    bubble.uiHint === "final-message-separator" ||
    !agentChatBubbleIsUserInput(bubble)
  ) {
    return null;
  }

  const cell = bubble.cell;
  const user = agentChatActivityCellPayload(cell, "User");
  if (user) {
    return agentChatQuickNavLabelFromPayload(user, bubble.title);
  }

  const telegram = agentChatActivityCellPayload(cell, "Telegram");
  if (telegram) {
    return agentChatQuickNavLabelFromPayload(telegram, bubble.title);
  }

  return agentChatQuickNavNormalizeLabel(
    agentChatQuickNavLabelFromBlocks(bubble.blocks) ?? bubble.title,
  );
}

function agentChatQuickNavOrderForBubble(bubble: AgentChatBubble) {
  const historySequence = agentChatHistorySequenceFromId(bubble.id);
  if (historySequence !== null) {
    return historySequence;
  }

  if (Number.isFinite(bubble.createdAt) && bubble.createdAt > 0) {
    return bubble.createdAt;
  }

  if (Number.isFinite(bubble.updatedAt) && bubble.updatedAt > 0) {
    return bubble.updatedAt;
  }

  return Number.MAX_SAFE_INTEGER;
}

function agentChatQuickNavItemCompare(
  left: AgentChatQuickNavItem,
  right: AgentChatQuickNavItem,
) {
  if (left.order !== right.order) {
    return left.order - right.order;
  }

  return left.id.localeCompare(right.id);
}

function agentChatHistorySequenceFromId(id: string) {
  const match = /^history-(\d+)$/.exec(id);
  if (!match) {
    return null;
  }

  const sequence = Number(match[1]);
  return Number.isFinite(sequence) ? sequence : null;
}

function agentChatQuickNavLabelFromPayload(
  payload: Record<string, unknown>,
  fallback: string,
) {
  const candidates = [
    nullableStringValue(payload.full_body),
    ...stringArrayValue(payload.message_lines),
    nullableStringValue(payload.title),
    ...stringArrayValue(payload.body_lines),
    fallback,
  ];

  for (const candidate of candidates) {
    const label = agentChatQuickNavNormalizeLabel(candidate);
    if (label) {
      return label;
    }
  }

  return null;
}

function agentChatQuickNavLabelFromBlocks(blocks: WebActivityBlock[]) {
  for (const block of blocks) {
    const record = asRecord(block);
    if (record?.type !== "text") {
      continue;
    }

    const label = agentChatQuickNavNormalizeLabel(nullableStringValue(record.text));
    if (label) {
      return label;
    }
  }

  return null;
}

function agentChatQuickNavNormalizeLabel(value: string | null | undefined) {
  if (!value) {
    return null;
  }

  const firstLine = value.split(/\r?\n/).find((line) => line.trim());
  if (!firstLine) {
    return null;
  }

  const normalized = firstLine.replace(/\s+/g, " ").trim();
  if (!normalized) {
    return null;
  }

  return normalized.length > 180
    ? `${normalized.slice(0, 177).trimEnd()}...`
    : normalized;
}

function agentChatQuickNavCollapsedItems(
  items: AgentChatQuickNavItem[],
  activeItemId: string | null,
) {
  if (items.length <= AGENT_CHAT_QUICK_NAV_COLLAPSED_ITEM_LIMIT) {
    return items;
  }

  const lastIndex = items.length - 1;
  const activeIndex = activeItemId
    ? items.findIndex((item) => item.id === activeItemId)
    : -1;
  const indexes = new Set<number>();

  for (let index = 0; index < AGENT_CHAT_QUICK_NAV_COLLAPSED_ITEM_LIMIT; index += 1) {
    indexes.add(
      Math.round(
        (index * lastIndex) / (AGENT_CHAT_QUICK_NAV_COLLAPSED_ITEM_LIMIT - 1),
      ),
    );
  }

  if (activeIndex > -1 && !indexes.has(activeIndex)) {
    const sortedIndexes = Array.from(indexes).sort((left, right) => left - right);
    const replaceableIndexes = sortedIndexes.filter(
      (index) => index !== 0 && index !== lastIndex,
    );
    const replaceIndex = replaceableIndexes.reduce(
      (nearest, index) =>
        Math.abs(index - activeIndex) < Math.abs(nearest - activeIndex)
          ? index
          : nearest,
      replaceableIndexes[0] ?? sortedIndexes[0],
    );
    indexes.delete(replaceIndex);
    indexes.add(activeIndex);
  }

  return Array.from(indexes)
    .sort((left, right) => left - right)
    .map((index) => items[index])
    .filter((item): item is AgentChatQuickNavItem => Boolean(item));
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

  const webSearch = agentChatActivityCellPayload(cell, "WebSearch");
  if (webSearch) {
    const action = stringValue(webSearch.action, "searched").toLowerCase();
    const url = nullableStringValue(webSearch.url);
    const detailLines = [url, ...stringArrayValue(webSearch.body_lines)].filter(
      (line): line is string => Boolean(line?.trim()),
    );
    return {
      kind: "browser",
      icon: "activity",
      title: `${action === "searching" ? "Searching" : "Searched"} the web: ${stringValue(webSearch.query, "")}`,
      detailLines,
    };
  }

  const codingOpenProject = agentChatActivityCellPayload(
    cell,
    "CodingOpenProject",
  );
  if (codingOpenProject) {
    return {
      kind: "text",
      icon: "activity",
      title: `Opened Project: ${stringValue(codingOpenProject.project_root, "unknown")}`,
      bodyLines: stringArrayValue(codingOpenProject.detail_lines),
    };
  }

  const codingReview = agentChatActivityCellPayload(cell, "CodingReview");
  if (codingReview) {
    const title = stringValue(codingReview.title, "Review").trim();
    return {
      kind: "text",
      icon: "activity",
      title: title || "Review",
      bodyLines: [],
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

  const codingEdit = agentChatActivityCellPayload(cell, "CodingEdit");
  if (codingEdit) {
    const files = agentChatCodingEditFilesFromActivityCell(codingEdit);
    return {
      kind: "patch",
      icon: "activity",
      title: agentChatCodingEditTitle(codingEdit, files),
      files,
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

  const warning = agentChatActivityCellPayload(cell, "Warning");
  if (warning) {
    return {
      kind: "text",
      icon: "activity",
      title: stringValue(warning.title, "Warning"),
      bodyLines: stringArrayValue(warning.body_lines),
      bodyLimit: AGENT_CHAT_ERROR_LINE_LIMIT,
      tone: "muted",
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

  const runtimeStatus = agentChatActivityCellPayload(cell, "RuntimeStatus");
  if (runtimeStatus) {
    return {
      kind: "runtimeStatus",
      icon: "activity",
      title: stringValue(runtimeStatus.label, "Working"),
      detail: nullableStringValue(runtimeStatus.detail),
      startedAtMs: nullableNumberValue(runtimeStatus.active_runtime_started_at_ms),
      reducedMotion: nullableStringValue(runtimeStatus.reduced_motion),
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
  let index = 0;

  while (index < calls.length) {
    const call = calls[index];
    if (call.action === "read") {
      const names = [agentChatExploredReadTarget(call)];
      index += 1;
      while (index < calls.length && calls[index].action === "read") {
        names.push(agentChatExploredReadTarget(calls[index]));
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
  return agentChatPatchFilesFromValue(cell.files);
}

function agentChatCodingEditFilesFromActivityCell(
  cell: Record<string, unknown>,
): AgentChatDiffFile[] {
  return agentChatPatchFilesFromValue(cell.diff_files);
}

function agentChatPatchFilesFromValue(value: unknown): AgentChatDiffFile[] {
  return diffFilesValue(
    arrayValue(value).map((file) => {
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

function agentChatCodingEditTitle(
  cell: Record<string, unknown>,
  files: AgentChatDiffFile[],
) {
  if (files.length > 0) {
    return agentChatPatchTitle(files);
  }

  const file = nullableStringValue(cell.file);
  const addedLines = numberValue(cell.added_lines, 0);
  const removedLines = numberValue(cell.removed_lines, 0);
  if (file) {
    return `Edited ${file} (+${addedLines} -${removedLines})`;
  }

  return `Edited Code (+${addedLines} -${removedLines})`;
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
