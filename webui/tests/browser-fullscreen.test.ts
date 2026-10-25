import { expect, test } from "bun:test";
import { browserFullscreenActive, browserFullscreenEvent } from "../src/lib/browser-fullscreen";

const TAB = "0123456789abcdef0123456789abcdef";

test("native fullscreen notifications parse strictly", () => {
  const event = new CustomEvent("dl-browser-fullscreen", { detail: { tab: TAB, fullscreen: true } });
  expect(browserFullscreenEvent(event)).toEqual({ tab: TAB, fullscreen: true });
  expect(browserFullscreenEvent(new CustomEvent("x", { detail: { tab: "not-a-tab", fullscreen: true } }))).toBeNull();
  expect(browserFullscreenEvent(new CustomEvent("x", { detail: { tab: TAB, fullscreen: "yes" } }))).toBeNull();
  expect(browserFullscreenEvent(new CustomEvent("x", { detail: null }))).toBeNull();
  expect(browserFullscreenEvent(new CustomEvent("x"))).toBeNull();
});

test("only the selected page may drive the shell fullscreen state", () => {
  expect(browserFullscreenActive(TAB, TAB)).toBe(true);
  expect(browserFullscreenActive(TAB, "ffffffffffffffffffffffffffffffff")).toBe(false);
  expect(browserFullscreenActive(TAB, null)).toBe(false);
  expect(browserFullscreenActive(null, TAB)).toBe(false);
});
