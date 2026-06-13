import type { PreparedTextWithSegments } from "@chenglou/pretext";
import { isAbsolutePath, splitWhitespaceTokens } from "./codeTokenLinks";
import type { InlineWrapMode, PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import {
  buildPreparedContentKey,
  getPreparedTextWithSegments,
  measureInlineSpaceWidth,
  measureSingleLineLayout,
  segmentGraphemes,
  segmentImplicitWordBreaks,
} from "./sessionMarkdownMeasurementCore";
import {
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_FAMILY,
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_SIZE_PX,
} from "./sessionThreadLayoutTokens";
import {
  isHyphenatedTextBreakToken,
  splitHyphenatedTextBreakToken,
} from "./sessionTextTokenClassifier";

const INLINE_CODE_MIN_START_GRAPHEMES = 4;

export const resolveInlineCodeFont = (textFont: string): string => {
  void textFont;
  return `${SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_INLINE_CODE_FONT_FAMILY}`;
};

export function measureInlineCodeMinStartTextWidth(
  text: string,
  font: string,
  minStartGraphemes: number = INLINE_CODE_MIN_START_GRAPHEMES,
): number {
  const sample = segmentGraphemes(text).slice(0, minStartGraphemes).join("");
  if (sample.length === 0) {
    return 0;
  }
  const prepared = getPreparedTextWithSegments(
    buildPreparedContentKey(`inline-code-min-start:${font}`, sample),
    sample,
    font,
    "pre-wrap",
  );
  const wholeLine = measureSingleLineLayout(prepared);
  return wholeLine?.width ?? 0;
}

function measureTextMinStartTextWidth(text: string, font: string): number {
  const firstToken = text.match(/^\S+/)?.[0] ?? "";
  if (firstToken.length === 0) {
    return 0;
  }
  const minStartSample = segmentImplicitWordBreaks(firstToken)[0] ?? firstToken;
  const prepared = getPreparedTextWithSegments(
    buildPreparedContentKey(`inline-text-min-start:${font}`, minStartSample),
    minStartSample,
    font,
    "normal",
  );
  const wholeLine = measureSingleLineLayout(prepared);
  return wholeLine?.width ?? 0;
}

function isSlashDelimitedTextToken(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return false;
  }
  if (!trimmed.includes("/")) {
    return false;
  }
  if (trimmed.includes("://")) {
    return false;
  }
  if (isAbsolutePath(trimmed)) {
    return false;
  }
  if (trimmed.startsWith("./") || trimmed.startsWith("../") || trimmed.startsWith("~/")) {
    return false;
  }
  return /\S\/\S/.test(trimmed);
}

function splitTextRunChunks(text: string, preserveSlashDelimitedTokens: boolean): string[] {
  const parts = splitWhitespaceTokens(text);
  if (parts.length === 0) {
    return [];
  }
  if (
    preserveSlashDelimitedTokens &&
    parts.some((part) => !/\s+/.test(part) && isSlashDelimitedTextToken(part))
  ) {
    return parts.flatMap((part) =>
      !/\s+/.test(part) && isHyphenatedTextBreakToken(part)
        ? splitHyphenatedTextBreakToken(part)
        : [part],
    );
  }

  const chunks: string[] = [];
  let current = "";
  const flushCurrent = () => {
    if (current.length === 0) {
      return;
    }
    chunks.push(current);
    current = "";
  };

  for (const part of parts) {
    if (preserveSlashDelimitedTokens && !/\s+/.test(part) && isSlashDelimitedTextToken(part)) {
      flushCurrent();
      chunks.push(part);
      continue;
    }
    current += part;
  }
  flushCurrent();
  return chunks;
}

export function splitTrailingPlainPathTailFragment(fragment: string): string[] {
  if (!fragment.includes("-")) {
    return [fragment];
  }
  const parts: string[] = [];
  let current = "";
  for (const char of fragment) {
    current += char;
    if (char === "-") {
      parts.push(current);
      current = "";
    }
  }
  if (current.length > 0) {
    parts.push(current);
  }
  return parts.length > 1 ? parts : [fragment];
}

