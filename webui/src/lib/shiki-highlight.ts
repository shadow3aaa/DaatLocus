import {
  bundledLanguages,
  codeToTokensBase,
  type BundledLanguage,
  type BundledTheme,
  type ThemedToken,
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
  const language = resolveShikiLanguage(languageOrPath);
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
    return {
      language,
      lines: lines.map((line) => line.map(shikiTokenFromThemedToken)),
    };
  } catch {
    return null;
  }
}

export function resolveShikiLanguage(input: string): BundledLanguage | null {
  const trimmed = input.trim();
  if (!trimmed) {
    return null;
  }

  for (const candidate of shikiLanguageCandidates(trimmed)) {
    const direct = shikiLanguage(candidate);
    if (direct) {
      return direct;
    }
    const aliased = shikiLanguageAliases[candidate];
    if (aliased && shikiLanguage(aliased)) {
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

function shikiLanguage(value: string | undefined): BundledLanguage | null {
  if (!value) {
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
