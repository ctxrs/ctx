import type { LayoutLine } from "@chenglou/pretext";
import { shouldApplyInlineCodeSoftBreakTextStartGuard } from "./sessionMarkdownInlineCodeFit";
import {
  isPunctuationOnlySeamText,
  resolveInlineCodeSoftBreakTextStartGuardPx,
} from "./sessionMarkdownInlineMeasurementContext";
import type {
  InlineTextPlacementParams,
  InlineTextPlacementResult,
} from "./sessionMarkdownInlineMeasurementTextPlacementTypes";
import { cursorsMatch, LINE_START_CURSOR } from "./sessionMarkdownMeasurementCore";

export function acceptWholeInlineTextSegment(
  params: InlineTextPlacementParams,
): InlineTextPlacementResult {
  const state = { ...params.state };
  const { item } = params;
  const codeGroupId = item.codeGroupId;
  const lineWasEmpty = !state.lineHasContent;
  state.remainingWidth = Math.max(0, state.remainingWidth - params.reservedWidth - item.fullWidth);
  state.lineOnlyCodeGroupId = null;
  state.lastAcceptedCodeGroupId = null;
  state.lineCurrentCodeGroupStartFragmentText = null;
  state.lineCurrentCodeGroupStartedNearFresh = false;
  state.lineCurrentCodeGroupLimitToFirstFragment = false;
  state.lineLastCodeFragmentText = null;
  state.lineLastCodeFragmentEndedWithHyphen = false;
  state.lineLastCodeFragmentEndedWithPathDelimiter = false;
  state.lineHasContent = true;
  if (item.isDecoratedText) {
    state.lineDecoratedTextSegmentCount += 1;
  }
  state.lineHasSoftHyphenText ||= item.text.includes("\u00ad");
  state.lineStartedWithCollapsedSoftBreakPlainText ||=
    lineWasEmpty && item.startsAfterCollapsedSoftBreak;
  state.lineTailAfterInlineCodeIsPunctuationOnly =
    item.startsAfterInlineCodeSeam && isPunctuationOnlySeamText(item.text);
  state.lineAcceptedPlainAfterContinuedCode ||= state.lineStartedWithContinuedCode;
  const shouldApplySoftBreakGuard =
    params.sliceStartsAtItemStart &&
    shouldApplyInlineCodeSoftBreakTextStartGuard({
      text: item.text,
      startsAfterCollapsedSoftBreak: item.startsAfterCollapsedSoftBreak,
      startsAfterPathLikeInlineCodeSeam: item.startsAfterPathLikeInlineCodeSeam,
      startsAfterInlineCodeSeam: item.startsAfterInlineCodeSeam,
      startsStyledTextAfterInlineCodeSeam: item.startsStyledTextAfterInlineCodeSeam,
      lastFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
      lastFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
    });
  state.lineAcceptedSoftBreakProseAfterInlineCode ||= shouldApplySoftBreakGuard;
  if (shouldApplySoftBreakGuard) {
    state.lineSoftBreakProseAfterInlineCodeGuardPx = Math.max(
      state.lineSoftBreakProseAfterInlineCodeGuardPx,
      resolveInlineCodeSoftBreakTextStartGuardPx(item.minStartTextWidth),
    );
  }
  state.pendingSpaceWidth = 0;
  if (params.debug.enabled) {
    params.debug.appendLineText(item.text);
  }
  state.itemIndex += 1;
  return { action: "continue", state };
}

