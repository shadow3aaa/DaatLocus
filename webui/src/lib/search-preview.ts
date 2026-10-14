/** A plain-text search excerpt. Full source stays in the conversation. */
export function searchPreview(source: string): string {
  return source
    .replace(/^\s*(`{3,}|~{3,})[^\n]*$/gm, "")
    .replace(/!?\[([^\]]+)\]\([^\n]*?\)/g, "$1")
    .replace(/^\s{0,3}(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+\.\s+)/gm, "")
    .replace(/^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)*\|?\s*$/gm, "")
    .replace(/(\*\*|__|~~)(\S(?:.*?\S)?)\1/g, "$2")
    .replace(/`([^`\n]+)`/g, "$1")
    .replace(/(^|\s)\*(\S(?:[^*\n]*?\S)?)\*(?=\s|[.,!?，。！？]|$)/g, "$1$2")
    .replace(/^[ \t]*\|[ \t]*|[ \t]*\|[ \t]*$/gm, "")
    .replace(/[ \t]*\|[ \t]*/g, " · ")
    .replace(/\s+/g, " ")
    .trim();
}
