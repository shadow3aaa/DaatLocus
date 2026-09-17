import type { StudyEdge, StudyGraphSnapshot } from "@/lib/daemon-api";

const FORCE_REPULSION = 60_000;
const FORCE_SPRING = 0.03;
const SPRING_LENGTH = 130;
const FORCE_CENTER = 0.008;
const DAMPING = 0.85;
const SIMULATION_STEPS = 160;
const INITIAL_RADIUS = 400;
const TARGET_CLOUD_RADIUS = 520;
const DEFAULT_FOCAL_LENGTH = 1300;
const DEFAULT_POINT_RADIUS = 15;
const MIN_DEPTH = -560;
const MAX_DEPTH = 560;

/**
 * Understanding color: a smooth gradient between the faint unseen tone and the
 * strong primary tone, driven by the understanding percentage.
 */
export function studyProgressColor(understanding: number): string {
  const clamped = Math.min(100, Math.max(0, Math.round(understanding)));
  const unseenTone =
    "color-mix(in oklab, var(--muted-foreground) 60%, var(--border) 40%)";
  return `color-mix(in oklab, var(--primary) ${clamped}%, ${unseenTone} ${100 - clamped}%)`;
}

export type StudyCircleLayoutOptions = {
  focalLength?: number;
  pointRadius?: number;
};

export type StudyCircleNodeLayout = {
  id: string;
  x: number;
  y: number;
  depth: number;
  scale: number;
  radius: number;
};

export type StudyCircleLayout = {
  nodes: StudyCircleNodeLayout[];
  nodeById: Map<string, StudyCircleNodeLayout>;
  extent: number;
};

type Vec3 = {
  x: number;
  y: number;
  z: number;
};

type SimNode = {
  id: string;
  position: Vec3;
  velocity: Vec3;
};

export type StudyNetworkNode = {
  id: string;
  x: number;
  y: number;
  z: number;
};

export type StudyNetwork = {
  nodes: StudyNetworkNode[];
};

export type StudyQuaternion = {
  x: number;
  y: number;
  z: number;
  w: number;
};

export const STUDY_IDENTITY_QUATERNION: StudyQuaternion = {
  x: 0,
  y: 0,
  z: 0,
  w: 1,
};

export function computeStudyNetwork(graph: StudyGraphSnapshot): StudyNetwork {
  const simulated = simulateNetwork(graph.nodes, graph.edges);
  return {
    nodes: simulated.map((node) => ({
      id: node.id,
      x: node.position.x,
      y: node.position.y,
      z: node.position.z,
    })),
  };
}

export function projectStudyNetwork(
  network: StudyNetwork,
  rotation: StudyQuaternion = STUDY_IDENTITY_QUATERNION,
  options: StudyCircleLayoutOptions = {},
): StudyCircleLayout {
  const focalLength = options.focalLength ?? DEFAULT_FOCAL_LENGTH;
  const pointRadius = options.pointRadius ?? DEFAULT_POINT_RADIUS;

  const nodes: StudyCircleNodeLayout[] = network.nodes.map((node) => {
    const rotated = rotateStudyVector(node, rotation);
    const scale = perspectiveScale(rotated.z, focalLength);
    const radius = pointRadius * scale;
    return {
      id: node.id,
      x: rotated.x * scale,
      y: rotated.y * scale,
      depth: rotated.z,
      scale,
      radius,
    };
  });

  nodes.sort((a, b) => a.id.localeCompare(b.id));

  const extent = nodes.reduce(
    (max, node) => Math.max(max, Math.hypot(node.x, node.y) + node.radius),
    0,
  );

  return {
    nodes,
    nodeById: new Map(nodes.map((node) => [node.id, node])),
    extent: extent + 60,
  };
}

export function computeStudyCircleLayout(
  graph: StudyGraphSnapshot,
  options: StudyCircleLayoutOptions = {},
): StudyCircleLayout {
  return projectStudyNetwork(computeStudyNetwork(graph), undefined, options);
}

/**
 * Rotation that brings the matched set's centroid toward the camera, so a
 * search recenters its matches instead of only dimming everything else.
 */
