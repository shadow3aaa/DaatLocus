import { getCurrentWebUiLanguage, t } from "@/lib/i18n";
import type { JsonValue, ToolSummary } from "./actor-api";
import { FileTextIcon, FilePlus2Icon, FileMinus2Icon, PencilIcon, TerminalSquareIcon, SearchIcon, Globe2Icon, MessageCircleIcon, ImageIcon, PackageIcon, PaperclipIcon, MonitorIcon } from "lucide-react";

export const asObject = (value: JsonValue | undefined): Record<string, JsonValue> => value && typeof value === "object" && !Array.isArray(value) ? value : {};
export const asText = (value: JsonValue | undefined): string => typeof value === "string" ? value : "";
export const asRows = (value: JsonValue | undefined): Record<string, JsonValue>[] => Array.isArray(value) ? value.map(asObject) : [];

const labels: Record<string, string> = {
  computer_use__adjust_window: "computer_use.adjust_window",
  computer_use__list_apps: "computer_use.list_apps", computer_use__list_windows: "computer_use.list_windows", computer_use__launch_app: "computer_use.launch_app", computer_use__open_window: "computer_use.open_window", computer_use__get_window_state: "computer_use.observe", computer_use__click: "computer_use.click", computer_use__type_text: "computer_use.type_text", computer_use__press_key: "computer_use.press_key", computer_use__scroll: "computer_use.scroll", computer_use__drag: "computer_use.drag", computer_use__give_window: "computer_use.give_window", computer_use__release_window: "computer_use.release_window", computer_use__close_window: "computer_use.close_window",
  actors__create: "tool_presentation.create_actor", actors__import_conversation: "imports.bubble_title", actors__send: "conversation_turns.delegate_task", actors__list: "tool_presentation.view_actor", actors__read_history: "tool_presentation.read_history",
  files__read_file: "tool_presentation.read_file", files__write_file: "tool_presentation.write_file", files__edit_file: "tool_presentation.edit_file", files__copy_file: "tool_presentation.copy_file", files__list_directory: "tool_presentation.browse_folder", files__search_text: "tool_presentation.search_files", files__read_skill: "tool_presentation.read_skill", files__list_skills: "tool_presentation.view_skills", files__view_image: "tool_presentation.view_image",
  terminal__terminal_exec: "tool_presentation.run_command", terminal__terminal_write_stdin: "tool_presentation.terminal_input", terminal__terminal_terminate: "tool_presentation.stop_process",
  browser__browser_snapshot: "tool_presentation.view_page", browser__browser_open_page: "tool_presentation.open_page", browser__browser_wait: "tool_presentation.wait_for_page", browser__browser_click: "tool_presentation.click_page", browser__browser_fill: "tool_presentation.fill_page", browser__browser_back: "tool_presentation.go_back", browser__browser_forward: "tool_presentation.go_forward", browser__browser_reload: "tool_presentation.reload_page", browser__browser_close_page: "tool_presentation.close_page", browser__read_url: "tool_presentation.read_page",
  apps_manager__list: "apps.list_action", apps_manager__inspect: "apps.inspect_action", apps_manager__refresh: "apps.refresh_action", apps_manager__install: "apps.install_action", apps_manager__enable: "apps.enable", apps_manager__close: "apps.close",
  artifacts__present: "tool_details.deliver_result",
  actors__read_resource: "tool_presentation.read_attachment_or_output",
  actors__report: "tool_presentation.report_progress", actors__yield: "tool_presentation.yield_input",
};

const fieldLabels: Record<string, string> = {
  path: "tool_presentation.file", source: "tool_presentation.source", destination: "tool_presentation.destination", command: "tool_presentation.command", workdir: "tool_presentation.working_directory", cwd: "tool_presentation.working_directory", text: "tool_presentation.content", content: "tool_presentation.content", old_text: "tool_presentation.before", new_text: "tool_presentation.after", expected_sha256: "tool_presentation.expected_file_version", sha256: "tool_presentation.file_version",
  start_line: "tool_presentation.start_line", end_line: "tool_presentation.end_line", line_count: "tool_presentation.lines_to_read", total_lines: "tool_presentation.total_lines", query: "tool_presentation.search_query", case_sensitive: "tool_presentation.case_sensitive", limit: "tool_presentation.result_limit", cursor: "tool_presentation.cursor",
  actor_id: "tool_presentation.target_actor", stage_id: "account_editor.device", name: "account_editor.name", workspace: "tool_presentation.working_directory", apps: "tool_presentation.installed_apps", model: "composer.models", instructions: "tool_presentation.instructions", read_only: "tool_details.read_only", url: "tool_presentation.url", refresh: "tool_presentation.refresh", page_id: "tool_presentation.page", element_ref: "tool_presentation.page_element", target: "tool_presentation.destination", value: "tool_presentation.value", wait_state: "tool_presentation.wait_state",
  session_id: "tool_presentation.terminal", yield_time_ms: "tool_presentation.wait_time_ms", max_chars: "tool_presentation.output_character_limit", wait_mode: "tool_presentation.wait_mode", shell: "Shell", environment: "provider_editor.environment_variable", env: "provider_editor.environment_variable", timeout: "tool_presentation.timeout", timeout_ms: "tool_presentation.timeout_ms",
  readable_roots: "tool_presentation.readable_paths", writable_roots: "tool_presentation.writable_paths", operation: "tool_presentation.action", operation_details: "tool_presentation.action_details", title: "tool_presentation.title", description: "tool_presentation.description", kind: "tool_presentation.type", uri: "tool_presentation.link", media_type: "tool_presentation.content_type",
};

