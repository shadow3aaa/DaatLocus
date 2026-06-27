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

export type SessionScope =
  | { kind: "general" }
  | { kind: "project"; project_dir: string };

export type SessionInfo = {
  session_id: string;
  scope: SessionScope;
  project_dir: string | null;
  title: string | null;
  started_at_ms: number;
  last_seen_at_ms: number | null;
};

export type DashboardSessionTitle = {
  title: string;
  generated: boolean;
  updated_at_ms: number;
};

export type DashboardPlanStep = {
  status: "pending" | "in_progress" | "completed";
  step: string;
};
export type DashboardStatusCommandSnapshot = {
  runtime_turn: string;
  bound_primitive: string;
  active_plans: number;
  events: string;
  plan_steps: DashboardPlanStep[];
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
  efficient_model?: string | null;
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
  model_context_window?: number | null;
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

export type DashboardPendingUserInput = {
  event_id: string;
  origin: string;
  incoming_text: string;
  arrived_at_ms: number;
  attachment_count: number;
};

export type ActivityImageAttachment = {
  label: string;
  uri: string;
  mime_type: string;
  description?: string | null;
};

export type SessionActivityCommon = {
  title: string;
  body_lines?: string[];
  full_body?: string | null;
};

export type SessionActivityMessage = {
  content: string;
};

export type SessionActivityFinalMessageSeparator = {
  elapsed_seconds?: number | null;
};

export type SessionActivityUser = SessionActivityMessage & {
  image_attachments?: ActivityImageAttachment[];
};

export type SessionActivityBrowser = SessionActivityCommon & {
  url?: string | null;
  line_count?: number | null;
  ref_count?: number | null;
};

export type SessionActivityWebSearch = {
  action: "searching" | "searched" | (string & {});
  query: string;
  url?: string | null;
  body_lines?: string[];
};

export type SessionActivityCodingOpenProject = {
  project_root: string;
  detail_lines?: string[];
};

export type SessionActivityLiveExec = {
  title: string;
  call_lines?: string[];
  meta?: string | null;
  output_lines?: string[];
  started_at_ms?: number | null;
};

export type SessionActivityExecResult = {
  title: string;
  meta?: string | null;
  output_lines?: string[];
};

export type SessionActivityPatchDiffLine = {
  kind: "context" | "delete" | "add" | "hunk_break" | (string & {});
  old_lineno?: number | null;
  new_lineno?: number | null;
  text: string;
};

export type SessionActivityPatchFile = {
  path: string;
  operation: "add" | "delete" | "update" | (string & {});
  added_lines: number;
  removed_lines: number;
  diff_lines?: SessionActivityPatchDiffLine[];
};

export type SessionActivityPatch = {
  summary_line: string;
  files?: SessionActivityPatchFile[];
};

export type SessionActivityCodingEdit = {
  stable_id: string;
  title: string;
  tool_name?: string | null;
  tool_app?: string | null;
  selector: string;
  file?: string | null;
  added_lines: number;
  removed_lines: number;
  propagation_count: number;
  impact_lines?: string[];
  diff_files?: SessionActivityPatchFile[];
};

export type SessionActivityCodingReview = {
  title: string;
  summary: string;
  review_pending: boolean;
};

export type SessionActivityTelegram = {
  title: string;
  detail_lines?: string[];
  message_lines?: string[];
};

export type SessionActivityReply = {
  disposition: "resolved" | "dismissed" | "failed" | (string & {});
  subject?: "message" | "notice" | (string & {});
  message_lines?: string[];
};

export type SessionActivityPlan = {
  steps?: Array<{
    status: "Pending" | "InProgress" | "Completed" | (string & {});
    text: string;
  }>;
};

export type SessionActivityRuntimeStatus = {
  label: string;
  detail?: string | null;
  active_runtime_started_at_ms?: number | null;
  reduced_motion?: "Full" | "Reduced" | (string & {});
};

export type SessionActivityThinking = {
  content: string;
};

export type SessionActivityPrimitive = {
  primitive_id: string;
};

export type SessionActivityEvent =
  | { Assistant: SessionActivityMessage }
  | { FinalMessageSeparator: SessionActivityFinalMessageSeparator }
  | { User: SessionActivityUser }
  | { Browser: SessionActivityBrowser }
  | { LiveBrowser: SessionActivityBrowser }
  | { WebSearch: SessionActivityWebSearch }
  | { GenericApp: SessionActivityCommon }
  | { CodingOpenProject: SessionActivityCodingOpenProject }
  | { PlanResult: SessionActivityPlan }
  | { CreatePrimitiveSpecResult: SessionActivityPrimitive }
  | { ActivatePrimitiveResult: SessionActivityPrimitive }
  | { ExecResult: SessionActivityExecResult }
  | { LiveExec: SessionActivityLiveExec }
  | { CodingEdit: SessionActivityCodingEdit }
  | { CodingReview: SessionActivityCodingReview }
  | { Patch: SessionActivityPatch }
  | { Telegram: SessionActivityTelegram }
  | { Reply: SessionActivityReply }
  | { TerminalWait: SessionActivityCommon }
  | { Warning: SessionActivityCommon }
  | { Error: SessionActivityCommon }
  | { Thinking: SessionActivityThinking }
  | { RuntimeStatus: SessionActivityRuntimeStatus }
  | Record<string, unknown>;

export type DashboardActivityHistoryItem = {
  id: string;
  created_at: number;
  updated_at: number;
  event: SessionActivityEvent;
};

export type DashboardActivityHistoryWindow = {
  items: DashboardActivityHistoryItem[];
  oldest_cursor: number | null;
  newest_cursor: number | null;
  has_more_before: boolean;
};

export type DashboardActivityHistoryPage = {
  items: DashboardActivityHistoryItem[];
  oldest_cursor: number | null;
  newest_cursor: number | null;
  has_more_before: boolean;
  has_more_after: boolean;
};

export type DashboardActivityHistoryCount = {
  matching_items: number;
  total_items: number;
};

export type DashboardSkillSummary = {
  name: string;
  description: string;
  path: string;
  scope: string;
  allow_implicit_invocation: boolean;
  user_disabled: boolean;
  auto_use_enabled: boolean;
};

export type DashboardSkillError = {
  path: string;
  message: string;
};

export type DashboardSnapshot = {
  agent_name: string;
  session_title?: DashboardSessionTitle | null;
  status_output: string;
  status_command?: DashboardStatusCommandSnapshot | null;
  sleep_status_output: string;
  inspect_telegram_output: string;
  system_prompt_output: string;
  preturn_context_output: string;
  app_status_outputs: Array<[string, string]>;
  skills?: DashboardSkillSummary[];
  skill_errors?: DashboardSkillError[];
  pending_access_requests: DashboardPendingAccessRequest[];
  pending_user_inputs?: DashboardPendingUserInput[];
  activity_events: SessionActivityEvent[];
  live_activity_events: Array<{
    key: string;
    event: SessionActivityEvent;
  }>;
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

export type SessionRuntimeStatus = {
  ready: boolean;
  pending_work_count: number;
  active_runtime_turn: boolean;
};

export type SessionStatusDashboard = {
  agent_name: string;
  session_title?: DashboardSessionTitle | null;
  last_cycle_elapsed_ms: number | null;
  runtime_status: string | null;
  runtime_status_level: DashboardRuntimeStatusLevel | null;
  runtime_activity: DashboardRuntimeActivity;
  current_plan_step: DashboardPlanStep | null;
  token_usage: DashboardTokenUsageSnapshot;
  primitive_optimization: DashboardPrimitiveOptimizationSnapshot;
  runtime_optimization: DashboardRuntimeOptimizationSnapshot;
  context_composition: DashboardContextCompositionSnapshot | null;
};

export type StatusSessionSummary = {
  session: SessionInfo;
  runtime_status: SessionRuntimeStatus | null;
  dashboard: SessionStatusDashboard | null;
  error: string | null;
};

export type StatusSummary = {
  loaded_at_ms: number;
  daemon: DaemonStatus;
  pending_access_requests: DashboardPendingAccessRequest[];
  sessions: StatusSessionSummary[];
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
  reserved_output_tokens: number;
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

export type ConfigReadinessKind = "unconfigured" | "incomplete" | "complete";

export type ConfigReadinessReport = {
  kind: ConfigReadinessKind;
  config_path: string;
  backup_path: string;
  port: number;
  message: string;
  recovery_note: string | null;
};

type ConfigReadinessResponse = {
  readiness: ConfigReadinessReport;
};

export type SetupProviderKind =
  | "openai"
  | "openai_compatible"
  | "openai_codex_oauth"
  | "github_copilot"
  | "ollama"
  | "ollama_cloud";

export type SetupProviderRequest = {
  kind: SetupProviderKind;
  name: string;
  api_key?: string | null;
  base_url?: string | null;
  keep_alive?: string | null;
  codex_auth_method?: string | null;
  codex_auth_file?: string | null;
  github_auth_method?: string | null;
};

export type SetupDiscoveredModel = {
  id: string;
  context_window_tokens?: number | null;
  max_completion_tokens?: number | null;
  supports_vision?: boolean | null;
  thinking_budgets?: string[];
};

type SetupDiscoverModelsResponse = {
  models: SetupDiscoveredModel[];
};

export type SetupProviderAuthStartResponse = {
  flow_id: string;
  provider_kind: SetupProviderKind;
  verification_url: string;
  user_code: string;
  expires_at_ms: number;
  interval_secs: number;
};

export type SetupProviderAuthResponse = {
  api_key?: string | null;
  auth_file?: string | null;
  message: string;
};

export type SetupModelRequest = {
  name: string;
  provider_name: string;
  model_id: string;
  context_window_tokens?: number | null;
  max_completion_tokens?: number | null;
  supports_vision?: boolean | null;
  thinking_budget?: string | null;
  temperature?: number | null;
  rpm?: number | null;
  request_timeout_secs?: number | null;
  stream_idle_timeout_secs?: number | null;
  auto_compact_token_limit?: number | null;
  effective_context_window_percent?: number | null;
  tool_output_max_tokens?: number | null;
};

export type SetupConfigRequest = {
  locale?: string;
  persona_name?: string | null;
  persona_language?: string | null;
  providers?: SetupProviderRequest[];
  models?: SetupModelRequest[];
  main_model?: string | null;
  efficient_model?: string | null;
  provider_kind?: SetupProviderKind;
  provider_name?: string;
  main_model_name?: string;
  main_model_id?: string;
  efficient_model_name?: string;
  efficient_model_id?: string;
  api_key?: string | null;
  base_url?: string | null;
  daemon_port?: number | null;
  telegram_enabled?: boolean | null;
  telegram_bot_token?: string | null;
};
export type SetupConfigResponse = {
  config: SetupConfigRequest;
  readiness: ConfigReadinessReport;
};


type FetchOptions = {
  signal?: AbortSignal;
  token?: string;
};

type DashboardSnapshotSubscriptionOptions = {
  token?: string;
  sessionId: string;
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

export type DashboardPendingUserInputMoveDirection = "up" | "down";

export type DashboardAction =
  | { kind: "run_sleep" }
  | { kind: "clear_conversation" }
  | { kind: "interrupt_runtime" }
  | { kind: "restart_daemon" }
  | { kind: "reload_skills" }
  | { kind: "set_skill_auto_use"; path: string; enabled: boolean }
  | { kind: "dismiss_pending_user_input"; event_id: string }
  | { kind: "clear_pending_user_inputs" }
  | {
      kind: "update_pending_user_input";
      event_id: string;
      incoming_text: string;
    }
  | {
      kind: "move_pending_user_input";
      event_id: string;
      direction: DashboardPendingUserInputMoveDirection;
    }
  | {
      kind: "move_pending_user_input_to_position";
      event_id: string;
      target_position: number;
    }
  | { kind: "preempt_pending_user_input"; event_id: string };

export type DashboardActionResult = {
  success: boolean;
  message: string;
  detail?: string | null;
};

type DashboardActionResponse = {
  result: DashboardActionResult;
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

export async function fetchStatusSummary({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<StatusSummary> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for status summary.");
  }

  const response = await fetch("/status/summary", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<StatusSummary>(response, "Status summary");
}

export async function fetchDashboardSnapshot({
  signal,
  token = getStoredDaemonToken(),
  sessionId,
}: FetchOptions & { sessionId: string }): Promise<DashboardSnapshot> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard snapshot.");
  }

  const url = new URL("/dashboard/snapshot", window.location.href);
  url.searchParams.set("session_id", sessionId);

  const response = await fetch(url, {
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
  sessionId,
}: FetchOptions & {
  before?: number;
  after?: number;
  limit?: number;
  sessionId: string;
}): Promise<DashboardActivityHistoryPage> {
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
  url.searchParams.set("session_id", sessionId);

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

export async function fetchDashboardActivityHistoryCount({
  signal,
  token = getStoredDaemonToken(),
  sessionId,
}: FetchOptions & {
  sessionId: string;
}): Promise<DashboardActivityHistoryCount> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard activity history count.");
  }

  const url = new URL("/dashboard/activity-history/count", window.location.href);
  url.searchParams.set("session_id", sessionId);

  const response = await fetch(url, {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<DashboardActivityHistoryCount>(
    response,
    "Dashboard activity history count",
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

export async function fetchConfigReadiness({
  signal,
}: FetchOptions = {}): Promise<ConfigReadinessReport> {
  const response = await fetch("/config/readiness", {
    method: "GET",
    headers: {
      Accept: "application/json",
    },
    signal,
  });

  const result = await parseJsonResponse<ConfigReadinessResponse>(
    response,
    "Config readiness",
  );
  return result.readiness;
}


export async function fetchSetupConfig({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<SetupConfigResponse> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup config.");
  }

  const response = await fetch("/config/setup", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<SetupConfigResponse>(response, "Setup config");
}
export async function saveSetupConfig(
  request: SetupConfigRequest,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<ConfigReadinessReport> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup.");
  }

  const response = await fetch("/config/setup", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  });

  const result = await parseJsonResponse<ConfigReadinessResponse>(
    response,
    "Config setup",
  );
  return result.readiness;
}

export async function probeSetupConfig(
  request: SetupConfigRequest,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<ConfigReadinessReport> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup probe.");
  }

  const response = await fetch("/config/probe", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  });

  const result = await parseJsonResponse<ConfigReadinessResponse>(
    response,
    "Config probe",
  );
  return result.readiness;
}

export async function discoverSetupModels(
  provider: SetupProviderRequest,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<SetupDiscoveredModel[]> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for model discovery.");
  }

  const response = await fetch("/config/discover-models", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ provider }),
    signal,
  });

  const result = await parseJsonResponse<SetupDiscoverModelsResponse>(
    response,
    "Model discovery",
  );
  return result.models;
}

export async function runSetupProviderAuth(
  provider: SetupProviderRequest,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<SetupProviderAuthResponse> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup auth.");
  }

  const response = await fetch("/config/provider-auth/run", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ provider }),
    signal,
  });

  return parseJsonResponse<SetupProviderAuthResponse>(
    response,
    "Provider auth",
  );
}