export function studySearchRotation(
  network: StudyNetwork,
  matchedIds: Set<string>,
): StudyQuaternion {
  const matched = network.nodes.filter((node) => matchedIds.has(node.id));
  if (matched.length === 0) {
    return STUDY_IDENTITY_QUATERNION;
  }
  const centroid = matched.reduce(
    (accumulator, node) => ({
      x: accumulator.x + node.x / matched.length,
      y: accumulator.y + node.y / matched.length,
      z: accumulator.z + node.z / matched.length,
    }),
    { x: 0, y: 0, z: 0 },
  );
  const length = Math.hypot(centroid.x, centroid.y, centroid.z);
  if (length < 1e-6) {
    return STUDY_IDENTITY_QUATERNION;
  }
  return studyQuaternionFromTo(
    { x: centroid.x / length, y: centroid.y / length, z: centroid.z / length },
    { x: 0, y: 0, z: 1 },
  );
}

export function studyQuaternionFromTo(
  from: Vec3,
  to: Vec3,
): StudyQuaternion {
  const cross = {
    x: from.y * to.z - from.z * to.y,
    y: from.z * to.x - from.x * to.z,
    z: from.x * to.y - from.y * to.x,
  };
  const dot = from.x * to.x + from.y * to.y + from.z * to.z;

  if (dot < -0.999_999) {
    const axis =
      Math.abs(from.x) < 0.9 ? { x: 1, y: 0, z: 0 } : { x: 0, y: 1, z: 0 };
    const perpendicular = normalizeVector({
      x: from.y * axis.z - from.z * axis.y,
      y: from.z * axis.x - from.x * axis.z,
      z: from.x * axis.y - from.y * axis.x,
    });
    return normalizeQuaternion({
      x: perpendicular.x,
      y: perpendicular.y,
      z: perpendicular.z,
      w: 0,
    });
  }

  return normalizeQuaternion({
    x: cross.x,
    y: cross.y,
    z: cross.z,
    w: 1 + dot,
  });
}

export function slerpStudyQuaternions(
  from: StudyQuaternion,
  to: StudyQuaternion,
  t: number,
): StudyQuaternion {
  const clamped = Math.min(1, Math.max(0, t));
  let target = to;
  let dot = from.x * to.x + from.y * to.y + from.z * to.z + from.w * to.w;
  if (dot < 0) {
    target = { x: -to.x, y: -to.y, z: -to.z, w: -to.w };
    dot = -dot;
  }
  if (dot > 0.999_5) {
    return normalizeQuaternion({
      x: from.x + (target.x - from.x) * clamped,
      y: from.y + (target.y - from.y) * clamped,
      z: from.z + (target.z - from.z) * clamped,
      w: from.w + (target.w - from.w) * clamped,
    });
  }
  const theta = Math.acos(Math.min(1, Math.max(-1, dot)));
  const sinTheta = Math.sin(theta);
  const fromWeight = Math.sin((1 - clamped) * theta) / sinTheta;
  const toWeight = Math.sin(clamped * theta) / sinTheta;
  return normalizeQuaternion({
    x: from.x * fromWeight + target.x * toWeight,
    y: from.y * fromWeight + target.y * toWeight,
    z: from.z * fromWeight + target.z * toWeight,
    w: from.w * fromWeight + target.w * toWeight,
  });
}

export function studyQuaternionAngle(
  from: StudyQuaternion,
  to: StudyQuaternion,
): number {
  const dot = Math.abs(
    from.x * to.x + from.y * to.y + from.z * to.z + from.w * to.w,
  );
  return 2 * Math.acos(Math.min(1, Math.max(-1, dot)));
}

export function rotateStudyVector(
  vector: Vec3,
  rotation: StudyQuaternion,
): Vec3 {
  const tx = 2 * (rotation.y * vector.z - rotation.z * vector.y);
  const ty = 2 * (rotation.z * vector.x - rotation.x * vector.z);
  const tz = 2 * (rotation.x * vector.y - rotation.y * vector.x);
  return {
    x: vector.x + rotation.w * tx + (rotation.y * tz - rotation.z * ty),
    y: vector.y + rotation.w * ty + (rotation.z * tx - rotation.x * tz),
    z: vector.z + rotation.w * tz + (rotation.x * ty - rotation.y * tx),
  };
}

function normalizeVector(vector: Vec3): Vec3 {
  const length = Math.hypot(vector.x, vector.y, vector.z);
  if (length < 1e-9) {
    return { x: 0, y: 0, z: 0 };
  }
  return {
    x: vector.x / length,
    y: vector.y / length,
    z: vector.z / length,
  };
}

function normalizeQuaternion(quaternion: StudyQuaternion): StudyQuaternion {
  const length = Math.hypot(
    quaternion.x,
    quaternion.y,
    quaternion.z,
    quaternion.w,
  );
  if (length < 1e-9) {
    return STUDY_IDENTITY_QUATERNION;
  }
  return {
    x: quaternion.x / length,
    y: quaternion.y / length,
    z: quaternion.z / length,
    w: quaternion.w / length,
  };
}

