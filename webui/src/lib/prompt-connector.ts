type Point = { x: number; y: number };

/** Route an outward lead around the stack, then enter the panel at its anchor. */
export function promptConnector(source: Point, lead: Point, target: Point, stack: { right: number; bottom: number }, stacked: boolean): string {
  const rail = stacked ? Math.max(stack.right + 14, lead.x) : Math.max(lead.x, Math.min(stack.right + 14, target.x - 16));
  const points = [source, lead, { x: rail, y: lead.y }];
  if (stacked) {
    const bottom = Math.max(lead.y, Math.min(stack.bottom + 14, target.y - 16));
    points.push({ x: rail, y: bottom }, { x: target.x, y: bottom });
  } else points.push({ x: rail, y: target.y });
  points.push(target);
  // Limit the radius to adjoining segment lengths, including compact layouts.
  let path = `M ${source.x} ${source.y}`;
  for (let i = 1; i < points.length - 1; i++) {
    const before = points[i - 1], corner = points[i], after = points[i + 1];
    const incoming = Math.hypot(corner.x - before.x, corner.y - before.y), outgoing = Math.hypot(after.x - corner.x, after.y - corner.y);
    if (!incoming || !outgoing) continue;
    const radius = Math.min(5, incoming / 2, outgoing / 2);
    path += ` L ${corner.x + (before.x - corner.x) * radius / incoming} ${corner.y + (before.y - corner.y) * radius / incoming}`;
    path += ` Q ${corner.x} ${corner.y} ${corner.x + (after.x - corner.x) * radius / outgoing} ${corner.y + (after.y - corner.y) * radius / outgoing}`;
  }
  return `${path} L ${target.x} ${target.y}`;
}
