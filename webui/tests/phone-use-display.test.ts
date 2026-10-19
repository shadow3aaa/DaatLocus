import { expect, test } from "bun:test";
import { validPhoneUseDisplay } from "../src/lib/phone-use-display";

test("virtual display preferences bound integer geometry, density and total pixels", () => {
  for (const value of [
    {width: 1280, height: 720, density: 160},
    {width: 256, height: 256, density: 80},
    {width: 4096, height: 1953, density: 640},
    {width: 1080, height: 1920, density: 320},
  ]) expect(validPhoneUseDisplay(value)).toBe(true);
  for (const patch of [
    {width: 255}, {height: 4097}, {density: 79}, {density: 641},
    {width: 1280.5}, {height: NaN}, {density: Infinity},
    {width: 4096, height: 1954},
  ]) expect(validPhoneUseDisplay({width: 1280, height: 720, density: 160, ...patch})).toBe(false);
});
