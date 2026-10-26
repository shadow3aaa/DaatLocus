type Key = Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey" | "isComposing">;
export function browserShortcut(event: Key): string | null {
  if (event.isComposing) return null;
  const key = event.key.toLowerCase();
  const command = event.ctrlKey || event.metaKey;
  if (!command && !event.altKey) {
    if (key === "f5" && !event.shiftKey) return "r";
    if (key === "f3") return event.shiftKey ? "find-previous" : "find-next";
    if (key === "escape" && !event.shiftKey) return "escape";
  }
  if (event.altKey && !command && !event.shiftKey) {
    if (key === "arrowleft") return "back";
    if (key === "arrowright") return "forward";
    if (key === "d") return "l";
  }
  if (!command || event.altKey) return null;
  if (key === "tab") return event.shiftKey ? "previous-tab" : "next-tab";
  if (key === "g") return event.shiftKey ? "find-previous" : "find-next";
  if (key === "+" || key === "=") return "zoom-in";
  if (key === "o" && event.shiftKey) return "bookmarks";
  if (event.shiftKey) return null;
  if (key === "pageup") return "previous-tab";
  if (key === "pagedown") return "next-tab";
  if (/^[1-9]$/.test(key)) return `tab-${key}`;
  if (key === "-") return "zoom-out";
  if (key === "0") return "zoom-reset";
  return ["l", "t", "h", "j", "f", "w", "r", "p", "d"].includes(key) ? key : null;
}
