import { describe, expect, test } from "bun:test";

import type { StudyGraphSnapshot, StudyNodeSummary } from "../src/lib/daemon-api";
import {
  STUDY_IDENTITY_QUATERNION,
  computeStudyCircleLayout,
  computeStudyFocusLayout,
  computeStudyNetwork,
  interpolateStudyLayouts,
  isStudyNodeVisibleForQuery,
  projectStudyNetwork,
  rotateStudyVector,
  studyEdgeColor,
  studyFocusViewExtent,
  studyProgressColor,
  studyQuaternionAngle,
  studySearchRotation,
} from "../src/lib/study-circle-layout";

function node(
  id: string,
  moduleId: string,
  overrides: Partial<StudyNodeSummary> = {},
): StudyNodeSummary {
  return {
    id,
    module_id: moduleId,
    title: id,
    summary: "",
    aliases: [],
    tags: [],
    content_version: 1,
    progress: {
      understanding: 0,
      evidence: "",
      updated_by: "code",
      updated_at_ms: 0,
    },
    question_count: 0,
    stale_question_count: 0,
    ...overrides,
  };
}

function graph(
  nodes: StudyNodeSummary[],
  edges: StudyGraphSnapshot["edges"],
): StudyGraphSnapshot {
  const moduleIds = [...new Set(nodes.map((item) => item.module_id))].sort();
  return {
    generated_at_ms: 0,
    modules: moduleIds.map((moduleId, index) => ({
      module: {
        id: moduleId,
        title: moduleId,
        description: "",
        created_at_ms: index,
        updated_at_ms: index,
      },
      node_count: nodes.filter((item) => item.module_id === moduleId).length,
      mastered_count: 0,
      reviewing_count: 0,
      learning_count: 0,
      unseen_count: 0,
    })),
    nodes,
    edges,
    stats: {
      module_count: moduleIds.length,
      node_count: nodes.length,
      edge_count: edges.length,
      mastered_count: 0,
      reviewing_count: 0,
      learning_count: 0,
      unseen_count: nodes.length,
      question_count: 0,
      stale_question_count: 0,
      attempt_count: 0,
    },
    maintenance: {
      orphan_node_ids: [],
      empty_module_ids: [],
      duplicate_candidate_groups: [],
      unlinked_node_ids: [],
      stale_question_count: 0,
    },
  };
}

describe("computeStudyCircleLayout", () => {
  const snapshot = graph(
    [
      node("a", "m1"),
      node("b", "m1"),
      node("c", "m2"),
      node("d", "m2"),
      node("e", "m2"),
    ],
    [
      { id: 1, from: "a", to: "b", relation: "prerequisite", note: "" },
      { id: 2, from: "b", to: "c", relation: "related", note: "" },
      { id: 3, from: "c", to: "d", relation: "contrast", note: "" },
    ],
  );

  test("places every node with finite coordinates and positive radius", () => {
    const layout = computeStudyCircleLayout(snapshot);

    expect(layout.nodes.map((item) => item.id).sort()).toEqual([
      "a",
      "b",
      "c",
      "d",
      "e",
    ]);
    for (const item of layout.nodes) {
      expect(Number.isFinite(item.x)).toBe(true);
      expect(Number.isFinite(item.y)).toBe(true);
      expect(item.radius).toBeGreaterThan(0);
      expect(Number.isFinite(item.depth)).toBe(true);
    }
    expect(layout.extent).toBeGreaterThan(0);
  });

  test("layout is deterministic", () => {
    const first = computeStudyCircleLayout(snapshot);
    const second = computeStudyCircleLayout(snapshot);
    expect(second.nodes).toEqual(first.nodes);
  });

  test("connected nodes do not collapse onto one point", () => {
    const layout = computeStudyCircleLayout(snapshot);
    const distances: number[] = [];
    for (let a = 0; a < layout.nodes.length; a += 1) {
      for (let b = a + 1; b < layout.nodes.length; b += 1) {
        distances.push(
          Math.hypot(
            layout.nodes[a].x - layout.nodes[b].x,
            layout.nodes[a].y - layout.nodes[b].y,
          ),
        );
      }
    }
    expect(Math.min(...distances)).toBeGreaterThan(1);
  });

  test("perspective makes closer nodes larger", () => {
    const layout = computeStudyCircleLayout(snapshot);
    for (const item of layout.nodes) {
      expect(item.radius).toBeGreaterThan(15 * 0.7);
      expect(item.radius).toBeLessThan(15 * 1.7);
      const deeper = layout.nodes.find((other) => other.depth > item.depth);
      if (deeper && item.id !== deeper.id) {
        expect(item.radius).toBeLessThan(deeper.radius + 0.0001);
      }
    }
  });
});

