import { getStoredDaemonToken } from "@/lib/daemon-auth";

export type DaemonLifecycleState =
  | "initializing"
  | "ready"
  | "stopping"
  | "failed";

export type DaemonStatus = {
  pid: number;
  started_at_ms: number;
  version: string;
  bind_host: string;
  port: number;
  state: DaemonLifecycleState;
  connected_clients: number;
};

export type DashboardPlanStep = {
  status: "pending" | "in_progress" | "completed";
  step: string;
};

export type TokenUsage = {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
};

export type DailyTokenUsage = {
  date: string;
  usage: TokenUsage;
};

export type TokenUsageInfo = {
  total_token_usage: TokenUsage;
  last_token_usage: TokenUsage;
  model_context_window: number | null;
  daily_token_usage: DailyTokenUsage[];
};

export type DashboardTokenUsageSnapshot = {
  main: TokenUsageInfo | null;
  main_model?: string | null;
  judge: TokenUsageInfo | null;
  judge_model?: string | null;
};

export type DashboardPrimitiveOptimizationSnapshot = {
  running: boolean;
  current_trigger: string | null;
  last_result: string | null;
  last_completed_at_ms: number | null;
  primitive_evidence_records: number;
  total_primitive_evidence_run_records: number;
  total_primitive_reflections: number;
  total_primitive_patch_candidates: number;
  total_primitive_merge_candidates: number;
  total_primitive_candidate_evaluations: number;
  total_primitive_frontier_entries: number;
  latest_primitive_frontier_root_entries: number;
  latest_primitive_frontier_branched_entries: number;
  latest_primitive_frontier_max_generation: number;
  total_primitive_patch_applied: number;
  total_primitive_merge_applied: number;
  total_primitive_update_rollbacks: number;
  total_primitive_optimization_rounds: number;
};

export type DashboardRuntimeOptimizationSnapshot = {
  running: boolean;
  current_trigger: string | null;
  last_result: string | null;
  last_completed_at_ms: number | null;
  unread_runtime_error_backlog: number;
  total_runtime_error_cases_consumed: number;
  total_runtime_error_cases: number;
  total_runtime_error_reflections: number;
  total_runtime_contract_candidates: number;
  total_runtime_contract_candidate_evaluations: number;
  total_runtime_contract_system_additions: number;
  total_runtime_contract_updates: number;
};

export type DashboardContextCompositionSegment = {
  name: string;
  label: string;
  source: string;
  tokens: number;
  bytes: number;
  percent: number;
  hash: string;
  cache_role: string;
};

export type DashboardContextCompositionPrefixUnit = {
  hash: string;
  tokens: number;
};

export type DashboardContextCompositionSnapshot = {
  captured_at_ms: number | null;
  model: string | null;
  total_estimated_tokens: number;
  total_bytes: number;
  message_count: number;
  tool_count: number;
  tools_schema_tokens: number;
  stable_prefix_tokens: number;
  new_suffix_tokens: number;
  changed_prefix_tokens: number;
  previous_common_prefix_tokens: number;
  previous_request_hash: string | null;
  current_request_hash: string | null;
  segments: DashboardContextCompositionSegment[];
  prefix_units: DashboardContextCompositionPrefixUnit[];
};

export type DashboardPendingAccessRequest = {
  chat_id: number;
  title: string;
  sender: string;
  last_message_preview: string;
  first_seen_at_ms: number;
  last_seen_at_ms: number;
};

export type WebActivityKind =
  | "message"
  | "tool"
  | "app"
  | "plan"
  | "primitive"
  | "memory"
  | "patch"
  | "error"
  | "unknown"
  | (string & {});

export type WebActivityStatus =
  | "pending"
  | "running"
  | "completed"
  | "failed"
  | "dismissed"
  | "unknown"
  | (string & {});

export type WebActivityActor =
  | "user"
  | "assistant"
  | "telegram"
  | "tool"
  | "system"
  | (string & {});

export type WebActivitySource = {
  source_type: string;
  label?: string | null;
};