export function studyEdgeColor(
  from: number | undefined,
  to: number | undefined,
): string {
  return `color-mix(in oklab, ${studyProgressColor(from ?? 0)} 50%, ${studyProgressColor(to ?? 0)} 50%)`;
}

export function isStudyNodeVisibleForQuery(
  query: string,
  node: { title: string; aliases: string[] },
): boolean {
  const normalized = query.trim().toLowerCase();
  if (!normalized) {
    return true;
  }
  return (
    node.title.toLowerCase().includes(normalized) ||
    node.aliases.some((alias) => alias.toLowerCase().includes(normalized))
  );
}

const FOCUS_RING_GAP = 170;
const FOCUS_MIN_ARC_SPACING = 84;

/**
 * Flat focus projection: the focused node sits at the center and every other
 * node is placed by its relational hop distance from it. Nodes inside each
 * distance ring are ordered by the barycenter of their already-placed
 * neighbors (Sugiyama-style sweeps) and spread evenly across the ring, which
 * keeps relations radiating outward instead of crossing. There is no
 * perspective in this projection.
 */
export function computeStudyFocusLayout(
  graph: StudyGraphSnapshot,
  focusNodeId: string,
  options: StudyCircleLayoutOptions = {},
): StudyCircleLayout {
  const pointRadius = options.pointRadius ?? DEFAULT_POINT_RADIUS;
  const adjacency = buildAdjacency(graph);
  const distances = relationalDistances(adjacency, focusNodeId);
  const furthest = graph.nodes.reduce(
    (max, node) => Math.max(max, distances.get(node.id) ?? 0),
    0,
  );
  const unreachableRing = furthest + 1;

  const ringIds = new Map<number, string[]>();
  for (const node of [...graph.nodes].sort((a, b) => a.id.localeCompare(b.id))) {
    const distance = distances.get(node.id) ?? unreachableRing;
    const ring = ringIds.get(distance);
    if (ring) {
      ring.push(node.id);
    } else {
      ringIds.set(distance, [node.id]);
    }
  }
  const ringNumbers = [...ringIds.keys()].sort((a, b) => a - b);
  const orderedRings = orderFocusRings(
    ringIds,
    ringNumbers,
    adjacency,
    distances,
  );

  const nodes: StudyCircleNodeLayout[] = [];
  let previousRadius = 0;
  for (const ring of ringNumbers) {
    const ids = orderedRings.get(ring) ?? [];
    const spreadRadius = (ids.length * FOCUS_MIN_ARC_SPACING) / (Math.PI * 2);
    const radius =
      ring === 0
        ? 0
        : Math.max(previousRadius + FOCUS_RING_GAP, spreadRadius);
    previousRadius = radius;
    ids.forEach((id, index) => {
      const angle = ((index + 0.5) * Math.PI * 2) / Math.max(1, ids.length);
      nodes.push({
        id,
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
        depth: 0,
        scale: 1,
        radius: pointRadius,
      });
    });
  }

  const extent = nodes.reduce(
    (max, node) => Math.max(max, Math.hypot(node.x, node.y) + node.radius),
    pointRadius,
  );

  return {
    nodes,
    nodeById: new Map(nodes.map((node) => [node.id, node])),
    extent: extent + 60,
  };
}

/**
 * Viewport extent for a focus projection: zoom to the focused node and its
 * immediate relational neighborhood instead of the whole radial layout.
 */
export function studyFocusViewExtent(
  graph: StudyGraphSnapshot,
  focusNodeId: string,
  options: StudyCircleLayoutOptions = {},
): number {
  const pointRadius = options.pointRadius ?? DEFAULT_POINT_RADIUS;
  const distances = relationalDistances(buildAdjacency(graph), focusNodeId);
  const furthest = [...distances.values()].reduce(
    (max, distance) => Math.max(max, distance),
    0,
  );
  const nearestRingCount = [...distances.values()].filter(
    (distance) => distance === 1,
  ).length;
  const nearestRingRadius = Math.max(
    FOCUS_RING_GAP,
    (nearestRingCount * FOCUS_MIN_ARC_SPACING) / (Math.PI * 2),
  );
  const rings = furthest >= 1 ? 1 : 0;
  return rings * nearestRingRadius + pointRadius + 90;
}

