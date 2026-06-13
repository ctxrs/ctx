import type { LayoutCursor, LayoutLine } from "@chenglou/pretext";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import {
  advancePreparedCursorOneGrapheme,
  isAtomicNonCodeTextSegment,
  isPunctuationOnlySeamText,
  slicePreparedTextBetweenCursors,
} from "./sessionMarkdownInlineMeasurementContext";
import {
  buildPreparedContentKey,
  cursorsMatch,
  getPreparedTextWithSegments,
  measureSingleLineLayout,
  segmentGraphemes,
} from "./sessionMarkdownMeasurementCore";

const INLINE_CODE_TAIL_WHOLE_SEGMENT_FIT_TOLERANCE_PX = 0;

export function resolveInlineCodeTailWholeSegmentFitAllowancePx(params: {
  lineStartedWithContinuedCode: boolean;
  seamGuardPx: number;
}): number {
  return params.lineStartedWithContinuedCode ? 0 : params.seamGuardPx;
}

export function measureBreakWordLine(params: {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  startCursor: LayoutCursor;
  availableWidth: number;
}): LayoutLine | null {
  let cursor: LayoutCursor | null = params.startCursor;
  const candidateCursors: LayoutCursor[] = [];

  while (cursor != null) {
    cursor = advancePreparedCursorOneGrapheme(params.item.prepared, cursor);
    if (cursor != null) {
      candidateCursors.push(cursor);
    }
  }

  if (
    !cursorsMatch(params.startCursor, params.item.endCursor) &&
    !candidateCursors.some((candidate) => cursorsMatch(candidate, params.item.endCursor))
  ) {
    candidateCursors.push(params.item.endCursor);
  }

  if (candidateCursors.length === 0) {
    return null;
  }

  const measureCandidate = (end: LayoutCursor): LayoutLine | null => {
    const slice = slicePreparedTextBetweenCursors(params.item.prepared, params.startCursor, end);
    if (slice.length === 0) {
      return null;
    }
    const prepared = getPreparedTextWithSegments(
      buildPreparedContentKey(`inline-break-word:${params.item.font}`, slice),
      slice,
      params.item.font,
      "normal",
    );
    const line = measureSingleLineLayout(prepared);
    return line == null ? null : { ...line, end };
  };

  let low = 0;
  let high = candidateCursors.length - 1;
  let bestFit: LayoutLine | null = null;

  while (low <= high) {
    const mid = Math.floor((low + high) / 2);
    const measured = measureCandidate(candidateCursors[mid]!);
    if (measured != null && measured.width <= params.availableWidth + 0.01) {
      bestFit = measured;
      low = mid + 1;
      continue;
    }
    high = mid - 1;
  }

  return bestFit ?? measureCandidate(candidateCursors[0]!);
}

export function backtrackBreakWordTokenContinuation(params: {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  startCursor: LayoutCursor;
  line: LayoutLine;
  lineHasContent: boolean;
  lineTailAfterInlineCodeIsPunctuationOnly: boolean;
}): LayoutLine {
  const { item, startCursor, line } = params;
  if (!item.allowsBreakWord) {
    return line;
  }
  if (!item.startsAfterInlineCodeSeam && !params.lineTailAfterInlineCodeIsPunctuationOnly) {
    return line;
  }

  const nextCursor = advancePreparedCursorOneGrapheme(item.prepared, line.end);
  if (nextCursor == null) {
    return line;
  }
  const nextText = slicePreparedTextBetweenCursors(item.prepared, line.end, nextCursor);
  if (/^\s+$/u.test(nextText)) {
    return line;
  }

  const lineText = slicePreparedTextBetweenCursors(item.prepared, startCursor, line.end);
  const withoutTrailingWhitespace = lineText.replace(/\s+$/u, "");
  const remainingText = slicePreparedTextBetweenCursors(item.prepared, line.end, item.endCursor);
  const match = /^(.*\s+)(\S+)$/su.exec(withoutTrailingWhitespace);
  if (match == null) {
    return line;
  }

  const [, prefixWithSpace, trailingFragment] = match;
  const shouldBacktrackTrailingFragment =
    isAtomicNonCodeTextSegment(trailingFragment) &&
    /[\\/]/.test(trailingFragment);
  const shouldBacktrackPrecedingWordForUpcomingSlashToken =
    params.lineHasContent &&
    (item.startsAfterPathLikeInlineCodeSeam || params.lineTailAfterInlineCodeIsPunctuationOnly) &&
    isAtomicNonCodeTextSegment(trailingFragment) &&
    !/[\\/]/.test(trailingFragment) &&
    /\S*[\\/]\S*[\\/]\S*/u.test(remainingText) &&
    /^[\p{P}\p{S}\s]+$/u.test(prefixWithSpace) &&
    /[\p{P}\p{S}]/u.test(prefixWithSpace);

  if (!shouldBacktrackTrailingFragment && !shouldBacktrackPrecedingWordForUpcomingSlashToken) {
    return line;
  }

  let adjustedEnd = startCursor;
  for (const _grapheme of segmentGraphemes(prefixWithSpace)) {
    const next = advancePreparedCursorOneGrapheme(item.prepared, adjustedEnd);
    if (next == null) {
      return line;
    }
    adjustedEnd = next;
  }
  if (cursorsMatch(adjustedEnd, startCursor) || cursorsMatch(adjustedEnd, line.end)) {
    return line;
  }

  const visiblePrefix = prefixWithSpace.replace(/\s+$/u, "");
  const prepared = getPreparedTextWithSegments(
    buildPreparedContentKey(`inline-break-word-prefix:${item.font}`, visiblePrefix),
    visiblePrefix,
    item.font,
    "normal",
  );
  const measured = measureSingleLineLayout(prepared);
  if (measured == null) {
    return line;
  }

  return {
    ...line,
    width: measured.width,
    end: adjustedEnd,
  };
}

export function shouldEmergencyBreakAtomicTextSegment(params: {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  maxWidth: number;
}): boolean {
  return (
    !params.item.allowsBreakWord &&
    params.item.codeGroupId == null &&
    isAtomicNonCodeTextSegment(params.item.text) &&
    params.item.fullWidth > params.maxWidth + 0.01
  );
}

export function shouldBreakBeforePunctuationOnlyContinuationTail(params: {
  codeGroupId: number | null;
  startsAfterPathLikeInlineCodeSeam: boolean;
  lineHasContent: boolean;
  atItemStart: boolean;
  lineStartedWithContinuedCode: boolean;
  lineEndsAtItemEnd: boolean;
  lineSegmentText: string;
  segmentFullWidth: number;
  availableWidth: number;
}): boolean {
  return (
    params.codeGroupId == null &&
    params.startsAfterPathLikeInlineCodeSeam &&
    params.lineHasContent &&
    params.atItemStart &&
    params.lineStartedWithContinuedCode &&
    (!params.lineEndsAtItemEnd || params.segmentFullWidth > params.availableWidth + 0.01) &&
    isPunctuationOnlySeamText(params.lineSegmentText)
  );
}
