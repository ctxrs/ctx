import {
  layout,
  layoutNextLine,
  prepare,
  prepareWithSegments,
  type LayoutCursor,
  type PreparedText,
  type PreparedTextWithSegments,
} from "@chenglou/pretext";
import {
  hashPretextPerfValue,
  incrementPretextPerfCounter,
} from "./pretextPerfDiagnostics";

export type TextWhiteSpace = "normal" | "pre-wrap";

export const SESSION_TEXT_MEASUREMENT_CACHE_LIMIT = 4000;

const LINE_START_CURSOR: LayoutCursor = { segmentIndex: 0, graphemeIndex: 0 };
const UNBOUNDED_WIDTH_PX = 100_000;

const preparedCache = new Map<string, PreparedText>();
const preparedSegmentsCache = new Map<string, PreparedTextWithSegments>();
const collapsedSpaceWidthCache = new Map<string, number>();

const graphemeSegmenter =
  typeof Intl !== "undefined" && typeof Intl.Segmenter === "function"
    ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
    : null;
const wordSegmenter =
  typeof Intl !== "undefined" && typeof Intl.Segmenter === "function"
    ? new Intl.Segmenter(undefined, { granularity: "word" })
    : null;
const IMPLICIT_WORD_BREAK_SCRIPT_PATTERN = /[\p{Script=Thai}\p{Script=Lao}\p{Script=Khmer}\p{Script=Myanmar}]/u;

export function pruneCache<T>(cache: Map<string, T>, limit: number) {
  while (cache.size > limit) {
    const oldestKey = cache.keys().next().value;
    if (typeof oldestKey !== "string") break;
    cache.delete(oldestKey);
  }
}

export const clampHeight = (value: number): number =>
  Number.isFinite(value) && value > 0 ? Math.max(1, value) : 1;

export const normalizeHeight = (value: number): number =>
  Math.round(clampHeight(value) * 16) / 16;

export function getPreparedText(
  cacheKey: string,
  text: string,
  font: string,
  whiteSpace: TextWhiteSpace,
): PreparedText {
  const preparedCacheKey = `${cacheKey}:${font}:${whiteSpace}`;
  const cached = preparedCache.get(preparedCacheKey);
  if (cached) {
    incrementPretextPerfCounter("pretext_markdown_prepared_text_hit");
    return cached;
  }
  incrementPretextPerfCounter("pretext_markdown_prepared_text_miss");
  const prepared = prepare(text, font, whiteSpace === "pre-wrap" ? { whiteSpace } : undefined);
  preparedCache.set(preparedCacheKey, prepared);
  pruneCache(preparedCache, SESSION_TEXT_MEASUREMENT_CACHE_LIMIT);
  return prepared;
}

export function getPreparedTextWithSegments(
  cacheKey: string,
  text: string,
  font: string,
  whiteSpace: TextWhiteSpace,
): PreparedTextWithSegments {
  const preparedCacheKey = `${cacheKey}:${font}:${whiteSpace}`;
  const cached = preparedSegmentsCache.get(preparedCacheKey);
  if (cached) {
    incrementPretextPerfCounter("pretext_markdown_prepared_segments_hit");
    return cached;
  }
  incrementPretextPerfCounter("pretext_markdown_prepared_segments_miss");
  const prepared = prepareWithSegments(text, font, whiteSpace === "pre-wrap" ? { whiteSpace } : undefined);
  preparedSegmentsCache.set(preparedCacheKey, prepared);
  pruneCache(preparedSegmentsCache, SESSION_TEXT_MEASUREMENT_CACHE_LIMIT);
  return prepared;
}

export function measureTextHeight(params: {
  cacheKey: string;
  text: string;
  font: string;
  width: number;
  lineHeight: number;
  whiteSpace?: TextWhiteSpace;
}): number {
  const whiteSpace = params.whiteSpace ?? "normal";
  const prepared = getPreparedText(params.cacheKey, params.text, params.font, whiteSpace);
  return clampHeight(layout(prepared, Math.max(1, params.width), params.lineHeight).height);
}

export function measureSessionTextHeight(params: {
  cacheKey: string;
  text: string;
  font: string;
  width: number;
  lineHeight: number;
  whiteSpace?: TextWhiteSpace;
}): number {
  return normalizeHeight(measureTextHeight(params));
}

export function measureSingleLineLayout(prepared: PreparedTextWithSegments) {
  return layoutNextLine(prepared, LINE_START_CURSOR, UNBOUNDED_WIDTH_PX);
}

export function measureCollapsedSpaceWidth(font: string): number {
  const cached = collapsedSpaceWidthCache.get(font);
  if (cached != null) {
    return cached;
  }
  const joined = measureSingleLineLayout(getPreparedTextWithSegments(`collapsed-space:${font}:joined`, "A A", font, "normal"));
  const compact = measureSingleLineLayout(getPreparedTextWithSegments(`collapsed-space:${font}:compact`, "AA", font, "normal"));
  const width = Math.max(0, (joined?.width ?? 0) - (compact?.width ?? 0));
  collapsedSpaceWidthCache.set(font, width);
  return width;
}

export function buildPreparedContentKey(prefix: string, text: string): string {
  return `${prefix}:${text.length}:${hashPretextPerfValue(text)}`;
}

export function segmentGraphemes(text: string): string[] {
  if (!text) return [];
  if (!graphemeSegmenter) return Array.from(text);
  return Array.from(graphemeSegmenter.segment(text), (segment) => segment.segment);
}

export function segmentWords(text: string): string[] {
  if (!text) return [];
  if (!wordSegmenter) return [text];
  return Array.from(wordSegmenter.segment(text), (segment) => segment.segment);
}

export function segmentImplicitWordBreaks(text: string): string[] {
  if (!text || !IMPLICIT_WORD_BREAK_SCRIPT_PATTERN.test(text)) {
    return [text];
  }

  const segments = segmentWords(text).filter((segment) => segment.length > 0);
  if (segments.length <= 1 || segments.join("") !== text) {
    return [text];
  }

  return segments;
}

export function clearSessionTextMeasurementCaches(): void {
  preparedCache.clear();
  preparedSegmentsCache.clear();
  collapsedSpaceWidthCache.clear();
}
