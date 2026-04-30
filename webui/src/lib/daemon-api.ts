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

export type DashboardWorkflowOptimizationSnapshot = {
  running: boolean;
  current_trigger: string | null;
  last_result: string | null;
  last_completed_at_ms: number | null;
  workflow_evidence_records: number;
  total_workflow_evidence_run_records: number;
  total_workflow_reflections: number;
  total_workflow_patch_candidates: number;
  total_workflow_merge_candidates: number;
  total_workflow_candidate_evaluations: number;
  total_workflow_frontier_entries: number;
  latest_workflow_frontier_root_entries: number;
  latest_workflow_frontier_branched_entries: number;
  latest_workflow_frontier_max_generation: number;
  total_workflow_patch_applied: number;
  total_workflow_merge_applied: number;
  total_workflow_update_rollbacks: number;
  total_workflow_optimization_rounds: number;
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

export type DashboardPendingAccessRequest = {
  chat_id: number;
  title: string;
  sender: string;
  last_message_preview: string;
  first_seen_at_ms: number;
  last_seen_at_ms: number;
};

export type DashboardSnapshot = {
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
  last_cycle_elapsed_ms: number | null;
  runtime_status: string | null;
  current_plan_step: DashboardPlanStep | null;
  token_usage?: DashboardTokenUsageSnapshot;
  workflow_optimization?: DashboardWorkflowOptimizationSnapshot;
  runtime_optimization?: DashboardRuntimeOptimizationSnapshot;
  footer_context: string;
  footer_estimated_input_tokens: number | null;
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

export async function runDashboardCommand(
  command: string,
  { signal, token = getStoredDaemonToken() }: FetchOptions = {},
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
    body: JSON.stringify({ command }),
    signal,
  });

  const result = await parseJsonResponse<DashboardCommandResponse>(
    response,
    "Dashboard command",
  );
  return result.output;
}

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

  socket.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      onError?.(new DaemonApiError("Dashboard stream returned a non-text message."));
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
