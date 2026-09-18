import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { XIcon } from "lucide-react";

import { AgentPage } from "@/components/status-page";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Spinner } from "@/components/ui/spinner";
import {
  ensureStudySession,
  fetchStudyGraph,
  fetchStudyNode,
  type DashboardSnapshot,
  type StudyGraphSnapshot,
  type StudyNodeDetail,
  type StudyNodeSummary,
} from "@/lib/daemon-api";
import { mockStudyNodeDetail } from "@/lib/study-mock";
import { StudyCircle } from "@/components/study-circle";
import { StudyTransferDialog } from "@/components/study-transfer-dialog";
import { studyProgressColor } from "@/lib/study-circle-layout";
import { useDashboardSnapshot } from "@/hooks/use-dashboard-snapshot";

export type StudySidebarNode = {
  id: string;
  moduleId: string;
  title: string;
  aliases: string[];
  understanding: number;
};

export type StudyGraphSummaryProps = {
  modules: Array<{
    id: string;
    title: string;
    nodeCount: number;
    masteredCount: number;
    inProgressCount: number;
    averageUnderstanding: number;
  }>;
  nodes: StudySidebarNode[];
  stats: StudyGraphSnapshot["stats"];
};

type StudyPageProps = {
  mockGraph?: StudyGraphSnapshot;
  mockSessionId?: string;
  mockSnapshot?: DashboardSnapshot;
  initialNodeId?: string;
  initialTab?: "overview" | "chat";
  selectedNodeId?: string | null;
  onSelectNode?: (nodeId: string | null) => void;
  searchQuery?: string;
  instantTransitions?: boolean;
  onGraphLoaded?: (summary: StudyGraphSummaryProps) => void;
};

type StudyPageStatus = "loading" | "ready" | "error";