function words(value: string) { return value.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/_+/g, " "); }
export function domainToolName(name: string) { return name.replace(/^d_[0-9a-f]{12}_/, ""); }
export function toolLabel(name: string) { const domain=domainToolName(name); return /^mcp_[a-f0-9]{32}__/.test(domain) ? t("apps.tool_action") : labels[domain] ? t(labels[domain]) : domain.split("__").map(words).join(" · "); }
export function fieldLabel(name: string) { return fieldLabels[name] ? t(fieldLabels[name]) : words(name); }
export function toolIcon(name: string, outcome?: JsonValue | null) {
  name = domainToolName(name);
  if (name.startsWith("computer_use__")) return MonitorIcon;
  if (name.startsWith("terminal__")) return TerminalSquareIcon;
  if (name.startsWith("actors__")) return MessageCircleIcon;
  if (name.includes("search")) return SearchIcon;
  if (name.startsWith("browser__")) return Globe2Icon;
  if (name.includes("delete")) return FileMinus2Icon;
  if (name.includes("edit_file")) return PencilIcon;
  if (name.includes("write_file")) return asObject(outcome).created === true ? FilePlus2Icon : PencilIcon;
  if (name.includes("image")) return ImageIcon;
  if (name.startsWith("files__")) return FileTextIcon;
  if (name.startsWith("artifacts__")) return PaperclipIcon;
  return PackageIcon;
}
export function activitySummary(names: string[]) {
  names = names.map(domainToolName);
  const parts: string[] = [];
  if (names.some(name => name.startsWith("browser__"))) parts.push(t("tool_presentation.browsed_pages"));
  if (names.some(name => /files__(write|edit|copy|delete)_/.test(name))) parts.push(t("tool_presentation.edited_files"));
  else if (names.some(name => name.startsWith("files__"))) parts.push(t("tool_presentation.read_files"));
  if (names.some(name => name.startsWith("terminal__"))) parts.push(t("tool_presentation.ran_commands"));
  if (names.some(name => name.includes("search"))) parts.push(t("tool_presentation.searched_content"));
  if (names.some(name => name.startsWith("actors__"))) parts.push(t("tool_presentation.handled_collaboration"));
  return parts.join(getCurrentWebUiLanguage() === "zh-CN" ? "" : " ") || t("tool_presentation.value0_actions", {value0: names.length});
}

export function activityIcon(names: string[]) {
  const domains = names.map(domainToolName);
  return toolIcon(domains.find(name => name.startsWith("browser__")) || domains.find(name => /files__(write|edit|copy)_/.test(name)) || domains.find(name => name.startsWith("terminal__")) || domains[0] || "");
}

export function activityLabel(tool: ToolSummary & { status?: "running" | "completed" | "unknown" }) {
  const name = domainToolName(tool.name);
  if (tool.status === "unknown") return { label: toolLabel(tool.name), detail: "" };
  const active = tool.status === "running";
  const outcome = asObject(tool.outcome ?? undefined);
  const summary = readableSummary(tool.summary);
  if (name.startsWith("browser__")) return {label:summary || toolLabel(name),detail:active?t("tool_presentation.running_2"):""};
  if (name.startsWith("apps_manager__")) return {label:toolLabel(name),detail:active?t("tool_presentation.running_2"):""};
  if (/^mcp_[a-f0-9]{32}__/.test(name)) { const app=asObject(outcome.app); return {label:asText(app.name) || t("apps.tool_action"),detail:asText(app.tool) || (active?t("tool_presentation.running_2"):"")}; }
  if (name === "terminal__terminal_exec") return {
    label: active ? t("tool_presentation.running") : tool.error ? t("tool_presentation.command_failed") : t("tool_presentation.ran"),
    detail: asText(outcome.command) || summary || t("tool_presentation.command"),
  };
  if (["files__write_file", "files__edit_file", "files__copy_file"].includes(name)) return {
    label: active ? t("tool_presentation.editing") : tool.error ? t("tool_presentation.edit_failed") : t("tool_presentation.edited"),
    detail: asText(outcome.path) || summary || t("tool_presentation.file"),
  };
  if (name === "files__read_file") return { label: active ? t("tool_presentation.reading") : t("tool_presentation.read"), detail: asText(outcome.path) || summary };
  return { label: toolLabel(tool.name), detail: active ? t("tool_presentation.running_2") : summary };
}

/** A tool's model transcript is never a UI fallback. Drop serialized envelopes in summaries too. */
export function readableSummary(value: string | null | undefined): string {
  const text = value?.trim() || "";
  if (text.startsWith("{") || text.startsWith("[")) {
    try { const parsed: unknown = JSON.parse(text); if (parsed && typeof parsed === "object") return ""; } catch { /* Ordinary prose can start with punctuation. */ }
  }
  if (/^```(?:json)?\s*[\[{]/i.test(text)) return "";
  return text;
}