function orderFocusRings(
  ringIds: Map<number, string[]>,
  ringNumbers: number[],
  adjacency: Map<string, string[]>,
  distances: Map<string, number>,
): Map<number, string[]> {
  const ordered = new Map<number, string[]>(
    ringNumbers.map((ring) => [ring, [...(ringIds.get(ring) ?? [])]]),
  );
  const positionOf = new Map<string, number>();

  const refreshPositions = (ids: string[]) => {
    ids.forEach((id, index) => {
      positionOf.set(id, (index + 0.5) / Math.max(1, ids.length));
    });
  };

  refreshPositions(ordered.get(0) ?? []);
  for (const id of ordered.values()) {
    for (const nodeId of id) {
      if (!positionOf.has(nodeId)) {
        positionOf.set(nodeId, 0.5);
      }
    }
  }

  const sweeps: Array<"down" | "up"> = ["down", "up", "down"];
  for (const direction of sweeps) {
    const rings =
      direction === "down" ? ringNumbers : [...ringNumbers].reverse();
    for (const ring of rings) {
      if (ring === 0) {
        continue;
      }
      const ids = ordered.get(ring) ?? [];
      const ranked = ids.map((id) => {
        const references = (adjacency.get(id) ?? []).filter((neighbor) => {
          const distance = distances.get(neighbor);
          if (distance === undefined || distance === ring) {
            return false;
          }
          return direction === "down" ? distance < ring : distance > ring;
        });
        if (references.length === 0) {
          return { id, barycenter: null as number | null };
        }
        const barycenter =
          references.reduce(
            (sum, neighbor) => sum + (positionOf.get(neighbor) ?? 0.5),
            0,
          ) / references.length;
        return { id, barycenter };
      });
      ranked.sort((a, b) => {
        if (a.barycenter === null && b.barycenter === null) {
          return a.id.localeCompare(b.id);
        }
        if (a.barycenter === null) {
          return 1;
        }
        if (b.barycenter === null) {
          return -1;
        }
        return a.barycenter - b.barycenter || a.id.localeCompare(b.id);
      });
      const nextOrder = ranked.map((item) => item.id);
      ordered.set(ring, nextOrder);
      refreshPositions(nextOrder);
    }
  }

  return ordered;
}

export function interpolateStudyLayouts(
  from: StudyCircleLayout,
  to: StudyCircleLayout,
  progress: number,
): StudyCircleLayout {
  const t = Math.min(1, Math.max(0, progress));
  const nodes = from.nodes.map((node) => {
    const target = to.nodeById.get(node.id);
    if (!target) {
      return node;
    }
    return {
      id: node.id,
      x: lerp(node.x, target.x, t),
      y: lerp(node.y, target.y, t),
      depth: lerp(node.depth, target.depth, t),
      scale: lerp(node.scale, target.scale, t),
      radius: lerp(node.radius, target.radius, t),
    };
  });
  return {
    nodes,
    nodeById: new Map(nodes.map((node) => [node.id, node])),
    extent: lerp(from.extent, to.extent, t),
  };
}

function lerp(from: number, to: number, t: number): number {
  return from + (to - from) * t;
}

function buildAdjacency(graph: StudyGraphSnapshot): Map<string, string[]> {
  const adjacency = new Map<string, string[]>();
  for (const node of graph.nodes) {
    adjacency.set(node.id, []);
  }
  for (const edge of graph.edges) {
    if (!adjacency.has(edge.from) || !adjacency.has(edge.to)) {
      continue;
    }
    adjacency.get(edge.from)?.push(edge.to);
    adjacency.get(edge.to)?.push(edge.from);
  }
  return adjacency;
}

function relationalDistances(
  adjacency: Map<string, string[]>,
  focusNodeId: string,
): Map<string, number> {
  const distances = new Map<string, number>();
  if (!adjacency.has(focusNodeId)) {
    return distances;
  }
  distances.set(focusNodeId, 0);
  const queue = [focusNodeId];
  for (let index = 0; index < queue.length; index += 1) {
    const current = queue[index];
    const distance = distances.get(current) ?? 0;
    for (const neighbor of adjacency.get(current) ?? []) {
      if (!distances.has(neighbor)) {
        distances.set(neighbor, distance + 1);
        queue.push(neighbor);
      }
    }
  }
  return distances;
}