export function StudyPage({
  mockGraph,
  mockSessionId,
  mockSnapshot,
  initialNodeId,
  initialTab,
  selectedNodeId: controlledNodeId,
  onSelectNode,
  searchQuery = "",
  instantTransitions = false,
  onGraphLoaded,
}: StudyPageProps) {
  const { t } = useTranslation();
  const isMock = Boolean(mockGraph);

  const [sessionId, setSessionId] = useState<string | null>(
    mockSessionId ?? null,
  );
  const [status, setStatus] = useState<StudyPageStatus>(
    mockGraph ? "ready" : "loading",
  );
  const [error, setError] = useState<string | null>(null);
  const [graph, setGraph] = useState<StudyGraphSnapshot | null>(
    mockGraph ?? null,
  );
  const [internalNodeId, setInternalNodeId] = useState<string | null>(
    initialNodeId ?? null,
  );
  const selectedNodeId =
    controlledNodeId !== undefined ? controlledNodeId : internalNodeId;
  const [detail, setDetail] = useState<StudyNodeDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [graphVersion, setGraphVersion] = useState(0);
  const [activeTab, setActiveTab] = useState<"overview" | "chat">(
    initialTab ?? "overview",
  );
  const [transferMode, setTransferMode] = useState<"import" | "export" | null>(
    null,
  );

  const mockGraphRef = useRef(mockGraph);
  mockGraphRef.current = mockGraph;
  const activeTurnRef = useRef(false);

  const loadGraph = useCallback(
    async (targetSessionId: string, signal?: AbortSignal) => {
      setStatus("loading");
      setError(null);
      try {
        const nextGraph = await fetchStudyGraph({
          sessionId: targetSessionId,
          signal,
        });
        if (signal?.aborted) {
          return;
        }
        setGraph(nextGraph);
        setStatus("ready");
        setGraphVersion((version) => version + 1);
      } catch (loadError) {
        if (signal?.aborted) {
          return;
        }
        setStatus("error");
        setError(
          loadError instanceof Error ? loadError.message : String(loadError),
        );
      }
    },
    [],
  );

  const { snapshot: studySnapshot } = useDashboardSnapshot(sessionId ?? "", {
    disabled: isMock || !sessionId,
  });
  const activeTurn =
    studySnapshot?.runtime_activity?.active_runtime_turn ?? false;
  const studyRevision =
    studySnapshot?.app_state_revisions?.find(([appId]) => appId === "study")?.[1] ??
    null;
  const loadedRevision = graph?.revision ?? null;

  useEffect(() => {
    if (isMock || !sessionId || studyRevision === null) {
      return;
    }
    if (loadedRevision !== null && studyRevision === loadedRevision) {
      return;
    }
    const timeout = window.setTimeout(() => {
      void loadGraph(sessionId);
    }, 250);
    return () => window.clearTimeout(timeout);
  }, [isMock, loadGraph, loadedRevision, sessionId, studyRevision]);

  useEffect(() => {
    if (isMock || !sessionId) {
      return;
    }
    const wasActive = activeTurnRef.current;
    activeTurnRef.current = activeTurn;
    if (wasActive && !activeTurn) {
      void loadGraph(sessionId);
    }
  }, [activeTurn, isMock, loadGraph, sessionId]);

  useEffect(() => {
    if (isMock) {
      setStatus("ready");
      return;
    }
    const controller = new AbortController();
    let cancelled = false;

    void (async () => {
      try {
        const session = await ensureStudySession({ signal: controller.signal });
        if (cancelled) {
          return;
        }
        setSessionId(session.session_id);
        await loadGraph(session.session_id, controller.signal);
      } catch (ensureError) {
        if (cancelled || controller.signal.aborted) {
          return;
        }
        setStatus("error");
        setError(
          ensureError instanceof Error
            ? ensureError.message
            : String(ensureError),
        );
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [isMock, loadGraph]);

  useEffect(() => {
    if (!graph || !onGraphLoaded) {
      return;
    }
    onGraphLoaded({
      modules: graph.modules.map((summary) => ({
        id: summary.module.id,
        title: summary.module.title,
        nodeCount: summary.node_count,
        masteredCount: summary.mastered_count,
        inProgressCount: summary.in_progress_count,
        averageUnderstanding: summary.average_understanding,
      })),
      nodes: graph.nodes.map((node) => ({
        id: node.id,
        moduleId: node.module_id,
        title: node.title,
        aliases: node.aliases,
        understanding: node.progress.understanding,
      })),
      stats: graph.stats,
    });
  }, [graph, onGraphLoaded]);

  const loadDetail = useCallback(
    async (nodeId: string, signal?: AbortSignal) => {
      const mockSource = mockGraphRef.current;
      if (mockSource) {
        setDetail(mockStudyNodeDetail(mockSource, nodeId));
        setDetailLoading(false);
        return;
      }
      if (!sessionId) {
        return;
      }
      setDetailLoading(true);
      try {
        const nextDetail = await fetchStudyNode({
          sessionId,
          nodeId,
          signal,
        });
        if (!signal?.aborted) {
          setDetail(nextDetail);
        }
      } catch (loadError) {
        if (!signal?.aborted) {
          setDetail(null);
          setError(
            loadError instanceof Error
              ? loadError.message
              : String(loadError),
          );
        }
      } finally {
        if (!signal?.aborted) {
          setDetailLoading(false);
        }
      }
    },
    [sessionId],
  );

  useEffect(() => {
    if (!selectedNodeId) {
      setDetail(null);
      return;
    }
    const controller = new AbortController();
    void loadDetail(selectedNodeId, controller.signal);
    return () => controller.abort();
  }, [selectedNodeId, loadDetail, graphVersion]);

  const handleSelectNode = useCallback(
    (nodeId: string | null) => {
      if (nodeId) {
        setActiveTab("overview");
      }
      if (onSelectNode) {
        onSelectNode(nodeId);
        return;
      }
      setInternalNodeId(nodeId);
    },
    [onSelectNode],
  );

  useEffect(() => {
    if (!graph || !selectedNodeId) {
      return;
    }
    if (!graph.nodes.some((node) => node.id === selectedNodeId)) {
      handleSelectNode(null);
    }
  }, [graph, selectedNodeId, handleSelectNode]);

  const studyFocus =
    studySnapshot?.app_focus?.find(([appId]) => appId === "study") ?? null;
  const consumedFocusRevisionRef = useRef(0);
  const pendingFocusNodeRef = useRef<string | null>(null);

  useEffect(() => {
    if (isMock || !studyFocus) {
      return;
    }
    const [, nodeId, revision] = studyFocus;
    if (revision <= consumedFocusRevisionRef.current) {
      return;
    }
    consumedFocusRevisionRef.current = revision;
    pendingFocusNodeRef.current = nodeId;
  }, [isMock, studyFocus]);

  useEffect(() => {
    const pending = pendingFocusNodeRef.current;
    if (!pending || !graph) {
      return;
    }
    if (graph.nodes.some((node) => node.id === pending)) {
      pendingFocusNodeRef.current = null;
      handleSelectNode(pending);
      return;
    }
    if (graph.revision >= consumedFocusRevisionRef.current) {
      pendingFocusNodeRef.current = null;
    }
  }, [graph, handleSelectNode]);

  const handleRefresh = useCallback(() => {
    if (isMock) {
      setStatus("ready");
      return;
    }
    if (sessionId) {
      void loadGraph(sessionId);
    }
  }, [isMock, sessionId, loadGraph]);

  const selectedSummary: StudyNodeSummary | null = useMemo(
    () =>
      graph?.nodes.find((node) => node.id === selectedNodeId) ?? null,
    [graph, selectedNodeId],
  );

  return (
    <section
      aria-label={t("study.pageAria")}
      className="flex h-screen w-full max-w-full overflow-hidden bg-background"
    >
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="relative min-h-0 flex-1">
          {status === "loading" && !graph ? (
            <StudyCenteredState>
              <Spinner />
              <p className="text-sm text-muted-foreground">
                {t("study.loadingGraph")}
              </p>
            </StudyCenteredState>
          ) : null}
          {status === "error" && !graph ? (
            <StudyCenteredState>
              <Alert variant="destructive" className="max-w-md">
                <AlertTitle>{t("study.loadErrorTitle")}</AlertTitle>
                <AlertDescription className="text-xs">{error}</AlertDescription>
              </Alert>
              <Button type="button" variant="outline" onClick={handleRefresh}>
                {t("common.retry")}
              </Button>
            </StudyCenteredState>
          ) : null}
          {graph && graph.nodes.length > 0 ? (
            <StudyCircle
              graph={graph}
              selectedNodeId={selectedNodeId}
              onSelectNode={handleSelectNode}
              searchQuery={searchQuery}
              instantTransitions={instantTransitions}
            />
          ) : null}
          {graph && graph.nodes.length === 0 ? (
            <StudyCenteredState>
              <Empty className="w-full max-w-md border border-dashed bg-card/60">
                <EmptyHeader>
                  <EmptyTitle>{t("study.emptyGraphTitle")}</EmptyTitle>
                  <EmptyDescription>
                    {t("study.emptyGraphDescription")}
                  </EmptyDescription>
                </EmptyHeader>
              </Empty>
            </StudyCenteredState>
          ) : null}
        </div>
      </div>

      <aside className="hidden w-[420px] shrink-0 flex-col border-l bg-background lg:flex">
        <div className="flex items-center gap-1 border-b px-2 py-2">
          <Button
            type="button"
            size="sm"
            variant={activeTab === "overview" ? "secondary" : "ghost"}
            onClick={() => setActiveTab("overview")}
          >
            {t("study.overviewTab")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant={activeTab === "chat" ? "secondary" : "ghost"}
            onClick={() => setActiveTab("chat")}
          >
            {t("study.chatTab")}
          </Button>
          {error ? (
            <span
              className="ml-auto truncate text-xs text-destructive"
              title={error}
            >
              {error}
            </span>
          ) : null}
        </div>

        <div className="min-h-0 flex-1">
          {activeTab === "overview" ? (
            <StudyOverviewPanel
              detail={detail}
              summary={selectedSummary}
              loading={detailLoading}
              onSelectNode={handleSelectNode}
              onClearFocus={() => handleSelectNode(null)}
            />
          ) : (
            <StudyChatPanel
              sessionId={sessionId}
              mockSnapshot={mockSnapshot}
              onImportGraph={() => setTransferMode("import")}
              onExportGraph={() => setTransferMode("export")}
            />
          )}
        </div>
      </aside>

      {transferMode ? (
        <StudyTransferDialog
          mode={transferMode}
          sessionId={sessionId}
          modules={(graph?.modules ?? []).map((summary) => ({
            id: summary.module.id,
            title: summary.module.title,
          }))}
          knownNodes={(graph?.nodes ?? []).map((node) => ({
            id: node.id,
            title: node.title,
          }))}
          onClose={() => setTransferMode(null)}
          onImported={() => {
            if (sessionId) {
              void loadGraph(sessionId);
            }
          }}
        />
      ) : null}
    </section>
  );
}

function StudyCenteredState({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-3 p-6">
      {children}
    </div>
  );
}

function StudyChatPanel({
  sessionId,
  mockSnapshot,
  onImportGraph,
  onExportGraph,
}: {
  sessionId: string | null;
  mockSnapshot?: DashboardSnapshot;
  onImportGraph: () => void;
  onExportGraph: () => void;
}) {
  const { t } = useTranslation();

  if (!sessionId && !mockSnapshot) {
    return (
      <StudyCenteredState>
        <Spinner />
        <p className="text-sm text-muted-foreground">
          {t("study.chatConnecting")}
        </p>
      </StudyCenteredState>
    );
  }

  return (
    <AgentPage
      sessionId={sessionId ?? "mock-study-session"}
      mockSnapshot={mockSnapshot}
      embedded
      allowSlashCommands={false}
      allowFileAttachments={false}
      onImportGraph={onImportGraph}
      onExportGraph={onExportGraph}
    />
  );
}
function StudyOverviewPanel({
  detail,
  summary,
  loading,
  onSelectNode,
  onClearFocus,
}: {
  detail: StudyNodeDetail | null;
  summary: StudyNodeSummary | null;
  loading: boolean;
  onSelectNode: (nodeId: string) => void;
  onClearFocus: () => void;
}) {
  const { t } = useTranslation();

  if (!detail && !summary) {
    return (
      <StudyCenteredState>
        <Empty className="w-full max-w-sm border border-dashed bg-card/60">
          <EmptyHeader>
            <EmptyTitle>{t("study.selectNodeTitle")}</EmptyTitle>
            <EmptyDescription>
              {t("study.selectNodeDescription")}
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </StudyCenteredState>
    );
  }

  const progress = detail?.progress ?? summary?.progress;
  const understanding = Math.min(
    100,
    Math.max(0, Math.round(progress?.understanding ?? 0)),
  );
  const title = detail?.node.title ?? summary?.title ?? "";
  const nodeSummary = detail?.node.summary ?? summary?.summary ?? "";
  const neighbors = detail?.neighbors ?? [];
  const prerequisites = neighbors.filter(
    (neighbor) =>
      neighbor.direction === "in" && neighbor.relation === "prerequisite",
  );
  const followUps = neighbors.filter(
    (neighbor) =>
      neighbor.direction === "out" && neighbor.relation === "prerequisite",
  );
  const related = neighbors.filter(
    (neighbor) => neighbor.relation !== "prerequisite",
  );
  const relationGroups = [
    { key: "prerequisites", label: t("study.prerequisitesLabel"), items: prerequisites },
    { key: "related", label: t("study.relatedLabel"), items: related },
    { key: "followUps", label: t("study.followUpsLabel"), items: followUps },
  ].filter((group) => group.items.length > 0);

  return (
    <ScrollArea className="h-full">
      <div className="flex flex-col gap-4 p-4">
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <span
              className="size-2.5 shrink-0 rounded-full"
              style={{ backgroundColor: studyProgressColor(understanding) }}
            />
            <h2 className="min-w-0 flex-1 truncate text-base font-semibold">
              {title}
            </h2>
            <Button
              type="button"
              size="icon-xs"
              variant="ghost"
              aria-label={t("study.exitFocus")}
              title={t("study.exitFocus")}
              onClick={onClearFocus}
            >
              <XIcon />
            </Button>
          </div>
          {nodeSummary ? (
            <p className="text-sm text-muted-foreground">{nodeSummary}</p>
          ) : null}
        </div>

        {loading && !detail ? <Spinner className="size-4" /> : null}

        {relationGroups.length > 0 ? (
          <div className="flex flex-col gap-3">
            {relationGroups.map((group) => (
              <div key={group.key} className="flex flex-col gap-1">
                <span className="text-xs font-medium text-muted-foreground">
                  {group.label}
                </span>
                <div className="flex flex-col">
                  {group.items.map((neighbor) => (
                    <button
                      key={neighbor.node.id}
                      type="button"
                      onClick={() => onSelectNode(neighbor.node.id)}
                      className="flex items-center gap-2 rounded-md px-1.5 py-1 text-left text-sm text-foreground/85 transition-colors hover:bg-muted"
                    >
                      <span
                        className="size-1.5 shrink-0 rounded-full"
                        style={{
                          backgroundColor: studyProgressColor(
                            neighbor.node.progress.understanding,
                          ),
                        }}
                        aria-hidden="true"
                      />
                      <span className="min-w-0 flex-1 truncate">
                        {neighbor.node.title}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        ) : null}
      </div>
    </ScrollArea>
  );
}
