import { expect, test } from "bun:test";
import { layerFrame } from "../src/lib/prompt-layer-motion";

test("outgoing layers travel one slot and fade in step with neighbouring layers in either direction", () => {
  for (const spacing of [.054, .3, .54]) for (const direction of [-1, 1]) {
    for (let elapsed = 0; elapsed <= 360; elapsed += 15) {
      const frame = layerFrame(0, direction * spacing, elapsed, "exit");
      const neighbour = layerFrame(0, direction * spacing, elapsed, "steady");
      expect(frame.y).toBe(neighbour.y);
      expect(Math.abs(frame.y)).toBeLessThanOrEqual(spacing);
      expect(frame.opacity).toBeCloseTo(1 - Math.abs(frame.y) / spacing);
    }
  }
});

test("ordinary opacity and its shadow fade continuously to zero before the layer is hidden", () => {
  const distance = .54;
  let previous = 1;
  for (let elapsed = 0; elapsed <= 360; elapsed += 10) {
    const frame = layerFrame(0, distance, elapsed, "exit");
    expect(frame.opacity).toBeLessThanOrEqual(previous);
    expect(previous - frame.opacity).toBeLessThan(.09);
    previous = frame.opacity;
  }
  expect(previous).toBe(0);
  expect(layerFrame(0, distance, 350, "exit").opacity).toBeLessThan(.01);
  expect(layerFrame(0, distance, 360, "exit").done).toBe(true);
});

test("an incoming layer stays invisible until it has cleared the virtual layer's neighbouring slot", () => {
  for (const spacing of [.054, .3, .54]) for (const direction of [-1, 1]) {
    const start = direction * spacing;
    for (let elapsed = 0; elapsed <= 360; elapsed += 5) {
      const frame = layerFrame(start, 0, elapsed, "enter", 0, false, spacing);
      if (frame.opacity > 0) expect(Math.abs(frame.y)).toBeLessThan(spacing);
    }
  }
});

test("retained layers resume their actual pose and reduced motion settles immediately", () => {
  expect(layerFrame(.73, 0, 0, "steady", .4)).toEqual({ y: .73, opacity: .4, done: false });
  expect(layerFrame(.73, 0, 0, "steady", .4, true)).toEqual({ y: 0, opacity: 1, done: true });
  expect(layerFrame(.73, 2, 0, "exit", .4, true)).toEqual({ y: 2, opacity: 0, done: true });
});
