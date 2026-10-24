import { expect, test } from "bun:test";
import { browserChrome } from "../src/lib/browser-chrome";

test("page tint chooses readable text for light, dark and saturated backgrounds", () => {
  for (const [background, foreground] of [
    ["#ffffff", "#000000"], ["#000000", "#ffffff"],
    ["#1c3557", "#ffffff"], ["#ffff00", "#000000"],
    ["#0000ff", "#ffffff"], ["#c88756", "#000000"],
  ]) expect(browserChrome(background)).toEqual({ background, foreground });
});

test("unavailable or malformed page color restores the ordinary shell palette", () => {
  for (const value of [undefined, null, "", "transparent", "red", "#fff", "#000000;display:none"])
    expect(browserChrome(value)).toBeNull();
});
