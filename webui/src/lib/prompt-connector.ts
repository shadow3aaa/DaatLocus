type Point = { x: number; y: number };
const cross = (a: Point, b: Point) => a.x * b.y - a.y * b.x;
const delta = (a: Point, b: Point) => ({ x: a.x - b.x, y: a.y - b.y });
const unit = (v: Point) => { const length = Math.hypot(v.x, v.y) || 1; return { x: v.x / length, y: v.y / length }; };
const advance = (p: Point, v: Point, length: number) => ({ x: p.x + v.x * length, y: p.y + v.y * length });

/** Preserve the projection of the 3D face normal, including camera foreshortening. */
export function projectedSurfaceLead(start: Point, exterior: Point) {
  return advance(start, unit(delta(exterior, start)), 16);
}

/** Clip a segment against a convex polygon's interior. Boundary contact is allowed. */
function intersects(a: Point, b: Point, polygon: Point[]) {
  const sign = Math.sign(polygon.reduce((area, p, i) => area + cross(p, polygon[(i + 1) % polygon.length]), 0));
  let low = .001, high = .999;
  for (let i = 0; i < polygon.length; i++) {
    const edge = delta(polygon[(i + 1) % polygon.length], polygon[i]);
    const origin = sign * cross(edge, delta(a, polygon[i])) - .5, slope = sign * cross(edge, delta(b, a));
    if (Math.abs(slope) < .0001) { if (origin <= 0) return false; }
    else if (slope > 0) low = Math.max(low, -origin / slope);
    else high = Math.min(high, -origin / slope);
    if (low >= high) return false;
  }
  return low < high;
}

/** Preserve both endpoint normals, prefer one bend, and avoid projected plates/controls. */
export function promptConnector(source: Point, lead: Point, target: Point, targetNormal: Point, obstacles: Point[][] = [], viewport?: { width: number; height: number }): string {
  const normal = unit(delta(lead, source)), entry = unit(targetNormal);
  const valid = (points: Point[]) => points.every(p => !viewport || (p.x >= 0 && p.y >= 0 && p.x <= viewport.width && p.y <= viewport.height)) &&
    points.slice(1).every((point, index) => obstacles.every(polygon => {
      // An occluded attachment may start inside the stack silhouette; exit it first.
      if (index === 0 && intersects(source, advance(source, normal, .01), polygon)) return !intersects(point, advance(point, normal, .01), polygon);
      return !intersects(points[index], point, polygon);
    }));
  const denominator = cross(normal, entry), offset = delta(target, source);
  let points: Point[] | undefined;
  if (Math.abs(denominator) > .001) {
    const from = cross(offset, entry) / denominator, to = cross(offset, normal) / denominator;
    const candidate = [source, advance(source, normal, from), target];
    if (from >= 12 && to >= 12 && valid(candidate)) points = candidate;
  }
  if (!points) {
    let shortest = Infinity;
    for (const from of [16, 24, 40, 64, 96, 144]) for (const to of [16, 24, 40, 64]) {
      const candidate = [source, advance(source, normal, from), advance(target, entry, to), target];
      const length = from + to + Math.hypot(candidate[1].x - candidate[2].x, candidate[1].y - candidate[2].y);
      if (length < shortest && valid(candidate)) { shortest = length; points = candidate; }
    }
  }
  if (!points) return "";
  let path = `M ${source.x} ${source.y}`;
  for (let i = 1; i < points.length - 1; i++) {
    const before = points[i - 1], corner = points[i], after = points[i + 1];
    const incoming = Math.hypot(corner.x - before.x, corner.y - before.y), outgoing = Math.hypot(after.x - corner.x, after.y - corner.y);
    if (!incoming || !outgoing) continue;
    const radius = Math.min(5, incoming / 2, outgoing / 2);
    const enter = advance(corner, unit(delta(before, corner)), radius), leave = advance(corner, unit(delta(after, corner)), radius);
    path += ` L ${enter.x} ${enter.y} Q ${corner.x} ${corner.y} ${leave.x} ${leave.y}`;
  }
  return `${path} L ${target.x} ${target.y}`;
}