export async function startSetupProviderAuthDevice(
  provider: SetupProviderRequest,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<SetupProviderAuthStartResponse> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup auth.");
  }

  const response = await fetch("/config/provider-auth/device/start", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ provider }),
    signal,
  });

  return parseJsonResponse<SetupProviderAuthStartResponse>(
    response,
    "Provider device auth start",
  );
}

export async function completeSetupProviderAuthDevice(
  provider: SetupProviderRequest,
  flowId: string,
  {
    signal,
    token = getStoredDaemonToken(),
  }: FetchOptions = {},
): Promise<SetupProviderAuthResponse> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for setup auth.");
  }

  const response = await fetch("/config/provider-auth/device/complete", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ provider, flow_id: flowId }),
    signal,
  });

  return parseJsonResponse<SetupProviderAuthResponse>(
    response,
    "Provider device auth complete",
  );
}

export async function runDashboardCommand(
  command: string,
  {
    attachments = [],
    signal,
    token = getStoredDaemonToken(),
    sessionId,
  }: FetchOptions & {
    attachments?: DashboardCommandAttachment[];
    sessionId?: string;
  },
): Promise<string> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard command.");
  }

  const body: {
    command: string;
    attachments: DashboardCommandAttachment[];
    origin: "web_ui";
    session_id?: string;
  } = {
    command,
    attachments,
    origin: "web_ui",
  };
  if (sessionId) {
    body.session_id = sessionId;
  }

  const response = await fetch("/commands/run", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
    signal,
  });

  const result = await parseJsonResponse<DashboardCommandResponse>(
    response,
    "Dashboard command",
  );
  return result.output;
}

