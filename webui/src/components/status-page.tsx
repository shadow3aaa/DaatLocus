import { useCallback, useEffect, useRef, useState } from "react";

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { fetchDaemonStatus, fetchDashboardSnapshot } from "@/lib/daemon-api";

const REFRESH_INTERVAL_MS = 5_000;

export function StatusPage({ onLogout }: { onLogout: () => void }) {
  const [lastUpdatedAt, setLastUpdatedAt] = useState<Date | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [errorMessage, setErrorMessage] = useState("");
  const isMountedRef = useRef(true);

  const refresh = useCallback(async (signal?: AbortSignal) => {
    setIsRefreshing(true);

    const [daemonResult, snapshotResult] = await Promise.allSettled([
      fetchDaemonStatus({ signal }),
      fetchDashboardSnapshot({ signal }),
    ]);

    if (signal?.aborted || !isMountedRef.current) {
      return;
    }

    const errors: string[] = [];

    if (daemonResult.status === "rejected") {
      errors.push(formatError("Daemon status", daemonResult.reason));
    }

    if (snapshotResult.status === "rejected") {
      errors.push(formatError("Dashboard snapshot", snapshotResult.reason));
    }

    setErrorMessage(errors.join("\n"));
    setLastUpdatedAt(new Date());
    setIsRefreshing(false);
  }, []);

  useEffect(() => {
    isMountedRef.current = true;

    const controller = new AbortController();
    void refresh(controller.signal);

    const intervalId = window.setInterval(() => {
      void refresh();
    }, REFRESH_INTERVAL_MS);

    return () => {
      isMountedRef.current = false;
      controller.abort();
      window.clearInterval(intervalId);
    };
  }, [refresh]);

  const refreshLabel = lastUpdatedAt
    ? lastUpdatedAt.toLocaleTimeString()
    : "waiting";
  const healthLabel = errorMessage
    ? "degraded"
    : isRefreshing && !lastUpdatedAt
      ? "loading"
      : "live";

  return (
    <section
      id="status"
      className="mx-auto min-h-[calc(100vh-4rem)] w-full max-w-7xl px-6 py-8"
    >
      <div className="mb-6 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="max-w-3xl space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant={errorMessage ? "destructive" : "secondary"}>
              {healthLabel}
            </Badge>
            <span className="text-xs text-muted-foreground">
              Auto refresh every {REFRESH_INTERVAL_MS / 1_000}s
            </span>
          </div>
          <h1 className="text-4xl font-semibold tracking-tight md:text-5xl">
            Status
          </h1>
          <p className="text-sm text-muted-foreground">
            Status cards have been cleared so the panel can be redesigned one
            card at a time.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            type="button"
            onClick={() => void refresh()}
            disabled={isRefreshing}
          >
            {isRefreshing ? "Refreshing" : "Refresh"}
          </Button>
          <Button variant="ghost" type="button" onClick={onLogout}>
            Log out
          </Button>
        </div>
      </div>

      {errorMessage ? (
        <Alert variant="destructive" className="mb-4">
          <AlertTitle>Status refresh failed</AlertTitle>
          <AlertDescription>
            <pre className="whitespace-pre-wrap font-sans">{errorMessage}</pre>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="py-16 text-center">
        <Badge variant="outline">No status cards</Badge>
        <h2 className="mt-4 text-2xl font-semibold tracking-tight">
          Card canvas is empty
        </h2>
        <p className="mx-auto mt-2 max-w-xl text-sm text-muted-foreground">
          Last refresh: {refreshLabel}. The existing Status cards have been
          removed; new cards can now be designed and added individually.
        </p>
      </div>
    </section>
  );
}

function formatError(label: string, reason: unknown) {
  if (reason instanceof Error) {
    return `${label}: ${reason.message}`;
  }

  return `${label}: ${String(reason)}`;
}