export function acceptMeasuredInlineTextLine(params: {
  placement: InlineTextPlacementParams;
  line: LayoutLine;
  lineSegmentText: string;
}): InlineTextPlacementResult {
  const state = { ...params.placement.state };
  const { item } = params.placement;
  const codeGroupId = item.codeGroupId;
  const lineWasEmpty = !state.lineHasContent;
  state.remainingWidth = Math.max(0, state.remainingWidth - params.placement.reservedWidth - params.line.width);
  if (!state.lineHasContent) {
    state.lineOnlyCodeGroupId = codeGroupId;
    state.lineLastCodeFragmentEndedWithHyphen =
      codeGroupId != null && cursorsMatch(params.line.end, item.endCursor) && item.text.endsWith("-");
    state.lineLastCodeFragmentEndedWithPathDelimiter =
      codeGroupId != null && cursorsMatch(params.line.end, item.endCursor) && /[\\/]+$/.test(item.text);
    state.lineStartedWithContinuedCode =
      codeGroupId != null &&
      (!item.isFirstCodeGroupFragment || !cursorsMatch(params.placement.startCursor, LINE_START_CURSOR));
    state.lineSawInlineCode = codeGroupId != null;
  } else if (state.lineOnlyCodeGroupId !== codeGroupId) {
    state.lineOnlyCodeGroupId = null;
  }
  state.lineLastCodeFragmentEndedWithHyphen =
    codeGroupId != null && cursorsMatch(params.line.end, item.endCursor) && item.text.endsWith("-");
  state.lineLastCodeFragmentEndedWithPathDelimiter =
    codeGroupId != null && cursorsMatch(params.line.end, item.endCursor) && /[\\/]+$/.test(item.text);
  if (codeGroupId != null && state.lastAcceptedCodeGroupId !== codeGroupId) {
    state.lineCurrentCodeGroupStartFragmentText = params.lineSegmentText;
  }
  state.lineLastCodeFragmentText = codeGroupId != null ? params.lineSegmentText : null;
  state.lastAcceptedCodeGroupId = codeGroupId;
  state.lineHasContent = true;

  if (codeGroupId != null) {
    state.chargedCodeGroups.add(codeGroupId);
    if (state.lineAcceptedSoftBreakProseAfterInlineCode && state.lineSoftBreakProseGuardCodeGroupId == null) {
      state.lineSoftBreakProseGuardCodeGroupId = codeGroupId;
    }
    state.lineSawInlineCode = true;
    state.lineTailAfterInlineCodeIsPunctuationOnly = false;
  } else {
    state.lastAcceptedCodeGroupId = null;
    state.lineLastCodeFragmentEndedWithHyphen = false;
    state.lineLastCodeFragmentEndedWithPathDelimiter = false;
    state.lineStartedWithCollapsedSoftBreakPlainText ||=
      lineWasEmpty && item.startsAfterCollapsedSoftBreak;
    if (item.isDecoratedText) {
      state.lineDecoratedTextSegmentCount += 1;
    }
    state.lineHasSoftHyphenText ||= item.text.includes("\u00ad");
    state.lineTailAfterInlineCodeIsPunctuationOnly =
      item.startsAfterInlineCodeSeam && isPunctuationOnlySeamText(item.text);
    state.lineAcceptedPlainAfterContinuedCode ||= state.lineStartedWithContinuedCode;
    const shouldApplySoftBreakGuard =
      params.placement.sliceStartsAtItemStart &&
      shouldApplyInlineCodeSoftBreakTextStartGuard({
        text: item.text,
        startsAfterCollapsedSoftBreak: item.startsAfterCollapsedSoftBreak,
        startsAfterPathLikeInlineCodeSeam: item.startsAfterPathLikeInlineCodeSeam,
        startsAfterInlineCodeSeam: item.startsAfterInlineCodeSeam,
        startsStyledTextAfterInlineCodeSeam: item.startsStyledTextAfterInlineCodeSeam,
        lastFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
        lastFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
      });
    state.lineAcceptedSoftBreakProseAfterInlineCode ||= shouldApplySoftBreakGuard;
    if (shouldApplySoftBreakGuard) {
      state.lineSoftBreakProseAfterInlineCodeGuardPx = Math.max(
        state.lineSoftBreakProseAfterInlineCodeGuardPx,
        resolveInlineCodeSoftBreakTextStartGuardPx(item.minStartTextWidth),
      );
    }
  }

  state.pendingSpaceWidth = 0;
  if (params.placement.debug.enabled) {
    params.placement.debug.appendLineText(params.lineSegmentText);
  }

  if (cursorsMatch(params.line.end, item.endCursor)) {
    state.itemIndex += 1;
    state.cursor = null;
    return { action: "continue", state };
  }

  state.cursor = params.line.end;
  return { action: "break", state };
}