export async function runDashboardAction(
  action: DashboardAction,
  {
    signal,
    token = getStoredDaemonToken(),
    sessionId,
  }: FetchOptions & {
    sessionId?: string;
  } = {},
): Promise<DashboardActionResult> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard action.");
  }

  const body: {
    action: DashboardAction;
    session_id?: string;
  } = { action };
  if (sessionId) {
    body.session_id = sessionId;
  }

  const response = await fetch("/dashboard/action", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
    signal,
  });

  const result = await parseJsonResponse<DashboardActionResponse>(
    response,
    "Dashboard action",
  );
  return result.result;
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

export function subscribeDashboardSnapshots({
  token = getStoredDaemonToken(),
  sessionId,
  onSnapshot,
  onError,
  onClose,
}: DashboardSnapshotSubscriptionOptions): DashboardSnapshotSubscription {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for dashboard stream.");
  }

  const socket = new WebSocket(dashboardStreamUrl(daemonToken, sessionId));

  socket.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      onError?.(
        new DaemonApiError("Dashboard stream returned a non-text message."),
      );
      return;
    }

    try {
      onSnapshot(JSON.parse(event.data) as DashboardSnapshot);
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

function dashboardStreamUrl(token: string, sessionId: string) {
  const url = new URL("/dashboard/stream", window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("token", token);
  url.searchParams.set("session_id", sessionId);
  return url.toString();
}

export async function fetchSessions({
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions = {}): Promise<SessionInfo[]> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for sessions.");
  }

  const response = await fetch("/sessions", {
    method: "GET",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  return parseJsonResponse<SessionInfo[]>(response, "Sessions");
}

export async function createSession({
  projectDir,
  title,
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions & { projectDir?: string; title?: string } = {}): Promise<SessionInfo> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for session creation.");
  }

  const response = await fetch("/sessions", {
    method: "POST",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      project_dir: projectDir,
      title,
    }),
    signal,
  });

  return parseJsonResponse<SessionInfo>(response, "Create session");
}

export async function deleteSession({
  sessionId,
  signal,
  token = getStoredDaemonToken(),
}: FetchOptions & { sessionId: string }): Promise<void> {
  const daemonToken = token.trim();

  if (!daemonToken) {
    throw new DaemonApiError("Missing daemon token for session deletion.");
  }

  const response = await fetch(`/sessions/${encodeURIComponent(sessionId)}`, {
    method: "DELETE",
    headers: {
      Accept: "application/json",
      Authorization: `Bearer ${daemonToken}`,
    },
    signal,
  });

  await parseJsonResponse<unknown>(response, "Delete session");
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