describe("study colors", () => {
  test("understanding drives a smooth theme-token gradient", () => {
    const zero = studyProgressColor(0);
    const half = studyProgressColor(50);
    const full = studyProgressColor(100);
    for (const color of [zero, half, full]) {
      expect(color).toContain("color-mix");
      expect(color).toContain("var(--primary)");
      expect(color).not.toMatch(/#[0-9a-f]{6}/i);
    }
    expect(full).toContain("var(--primary) 100%");
    expect(half).toContain("var(--primary) 50%");
    expect(zero).toContain("var(--primary) 0%");
  });

  test("edge color mixes both endpoint colors", () => {
    const edge = studyEdgeColor(100, 0);
    expect(edge).toContain("color-mix");
    expect(edge).toContain(studyProgressColor(100));
    expect(edge).toContain(studyProgressColor(0));
  });
});

describe("isStudyNodeVisibleForQuery", () => {
  const subject = {
    title: "Derivatives",
    aliases: ["differentiation"],
  };

  test("empty query keeps every node visible", () => {
    expect(isStudyNodeVisibleForQuery("", subject)).toBe(true);
    expect(isStudyNodeVisibleForQuery("   ", subject)).toBe(true);
  });

  test("matches title and aliases case-insensitively", () => {
    expect(isStudyNodeVisibleForQuery("deriv", subject)).toBe(true);
    expect(isStudyNodeVisibleForQuery("DIFFER", subject)).toBe(true);
    expect(isStudyNodeVisibleForQuery("integrals", subject)).toBe(false);
  });
});

describe("computeStudyFocusLayout", () => {
  const snapshot = graph(
    [node("focus", "m1"), node("near", "m1"), node("far", "m1"), node("lost", "m1")],
    [
      { id: 1, from: "focus", to: "near", relation: "related", note: "" },
      { id: 2, from: "near", to: "far", relation: "related", note: "" },
    ],
  );

  test("places the focused node at the center", () => {
    const layout = computeStudyFocusLayout(snapshot, "focus");
    const focus = layout.nodeById.get("focus");
    expect(focus?.x).toBeCloseTo(0, 6);
    expect(focus?.y).toBeCloseTo(0, 6);
  });

  test("spreads nodes by relational hop distance", () => {
    const layout = computeStudyFocusLayout(snapshot, "focus");
    const radius = (id: string) => {
      const item = layout.nodeById.get(id);
      return item ? Math.hypot(item.x, item.y) : null;
    };
    const near = radius("near");
    const far = radius("far");
    const lost = radius("lost");
    expect(near).not.toBeNull();
    expect(far).not.toBeNull();
    expect(lost).not.toBeNull();
    expect(near!).toBeGreaterThan(0);
    expect(far!).toBeGreaterThan(near!);
    // `lost` has no relation at all, so it lands outside the furthest hop ring.
    expect(lost!).toBeGreaterThan(far!);
  });

  test("focus projection is flat", () => {
    const layout = computeStudyFocusLayout(snapshot, "focus");
    for (const item of layout.nodes) {
      expect(item.depth).toBe(0);
      expect(item.scale).toBe(1);
    }
  });

  test("focus view extent covers only the immediate neighborhood", () => {
    const layout = computeStudyFocusLayout(snapshot, "focus");
    const viewExtent = studyFocusViewExtent(snapshot, "focus");
    expect(viewExtent).toBeLessThan(layout.extent);
    expect(viewExtent).toBeGreaterThanOrEqual(170 + 90);
  });

  test("focus view extent stays at the center for isolated nodes", () => {
    const isolated = graph([node("lone", "m1")], []);
    const viewExtent = studyFocusViewExtent(isolated, "lone");
    expect(viewExtent).toBeLessThan(170);
  });

  test("orders outer rings so radial relations do not cross", () => {
    const snapshot = graph(
      [
        node("focus", "m1"),
        node("a-node", "m1"),
        node("b-node", "m1"),
        node("a-child", "m1"),
        node("z-child", "m1"),
      ],
      [
        { id: 1, from: "focus", to: "a-node", relation: "related", note: "" },
        { id: 2, from: "focus", to: "b-node", relation: "related", note: "" },
        { id: 3, from: "b-node", to: "a-child", relation: "related", note: "" },
        { id: 4, from: "a-node", to: "z-child", relation: "related", note: "" },
      ],
    );

    const layout = computeStudyFocusLayout(snapshot, "focus");
    const a = layout.nodeById.get("a-node")!;
    const b = layout.nodeById.get("b-node")!;
    const zChild = layout.nodeById.get("z-child")!;
    const aChild = layout.nodeById.get("a-child")!;

    // The child of `a-node` stays on the same side as `a-node` instead of
    // being alphabetically placed next to `b-node`.
    expect(Math.sign(zChild.x) === Math.sign(a.x)).toBe(true);
    expect(Math.sign(zChild.y) === Math.sign(a.y)).toBe(true);
    expect(Math.sign(aChild.x) === Math.sign(b.x)).toBe(true);
    expect(Math.sign(aChild.y) === Math.sign(b.y)).toBe(true);
  });

  test("spreads each ring evenly", () => {
    const snapshot = graph(
      [
        node("focus", "m1"),
        node("n1", "m1"),
        node("n2", "m1"),
        node("n3", "m1"),
        node("n4", "m1"),
      ],
      [
        { id: 1, from: "focus", to: "n1", relation: "related", note: "" },
        { id: 2, from: "focus", to: "n2", relation: "related", note: "" },
        { id: 3, from: "focus", to: "n3", relation: "related", note: "" },
        { id: 4, from: "focus", to: "n4", relation: "related", note: "" },
      ],
    );

    const layout = computeStudyFocusLayout(snapshot, "focus");
    const angles = ["n1", "n2", "n3", "n4"]
      .map((id) => {
        const item = layout.nodeById.get(id)!;
        return Math.atan2(item.y, item.x);
      })
      .sort((left, right) => left - right);
    const gaps = angles.slice(1).map((angle, index) => angle - angles[index]);
    for (const gap of gaps) {
      expect(gap).toBeCloseTo(gaps[0], 9);
    }
  });
});

describe("interpolateStudyLayouts", () => {
  const snapshot = graph(
    [node("a", "m1"), node("b", "m1")],
    [{ id: 1, from: "a", to: "b", relation: "related", note: "" }],
  );

  test("returns endpoints unchanged", () => {
    const cloud = computeStudyCircleLayout(snapshot);
    const focus = computeStudyFocusLayout(snapshot, "a");
    const atStart = interpolateStudyLayouts(cloud, focus, 0);
    const atEnd = interpolateStudyLayouts(cloud, focus, 1);
    for (const item of atStart.nodes) {
      const expected = cloud.nodeById.get(item.id)!;
      expect(item.x).toBeCloseTo(expected.x, 6);
      expect(item.y).toBeCloseTo(expected.y, 6);
      expect(item.radius).toBeCloseTo(expected.radius, 6);
    }
    for (const item of atEnd.nodes) {
      const expected = focus.nodeById.get(item.id)!;
      expect(item.x).toBeCloseTo(expected.x, 6);
      expect(item.y).toBeCloseTo(expected.y, 6);
      expect(item.radius).toBeCloseTo(expected.radius, 6);
    }
  });

  test("is linear at the midpoint", () => {
    const cloud = computeStudyCircleLayout(snapshot);
    const focus = computeStudyFocusLayout(snapshot, "a");
    const middle = interpolateStudyLayouts(cloud, focus, 0.5);
    for (const item of middle.nodes) {
      const start = cloud.nodeById.get(item.id);
      const end = focus.nodeById.get(item.id);
      if (!start || !end) {
        continue;
      }
      expect(item.x).toBeCloseTo((start.x + end.x) / 2, 6);
      expect(item.y).toBeCloseTo((start.y + end.y) / 2, 6);
    }
  });
});

describe("studySearchRotation", () => {
  test("turns the matched centroid toward the camera axis", () => {
    const snapshot = graph(
      [node("a", "m1"), node("b", "m1"), node("c", "m1")],
      [],
    );
    const network = computeStudyNetwork(snapshot);
    const matched = new Set(["a", "b"]);
    const rotation = studySearchRotation(network, matched);

    const centroid = network.nodes
      .filter((item) => matched.has(item.id))
      .reduce(
        (accumulator, item) => ({
          x: accumulator.x + item.x / 2,
          y: accumulator.y + item.y / 2,
          z: accumulator.z + item.z / 2,
        }),
        { x: 0, y: 0, z: 0 },
      );
    const rotated = rotateStudyVector(centroid, rotation);
    const originalLength = Math.hypot(centroid.x, centroid.y, centroid.z);
    expect(rotated.x).toBeCloseTo(0, 4);
    expect(rotated.y).toBeCloseTo(0, 4);
    expect(rotated.z).toBeCloseTo(originalLength, 4);
  });

  test("returns identity for empty matches", () => {
    const snapshot = graph([node("a", "m1")], []);
    const network = computeStudyNetwork(snapshot);
    expect(studySearchRotation(network, new Set())).toEqual(
      STUDY_IDENTITY_QUATERNION,
    );
  });

  test("quaternion angle is zero for identical rotations", () => {
    expect(studyQuaternionAngle(STUDY_IDENTITY_QUATERNION, STUDY_IDENTITY_QUATERNION)).toBeCloseTo(0, 6);
  });
});

describe("projectStudyNetwork", () => {
  test("rotation changes the projected depth of nodes", () => {
    const snapshot = graph([node("a", "m1"), node("b", "m1")], []);
    const network = computeStudyNetwork(snapshot);
    const plain = projectStudyNetwork(network, STUDY_IDENTITY_QUATERNION);
    const turned = projectStudyNetwork(network, {
      x: Math.SQRT1_2,
      y: 0,
      z: 0,
      w: Math.SQRT1_2,
    });
    expect(plain.nodes.length).toBe(2);
    expect(turned.nodes.length).toBe(2);
    const changed = plain.nodes.some((item, index) => {
      const other = turned.nodes[index];
      return Math.abs(item.depth - other.depth) > 0.001;
    });
    expect(changed).toBe(true);
  });
});
