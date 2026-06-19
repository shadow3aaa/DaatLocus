import { useEffect, useMemo, useState } from "react";

import {
  subscribeDashboardSnapshots,
  type DashboardSnapshot,
} from "@/lib/daemon-api";

const DASHBOARD_STREAM_RECONNECT_MS = 1500;

type UseDashboardSnapshotOptions = {
  disabled?: boolean;
  initialSnapshot?: DashboardSnapshot | null;
};
type DashboardSnapshotState = {
  sessionId: string;
  snapshot: DashboardSnapshot | null;
};

export function useDashboardSnapshot(
  sessionId: string,
  options: UseDashboardSnapshotOptions = {},
) {
  const { disabled = false, initialSnapshot = null } = options;
  const [snapshotState, setSnapshotState] = useState<DashboardSnapshotState>(
    () => ({
      sessionId,
      snapshot: initialSnapshot,
    }),
  );
  const snapshot = useMemo(
    () => (snapshotState.sessionId === sessionId ? snapshotState.snapshot : null),
    [sessionId, snapshotState],
  );
  const [isLoading, setIsLoading] = useState(!disabled && !initialSnapshot);
  const [loadError, setLoadError] = useState<Error | null>(null);

  useEffect(() => {
    if (disabled) {
      setSnapshotState({ sessionId, snapshot: initialSnapshot });
      setIsLoading(false);
      setLoadError(null);
      return;
    }

    let isActive = true;
    let reconnectTimeout: number | undefined;
    let subscription: ReturnType<typeof subscribeDashboardSnapshots> | null =
      null;

    function connect() {
      try {
        subscription = subscribeDashboardSnapshots({
          sessionId,
          onSnapshot: (nextSnapshot) => {
            if (!isActive) {
              return;
            }

            setSnapshotState({ sessionId, snapshot: nextSnapshot });
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
        reconnectTimeout = window.setTimeout(
          connect,
          DASHBOARD_STREAM_RECONNECT_MS,
        );
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
  }, [disabled, initialSnapshot, sessionId]);

  return {
    isLoading: isLoading || snapshotState.sessionId !== sessionId,
    loadError,
    snapshot,
  };
}
