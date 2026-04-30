import { useEffect, useMemo, useState } from "react";

import {
  AgentStatusAnimation,
  type AgentAnimationStatus,
} from "@/components/agent-status-animation";
import {
  fetchDashboardSnapshot,
  type DashboardSnapshot,
} from "@/lib/daemon-api";

const DASHBOARD_SNAPSHOT_POLL_MS = 2500;

type AgentStatusView = {
  animationStatus: AgentAnimationStatus;
  label: string;
};

export function StatusPage() {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [loadError, setLoadError] = useState<Error | null>(null);

  useEffect(() => {
    const controller = new AbortController();

    async function loadSnapshot() {
      try {
        const nextSnapshot = await fetchDashboardSnapshot({
          signal: controller.signal,
        });

        setSnapshot(nextSnapshot);
        setLoadError(null);
      } catch (error) {
        if (!controller.signal.aborted) {
          setLoadError(error instanceof Error ? error : new Error(String(error)));
        }
      } finally {
        if (!controller.signal.aborted) {
          setIsLoading(false);
        }
      }
    }

    void loadSnapshot();
    const intervalId = window.setInterval(
      () => void loadSnapshot(),
      DASHBOARD_SNAPSHOT_POLL_MS,
    );

    return () => {
      controller.abort();
      window.clearInterval(intervalId);
    };
  }, []);

  const agentStatus = useMemo(
    () =>
      deriveAgentStatus({
        hasLoadError: Boolean(loadError),
        isLoading,
        snapshot,
      }),
    [isLoading, loadError, snapshot],
  );

  return (
    <section
      id="status"
      className="flex min-h-[calc(100vh-5rem)] w-full items-center justify-center px-6 py-10"
    >
      <div className="flex flex-col items-center justify-center gap-5 text-center">
        <AgentStatusAnimation
          status={agentStatus.animationStatus}
          className="w-64 md:w-80"
        />
        <p
          aria-live="polite"
          className="text-2xl font-semibold tracking-tight"
        >
          {agentStatus.label}
        </p>
      </div>
    </section>
  );
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

  if (!snapshot?.runtime_status) {
    return { animationStatus: "idle", label: "空闲" };
  }

  const runtimeStatus = snapshot.runtime_status.toLowerCase();
  const dashboardText = [snapshot.runtime_status, snapshot.status_output]
    .join(" ")
    .toLowerCase();

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
