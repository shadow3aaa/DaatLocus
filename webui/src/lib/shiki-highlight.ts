import type {
  BundledLanguage,
  BundledTheme,
  ThemedToken,
} from "shiki";

export type ShikiColorScheme = "light" | "dark";

const SHIKI_THEME_BY_SCHEME: Record<ShikiColorScheme, BundledTheme> = {
  light: "github-light-default",
  dark: "github-dark-default",
};
const SHIKI_MAX_CODE_CHARS = 200_000;
const SHIKI_MAX_CODE_LINES = 5_000;
const SHIKI_MAX_LINE_LENGTH = 2_000;
const SHIKI_TOKENIZE_TIME_LIMIT_MS = 250;

const shikiLanguageAliases: Record<string, BundledLanguage> = {
  csharp: "c#",
  golang: "go",
  objcpp: "objective-cpp",
  objc: "objective-c",
  pwsh: "powershell",
  python3: "python",
  shell: "bash",
};

const shikiFilenameLanguages: Record<string, BundledLanguage> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
};

export type ShikiHighlightToken = {
  content: string;
  color?: string;
  fontStyle?: number;
  offset?: number;
};

export type ShikiHighlightedCode = {
  language: BundledLanguage;
  lines: ShikiHighlightToken[][];
};

export async function highlightCodeWithShiki(
  code: string,
  languageOrPath: string,
  colorScheme: ShikiColorScheme = "light",
): Promise<ShikiHighlightedCode | null> {
  const { codeToTokensBase, bundledLanguages } = await import("shiki");
  const language = resolveShikiLanguage(languageOrPath, bundledLanguages);
  if (!language || !isHighlightableCode(code)) {
    return null;
  }

  try {
    const lines = await codeToTokensBase(code, {
      lang: language,
      theme: SHIKI_THEME_BY_SCHEME[colorScheme],
      tokenizeMaxLineLength: SHIKI_MAX_LINE_LENGTH,
      tokenizeTimeLimit: SHIKI_TOKENIZE_TIME_LIMIT_MS,
    });
    const filledLines = fillTokenGaps(code, lines);
    return {
      language,
      lines: filledLines.map((line) => line.map(shikiTokenFromThemedToken)),
    };
  } catch {
    return null;
  }
}

export function resolveShikiLanguage(
  input: string,
  bundledLanguages?: Record<string, unknown>,
): BundledLanguage | null {
  const trimmed = input.trim();
  if (!trimmed) {
    return null;
  }

  for (const candidate of shikiLanguageCandidates(trimmed)) {
    const direct = shikiLanguage(candidate, bundledLanguages);
    if (direct) {
      return direct;
    }
    const aliased = shikiLanguageAliases[candidate];
    if (aliased && shikiLanguage(aliased, bundledLanguages)) {
      return aliased;
    }
  }

  return null;
}

function isHighlightableCode(code: string) {
  if (!code.trim()) {
    return false;
  }
  if (code.length > SHIKI_MAX_CODE_CHARS) {
    return false;
  }
  return code.split(/\r?\n/).length <= SHIKI_MAX_CODE_LINES;
}

function shikiLanguageCandidates(input: string): string[] {
  const normalized = input.replace(/\\/g, "/").toLowerCase();
  const fileName = normalized.split("/").filter(Boolean).at(-1) ?? normalized;
  const extension = fileName.includes(".") ? fileName.split(".").at(-1) : null;

  return dedupeStrings([
    shikiFilenameLanguages[fileName],
    extension,
    fileName,
    normalized,
  ]);
}

function shikiLanguage(
  value: string | undefined,
  bundledLanguages?: Record<string, unknown>,
): BundledLanguage | null {
  if (!value || !bundledLanguages) {
    return null;
  }
  return Object.hasOwn(bundledLanguages, value)
    ? (value as BundledLanguage)
    : null;
}

function shikiTokenFromThemedToken(token: ThemedToken): ShikiHighlightToken {
  return {
    content: token.content,
    color: token.color,
    fontStyle: token.fontStyle,
    offset: token.offset,
  };
}

function dedupeStrings(values: Array<string | undefined | null>) {
  const seen = new Set<string>();
  return values.filter((value): value is string => {
    if (!value || seen.has(value)) {
      return false;
    }
    seen.add(value);
    return true;
  });
}

/**
 * Fill gaps in Shiki token lines where the grammar did not emit tokens
 * for leading whitespace or other unmatched characters.
 */
function fillTokenGaps(
  code: string,
  shikiLines: ThemedToken[][],
): ThemedToken[][] {
  const codeLines = code.split("\n");
  const lineOffsets = computeLineOffsets(code);
  return shikiLines.map((lineTokens, lineIndex) => {
    const lineText = codeLines[lineIndex];
    if (lineIndex >= codeLines.length || lineText === undefined) {
      return lineTokens;
    }

    const lineStartOffset = lineOffsets[lineIndex] ?? 0;
    const filled: ThemedToken[] = [];
    let linePos = 0;

    for (const token of lineTokens) {
      if (token.offset === undefined) {
        filled.push(token);
        continue;
      }
      const tokenStart = token.offset - lineStartOffset;
      if (tokenStart > linePos) {
        filled.push({
          content: lineText.slice(linePos, tokenStart),
          offset: lineStartOffset + linePos,
          color: undefined,
          fontStyle: undefined,
        });
      }
      filled.push(token);
      linePos = tokenStart + token.content.length;
    }

    if (linePos < lineText.length) {
      filled.push({
        content: lineText.slice(linePos),
        offset: lineStartOffset + linePos,
        color: undefined,
        fontStyle: undefined,
      });
    }

    return filled;
  });
}

function computeLineOffsets(code: string): number[] {
  const RE_NEWLINE = /(\r?\n)/g;
  const parts = code.split(RE_NEWLINE);
  const offsets: number[] = [];
  let index = 0;
  for (let i = 0; i < parts.length; i += 2) {
    offsets.push(index);
    index += parts[i].length;
    index += parts[i + 1]?.length || 0;
  }
  return offsets;
}
