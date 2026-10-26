import { expect, test } from "bun:test";
import { promptConnector } from "../src/lib/prompt-connector";

test("a side panel is reached through an outside rail rather than a diagonal across the stack", () => {
  const path = promptConnector({ x: 20, y: 50 }, { x: 30, y: 55 }, { x: 120, y: 20 }, { right: 80, bottom: 100 }, false);
  expect(path.startsWith("M 20 50 ")).toBe(true);
  expect(path).toContain("L 89 55 Q 94 55 94 50");
  expect(path).toContain("L 94 25 Q 94 20 99 20");
  expect(path.endsWith("L 120 20")).toBe(true);
});

test("a panel below the diagram is approached from its top center after clearing the stack", () => {
  const path = promptConnector({ x: 20, y: 50 }, { x: 30, y: 55 }, { x: 50, y: 140 }, { right: 80, bottom: 100 }, true);
  expect(path).toContain("L 94 109 Q 94 114 89 114");
  expect(path).toContain("L 55 114 Q 50 114 50 119");
  expect(path.endsWith("L 50 140")).toBe(true);
});

test("coincident lead points and short segments do not generate invalid curve coordinates", () => {
  const path = promptConnector({ x: 20, y: 50 }, { x: 20, y: 50 }, { x: 22, y: 51 }, { right: 20, bottom: 50 }, true);
  expect(path).not.toMatch(/NaN|Infinity/);
  expect(path.endsWith("L 22 51")).toBe(true);
});
