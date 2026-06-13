import type {
  InlineCodePlacementDebug,
  InlineCodePlacementParams,
  InlineCodePlacementResult,
} from "./sessionMarkdownInlineMeasurementCodePlacementShared";
import { acceptCodeFragmentState } from "./sessionMarkdownInlineMeasurementCodePlacementShared";

function appendWholeFragmentDebug(params: {
  debug: InlineCodePlacementDebug;
  text: string;
  reservedWidth: number;
  remainingWidth: number;
  guardedRemainingWidth: number;
  fullWidth: number;
  currentLineFitSlackPx: number;
  lineLastCodeFragmentText: string | null;
  lineLastCodeFragmentEndedWithPathDelimiter: boolean;
  lineLastCodeFragmentEndedWithHyphen: boolean;
}): void {
  if (!params.debug.enabled) {
    return;
  }
  params.debug.continuationDecisions.push({
    text: params.text,
    reservedWidth: params.reservedWidth,
    remainingWidth: params.remainingWidth,
    guardedRemainingWidth: params.guardedRemainingWidth,
    fullWidth: params.fullWidth,
    currentLineFitSlackPx: params.currentLineFitSlackPx,
    lineLastCodeFragmentText: params.lineLastCodeFragmentText,
    lineLastCodeFragmentEndedWithPathDelimiter: params.lineLastCodeFragmentEndedWithPathDelimiter,
    lineLastCodeFragmentEndedWithHyphen: params.lineLastCodeFragmentEndedWithHyphen,
    acceptedWholeFragment: true,
  });
}

export function acceptWholeInlineCodeFragment(params: {
  placement: InlineCodePlacementParams;
  state: InlineCodePlacementParams["state"];
  fullWidth: number;
  guardedRemainingWidth: number;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldTrackWeakProseStartCodeGroup: boolean;
  shouldForceSoftBreakWeakProseContinuationWrap: boolean;
}): InlineCodePlacementResult {
  const { placement, state } = params;
  const remainingWidthBeforeFragmentAccept = state.remainingWidth;
  appendWholeFragmentDebug({
    debug: placement.debug,
    text: placement.item.text,
    reservedWidth: placement.reservedWidth,
    remainingWidth: state.remainingWidth,
    guardedRemainingWidth: params.guardedRemainingWidth,
    fullWidth: params.fullWidth,
    currentLineFitSlackPx: placement.currentLineFitSlackPx,
    lineLastCodeFragmentText: state.lineLastCodeFragmentText,
    lineLastCodeFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
    lineLastCodeFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
  });
  acceptCodeFragmentState({
    state,
    item: placement.item,
    codeGroupId: placement.codeGroupId,
    remainingWidthBeforeAccept: remainingWidthBeforeFragmentAccept,
    remainingWidthAfterAccept: Math.max(0, state.remainingWidth - params.fullWidth),
    lastFragmentText: placement.item.text,
    lastFragmentEndedWithHyphen: placement.item.text.endsWith("-"),
    lastFragmentEndedWithPathDelimiter: /[\\/]+$/.test(placement.item.text),
    shouldLimitCurrentCodeGroupToFirstFragment: params.shouldLimitCurrentCodeGroupToFirstFragment,
    shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
    shouldForceSoftBreakWeakProseContinuationWrap:
      params.shouldForceSoftBreakWeakProseContinuationWrap,
    maxWidth: placement.maxWidth,
  });
  state.itemIndex += 1;
  state.pendingSpaceWidth = 0;
  if (placement.debug.enabled) {
    placement.debug.appendLineText(placement.item.text);
  }
  return {
    action: "continue",
    state,
    forcedFreshWholeCodeGroupIndex: placement.forcedFreshWholeCodeGroupIndex,
  };
}