export type WebActivityTool = {
  name: string;
  app?: string | null;
  input_preview?: string | null;
  output_preview?: string | null;
  output_ref?: string | null;
  duration_ms?: number | null;
  exit_code?: number | null;
  affected_files?: string[];
};

export type WebActivityTextBlock = {
  type: "text";
  text: string;
};

export type WebActivityCodeBlock = {
  type: "code";
  code: string;
  language?: string | null;
};

export type WebActivityKvBlock = {
  type: "kv";
  entries: Array<{
    key: string;
    value: string;
  }>;
};

export type WebActivityListBlock = {
  type: "list";
  items: string[];
};

export type WebActivityDiffBlock = {
  type: "diff";
  files: Array<{
    path: string;
    operation: string;
    added_lines: number;
    removed_lines: number;
    lines: Array<{
      kind: "context" | "delete" | "add" | "hunk_break" | (string & {});
      old_lineno?: number | null;
      new_lineno?: number | null;
      text: string;
    }>;
  }>;
};

export type WebActivityLinkBlock = {
  type: "link";
  label: string;
  url: string;
};

export type WebActivityArtifactBlock = {
  type: "artifact";
  label: string;
  uri?: string | null;
  mime_type?: string | null;
};

export type WebActivityImageBlock = {
  type: "image";
  label: string;
  uri: string;
  mime_type?: string | null;
};

export type WebActivityImageAttachment = {
  label: string;
  uri: string;
  mime_type: string;
  description?: string | null;
};

export type WebActivityUnknownBlock = {
  type: string;
  [key: string]: unknown;
};

export type WebActivityBlock =
  | WebActivityTextBlock
  | WebActivityCodeBlock
  | WebActivityKvBlock
  | WebActivityListBlock
  | WebActivityDiffBlock
  | WebActivityLinkBlock
  | WebActivityArtifactBlock
  | WebActivityImageBlock
  | WebActivityUnknownBlock;

export type ActivityCellCommon = {
  title: string;
  body_lines?: string[];
  full_body?: string | null;
};

export type ActivityCellUser = ActivityCellCommon & {
  image_attachments?: WebActivityImageAttachment[];
};

export type ActivityCellBrowser = ActivityCellCommon & {
  url?: string | null;
  line_count?: number | null;
  ref_count?: number | null;
};

export type ActivityCellLiveExec = {
  title: string;
  call_lines?: string[];
  meta?: string | null;
  output_lines?: string[];
  started_at_ms?: number | null;
};

export type ActivityCellExecResult = {
  title: string;
  meta?: string | null;
  output_lines?: string[];
};

export type ActivityCellPatchDiffLine = {
  kind: "context" | "delete" | "add" | "hunk_break" | (string & {});
  old_lineno?: number | null;
  new_lineno?: number | null;
  text: string;
};

export type ActivityCellPatchFile = {
  path: string;
  operation: "add" | "delete" | "update" | (string & {});
  added_lines: number;
  removed_lines: number;
  diff_lines?: ActivityCellPatchDiffLine[];
};

export type ActivityCellPatch = {
  summary_line: string;
  files?: ActivityCellPatchFile[];
};

export type ActivityCellTelegram = {
  title: string;
  detail_lines?: string[];
  message_lines?: string[];
};

export type ActivityCellReply = {
  disposition: "resolved" | "dismissed" | "failed" | (string & {});
  subject?: "message" | "notice" | (string & {});
  message_lines?: string[];
};

export type ActivityCellPlan = {
  steps?: Array<{
    status: "Pending" | "InProgress" | "Completed" | (string & {});
    text: string;
  }>;
};

export type ActivityCellPrimitive = {
  primitive_id: string;
};

