import {
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useViewport,
  type Node as FlowNode,
  type NodeProps,
  type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { StudyEdge, StudyGraphSnapshot } from "@/lib/daemon-api";
import {
  STUDY_IDENTITY_QUATERNION,
  computeStudyFocusLayout,
  computeStudyNetwork,
  interpolateStudyLayouts,
  isStudyNodeVisibleForQuery,
  projectStudyNetwork,
  slerpStudyQuaternions,
  studyEdgeColor,
  studyProgressColor,
  studyQuaternionAngle,
  studySearchRotation,
  studyFocusViewExtent,
  type StudyCircleLayout,
  type StudyQuaternion,
} from "@/lib/study-circle-layout";
import { cn } from "@/lib/utils";

const LABEL_ZOOM_THRESHOLD = 1.12;
const FOCUS_TRANSITION_MS = 650;
const ROTATION_APPROACH_PER_SECOND = 6;
const ROTATION_ANGLE_EPSILON = 0.004;

type StudyPointNodeData = {
  title: string;
  color: string;
  radius: number;
  dimmed: boolean;
  matched: boolean;
  selected: boolean;
};

type StudyPointFlowNode = FlowNode<StudyPointNodeData>;

type LayoutTransition = {
  from: StudyCircleLayout;
  to: StudyCircleLayout;
  progress: number;
};

const StudyPointNode = memo(function StudyPointNode({
  data,
}: NodeProps<StudyPointFlowNode>) {
  const { zoom } = useViewport();
  const [hovered, setHovered] = useState(false);
  const showLabel =
    hovered || data.selected || data.matched || zoom >= LABEL_ZOOM_THRESHOLD;
  const diameter = data.radius * 2;
  const glow = `color-mix(in oklab, ${data.color} 32%, transparent)`;

  return (
    <div
      className={cn(
        "relative flex items-center justify-center overflow-visible rounded-full transition-opacity duration-200",
        data.dimmed && "opacity-15",
      )}
      style={{ width: diameter, height: diameter }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <span
        className="block rounded-full"
        style={{
          width: diameter,
          height: diameter,
          background: data.color,
          boxShadow: data.selected
            ? `0 0 0 2px var(--ring), 0 0 ${diameter}px ${glow}`
            : `0 0 ${Math.max(5, diameter * 0.8)}px ${glow}`,
        }}
      />
      {showLabel ? (
        <span
          className={cn(
            "pointer-events-none absolute top-full mt-1 max-w-40 truncate whitespace-nowrap text-[11px] leading-4",
            data.selected ? "text-foreground" : "text-muted-foreground",
          )}
        >
          {data.title}
        </span>
      ) : null}
    </div>
  );
});

const STUDY_CIRCLE_NODE_TYPES: NodeTypes = {
  studyPoint: StudyPointNode,
};

export function StudyCircle({
  graph,
  selectedNodeId,
  onSelectNode,
  searchQuery = "",
  instantTransitions = false,
}: {
  graph: StudyGraphSnapshot;
  selectedNodeId: string | null;
  onSelectNode: (nodeId: string | null) => void;
  searchQuery?: string;
  instantTransitions?: boolean;
}) {
  return (
    <ReactFlowProvider>
      <StudyCircleCanvas
        graph={graph}
        selectedNodeId={selectedNodeId}
        onSelectNode={onSelectNode}
        searchQuery={searchQuery}
        instantTransitions={instantTransitions}
      />
    </ReactFlowProvider>
  );
}

function StudyCircleCanvas({
  graph,
  selectedNodeId,
  onSelectNode,
  searchQuery,
  instantTransitions,
}: {
  graph: StudyGraphSnapshot;
  selectedNodeId: string | null;
  onSelectNode: (nodeId: string | null) => void;
  searchQuery: string;
  instantTransitions: boolean;
}) {
  const network = useMemo(() => computeStudyNetwork(graph), [graph]);
  const summaryById = useMemo(
    () => new Map(graph.nodes.map((node) => [node.id, node])),
    [graph.nodes],
  );
  const understandingById = useMemo(
    () =>
      new Map(
        graph.nodes.map((node) => [node.id, node.progress.understanding]),
      ),
    [graph.nodes],
  );

  const searchActive = searchQuery.trim().length > 0;
  const matchedIds = useMemo(() => {
    if (!searchActive) {
      return null;
    }
    return new Set(
      graph.nodes
        .filter((node) => isStudyNodeVisibleForQuery(searchQuery, node))
        .map((node) => node.id),
    );
  }, [graph.nodes, searchActive, searchQuery]);

  const rotationRef = useRef<StudyQuaternion>(STUDY_IDENTITY_QUATERNION);
  const [rotation, setRotation] = useState<StudyQuaternion>(
    STUDY_IDENTITY_QUATERNION,
  );

  useEffect(() => {
    const target = matchedIds
      ? studySearchRotation(network, matchedIds)
      : STUDY_IDENTITY_QUATERNION;
    if (instantTransitions) {
      rotationRef.current = target;
      setRotation(target);
      return;
    }
    let raf = 0;
    let cancelled = false;
    let last = performance.now();

    const step = (now: number) => {
      if (cancelled) {
        return;
      }
      const delta = Math.max(0, (now - last) / 1000);
      last = now;
      const current = rotationRef.current;
      if (studyQuaternionAngle(current, target) < ROTATION_ANGLE_EPSILON) {
        rotationRef.current = target;
        setRotation(target);
        return;
      }
      const next = slerpStudyQuaternions(
        current,
        target,
        1 - Math.exp(-delta * ROTATION_APPROACH_PER_SECOND),
      );
      rotationRef.current = next;
      setRotation(next);
      raf = requestAnimationFrame(step);
    };

    raf = requestAnimationFrame(step);
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
    };
  }, [instantTransitions, matchedIds, network]);

  const cloudLayout = useMemo(
    () => projectStudyNetwork(network, rotation),
    [network, rotation],
  );
  const cloudLayoutRef = useRef(cloudLayout);
  cloudLayoutRef.current = cloudLayout;

  const { setViewport } = useReactFlow();
  const wrapperRef = useRef<HTMLDivElement>(null);
  const frameCircle = useCallback(
    (extent: number, duration?: number) => {
      const rect = wrapperRef.current?.getBoundingClientRect();
      if (!rect || rect.width < 2 || rect.height < 2) {
        return;
      }
      const zoom = Math.min(
        3,
        Math.max(0.1, Math.min(rect.width, rect.height) / (2 * extent)),
      );
      const viewport = {
        x: rect.width / 2,
        y: rect.height / 2,
        zoom,
      };
      setViewport(viewport, duration ? { duration } : undefined);
      // The first frame after a graph or focus change can be measured before
      // the surrounding layout settles; re-frame once the container is final.
      window.setTimeout(() => {
        const settled = wrapperRef.current?.getBoundingClientRect();
        if (!settled || settled.width < 2 || settled.height < 2) {
          return;
        }
        if (
          Math.abs(settled.width - rect.width) < 1 &&
          Math.abs(settled.height - rect.height) < 1
        ) {
          return;
        }
        const settledZoom = Math.min(
          3,
          Math.max(
            0.1,
            Math.min(settled.width, settled.height) / (2 * extent),
          ),
        );
        setViewport({
          x: settled.width / 2,
          y: settled.height / 2,
          zoom: settledZoom,
        });
      }, 80);
    },
    [setViewport],
  );
  const [transition, setTransition] = useState<LayoutTransition | null>(null);
  const displayLayout = useMemo(
    () =>
      transition
        ? interpolateStudyLayouts(
            transition.from,
            transition.to,
            easeInOutCubic(transition.progress),
          )
        : cloudLayout,
    [cloudLayout, transition],
  );
  const displayLayoutRef = useRef(displayLayout);
  displayLayoutRef.current = displayLayout;
  const fittedIdentityRef = useRef<string | null>(null);
  const focusedNodeRef = useRef<string | null>(null);
  const viewExtentRef = useRef(cloudLayout.extent);
  const graphIdentity = useMemo(() => studyGraphIdentity(graph), [graph]);

  useEffect(() => {
    if (fittedIdentityRef.current === graphIdentity) {
      return;
    }
    fittedIdentityRef.current = graphIdentity;
    if (cloudLayout.nodes.length === 0) {
      return;
    }
    viewExtentRef.current = cloudLayout.extent;
    frameCircle(cloudLayout.extent);
  }, [cloudLayout.extent, cloudLayout.nodes.length, frameCircle, graphIdentity]);

  useEffect(() => {
    if (selectedNodeId === null && focusedNodeRef.current === null) {
      return;
    }
    focusedNodeRef.current = selectedNodeId;
    const target = selectedNodeId
      ? computeStudyFocusLayout(graph, selectedNodeId)
      : cloudLayoutRef.current;
    const viewExtent = selectedNodeId
      ? studyFocusViewExtent(graph, selectedNodeId)
      : cloudLayoutRef.current.extent;
    viewExtentRef.current = viewExtent;
    const from = displayLayoutRef.current;
    if (instantTransitions) {
      setTransition({ from: target, to: target, progress: 1 });
      frameCircle(viewExtent);
      return;
    }
    const startedAt = performance.now();
    let raf = 0;
    let cancelled = false;

    setTransition({ from, to: target, progress: 0 });
    frameCircle(viewExtent, FOCUS_TRANSITION_MS);

    const step = (now: number) => {
      if (cancelled) {
        return;
      }
      const progress = Math.min(1, (now - startedAt) / FOCUS_TRANSITION_MS);
      setTransition((current) =>
        current ? { ...current, progress } : current,
      );
      if (progress < 1) {
        raf = requestAnimationFrame(step);
      }
    };

    raf = requestAnimationFrame(step);
    return () => {
      cancelled = true;
      cancelAnimationFrame(raf);
    };
  }, [frameCircle, graph, instantTransitions, selectedNodeId]);

  useEffect(() => {
    const element = wrapperRef.current;
    if (!element) {
      return;
    }
    const observer = new ResizeObserver(() => {
      if (displayLayoutRef.current.nodes.length > 0) {
        frameCircle(viewExtentRef.current);
      }
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [frameCircle]);

  const flowNodes = useMemo<StudyPointFlowNode[]>(
    () =>
      displayLayout.nodes.map((item) => {
        const summary = summaryById.get(item.id);
        const understanding = summary?.progress.understanding ?? 0;
        return {
          id: item.id,
          type: "studyPoint",
          position: { x: item.x, y: item.y },
          origin: [0.5, 0.5] as [number, number],
          zIndex: Math.round(1000 + item.depth),
          draggable: false,
          selectable: false,
          data: {
            title: summary?.title ?? item.id,
            color: studyProgressColor(understanding),
            radius: item.radius,
            dimmed: matchedIds !== null && !matchedIds.has(item.id),
            matched: matchedIds?.has(item.id) ?? false,
            selected: item.id === selectedNodeId,
          },
          style: { width: item.radius * 2, height: item.radius * 2 },
        };
      }),
    [displayLayout, matchedIds, selectedNodeId, summaryById],
  );

  const flowEdges: never[] = [];

  return (
    <div ref={wrapperRef} className="h-full w-full bg-background">
      <ReactFlow
        nodes={flowNodes}
        edges={flowEdges}
        nodeTypes={STUDY_CIRCLE_NODE_TYPES}
        onInit={() => {
          frameCircle(cloudLayout.extent);
        }}
        minZoom={0.1}
        maxZoom={3}
        nodesDraggable={false}
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        panOnDrag
        panOnScroll
        zoomOnScroll
        zoomOnPinch
        zoomOnDoubleClick={false}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_, node) =>
          onSelectNode(node.id === selectedNodeId ? null : node.id)
        }
        onPaneClick={() => onSelectNode(null)}
        style={{ background: "transparent" }}
      >
        <StudyEdgeLayer
          edges={graph.edges}
          layout={displayLayout}
          understandingById={understandingById}
          matchedIds={matchedIds}
        />
      </ReactFlow>
    </div>
  );
}

function StudyEdgeLayer({
  edges,
  layout,
  understandingById,
  matchedIds,
}: {
  edges: StudyEdge[];
  layout: StudyCircleLayout;
  understandingById: Map<string, number>;
  matchedIds: Set<string> | null;
}) {
  const { x, y, zoom } = useViewport();

  return (
    <svg
      aria-hidden="true"
      className="pointer-events-none absolute inset-0 z-0 h-full w-full overflow-visible"
    >
      <g transform={`translate(${x} ${y}) scale(${zoom})`}>
        {edges.map((edge) => {
          const from = layout.nodeById.get(edge.from);
          const to = layout.nodeById.get(edge.to);
          if (!from || !to) {
            return null;
          }
          const dimmed =
            matchedIds !== null &&
            !(matchedIds.has(edge.from) && matchedIds.has(edge.to));
          return (
            <line
              key={`study-edge-${edge.id}`}
              x1={from.x}
              y1={from.y}
              x2={to.x}
              y2={to.y}
              stroke={studyEdgeColor(
                understandingById.get(edge.from),
                understandingById.get(edge.to),
              )}
              strokeWidth={1}
              opacity={dimmed ? 0.06 : 0.5}
            />
          );
        })}
      </g>
    </svg>
  );
}

function easeInOutCubic(t: number): number {
  return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
}

function studyGraphIdentity(graph: StudyGraphSnapshot) {
  const nodeIds = graph.nodes
    .map((node) => node.id)
    .sort()
    .join(",");
  const edgeIds = graph.edges
    .map((edge) => `${edge.id}:${edge.from}>${edge.to}`)
    .sort()
    .join(",");
  const moduleIds = graph.modules
    .map((summary) => summary.module.id)
    .sort()
    .join(",");
  return `${nodeIds}#${edgeIds}#${moduleIds}`;
}
