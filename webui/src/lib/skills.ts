import { request, type FetchOptions } from "./actor-api";

export type SkillReference = { stage_id: string; workspace: string; path: string; name: string };
export type SkillEntry = { name: string; description: string; path: string; scope: "project" | "user" | "daat_locus_home" | "builtin" };
export type SkillsPage = { workspace: string; skills: SkillEntry[]; next_cursor: number | null };
export const skillKey = (skill: SkillReference) => `${skill.stage_id}:${skill.path}`;
export const getSkills = (stage: string, query: string, cursor: number | null, options?: FetchOptions) =>
  request<SkillsPage>(`/v1/stages/${encodeURIComponent(stage)}/skills?query=${encodeURIComponent(query)}&limit=20${cursor === null ? "" : `&cursor=${cursor}`}`, options);
export function skillQuery(text: string, caret: number) {
  const match = /\$([\p{L}\p{N}_.:-]*)$/u.exec(text.slice(0, caret));
  return match && match[1].length <= 128 ? { start: caret - match[0].length, end: caret + (/^[\p{L}\p{N}_.:-]*/u.exec(text.slice(caret))?.[0].length || 0), query: match[1] } : null;
}
export function mentionsSkill(text: string, name: string): boolean {
  return new RegExp(`\\$${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\p{L}\\p{N}_.:-])`, "u").test(text);
}
export function storedSkills(value: unknown): SkillReference[] {
  if (!Array.isArray(value)) return [];
  return value.slice(0, 32).filter((skill): skill is SkillReference => skill && typeof skill.stage_id === "string" && /^[0-9a-f-]{36}$/i.test(skill.stage_id) && [skill.workspace, skill.path, skill.name].every(part => typeof part === "string" && part.length > 0 && part.length <= 32768))
    .map(({stage_id, workspace, path, name}) => ({stage_id, workspace, path, name}));
}
