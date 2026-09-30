function stripMarkdownEmphasis(value: string) {
  const trimmed = value.trim();
  const strongMatch = trimmed.match(/^(\*\*|__)([\s\S]+)\1$/);
  if (strongMatch) {
    return strongMatch[2].trim();
  }
  const emphasisMatch = trimmed.match(/^(\*|_)([\s\S]+)\1$/);
  if (emphasisMatch) {
    return emphasisMatch[2].trim();
  }
  return trimmed.replace(/^#{1,6}\s+/, "").trim();
}

function isMarkdownBlockSyntax(value: string) {
  return /^(#{1,6}\s+|[-*+]\s+|\d+[.)]\s+|>\s*|```|~~~|\|)/.test(value);
}

function isStandaloneThinkingHeading(value: string) {
  const trimmed = value.trim();
  if (!trimmed || isMarkdownBlockSyntax(trimmed)) {
    return false;
  }

  const text = stripMarkdownEmphasis(trimmed);
  if (!text || text.length > 88 || /[.!?。！？]$/.test(text)) {
    return false;
  }

  if (/^(\*\*|__)[\s\S]+(\*\*|__)$/.test(trimmed)) {
    return true;
  }

  const words = text.split(/\s+/).filter(Boolean);
  if (words.length > 9) {
    return false;
  }

  return /^[A-Z]/.test(text) || /^[\p{Script=Han}]/u.test(text);
}

function lastOutputLineIsBlank(lines: string[]) {
  return lines.length === 0 || lines[lines.length - 1].trim() === "";
}

/**
 * Normalize agent thinking text before it is rendered as markdown.
 *
 * Fenced code is parsed by a small scanner first, so `**` inside a fence is
 * left alone. Outside fences, a standalone `**heading**` that is glued to the
 * previous sentence is split onto its own paragraph. `react-markdown` renders
 * the result in the chat UI.
 */
export function normalizeThinkingMarkdown(text: string) {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const output: string[] = [];

  for (const block of scanMarkdownBlocks(normalized)) {
    if (block.kind === "fence") {
      for (const line of block.text.split("\n")) {
        output.push(line);
      }
      continue;
    }

    const lines = block.text.split("\n");
    lines.forEach((line, index) => {
      const pieces = splitInlineThinkingHeadings(line);
      pieces.forEach((piece, pieceIndex) => {
        const heading = isStandaloneThinkingHeading(piece.trim());
        if (heading && !lastOutputLineIsBlank(output)) {
          output.push("");
        }
        output.push(piece);
        const morePieces = pieceIndex + 1 < pieces.length;
        const moreLines =
          index + 1 < lines.length && lines[index + 1].trim() !== "";
        if (heading && (morePieces || moreLines)) {
          output.push("");
        }
      });
    });
  }

  return output.join("\n");
}

type MarkdownBlock =
  | { kind: "fence"; text: string }
  | { kind: "text"; text: string };

/** Split text into fenced-code blocks and the prose between them. */
function scanMarkdownBlocks(text: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = text.split("\n");
  let textLines: string[] = [];
  let fence: { marker: "`" | "~"; width: number } | null = null;
  let fenceLines: string[] = [];

  const flushText = () => {
    if (textLines.length === 0) {
      return;
    }
    blocks.push({ kind: "text", text: textLines.join("\n") });
    textLines = [];
  };

  for (const line of lines) {
    if (!fence) {
      const opener = fenceOpener(line);
      if (opener) {
        flushText();
        fence = opener;
        fenceLines = [line];
        continue;
      }
      textLines.push(line);
      continue;
    }

    fenceLines.push(line);
    if (fenceCloser(line, fence.marker, fence.width)) {
      blocks.push({ kind: "fence", text: fenceLines.join("\n") });
      fence = null;
      fenceLines = [];
    }
  }

  if (fenceLines.length > 0) {
    blocks.push({ kind: "fence", text: fenceLines.join("\n") });
  } else {
    flushText();
  }
  return blocks;
}


function fenceOpener(line: string): { marker: "`" | "~"; width: number } | null {
  let index = 0;
  while (index < line.length && index < 3 && line[index] === " ") {
    index += 1;
  }
  const marker = line[index];
  if (marker !== "`" && marker !== "~") {
    return null;
  }
  let width = 0;
  while (index + width < line.length && line[index + width] === marker) {
    width += 1;
  }
  if (width < 3) {
    return null;
  }
  const info = line.slice(index + width);
  if (marker === "`" && info.includes("`")) {
    return null;
  }
  return { marker, width };
}

function fenceCloser(line: string, marker: "`" | "~", width: number) {
  let index = 0;
  while (index < line.length && index < 3 && line[index] === " ") {
    index += 1;
  }
  let seen = 0;
  while (index < line.length && line[index] === marker) {
    seen += 1;
    index += 1;
  }
  if (seen < width) {
    return false;
  }
  return line.slice(index).trim() === "";
}

function splitInlineThinkingHeadings(line: string) {
  const pieces: string[] = [];
  let cursor = 0;
  let search = 0;
  while (search < line.length) {
    const start = line.indexOf("**", search);
    if (start < 0) {
      break;
    }
    if (isInsideInlineCode(line, start)) {
      search = start + 2;
      continue;
    }
    const endMarker = line.indexOf("**", start + 2);
    if (endMarker < 0 || isInsideInlineCode(line, endMarker)) {
      break;
    }
    const end = endMarker + 2;
    const candidate = line.slice(start, end);
    if (
      start > 0 &&
      line[start - 1].trim() !== "" &&
      isStandaloneThinkingHeading(candidate)
    ) {
      pieces.push(line.slice(cursor, start));
      pieces.push(candidate);
      cursor = end;
    }
    search = end;
  }
  pieces.push(line.slice(cursor));
  return pieces.filter((piece, index) => piece.length > 0 || index === 0);
}

function isInsideInlineCode(line: string, index: number) {
  let ticks = 0;
  for (let cursor = 0; cursor < index; cursor += 1) {
    if (line[cursor] === "`") {
      ticks += 1;
    }
  }
  return ticks % 2 === 1;
}
