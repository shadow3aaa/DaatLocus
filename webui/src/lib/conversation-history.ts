export type HistoryStop = { target: number; first: number; last: number };

/** Fixed, contiguous buckets keep a dense history index stable while scrolling. */
export function historyStops(length: number, capacity: number): HistoryStop[] {
  const count = Math.min(length, Math.max(0, Math.floor(capacity)));
  if (!count) return [];
  const targets = Array.from({length: count}, (_, index) => count === 1 ? length - 1 : Math.round(index * (length - 1) / (count - 1)));
  return targets.map((target, index) => ({
    target,
    first: index ? Math.floor((targets[index - 1] + target) / 2) + 1 : 0,
    last: index === count - 1 ? length - 1 : Math.floor((target + targets[index + 1]) / 2),
  }));
}

/** Only content inside the readable viewport contributes to its visible range. */
export function historyViewport(rects: {top: number; bottom: number}[], top: number, bottom: number) {
  const visible = rects.flatMap((rect, index) => bottom > top && rect.bottom > rect.top && rect.bottom > top && rect.top < bottom ? [index] : []);
  return {visible, current: visible[0] ?? -1};
}
