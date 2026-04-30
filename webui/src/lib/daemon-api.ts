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

export type DashboardSnapshot = {
  focused_app: string | null;
  status_output: string;
  sleep_status_output: string;
  inspect_telegram_output: string;
  system_prompt_output: string;
  preturn_context_output: string;
  app_status_outputs: Array<[string, string]>;
  pending_access_requests: unknown[];
  activity_cells: unknown[];
  live_activity_cells: Array<{
    key: string;
    cell: unknown;
  }>;
  last_cycle_elapsed_ms: number | null;
  runtime_status: string | null;
  current_plan_step: DashboardPlanStep | null;
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