export type ActivityCellVariant =
  | { Assistant: ActivityCellCommon }
  | { User: ActivityCellUser }
  | { AppAttention: ActivityCellCommon }
  | { Browser: ActivityCellBrowser }
  | { LiveBrowser: ActivityCellBrowser }
  | { GenericApp: ActivityCellCommon }
  | { ToolResult: ActivityCellCommon }
  | { PlanResult: ActivityCellPlan }
  | { CreatePrimitiveSpecResult: ActivityCellPrimitive }
  | { ActivatePrimitiveResult: ActivityCellPrimitive }
  | { ExecResult: ActivityCellExecResult }
  | { LiveExec: ActivityCellLiveExec }
  | { Patch: ActivityCellPatch }
  | { Telegram: ActivityCellTelegram }
  | { Reply: ActivityCellReply }
  | { TerminalWait: ActivityCellCommon }
  | { Error: ActivityCellCommon }
  | { Thinking: ActivityCellCommon & { full_body?: string | null } }
  | Record<string, unknown>;

export type WebActivityItem = {
  web_activity_version: number;
  id: string;
  kind: WebActivityKind;
  status: WebActivityStatus;
  ui_hint?: string | null;
  title: string;
  actor?: WebActivityActor | null;
  created_at: number;
  updated_at: number;
  source?: WebActivitySource | null;
  tool?: WebActivityTool | null;
  blocks?: WebActivityBlock[];
  detail_blocks?: WebActivityBlock[];
  error?: {
    message: string;
    details?: string[];
  } | null;
  metadata?: unknown;
  cell?: ActivityCellVariant | null;
};

export type LiveWebActivityItem = {
  key: string;
  item: WebActivityItem;
};

export type DashboardActivityHistoryWindow = {
  items: WebActivityItem[];
  oldest_cursor: number | null;
  newest_cursor: number | null;
  has_more_before: boolean;
};

export type DashboardActivityHistoryPage = {
  items: WebActivityItem[];
  oldest_cursor: number | null;
  newest_cursor: number | null;
  has_more_before: boolean;
  has_more_after: boolean;
};

export type DashboardSnapshot = {
  agent_name: string;
  focused_app: string | null;
  status_output: string;
  sleep_status_output: string;
  inspect_telegram_output: string;
  system_prompt_output: string;
  preturn_context_output: string;
  app_status_outputs: Array<[string, string]>;
  pending_access_requests: DashboardPendingAccessRequest[];
  activity_cells: unknown[];
  live_activity_cells: Array<{
    key: string;
    cell: unknown;
  }>;
  web_activity_version?: number;
  web_activity_items?: WebActivityItem[];
  live_web_activity_items?: LiveWebActivityItem[];
  activity_history?: DashboardActivityHistoryWindow;
  last_cycle_elapsed_ms: number | null;
  runtime_status: string | null;
  runtime_status_level?: DashboardRuntimeStatusLevel | null;
  runtime_activity?: DashboardRuntimeActivity;
  current_plan_step: DashboardPlanStep | null;
  token_usage?: DashboardTokenUsageSnapshot;
  primitive_optimization?: DashboardPrimitiveOptimizationSnapshot;
  runtime_optimization?: DashboardRuntimeOptimizationSnapshot;
  context_composition?: DashboardContextCompositionSnapshot | null;
  footer_context: string;
  footer_estimated_input_tokens: number | null;
};

export type DashboardRuntimeStatusLevel =
  | "debug"
  | "info"
  | "warn"
  | "error";

export type DashboardRuntimeActivityStatus =
  | "idle"
  | "thinking"
  | "running"
  | "tooling"
  | "waiting"
  | "error";

export type DashboardRuntimeActivity = {
  status: DashboardRuntimeActivityStatus;
  label: string;
  detail?: string | null;
  active_runtime_turn: boolean;
  active_runtime_phase?: string | null;
};

export type LogSource = {
  id: string;
  label: string;
  description: string;
  path: string;
  exists: boolean;
  size_bytes: number;
  modified_at_ms: number | null;
};

export type LogSourcesResponse = {
  sources: LogSource[];
};

export type LogReadResponse = {
  source: LogSource;
  lines: string[];
  next_cursor: number;
  file_size_bytes: number;
  truncated_start: boolean;
  has_more: boolean;
  reset: boolean;
};

