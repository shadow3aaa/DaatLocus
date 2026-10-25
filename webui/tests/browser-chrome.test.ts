import { expect, test } from "bun:test";
import { browserChrome } from "../src/lib/browser-chrome";

const luminance = (color: string) => {
  const channels = [1, 3, 5].map(index => parseInt(color.slice(index, index + 2), 16) / 255)
    .map(channel => channel <= .04045 ? channel / 12.92 : ((channel + .055) / 1.055) ** 2.4);
  return .2126 * channels[0] + .7152 * channels[1] + .0722 * channels[2];
};
const contrast = (one: string, other: string) => {
  const [high, low] = [luminance(one), luminance(other)].sort((a, b) => b - a);
  return (high + .05) / (low + .05);
};
const distance = (one: string, other: string) =>
  Math.max(...[1, 3, 5].map(index => Math.abs(parseInt(one.slice(index, index + 2), 16) - parseInt(other.slice(index, index + 2), 16))));

test("page tint chooses readable text for light, dark and saturated backgrounds", () => {
  for (const [background, foreground] of [
    ["#ffffff", "#000000"], ["#000000", "#ffffff"],
    ["#1c3557", "#ffffff"], ["#ffff00", "#000000"],
    ["#0000ff", "#ffffff"], ["#c88756", "#000000"],
  ]) expect(browserChrome(background)).toMatchObject({ background, foreground });
});

test("tab and menu surfaces stay readable and distinct on every page color", () => {
  // Mock scenes: light, dark, blue and warm, plus midtone and pure extremes.
  for (const background of ["#ffffff", "#171717", "#1c3557", "#c88756", "#767676", "#000000", "#ffff00"]) {
    const palette = browserChrome(background)!;
    expect(contrast(palette.foreground, background)).toBeGreaterThanOrEqual(4.5);
    for (const surface of [palette.surface, palette.hover]) {
      expect(contrast(palette.foreground, surface)).toBeGreaterThanOrEqual(4.5);
      expect(distance(surface, background)).toBeGreaterThanOrEqual(6);
    }
    expect(distance(palette.hover, background)).toBeGreaterThanOrEqual(distance(palette.surface, background));
    expect(palette.scheme).toBe(palette.foreground === "#ffffff" ? "dark" : "light");
  }
});

test("unavailable or malformed page color restores the ordinary shell palette", () => {
  for (const value of [undefined, null, "", "transparent", "red", "#fff", "#000000;display:none"])
    expect(browserChrome(value)).toBeNull();
});
