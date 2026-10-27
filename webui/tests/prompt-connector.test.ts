import { expect, test } from "bun:test";
import { projectedSurfaceLead, promptConnector } from "../src/lib/prompt-connector";

const coordinates = (path: string) => path.match(/-?\d+(?:\.\d+)?(?:e[+-]?\d+)?/g)!.map(Number);
function expectNormals(path: string, lead: { x: number; y: number }, endNormal: { x: number; y: number }) {
  const p = coordinates(path), n = p.length;
  const dx = p[2] - p[0], dy = p[3] - p[1], nx = lead.x - p[0], ny = lead.y - p[1];
  expect(dx * ny - dy * nx).toBeCloseTo(0, 6);
  expect(dx * nx + dy * ny).toBeGreaterThan(0);
  const ex = p[n - 2] - p[n - 4], ey = p[n - 1] - p[n - 3];
  expect(ex * endNormal.y - ey * endNormal.x).toBeCloseTo(0, 6);
  expect(ex * endNormal.x + ey * endNormal.y).toBeLessThan(0);
}

test("projected surface normals retain their direction rather than becoming screen axes", () => {
  const start = { x: 100, y: 100 };
  const lead = projectedSurfaceLead(start, { x: 112, y: 105 });
  expect(lead.x).toBeCloseTo(100 + 16 * 12 / 13); expect(lead.y).toBeCloseTo(100 + 16 * 5 / 13);
  expect((lead.x - start.x) * 5 - (lead.y - start.y) * 12).toBeCloseTo(0);
  const mirrored = projectedSurfaceLead(start, { x: 88, y: 95 });
  expect(mirrored.x).toBeCloseTo(100 - 16 * 12 / 13); expect(mirrored.y).toBeCloseTo(100 - 16 * 5 / 13);
});

test("a clear ray intersection uses one bend and approaches the content panel vertically", () => {
  const lead = { x: 36, y: 28 }, normal = { x: 0, y: -1 };
  const path = promptConnector({ x: 20, y: 20 }, lead, { x: 100, y: 100 }, normal);
  expect(path.match(/ Q /g)).toHaveLength(1);
  expect(path).toContain("Q 100 60");
  expectNormals(path, lead, normal);
});

test("targets behind the source use two bends without rotating either attachment normal", () => {
  const lead = { x: 36, y: 28 }, normal = { x: 0, y: -1 };
  const path = promptConnector({ x: 20, y: 20 }, lead, { x: 10, y: 100 }, normal);
  expect(path.match(/ Q /g)).toHaveLength(2);
  expectNormals(path, lead, normal);
});

test("a lower-edge connector reaches a left button perpendicular to its right edge", () => {
  const lead = { x: 90, y: 66 }, normal = { x: 1, y: 0 };
  const path = promptConnector({ x: 100, y: 50 }, lead, { x: 20, y: 120 }, normal);
  expect(path.match(/ Q /g)).toHaveLength(1);
  expectNormals(path, lead, normal);
});

test("a blocked one-bend route is replaced by a two-bend route outside the obstacle", () => {
  const box = [{ x: 55, y: 35 }, { x: 75, y: 35 }, { x: 75, y: 70 }, { x: 55, y: 70 }];
  const lead = { x: 36, y: 28 }, normal = { x: 0, y: -1 };
  const path = promptConnector({ x: 20, y: 20 }, lead, { x: 100, y: 100 }, normal, [box], { width: 140, height: 140 });
  expect(path.match(/ Q /g)).toHaveLength(2);
  expectNormals(path, lead, normal);
  let previous = { x: 0, y: 0 };
  for (const command of path.match(/[MLQ] [^MLQ]+/g)!) {
    const p = coordinates(command), curved = command[0] === "Q", end = { x: p.at(-2)!, y: p.at(-1)! };
    if (command[0] !== "M") for (let i = 1; i <= 40; i++) {
      const t = i / 40;
      const x = curved ? (1-t)**2 * previous.x + 2*(1-t)*t*p[0] + t*t*end.x : previous.x + t * (end.x - previous.x);
      const y = curved ? (1-t)**2 * previous.y + 2*(1-t)*t*p[1] + t*t*end.y : previous.y + t * (end.y - previous.y);
      expect(x > 55 && x < 75 && y > 35 && y < 70).toBe(false);
    }
    previous = end;
  }
});

test("coincident points remain finite and never introduce more than two bends", () => {
  const path = promptConnector({ x: 20, y: 50 }, { x: 20, y: 50 }, { x: 22, y: 51 }, { x: 0, y: -1 });
  expect(path).not.toMatch(/NaN|Infinity/);
  expect((path.match(/ Q /g) ?? []).length).toBeLessThanOrEqual(2);
  expect(path.endsWith("L 22 51")).toBe(true);
});

test("an unrouteable connector is omitted rather than drawn across an obstacle", () => {
  const wall = [{ x: 0, y: 0 }, { x: 140, y: 0 }, { x: 140, y: 140 }, { x: 0, y: 140 }];
  expect(promptConnector({ x: 20, y: 20 }, { x: 36, y: 28 }, { x: 100, y: 100 }, { x: 0, y: -1 }, [wall], { width: 140, height: 140 })).toBe("");
});
