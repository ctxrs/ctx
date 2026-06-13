import type { LayoutLine } from "@chenglou/pretext";
import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import {
  STYLED_TEXT_BODY_START_CURRENT_LINE_RATIO_THRESHOLD,
} from "./sessionMarkdownInlineMeasurementContext";
import type {
  InlineTextPlacementParams,
  InlineTextPlacementWidths,
} from "./sessionMarkdownInlineMeasurementTextPlacementTypes";
import { resolveInlineCodeTailWholeSegmentFitAllowancePx } from "./sessionMarkdownInlineMeasurementTextPlacementHelpers";
import { cursorsMatch } from "./sessionMarkdownMeasurementCore";

const INLINE_CODE_TAIL_WHOLE_SEGMENT_FIT_TOLERANCE_PX = 0;

export function resolveInlineTextPlacementWidths(
  params: InlineTextPlacementParams,
): InlineTextPlacementWidths {
  const codeGroupId = params.item.codeGroupId;
  const availableWidth = Math.max(
    1,
    params.state.remainingWidth -
      params.reservedWidth -
      params.currentLineWhitespaceContinuationGuardPx -
      params.currentLineStyledSeamGuardPx -
      params.currentLineStyledAfterInlineCodeClusterGuardPx -
      params.currentLineStyledBodyStartGuardPx -
      params.currentLineInlineCodeTailTextSeamGuardPx -
      params.currentLineInlineCodeSoftBreakTextStartGuardPx,
  );
  const availableWidthWithoutLeadingSpace = params.canDropLeadingCollapsedSpaceAtWrap
    ? Math.max(
        1,
        params.state.remainingWidth -
          params.currentLineWhitespaceContinuationGuardPx -
          params.currentLineStyledSeamGuardPx -
          params.currentLineStyledAfterInlineCodeClusterGuardPx -
          params.currentLineStyledBodyStartGuardPx -
          params.currentLineInlineCodeTailTextSeamGuardPx -
          params.currentLineInlineCodeSoftBreakTextStartGuardPx,
      )
    : availableWidth;
  const inlineCodeTailWholeSegmentFitAllowancePx = resolveInlineCodeTailWholeSegmentFitAllowancePx({
    lineStartedWithContinuedCode: params.state.lineStartedWithContinuedCode,
    seamGuardPx: params.currentLineInlineCodeTailTextSeamGuardPx,
  });
  const wholeSegmentAvailableWidth = Math.max(
    1,
    availableWidth + inlineCodeTailWholeSegmentFitAllowancePx,
  );
  const wholeSegmentFitsCurrentLine =
    params.item.fullWidth <=
    wholeSegmentAvailableWidth + INLINE_CODE_TAIL_WHOLE_SEGMENT_FIT_TOLERANCE_PX + 0.01;
  const codeSegmentAvailableWidth =
    codeGroupId != null && !params.item.isSealedInlineCodeFragment
      ? Math.max(
          1,
          availableWidth + params.currentLineFitSlackPx + params.currentLineNearFitLeadingHangPx,
        )
      : availableWidth;
  return {
    availableWidth,
    availableWidthWithoutLeadingSpace,
    inlineCodeTailWholeSegmentFitAllowancePx,
    wholeSegmentAvailableWidth,
    wholeSegmentFitsCurrentLine,
    codeSegmentAvailableWidth,
  };
}

export function shouldBreakBeforeInlineTextSegment(params: {
  placement: InlineTextPlacementParams;
  widths: InlineTextPlacementWidths;
  allowsBreakWord: boolean;
  allowsEmergencyBreakWord: boolean;
  styledStartLine: LayoutLine | null;
}): boolean {
  const { placement, widths } = params;
  const { item, state } = placement;
  const codeGroupId = item.codeGroupId;

  if (
    state.lineHasContent &&
    state.remainingWidth < placement.reservedWidth - 0.01 &&
    !placement.canDropLeadingCollapsedSpaceAtWrap
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    params.allowsEmergencyBreakWord
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    !params.allowsBreakWord &&
    item.startsAfterInlineCodeSeam &&
    !state.lineStartedWithContinuedCode &&
    item.minStartTextWidth > widths.availableWidth + 0.01 &&
    item.minStartTextWidth <= placement.maxWidth + 0.01
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    !params.allowsBreakWord &&
    item.startsAfterStyledTextSeam &&
    item.minStartTextWidth > widths.availableWidth + 0.01 &&
    item.minStartTextWidth <= placement.maxWidth + 0.01
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    state.pendingSpaceWidth > 0 &&
    !params.allowsBreakWord &&
    item.minStartTextWidth > widths.availableWidthWithoutLeadingSpace + 0.01 &&
    item.minStartTextWidth <= placement.maxWidth + 0.01
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    !params.allowsBreakWord &&
    item.startsStyledTextAfterInlineCodeSeam &&
    item.hasTrailingInlineCode &&
    item.minStartTextWidth > widths.availableWidth + 0.01 &&
    item.minStartTextWidth <= placement.maxWidth + 0.01
  ) {
    return true;
  }
  if (
    codeGroupId == null &&
    state.lineHasContent &&
    state.cursor === null &&
    item.startsStyledTextAfterBodySeam &&
    item.fullWidth > widths.availableWidth + 0.01 &&
    (params.styledStartLine == null ||
      cursorsMatch(placement.startCursor, params.styledStartLine.end) ||
      params.styledStartLine.width / Math.max(1, item.fullWidth) <
        STYLED_TEXT_BODY_START_CURRENT_LINE_RATIO_THRESHOLD)
  ) {
    return true;
  }
  return false;
}

export function resolveWholeInlineTextFastPath(params: {
  placement: InlineTextPlacementParams;
  widths: InlineTextPlacementWidths;
}): boolean {
  const { placement, widths } = params;
  const { item, state } = placement;
  const codeGroupId = item.codeGroupId;
  const allowWholeSegmentAfterContinuedInlineCode =
    browserAllowsInlineCodeLeadingHang() &&
    state.lineStartedWithContinuedCode &&
    item.startsAfterPathLikeInlineCodeSeam &&
    !item.startsAfterCollapsedSoftBreak &&
    !item.startsStyledTextAfterInlineCodeSeam &&
    widths.wholeSegmentFitsCurrentLine;
  return (
    codeGroupId == null &&
    state.cursor === null &&
    !item.startsAfterStyledTextSeam &&
    (!item.startsAfterInlineCodeSeam ||
      ((!state.lineStartedWithContinuedCode || allowWholeSegmentAfterContinuedInlineCode) &&
        !item.startsAfterCollapsedSoftBreak &&
        !item.startsStyledTextAfterInlineCodeSeam &&
        widths.wholeSegmentFitsCurrentLine))
  );
}
