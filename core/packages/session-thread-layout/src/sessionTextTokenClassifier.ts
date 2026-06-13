import { isAbsolutePath, parseUrlToken } from "./codeTokenLinks";

const STRONG_RTL_SCRIPT_PATTERN =
  /[\p{Script=Arabic}\p{Script=Hebrew}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Nko}\p{Script=Adlam}]/u;

function trimEdgePunctuation(text: string): string {
  return text.replace(/^[([{]+/u, "").replace(/[)\],.:;!?]+$/u, "");
}

function isRelativeOrHomePath(text: string): boolean {
  return text.startsWith("./") || text.startsWith("../") || text.startsWith("~/");
}

export function isPunctuationOnlySeamText(text: string): boolean {
  const trimmed = text.trim();
  return trimmed.length > 0 && /^[\p{P}\p{S}]+$/u.test(trimmed);
}

export function containsStrongRtlText(text: string): boolean {
  return STRONG_RTL_SCRIPT_PATTERN.test(text);
}

export function isOpeningPunctuationOnlyText(text: string | null | undefined): boolean {
  const trimmed = text?.trim() ?? "";
  return trimmed.length > 0 && /^[([{]+$/u.test(trimmed);
}

export function isStandaloneHyphenText(text: string | null | undefined): boolean {
  return (text?.trim() ?? "") === "-";
}

export function isHyphenatedLabelLikeToken(text: string): boolean {
  const normalized = trimEdgePunctuation(text);
  if (normalized.length === 0) {
    return false;
  }
  return (
    /^[\p{L}\p{N}]+(?:-[\p{L}\p{N}]+)+$/u.test(normalized) ||
    /^[\p{L}\p{N}]+(?:-[\p{L}\p{N}]+)*-$/u.test(normalized)
  );
}

export function isPlainSlashDelimitedTextFragment(text: string | null | undefined): boolean {
  const trimmed = text?.trim() ?? "";
  if (trimmed.length === 0 || trimmed.includes("://")) {
    return false;
  }
  return /^\S*[\\/]\S*$/u.test(trimmed);
}

export function isCompactSlashDelimitedSeamAnchor(text: string | null | undefined): boolean {
  const trimmed = text?.trim() ?? "";
  if (!isPlainSlashDelimitedTextFragment(trimmed) || trimmed.includes(".")) {
    return false;
  }
  const segments = trimmed.split(/[\\/]/u);
  if (segments.length < 3) {
    return false;
  }
  return segments.every((segment) => {
    const normalized = trimEdgePunctuation(segment);
    return (
      /^[\p{L}\p{N}_-]+$/u.test(normalized) &&
      Array.from(normalized).length > 0 &&
      Array.from(normalized).length <= 6
    );
  });
}

export function isCompactPathTailContinuationAnchor(text: string | null | undefined): boolean {
  if (
    typeof text !== "string" ||
    text.length === 0 ||
    text.includes(".") ||
    text.includes(":") ||
    /\s/u.test(text)
  ) {
    return false;
  }
  const parts = text.split(/[\\/]/u);
  if (parts.length !== 2) {
    return false;
  }
  const [head, tail] = parts;
  if (head == null || tail == null || head.length === 0 || tail.length === 0) {
    return false;
  }
  return Array.from(head).length <= 4 && Array.from(tail).length <= 6;
}

export function isCompactTrailingSlashBridgeToken(text: string | null | undefined): boolean {
  if (
    typeof text !== "string" ||
    text.length === 0 ||
    text.includes(".") ||
    text.includes(":") ||
    /\s/u.test(text)
  ) {
    return false;
  }
  return /^[\p{L}\p{N}_+-]{1,6}[\\/]+$/u.test(text);
}

export function isAttachedInlineCodeChainSeparator(text: string): boolean {
  const trimmed = text.trim();
  return trimmed === "." || trimmed === "/" || trimmed === "\\" || trimmed === "::";
}

export function isWhitespaceSeparatedSlashInlineCodeSeparator(text: string): boolean {
  const trimmed = text.trim();
  return trimmed === "/" && trimmed.length !== text.length;
}

export function isEnvVarLikeToken(text: string): boolean {
  return /^[A-Z0-9]+(?:_[A-Z0-9]+)+$/.test(text.trim());
}

export function shouldPreferWholeInlineTokenFreshLine(text: string, minGraphemeLength: number): boolean {
  if (/\s/.test(text) || text.includes("/") || text.includes("\\") || text.includes("-")) {
    return false;
  }
  const graphemeLength = Array.from(text).length;
  return graphemeLength >= minGraphemeLength || /[_():.=]/.test(text);
}

export function startsWithPunctuationPrefix(text: string): boolean {
  return /^[\p{P}\p{S}]/u.test(text.trimStart());
}

export function isPathLikeColonLeadText(text: string | null | undefined): boolean {
  const trimmed = (text ?? "").trimEnd();
  if (!trimmed.endsWith(":")) {
    return false;
  }
  const body = trimmed.slice(0, -1);
  return body.includes("/") || body.includes("\\") || body.includes(".");
}

export function isPathLikeOrDottedText(text: string | null | undefined): boolean {
  const trimmed = (text ?? "").trim();
  return trimmed.includes(".") || trimmed.includes("/") || trimmed.includes("\\");
}

export function isPlainWholeTokenWord(text: string | null | undefined): boolean {
  return /^[\p{L}\p{N}]+$/u.test((text ?? "").trim());
}

export function isWholeTokenIdentifier(text: string | null | undefined): boolean {
  return /^[\p{L}\p{N}_]+$/u.test((text ?? "").trim());
}

export function isLongWholeTokenIdentifier(
  text: string | null | undefined,
  minGraphemeLength: number,
): boolean {
  const trimmed = (text ?? "").trim();
  return isWholeTokenIdentifier(trimmed) && Array.from(trimmed).length >= minGraphemeLength;
}

export function isHyphenatedTextBreakToken(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed.includes("-")) {
    return false;
  }
  if (trimmed.includes("\\") || trimmed.includes("://")) {
    return false;
  }
  if (isAbsolutePath(trimmed) || isRelativeOrHomePath(trimmed)) {
    return false;
  }
  return /[\p{L}\p{N}]-[\p{L}\p{N}]/u.test(trimmed);
}

export function splitHyphenatedTextBreakToken(text: string): string[] {
  if (!isHyphenatedTextBreakToken(text)) {
    return [text];
  }
  const parts: string[] = [];
  const graphemes = Array.from(text);
  let current = "";
  for (let index = 0; index < graphemes.length; index += 1) {
    const char = graphemes[index]!;
    current += char;
    if (char !== "-") {
      continue;
    }
    const previous = graphemes[index - 1] ?? "";
    const next = graphemes[index + 1] ?? "";
    if (/[\p{L}\p{N}]/u.test(previous) && /[\p{L}\p{N}]/u.test(next)) {
      parts.push(current);
      current = "";
    }
  }
  if (current.length > 0) {
    parts.push(current);
  }
  return parts.length > 1 ? parts : [text];
}

export function splitLeadingHyphenTextBreakToken(text: string): string[] {
  if (!/^-[\p{L}\p{N}]/u.test(text)) {
    return [text];
  }
  return ["-", ...splitHyphenatedTextBreakToken(text.slice(1))];
}

export function isSlashDelimitedTextBreakToken(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length === 0 || !trimmed.includes("/")) {
    return false;
  }
  if (trimmed.includes("://")) {
    return false;
  }
  if (isAbsolutePath(trimmed) || isRelativeOrHomePath(trimmed)) {
    return false;
  }
  return /\S\/\S/.test(trimmed);
}

export function isUrlLikeTextBreakToken(text: string): boolean {
  return parseUrlToken(text.trim()) != null;
}

export function splitUrlLikeTextBreakToken(text: string): string[] {
  if (!isUrlLikeTextBreakToken(text)) {
    return [text];
  }
  const parts: string[] = [];
  const graphemes = Array.from(text);
  let current = "";
  const pushCurrent = () => {
    if (current.length === 0) {
      return;
    }
    parts.push(current);
    current = "";
  };
  const shouldSplitAfterChar = (char: string, index: number) => {
    if (char === "/") {
      const previous = graphemes[index - 1] ?? "";
      const previousPrevious = graphemes[index - 2] ?? "";
      const next = graphemes[index + 1] ?? "";
      if (previous === ":" && next === "/") {
        return false;
      }
      if (previous === "/" && previousPrevious === ":") {
        return true;
      }
      return true;
    }
    return (
      char === "." ||
      char === "-" ||
      char === "?" ||
      char === "&" ||
      char === "=" ||
      char === "#" ||
      char === "_"
    );
  };

  for (let index = 0; index < graphemes.length; index += 1) {
    const char = graphemes[index]!;
    current += char;
    if (shouldSplitAfterChar(char, index)) {
      pushCurrent();
    }
  }
  pushCurrent();
  return parts.length > 1 ? parts : [text];
}

export function isStandaloneOpeningPunctuationToken(text: string): boolean {
  return /^[([{]+$/u.test(text);
}

export function isShortDottedNumericPrefixFragment(text: string): boolean {
  const trimmed = text.trim();
  return /^(?:[\p{L}]?\d+)\.$/u.test(trimmed) && Array.from(trimmed).length <= 4;
}
