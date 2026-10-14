import { t } from "@/lib/i18n";
import { useState } from "react";
import { ChevronRightIcon } from "lucide-react";
import type { JsonValue } from "@/lib/actor-api";
import { fieldLabel } from "@/lib/tool-presentation";
import { cn } from "@/lib/utils";

/** Human row label for one array entry: its own name/title/path when present, else the 1-based position. */
function entryLabel(entry: JsonValue, position: number) {
  if (entry && typeof entry === "object" && !Array.isArray(entry)) {
    for (const field of ["name", "title", "path"]) {
      const text = entry[field];
      if (typeof text === "string" && text.trim()) return text;
    }
  }
  return String(position);
}

function ParameterValue({ value }: { value: JsonValue }) {
  const [open, setOpen] = useState(false);
  if (value === null) return <span className="dl-parameter-empty">{t("tool_parameters.not_specified")}</span>;
  if (typeof value === "boolean") return <span>{value ? t("tool_parameters.yes") : t("tool_parameters.no")}</span>;
  if (typeof value === "string") return <span className="dl-parameter-text">{value || t("tool_parameters.empty_text")}</span>;
  if (typeof value === "number") return <span>{String(value)}</span>;
  const array = Array.isArray(value);
  const entries: [string, JsonValue, string][] = array ? value.map((entry, index) => [String(index + 1), entry, entryLabel(entry, index + 1)]) : Object.entries(value).map(([key, entry]) => [key, entry, fieldLabel(key)]);
  if (!entries.length) return <span className="dl-parameter-empty">{array ? t("tool_parameters.no_items") : t("tool_parameters.no_content")}</span>;
  return <div className="dl-parameter-collection"><button type="button" className="dl-disclosure" aria-expanded={open} onClick={() => setOpen(!open)}>{t(array ? "tool_parameters.items_count" : "tool_parameters.details_count", {count:entries.length})}<ChevronRightIcon className={cn(open && "dl-chevron-open")} aria-hidden="true" /></button>{open && <dl className="dl-parameter-fields">{entries.map(([key, entry, label]) => <div className="dl-parameter-row" key={key}><dt>{label}</dt><dd><ParameterValue value={entry} /></dd></div>)}</dl>}</div>;
}

/** Immutable action values remain inspectable, without exposing serialized protocol objects. */
export function ToolParameters({ value }: { value: JsonValue }) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return <ParameterValue value={value} />;
  return <dl className="dl-parameter-fields">{Object.entries(value).map(([key, entry]) => <div className="dl-parameter-row" key={key}><dt>{fieldLabel(key)}</dt><dd><ParameterValue value={entry} /></dd></div>)}</dl>;
}