export type SettingsCredentialStatus =
  | "configured"
  | "env_configured"
  | "env_missing"
  | "missing"
  | "placeholder"
  | "oauth_file";

export type SettingsCredentialSummary = {
  status: SettingsCredentialStatus;
  source: string | null;
};

export type SettingsProviderSummary = {
  name: string;
  provider_type: string;
  base_url: string | null;
  credential: SettingsCredentialSummary;
  auth_file: string | null;
};

export type SettingsModelSummary = {
  name: string;
  provider: string;
  model_id: string;
  is_main: boolean;
  is_judge: boolean;
  temperature: number;
  thinking_budget: string | null;
  rpm: number | null;
  request_timeout_secs: number;
  stream_idle_timeout_secs: number;
  context_window_tokens: number;
  effective_context_window_percent: number;
  effective_context_window_tokens: number;
  auto_compact_token_limit: number;
  max_completion_tokens: number;
  tool_output_max_tokens: number;
  /** Whether the model accepts image/vision input in messages (resolved). */
  supports_vision: boolean;
};

export type SettingsSummary = {
  loaded_at_ms: number;
  home_path: string;
  config_path: string;
  locale: string;
  locale_label: string;
  main_model: string;
  judge_model: string;
  providers: SettingsProviderSummary[];
  models: SettingsModelSummary[];
  daemon: {
    bind_host: string;
    configured_port: number;
    serving_port: number;
  };
  judge: {
    enabled: boolean;
    model: string | null;
    effective_model: string;
    max_pairwise_candidates: number;
    max_pairwise_cases: number;
  };
  sandbox: {
    enabled: boolean;
    strong_filesystem: string;
  };
  telegram: {
    enabled: boolean;
    credential: SettingsCredentialSummary;
    has_real_credentials: boolean;
    poll_timeout_secs: number;
  };
};

type FetchOptions = {
  signal?: AbortSignal;
  token?: string;
};

type DashboardSnapshotSubscriptionOptions = {
  token?: string;
  onSnapshot: (snapshot: DashboardSnapshot) => void;
  onError?: (error: Error) => void;
  onClose?: (event: CloseEvent) => void;
};

export type DashboardSnapshotSubscription = {
  close: () => void;
};

type DashboardCommandResponse = {
  output: string;
};

export type DashboardCommandAttachment = {
  name: string;
  media_type: string;
  data_url: string;
};

export class DaemonApiError extends Error {
  status?: number;

  constructor(message: string, status?: number) {
    super(message);
    this.name = "DaemonApiError";
    this.status = status;
  }
}

export async function fetchDaemonStatus({
  signal,
}: FetchOptions = {}): Promise<DaemonStatus> {
  const response = await fetch("/status", {
    method: "GET",
    headers: {
      Accept: "application/json",
    },
    signal,
  });

  return parseJsonResponse<DaemonStatus>(response, "Daemon status");
}

export async function fetchDashboardSnapshot({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<DashboardSnapshot> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard snapshot.");
  }

  const response = await fetch("/dashboard/snapshot", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<DashboardSnapshot>(response, "Dashboard snapshot");
}

export async function fetchDashboardActivityHistory({
  before,
  after,
  limit = 80,
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions & {
  before?: number;
  after?: number;
  limit?: number;
} = {}): Promise<DashboardActivityHistoryPage> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard activity history.");
  }

  const url = new URL("/dashboard/activity-history", window.location.href);
  url.searchParams.set("limit", String(limit));
  if (before !== undefined) {
    url.searchParams.set("before", String(before));
  }
  if (after !== undefined) {
    url.searchParams.set("after", String(after));
  }

  const response = await fetch(url, {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<DashboardActivityHistoryPage>(
    response,
    "Dashboard activity history",
  );
}

export function getDashboardAttachmentUrl(uri: string) {
  if (!uri.startsWith("/dashboard/attachments/")) {
    return uri;
  }

  const token = getStoredDaemonToken().trim();
  const separator = uri.includes("?") ? "&" : "?";
  return token ? `${uri}${separator}token=${encodeURIComponent(token)}` : uri;
}

export async function fetchSettingsSummary({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<SettingsSummary> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for settings summary.");
  }

  const response = await fetch("/settings/summary", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<SettingsSummary>(response, "Settings summary");
}

export async function runDashboardCommand(
  command: string,
  {
    attachments = [],
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions & { attachments?: DashboardCommandAttachment[] } = {},
): Promise<string> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard command.");
  }

  const response = await fetch("/commands/run", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ command, attachments }),
    signal,
  });

  const result = await parseJsonResponse<DashboardCommandResponse>(
    response,
    "Dashboard command",
  );
  return result.output;
}