function simulateNetwork(
  graphNodes: StudyGraphSnapshot["nodes"],
  edges: StudyEdge[],
): SimNode[] {
  const ordered = [...graphNodes].sort((a, b) => a.id.localeCompare(b.id));
  const count = ordered.length;
  if (count === 0) {
    return [];
  }

  const golden = Math.PI * (3 - Math.sqrt(5));
  const nodes: SimNode[] = ordered.map((node, position) => {
    const y = count === 1 ? 0 : 1 - (position / (count - 1)) * 2;
    const ringRadius = Math.sqrt(Math.max(0, 1 - y * y));
    const theta = golden * position;
    return {
      id: node.id,
      position: {
        x: Math.cos(theta) * ringRadius * INITIAL_RADIUS,
        y: y * INITIAL_RADIUS,
        z: Math.sin(theta) * ringRadius * INITIAL_RADIUS,
      },
      velocity: { x: 0, y: 0, z: 0 },
    };
  });

  const index = new Map(nodes.map((node, position) => [node.id, position]));
  const springs = edges
    .map((edge) => ({
      from: index.get(edge.from),
      to: index.get(edge.to),
    }))
    .filter(
      (spring): spring is { from: number; to: number } =>
        spring.from !== undefined && spring.to !== undefined,
    );

  for (let step = 0; step < SIMULATION_STEPS; step += 1) {
    const forces = nodes.map(() => ({ x: 0, y: 0, z: 0 }));

    for (let a = 0; a < nodes.length; a += 1) {
      for (let b = a + 1; b < nodes.length; b += 1) {
        const dx = nodes[a].position.x - nodes[b].position.x;
        const dy = nodes[a].position.y - nodes[b].position.y;
        const dz = nodes[a].position.z - nodes[b].position.z;
        const distanceSquared = Math.max(dx * dx + dy * dy + dz * dz, 1);
        const distance = Math.sqrt(distanceSquared);
        const magnitude = FORCE_REPULSION / distanceSquared;
        const fx = (dx / distance) * magnitude;
        const fy = (dy / distance) * magnitude;
        const fz = (dz / distance) * magnitude;
        forces[a].x += fx;
        forces[a].y += fy;
        forces[a].z += fz;
        forces[b].x -= fx;
        forces[b].y -= fy;
        forces[b].z -= fz;
      }
    }

    for (const spring of springs) {
      const dx = nodes[spring.to].position.x - nodes[spring.from].position.x;
      const dy = nodes[spring.to].position.y - nodes[spring.from].position.y;
      const dz = nodes[spring.to].position.z - nodes[spring.from].position.z;
      const distance = Math.max(Math.hypot(dx, dy, dz), 1);
      const magnitude = (distance - SPRING_LENGTH) * FORCE_SPRING;
      const fx = (dx / distance) * magnitude;
      const fy = (dy / distance) * magnitude;
      const fz = (dz / distance) * magnitude;
      forces[spring.from].x += fx;
      forces[spring.from].y += fy;
      forces[spring.from].z += fz;
      forces[spring.to].x -= fx;
      forces[spring.to].y -= fy;
      forces[spring.to].z -= fz;
    }

    for (let position = 0; position < nodes.length; position += 1) {
      const node = nodes[position];
      const force = forces[position];
      node.velocity.x = (node.velocity.x + force.x - node.position.x * FORCE_CENTER) * DAMPING;
      node.velocity.y = (node.velocity.y + force.y - node.position.y * FORCE_CENTER) * DAMPING;
      node.velocity.z = (node.velocity.z + force.z - node.position.z * FORCE_CENTER) * DAMPING;
      node.position.x += node.velocity.x;
      node.position.y += node.velocity.y;
      node.position.z += node.velocity.z;
    }
  }

  return normalizeCloud(nodes);
}

function normalizeCloud(nodes: SimNode[]): SimNode[] {
  const centroid = nodes.reduce(
    (accumulator, node) => ({
      x: accumulator.x + node.position.x / nodes.length,
      y: accumulator.y + node.position.y / nodes.length,
      z: accumulator.z + node.position.z / nodes.length,
    }),
    { x: 0, y: 0, z: 0 },
  );

  for (const node of nodes) {
    node.position.x -= centroid.x;
    node.position.y -= centroid.y;
    node.position.z -= centroid.z;
  }

  const furthest = nodes.reduce(
    (max, node) => Math.max(max, Math.hypot(node.position.x, node.position.y, node.position.z)),
    0,
  );
  const scale = furthest > 0 ? TARGET_CLOUD_RADIUS / furthest : 1;

  for (const node of nodes) {
    node.position.x *= scale;
    node.position.y *= scale;
    node.position.z *= scale;
  }

  return nodes;
}

function perspectiveScale(depth: number, focalLength: number): number {
  const clamped = Math.min(Math.max(depth, MIN_DEPTH), MAX_DEPTH);
  return focalLength / (focalLength - clamped);
}
