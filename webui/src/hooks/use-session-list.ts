import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ActorApiError, subscribeCatalogEvents, type SessionSummary } from "@/lib/actor-api";
import { listSessions } from "@/lib/session-api";
import { getDevices, type Device } from "@/lib/stage-api";
import { conversationDevice } from "@/lib/conversation-links";
import { applySessionEvent, compareSessions, nextSessionSource, sessionCursorKey, upsertSession, visibleSessions, type SessionSource } from "@/lib/session-list";

const PAGE = 30;

async function onlineDevices(signal: AbortSignal): Promise<Device[]> {
  const devices: Device[] = [];
  let after: string | undefined;
  do {
    const page = await getDevices({after, signal});
    devices.push(...page.items);
    after = page.next_cursor ?? undefined;
  } while (after);
  const local = conversationDevice();
  return devices.filter(device => device.stage_id !== local && device.connection.status === "online");
}

/**
 * Sessions of every reachable account device, merged by last activity. Each
 * device streams its own catalog while the list is active.
 */
export function useSessionList(archived: boolean, active: boolean, onAccessRevoked: () => void) {
  const [sources, setSources] = useState<SessionSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [deviceListFailed, setDeviceListFailed] = useState(false);
  const [revision, setRevision] = useState(0);
  const revoked = useRef(onAccessRevoked);
  revoked.current = onAccessRevoked;
  const scope = useRef<AbortController | null>(null);
  const shown = useRef(archived);
  const denied = (error: unknown) => { if (error instanceof ActorApiError && [401, 403, 410].includes(error.status)) { revoked.current(); return true; } return false; };

  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    scope.current = controller;
    const signal = controller.signal;
    if (shown.current !== archived) { shown.current = archived; setSources([]); }
    setLoading(true); setFailed(false);
    const replace = (stage: string | null, change: (source: SessionSource) => SessionSource) =>
      setSources(old => old.map(source => source.stage === stage ? change(source) : source));
    let resyncTimer: ReturnType<typeof setTimeout> | undefined;
    const resync = () => { clearTimeout(resyncTimer); resyncTimer = setTimeout(() => setRevision(value => value + 1), 300); };
    function stream(stage: string | null, after: number) {
      void subscribeCatalogEvents({after, signal,
        onEvent: record => replace(stage, source => applySessionEvent(source, record.event, archived)),
        onAccessRevoked: () => revoked.current(),
        onResync: resync,
      }, stage ?? undefined).catch(error => { if (!signal.aborted) denied(error); });
    }
    async function load() {
      const [local, devices] = await Promise.allSettled([
        listSessions({archived, limit: PAGE, signal}),
        onlineDevices(signal),
      ]);
      if (signal.aborted) return;
      setDeviceListFailed(devices.status === "rejected");
      if (devices.status === "rejected") denied(devices.reason);
      if (local.status === "rejected") { if (!denied(local.reason)) setFailed(true); return; }
      const remote = devices.status === "fulfilled" ? devices.value : [];
      const pages = await Promise.allSettled(remote.map(device => listSessions({archived, limit: PAGE, stage: device.stage_id, signal})));
      if (signal.aborted) return;
      const next: SessionSource[] = [{stage: null, label: null, items: local.value.items, next: local.value.next_cursor, failed: false}];
      remote.forEach((device, index) => {
        const page = pages[index];
        if (page.status === "fulfilled") {
          next.push({stage: device.stage_id, label: device.label, items: page.value.items, next: page.value.next_cursor, failed: false});
        } else next.push({stage: device.stage_id, label: device.label, items: [], next: null, failed: true});
      });
      setSources(next);
      stream(null, local.value.event_cursor ?? 0);
      remote.forEach((device, index) => {
        const page = pages[index];
        if (page.status === "fulfilled") stream(device.stage_id, page.value.event_cursor ?? 0);
      });
    }
    void load().finally(() => { if (!signal.aborted) setLoading(false); });
    return () => { controller.abort(); clearTimeout(resyncTimer); };
  }, [archived, active, revision]);

  const more = useCallback(async (signal: AbortSignal) => {
    const source = nextSessionSource(sources);
    if (!source?.next || !scope.current) return;
    try {
      const page = await listSessions({archived, after: source.next, limit: PAGE, stage: source.stage ?? undefined, signal: AbortSignal.any([signal, scope.current.signal])});
      if (signal.aborted) return;
      setSources(old => old.map(item => item.stage === source.stage && item.next === source.next
        ? {...item, items: [...item.items, ...page.items.filter(session => !item.items.some(old => old.id === session.id))].sort(compareSessions), next: page.next_cursor}
        : item));
    } catch (error) {
      if (!signal.aborted) { denied(error); throw error; }
    }
  }, [sources, archived]);

  /** Apply a summary returned by a session mutation before its catalog event arrives. */
  const update = useCallback((stage: string | null, session: SessionSummary) =>
    setSources(old => old.map(source => source.stage === stage ? upsertSession(source, session, archived) : source)), [archived]);
  const remove = useCallback((stage: string | null, id: string) =>
    setSources(old => old.map(source => source.stage === stage ? {...source, items: source.items.filter(item => item.id !== id)} : source)), []);

  const entries = useMemo(() => visibleSessions(sources), [sources]);
  return {
    entries, loading, failed, more, update, remove,
    hasMore: nextSessionSource(sources) !== null,
    cursor: sessionCursorKey(sources),
    unavailable: sources.filter(source => source.failed).length + Number(deviceListFailed),
    retry: () => setRevision(value => value + 1),
  };
}
