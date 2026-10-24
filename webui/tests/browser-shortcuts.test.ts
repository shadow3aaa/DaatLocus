import { expect, test } from "bun:test";
import { browserShortcut } from "../src/lib/browser-shortcuts";

test("browser shortcuts distinguish navigation, tab cycling and ordinary editing", () => {
  const cases: [string, Partial<KeyboardEvent>, string | null][] = [
    ["F5", {}, "r"], ["f", {ctrlKey:true}, "f"], ["f", {metaKey:true}, "f"],
    ["Tab", {ctrlKey:true}, "next-tab"], ["Tab", {ctrlKey:true,shiftKey:true}, "previous-tab"],
    ["PageUp", {ctrlKey:true}, "previous-tab"], ["9", {ctrlKey:true}, "tab-9"],
    ["ArrowLeft", {altKey:true}, "back"], ["F3", {shiftKey:true}, "find-previous"],
    ["+", {ctrlKey:true,shiftKey:true}, "zoom-in"], ["0", {ctrlKey:true}, "zoom-reset"],
    ["a", {ctrlKey:true}, null], ["ArrowLeft", {}, null], ["Tab", {}, null],
    ["f", {ctrlKey:true,altKey:true}, null], ["w", {ctrlKey:true,isComposing:true}, null],
    ["w", {ctrlKey:true,shiftKey:true}, null],
  ];
  for(const [key,modifiers,expected] of cases)expect(browserShortcut({key,ctrlKey:false,metaKey:false,altKey:false,shiftKey:false,isComposing:false,...modifiers})).toBe(expected);
});
