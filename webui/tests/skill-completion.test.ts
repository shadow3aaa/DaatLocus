import { expect, test } from "bun:test";
import { mentionsSkill, skillQuery } from "../src/lib/skills";

test("Skill completion follows the token at the caret, including Chinese text", () => {
  expect(skillQuery("请使用 $review 后再总结", 11)).toEqual({start:4, end:11, query:"review"});
  expect(skillQuery("$review", 3)).toEqual({start:0, end:7, query:"re"});
  expect(skillQuery("$first $", 8)).toEqual({start:7, end:8, query:""});
  expect(skillQuery("$review ", 8)).toBeNull();
  expect(skillQuery("价格为 $25。", 8)).toBeNull();
});

test("Editing a Skill name drops its selected source instead of matching a prefix", () => {
  expect(mentionsSkill("使用 $review 后总结", "review")).toBe(true);
  expect(mentionsSkill("使用 $reviewer", "review")).toBe(false);
  expect(mentionsSkill("使用 $research.v2", "research.v2")).toBe(true);
  expect(mentionsSkill("使用 $research-v2", "research.v2")).toBe(false);
});
