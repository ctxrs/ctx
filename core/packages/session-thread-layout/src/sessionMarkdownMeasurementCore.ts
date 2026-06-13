import type { LayoutCursor } from "@chenglou/pretext";
import { incrementPretextPerfCounter } from "./pretextPerfDiagnostics";
import {
  createSessionMarkdownDocument,
  type SessionMarkdownDocument,
  type SessionMarkdownInlineRun,
} from "./sessionMarkdownContract";
import type { SessionMarkdownDebugWindow as SessionMarkdownInlineCodeDebugWindow } from "./sessionMarkdownInlineMeasurementDebug";
import {
  buildPreparedContentKey,
  clampHeight,
  clearSessionTextMeasurementCaches,
  getPreparedText,
  getPreparedTextWithSegments,
  measureCollapsedSpaceWidth,
  measureSingleLineLayout,
  measureTextHeight,
  normalizeHeight,
  pruneCache,
  segmentGraphemes,
  segmentImplicitWordBreaks,
  type TextWhiteSpace,
} from "./sessionTextMeasurement";
import { SESSION_MARKDOWN_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";
import { clearSessionPlainTextMeasurementCaches } from "./sessionPlainTextMeasurement";

const AST_CACHE_LIMIT = 1000;

export const BODY_LINE_HEIGHT_PX = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyLineHeightPx;
const HEADING_FONT_SIZE_BY_DEPTH = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.headingFontSizePxByDepth;
export const MONO_FONT = `${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.codeBlockFontSizePx}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.inlineCodeFontFamily}`;
export const MONO_LINE_HEIGHT_PX = SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.codeBlockLineHeightPx;
export const CODE_BLOCK_VERTICAL_PADDING_PX =
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT.codeBlock.paddingTopPx +
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT.codeBlock.paddingBottomPx;
export const LINE_START_CURSOR: LayoutCursor = { segmentIndex: 0, graphemeIndex: 0 };
const { bodyStrong, heading, headingStrong, tableHeader, tableHeaderStrong } =
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.fontWeight;

export type TextBlockTypography = {
  body: string;
  strong: string;
  emphasis: string;
  strongEmphasis: string;
  lineHeight: number;
};

export type SessionMarkdownDebugWindow = SessionMarkdownInlineCodeDebugWindow & {
  __ctxForcePlainTextDebug?: boolean;
  __ctxPlainTextDebugTarget?: string;
  __ctxPlainTextDebugWidth?: number;
  __ctxPlainTextDebug?: {
    lineCount: number;
    lines: string[];
    lineWidths: number[];
    text: string;
    width: number;
  };
};

const markdownDocumentCache = new Map<string, SessionMarkdownDocument>();

const buildBodyFont = (weight: number, italic = false): string =>
  `${italic ? "italic " : ""}${weight} ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontSizePx}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontFamily}`;

const buildHeadingFont = (
  depth: keyof typeof HEADING_FONT_SIZE_BY_DEPTH,
  weight: number,
  italic = false,
): string =>
  `${italic ? "italic " : ""}${weight} ${HEADING_FONT_SIZE_BY_DEPTH[depth]}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontFamily}`;

export const BODY_TYPOGRAPHY: TextBlockTypography = {
  body: buildBodyFont(400),
  strong: buildBodyFont(bodyStrong),
  emphasis: buildBodyFont(400, true),
  strongEmphasis: buildBodyFont(bodyStrong, true),
  lineHeight: BODY_LINE_HEIGHT_PX,
};

export const TABLE_HEADER_TYPOGRAPHY: TextBlockTypography = {
  body: buildBodyFont(tableHeader),
  strong: buildBodyFont(tableHeaderStrong),
  emphasis: buildBodyFont(tableHeader, true),
  strongEmphasis: buildBodyFont(tableHeaderStrong, true),
  lineHeight: BODY_LINE_HEIGHT_PX,
};

export function buildHeadingTypography(depth: number): TextBlockTypography {
  const normalizedDepth = Math.max(1, Math.min(4, depth));
  const normalizedKey = normalizedDepth as keyof typeof HEADING_FONT_SIZE_BY_DEPTH;
  return {
    body: buildHeadingFont(normalizedKey, heading),
    strong: buildHeadingFont(normalizedKey, headingStrong),
    emphasis: buildHeadingFont(normalizedKey, heading, true),
    strongEmphasis: buildHeadingFont(normalizedKey, headingStrong, true),
    lineHeight:
      SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.headingLineHeightPxByDepth[
        normalizedDepth as keyof typeof SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.headingLineHeightPxByDepth
      ],
  };
}

export {
  buildPreparedContentKey,
  clampHeight,
  getPreparedText,
  getPreparedTextWithSegments,
  measureCollapsedSpaceWidth,
  measureSingleLineLayout,
  measureTextHeight,
  normalizeHeight,
  pruneCache,
  segmentGraphemes,
  segmentImplicitWordBreaks,
};
export type { TextWhiteSpace };

export function parseMarkdown(content: string): SessionMarkdownDocument {
  const cached = markdownDocumentCache.get(content);
  if (cached) {
    incrementPretextPerfCounter("pretext_markdown_ast_hit");
    return cached;
  }
  incrementPretextPerfCounter("pretext_markdown_ast_miss");
  const parsed = createSessionMarkdownDocument(content);
  markdownDocumentCache.set(content, parsed);
  pruneCache(markdownDocumentCache, AST_CACHE_LIMIT);
  return parsed;
}

export function resolveTextRunFont(
  run: Extract<SessionMarkdownInlineRun, { kind: "text" }>,
  typography: TextBlockTypography,
): string {
  switch (run.style) {
    case "strong":
      return typography.strong;
    case "emphasis":
      return typography.emphasis;
    case "strongEmphasis":
      return typography.strongEmphasis;
    case "body":
    default:
      return typography.body;
  }
}

export function cursorsMatch(a: LayoutCursor, b: LayoutCursor): boolean {
  return a.segmentIndex === b.segmentIndex && a.graphemeIndex === b.graphemeIndex;
}

export function measureInlineSpaceWidth(
  cacheKey: string,
  text: string,
  font: string,
  whiteSpace: TextWhiteSpace,
): number {
  const prepared = getPreparedTextWithSegments(cacheKey, text, font, whiteSpace);
  return Math.max(0, measureSingleLineLayout(prepared)?.width ?? 0);
}

export function clearSessionMarkdownMeasurementCaches(): void {
  clearSessionTextMeasurementCaches();
  clearSessionPlainTextMeasurementCaches();
  markdownDocumentCache.clear();
}