export function pushTextRunItems(
  items: PreparedInlineLayoutItem[],
  params: {
    text: string;
    font: string;
    cacheKeyPrefix: string;
    collapsedSpaceWidth: number;
    wrapMode: InlineWrapMode;
    startsAfterInlineCodeSeam: boolean;
    startsAfterCollapsedSoftBreak: boolean;
    startsAfterPathLikeInlineCodeSeam: boolean;
    startsStyledTextAfterInlineCodeSeam: boolean;
    startsAfterStyledTextSeam: boolean;
    startsStyledTextAfterBodySeam: boolean;
    hasTrailingStyledText: boolean;
    hasTrailingInlineCode: boolean;
    isDecoratedText: boolean;
  },
): void {
  const pushCollapsedSpace = () => {
    const previous = items[items.length - 1];
    if (previous?.kind === "space" && previous.codeGroupId == null) {
      return;
    }
    items.push({ kind: "space", width: params.collapsedSpaceWidth, codeGroupId: null, text: " " });
  };
  const pushTextChunk = (text: string, options: {
    startsAfterInlineCodeSeam: boolean;
    startsAfterCollapsedSoftBreak: boolean;
    startsAfterPathLikeInlineCodeSeam: boolean;
    startsStyledTextAfterInlineCodeSeam: boolean;
    startsAfterStyledTextSeam: boolean;
    startsStyledTextAfterBodySeam: boolean;
    hasTrailingStyledText: boolean;
    hasTrailingInlineCode: boolean;
  }) => {
    if (text.length === 0) {
      return;
    }
    const leadingWhitespace = text.match(/^\s+/)?.[0] ?? "";
    const trailingWhitespace = text.match(/\s+$/)?.[0] ?? "";
    const core = text.slice(leadingWhitespace.length, text.length - trailingWhitespace.length);

    if (leadingWhitespace.length > 0) {
      pushCollapsedSpace();
    }

    if (core.length > 0) {
      const allowsBreakWord =
        params.wrapMode === "break-word" || params.wrapMode === "anywhere";
      const prepared = getPreparedTextWithSegments(
        buildPreparedContentKey(params.cacheKeyPrefix, core),
        core,
        params.font,
        "normal",
      );
      const wholeLine = measureSingleLineLayout(prepared);
      if (wholeLine != null) {
        items.push({
          kind: "segment",
          allowsBreakWord,
          codeGroupId: null,
          codeGroupHasDottedPath: false,
          codeGroupHasWhitespace: false,
          codeGroupHasTrailingText: false,
          codeGroupIsOnlyInlineCodeInSegment: false,
          codeGroupStartsAfterText: false,
          codeGroupStartsAfterStyledTextSeam: false,
          codePartStartsAfterWhitespace: false,
          chromeWidth: 0,
          endCursor: wholeLine.end,
          fullWidth: wholeLine.width,
          isFirstCodeGroupFragment: false,
          startsAfterCodeWhitespace: false,
          isFirstPathFragmentAfterHyphenRun: false,
          isPathTailFragment: false,
          isSealedInlineCodeFragment: false,
          minStartTextWidth: allowsBreakWord ? 0 : measureTextMinStartTextWidth(core, params.font),
          prefersFreshLineStart: false,
          prefersFreshLineStartWithoutLeadingHang: false,
          startsAfterInlineCodeSeam: options.startsAfterInlineCodeSeam,
          startsAfterCollapsedSoftBreak: options.startsAfterCollapsedSoftBreak,
          startsAfterPathLikeInlineCodeSeam: options.startsAfterPathLikeInlineCodeSeam,
          startsStyledTextAfterInlineCodeSeam: options.startsStyledTextAfterInlineCodeSeam,
          startsAfterStyledTextSeam: options.startsAfterStyledTextSeam,
          startsStyledTextAfterBodySeam: options.startsStyledTextAfterBodySeam,
          font: params.font,
          hasTrailingStyledText: options.hasTrailingStyledText,
          hasTrailingInlineCode: options.hasTrailingInlineCode,
          isDecoratedText: params.isDecoratedText,
          prepared,
          text: core,
        });
      }
    }

    if (trailingWhitespace.length > 0) {
      pushCollapsedSpace();
    }
  };

  const normalizedSource = params.text.replace(/\u00a0/g, " ").replace(/\r\n?/g, "\n");
  const normalized = normalizedSource.replace(/\n/g, " ");
  if (normalized.length === 0) {
    return;
  }
  const startsAfterCollapsedSoftBreak =
    params.startsAfterCollapsedSoftBreak ||
    (normalizedSource.match(/^\s+/)?.[0] ?? "").includes("\n");

  const chunks = splitTextRunChunks(normalized, (params.wrapMode ?? "normal") === "normal");
  chunks.forEach((chunk, index) => {
    const firstChunk = index === 0;
    pushTextChunk(chunk, {
      startsAfterInlineCodeSeam: firstChunk ? params.startsAfterInlineCodeSeam : false,
      startsAfterCollapsedSoftBreak: firstChunk ? startsAfterCollapsedSoftBreak : false,
      startsAfterPathLikeInlineCodeSeam: firstChunk ? params.startsAfterPathLikeInlineCodeSeam : false,
      startsStyledTextAfterInlineCodeSeam: firstChunk ? params.startsStyledTextAfterInlineCodeSeam : false,
      startsAfterStyledTextSeam: firstChunk ? params.startsAfterStyledTextSeam : false,
      startsStyledTextAfterBodySeam: firstChunk ? params.startsStyledTextAfterBodySeam : false,
      hasTrailingStyledText: firstChunk ? params.hasTrailingStyledText : false,
      hasTrailingInlineCode: firstChunk ? params.hasTrailingInlineCode : false,
    });
  });
}

export function pushInlineCodeWhitespaceItems(
  items: PreparedInlineLayoutItem[],
  params: {
    text: string;
    font: string;
    codeGroupId: number;
    cacheKeyPrefix: string;
  },
): void {
  const normalized = params.text.replace(/\r\n/g, "\n");
  let spaces = "";
  let partIndex = 0;

  const flushSpaces = () => {
    if (spaces.length === 0) {
      return;
    }
    items.push({
      kind: "space",
      width: measureInlineSpaceWidth(
        buildPreparedContentKey(`${params.cacheKeyPrefix}:space:${partIndex}`, spaces),
        spaces,
        params.font,
        "pre-wrap",
      ),
      codeGroupId: params.codeGroupId,
      text: spaces,
    });
    spaces = "";
    partIndex += 1;
  };

  for (let index = 0; index < normalized.length; index += 1) {
    const character = normalized[index]!;
    if (character === "\n") {
      flushSpaces();
      items.push({ kind: "hardBreak" });
      continue;
    }
    spaces += character;
  }
  flushSpaces();
}