export async function fetchLogSources({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<LogSource[]> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for log sources.");
  }

  const response = await fetch("/logs/sources", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  const result = await parseJsonResponse<LogSourcesResponse>(
    response,
    "Log sources",
  );
  return result.sources;
}

export async function readLogSource({
  source,
  cursor,
  limit = 500,
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions & {
  source: string;
  cursor?: number;
  limit?: number;
}): Promise<LogReadResponse> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for log read.");
  }

  const url = new URL("/logs/read", window.location.href);
  url.searchParams.set("source", source);
  url.searchParams.set("limit", String(limit));
  if (cursor !== undefined) {
    url.searchParams.set("cursor", String(cursor));
  }

  const response = await fetch(url, {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<LogReadResponse>(response, "Log read");
}

type DashboardWsMessage =
  | { type: "snapshot"; data: DashboardSnapshot }
  | { type: "delta"; data: Record<string, unknown> };


export function subscribeDashboardSnapshots({
  token = getStoredDaemonToken(),
  onSnapshot,
  onError,
  onClose,
}: DashboardSnapshotSubscriptionOptions): DashboardSnapshotSubscription {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard stream.");
  }

  const socket = new WebSocket(dashboardStreamUrl(daemonToken));

  let currentSnapshot: DashboardSnapshot | null = null;

  socket.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      onError?.(
        new DaemonApiError("Dashboard stream returned a non-text message."),
      );
      return;
    }

    try {
      const msg = JSON.parse(event.data) as DashboardWsMessage;
      if (msg.type === "snapshot") {
        currentSnapshot = msg.data;
        onSnapshot(currentSnapshot);
      } else if (msg.type === "delta") {
        if (!currentSnapshot) {
          onError?.(
            new DaemonApiError("Received delta before initial snapshot."),
          );
          return;
        }
        // Shallow merge changed fields into the current snapshot.
        const changed = msg.data as Record<string, unknown>;
        const merged = { ...currentSnapshot };
        for (const key of Object.keys(changed)) {
          (merged as Record<string, unknown>)[key] = changed[key];
        }
        currentSnapshot = merged as DashboardSnapshot;
        onSnapshot(currentSnapshot);
      } else {
        onError?.(
          new DaemonApiError(
            `Unknown dashboard ws message type: ${(msg as { type: string }).type}`,
          ),
        );
      }
    } catch (error) {
      onError?.(
        new DaemonApiError(
          `Unable to decode dashboard stream message: ${
            error instanceof Error ? error.message : String(error)
          }`,
        ),
      );
    }
  });

  socket.addEventListener("error", () => {
    onError?.(new DaemonApiError("Dashboard stream connection failed."));
  });

  socket.addEventListener("close", (event) => {
    onClose?.(event);
  });

  return {
    close: () => socket.close(1000, "dashboard stream subscription closed"),
  };
}

function dashboardStreamUrl(token: string) {
  const url = new URL("/dashboard/stream", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("token", token);
  return url.toString();
}

async function parseJsonResponse<T>(
  response: Response,
  label: string,
): Promise<T> {
  if (!response.ok) {
    const details = await response.text().catch(() => "");
    const statusText = response.statusText ? ` ${response.statusText}` : "";
    const detailText = details ? `: ${details}` : "";

    throw new DaemonApiError(
      `${label} returned ${response.status}${statusText}${detailText}`,
      response.status,
    );
  }

  return response.json() as Promise<T>;
}
