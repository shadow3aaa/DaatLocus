const clamp = (value: number) => Math.max(0, Math.min(1, value));
const smooth = (value: number) => { const t = clamp(value); return t * t * (3 - 2 * t); };

/** Move one slot while colour and shadow share the same fade progress. */
export function layerFrame(start: number, target: number, elapsed: number, phase: "steady" | "enter" | "exit", initialOpacity = 1, reduced = false, spacing = .54) {
  const progress = reduced ? 1 : clamp(elapsed / 360);
  const travel = 1 - (1 - progress) ** 3;
  const y = start + (target - start) * travel;
  const opacity = phase === "exit" ? initialOpacity * (1 - travel)
    : phase === "enter" ? smooth(1 - Math.abs(target - y) / (spacing * .8))
    : initialOpacity + (1 - initialOpacity) * travel;
  return { y, opacity: reduced ? (phase === "exit" ? 0 : 1) : opacity, done: progress === 1 };
}
