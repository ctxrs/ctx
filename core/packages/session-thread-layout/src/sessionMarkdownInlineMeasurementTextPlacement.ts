import { layoutNextLine, type LayoutCursor, type LayoutLine } from "@chenglou/pretext";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import type { InlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";
import type {
  InlineTextPlacementDebug,
  InlineTextPlacementParams,
  InlineTextPlacementResult,
} from "./sessionMarkdownInlineMeasurementTextPlacementTypes";
export type {
  InlineTextPlacementParams,
  InlineTextPlacementResult,
} from "./sessionMarkdownInlineMeasurementTextPlacementTypes";
import {
  advancePreparedCursorOneGrapheme,
  isAtomicNonCodeTextSegment,
  isWhitespaceOnlyLineSlice,
  slicePreparedTextBetweenCursors,
} from "./sessionMarkdownInlineMeasurementContext";
import { cursorsMatch } from "./sessionMarkdownMeasurementCore";

import {
  backtrackBreakWordTokenContinuation,
  measureBreakWordLine,
  shouldBreakBeforePunctuationOnlyContinuationTail,
  shouldEmergencyBreakAtomicTextSegment,
} from "./sessionMarkdownInlineMeasurementTextPlacementHelpers";
import {
  acceptMeasuredInlineTextLine,
  acceptWholeInlineTextSegment,
} from "./sessionMarkdownInlineTextPlacementAccept";
import {
  resolveInlineTextPlacementWidths,
  resolveWholeInlineTextFastPath,
  shouldBreakBeforeInlineTextSegment,
} from "./sessionMarkdownInlineTextPlacementRules";

export {
  backtrackBreakWordTokenContinuation,
  measureBreakWordLine,
  resolveInlineCodeTailWholeSegmentFitAllowancePx,
  shouldBreakBeforePunctuationOnlyContinuationTail,
  shouldEmergencyBreakAtomicTextSegment,
} from "./sessionMarkdownInlineMeasurementTextPlacementHelpers";

export function placeInlineTextSegment(params: InlineTextPlacementParams): InlineTextPlacementResult {
  const state = { ...params.state };
  const { item } = params;
  const codeGroupId = item.codeGroupId;
  const allowsEmergencyBreakWord = shouldEmergencyBreakAtomicTextSegment({
    item,
    maxWidth: params.maxWidth,
  });
  const allowsBreakWord =
    item.allowsBreakWord || allowsEmergencyBreakWord;
  const widths = resolveInlineTextPlacementWidths(params);

  if (
    params.debug.enabled &&
    codeGroupId != null &&
    !item.isSealedInlineCodeFragment &&
    state.cursor === null &&
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId
  ) {
    params.debug.continuationDecisions.push({
      text: item.text,
      reservedWidth: params.reservedWidth,
      remainingWidth: state.remainingWidth,
      guardedRemainingWidth: params.guardedRemainingWidth,
      availableLineWidth: widths.codeSegmentAvailableWidth,
      fullWidth: item.fullWidth,
      currentLineFitSlackPx: params.currentLineFitSlackPx,
      lineLastCodeFragmentText: state.lineLastCodeFragmentText,
      lineLastCodeFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
      lineLastCodeFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
    });
  }

  const styledStartLine: LayoutLine | null =
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    item.startsStyledTextAfterBodySeam &&
    item.fullWidth > widths.availableWidth + 0.01
      ? layoutNextLine(item.prepared, params.startCursor, widths.availableWidth)
      : null;

  if (
    shouldBreakBeforeInlineTextSegment({
      placement: params,
      widths,
      allowsBreakWord,
      allowsEmergencyBreakWord,
      styledStartLine,
    })
  ) {
    state.cursor = null;
    return { action: "break", state };
  }
  const allowWholeSegmentFastPath = resolveWholeInlineTextFastPath({
    placement: params,
    widths,
  });

  if (
    params.debug.enabled &&
    codeGroupId == null &&
    state.cursor === null &&
    item.startsAfterInlineCodeSeam
  ) {
    params.debug.segmentSeamAdjustments.push({
      type: "inline-code-whole-fit",
      lineHasContent: state.lineHasContent,
      text: item.text,
      reservedWidth: params.reservedWidth,
      availableWidth: widths.availableWidth,
      wholeSegmentAvailableWidth: widths.wholeSegmentAvailableWidth,
      inlineCodeTailWholeSegmentFitAllowancePx: widths.inlineCodeTailWholeSegmentFitAllowancePx,
      fullWidth: item.fullWidth,
      lineStartedWithContinuedCode: state.lineStartedWithContinuedCode,
      startsAfterCollapsedSoftBreak: item.startsAfterCollapsedSoftBreak,
      startsStyledTextAfterInlineCodeSeam: item.startsStyledTextAfterInlineCodeSeam,
      allowed: allowWholeSegmentFastPath,
    });
  }

  if (allowWholeSegmentFastPath && widths.wholeSegmentFitsCurrentLine) {
    return acceptWholeInlineTextSegment(params);
  }

  const regularLineWithReservedSpace: LayoutLine | null =
    styledStartLine ?? layoutNextLine(item.prepared, params.startCursor, widths.codeSegmentAvailableWidth);
  const regularLineWithoutLeadingSpace: LayoutLine | null =
    params.canDropLeadingCollapsedSpaceAtWrap &&
    widths.availableWidthWithoutLeadingSpace > widths.availableWidth + 0.01
      ? layoutNextLine(
          item.prepared,
          params.startCursor,
          widths.availableWidthWithoutLeadingSpace,
        )
      : null;
  const lineWithReservedSpace: LayoutLine | null =
    allowsBreakWord &&
    (allowsEmergencyBreakWord ||
      regularLineWithReservedSpace == null ||
      cursorsMatch(params.startCursor, regularLineWithReservedSpace.end))
      ? measureBreakWordLine({
          item,
          startCursor: params.startCursor,
          availableWidth: widths.codeSegmentAvailableWidth,
        })
      : regularLineWithReservedSpace;
  const lineWithoutLeadingSpace: LayoutLine | null =
    allowsBreakWord &&
    params.canDropLeadingCollapsedSpaceAtWrap &&
    widths.availableWidthWithoutLeadingSpace > widths.availableWidth + 0.01 &&
    (allowsEmergencyBreakWord ||
      regularLineWithoutLeadingSpace == null ||
      cursorsMatch(params.startCursor, regularLineWithoutLeadingSpace.end))
      ? measureBreakWordLine({
          item,
          startCursor: params.startCursor,
          availableWidth: widths.availableWidthWithoutLeadingSpace,
        })
      : regularLineWithoutLeadingSpace;
  const useLineWithoutLeadingSpace = false;
  const rawLine: LayoutLine | null = lineWithReservedSpace;
  const line: LayoutLine | null =
    rawLine == null || cursorsMatch(params.startCursor, rawLine.end)
      ? rawLine
      : backtrackBreakWordTokenContinuation({
          item,
          startCursor: params.startCursor,
          line: rawLine,
          lineHasContent: state.lineHasContent,
          lineTailAfterInlineCodeIsPunctuationOnly: state.lineTailAfterInlineCodeIsPunctuationOnly,
        });

  if (
    params.debug.enabled &&
    codeGroupId != null &&
    !item.isSealedInlineCodeFragment &&
    state.cursor === null &&
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId
  ) {
    params.debug.continuationDecisions.push({
      text: item.text,
      reservedWidth: params.reservedWidth,
      remainingWidth: state.remainingWidth,
      guardedRemainingWidth: params.guardedRemainingWidth,
      availableLineWidth: widths.codeSegmentAvailableWidth,
      lineWidth: line?.width,
      fullWidth: item.fullWidth,
      currentLineFitSlackPx: params.currentLineFitSlackPx,
      lineLastCodeFragmentText: state.lineLastCodeFragmentText,
      lineLastCodeFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
      lineLastCodeFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
      acceptedWholeFragment: line != null && cursorsMatch(line.end, item.endCursor),
      brokeBeforeFragment: line == null || cursorsMatch(params.startCursor, line.end),
    });
  }

  if (line == null || cursorsMatch(params.startCursor, line.end)) {
    if (!state.lineHasContent) {
      if (codeGroupId == null && item.startsAfterCollapsedSoftBreak) {
        const advancedCursor = advancePreparedCursorOneGrapheme(item.prepared, params.startCursor);
        if (advancedCursor != null) {
          if (params.debug.enabled) {
            params.debug.segmentSeamAdjustments.push({
              type: "no-progress-advance",
              lineHasContent: state.lineHasContent,
              text: item.text,
            });
          }
          state.cursor = advancedCursor;
          return { action: "continue", state };
        }
      }
      if (params.debug.enabled) {
        params.debug.segmentSeamAdjustments.push({
          type: "no-progress-drop",
          lineHasContent: state.lineHasContent,
          text: item.text,
        });
      }
      state.itemIndex += 1;
    }
    state.cursor = null;
    return { action: "break", state };
  }

  const lineSegmentText = slicePreparedTextBetweenCursors(item.prepared, params.startCursor, line.end);
  if (
    shouldBreakBeforePunctuationOnlyContinuationTail({
      codeGroupId,
      startsAfterPathLikeInlineCodeSeam: item.startsAfterPathLikeInlineCodeSeam,
      lineHasContent: state.lineHasContent,
      atItemStart: state.cursor === null,
      lineStartedWithContinuedCode: state.lineStartedWithContinuedCode,
      lineEndsAtItemEnd: cursorsMatch(line.end, item.endCursor),
      lineSegmentText,
      segmentFullWidth: item.fullWidth,
      availableWidth: widths.availableWidth,
    })
  ) {
    state.cursor = null;
    return { action: "break", state };
  }
  if (
    codeGroupId == null &&
    item.startsAfterCollapsedSoftBreak &&
    state.lineHasContent &&
    !cursorsMatch(line.end, item.endCursor) &&
    isWhitespaceOnlyLineSlice(lineSegmentText)
  ) {
    if (params.debug.enabled) {
      params.debug.segmentSeamAdjustments.push({
        type: "whitespace-only-break",
        lineHasContent: state.lineHasContent,
        text: item.text,
      });
    }
    state.cursor = null;
    return { action: "break", state };
  }
  if (
    codeGroupId == null &&
    item.startsAfterCollapsedSoftBreak &&
    !state.lineHasContent &&
    !cursorsMatch(line.end, item.endCursor) &&
    isWhitespaceOnlyLineSlice(lineSegmentText)
  ) {
    if (params.debug.enabled) {
      params.debug.segmentSeamAdjustments.push({
        type: "whitespace-only-advance",
        lineHasContent: state.lineHasContent,
        text: item.text,
      });
    }
    state.cursor = line.end;
    return { action: "continue", state };
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    !cursorsMatch(line.end, item.endCursor) &&
    isAtomicNonCodeTextSegment(item.text) &&
    (item.fullWidth <= params.maxWidth + 0.01 ||
      (allowsBreakWord &&
        state.lineTailAfterInlineCodeIsPunctuationOnly &&
        state.pendingSpaceWidth > 0))
  ) {
    state.cursor = null;
    return { action: "break", state };
  }
  return acceptMeasuredInlineTextLine({
    placement: params,
    line,
    lineSegmentText,
  });
}
