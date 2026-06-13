import type {
  InlineCodePlacementParams,
  InlineCodePlacementResult,
} from "./sessionMarkdownInlineMeasurementCodePlacementShared";
import { acceptCodeFragmentState } from "./sessionMarkdownInlineMeasurementCodePlacementShared";

export function tryAcceptForcedFreshWholeCodeGroup(
  params: InlineCodePlacementParams,
): InlineCodePlacementResult | null {
  const state = { ...params.state };
  const { item, codeGroupId } = params;
  if (
    params.forcedFreshWholeCodeGroupIndex !== state.itemIndex ||
    !item.isFirstCodeGroupFragment ||
    params.wholeCodeGroupWidth <= 0 ||
    params.wholeCodeGroupWidth > params.maxWidth + 0.01
  ) {
    return null;
  }

  let scanIndex = state.itemIndex;
  let lastCodeFragmentText = item.text;
  while (scanIndex < params.items.length) {
    const candidate = params.items[scanIndex]!;
    if (candidate.kind === "hardBreak") {
      break;
    }
    if (candidate.kind === "space") {
      if (candidate.codeGroupId !== codeGroupId) {
        break;
      }
      if (params.debug.enabled) {
        params.debug.appendLineText(candidate.text);
      }
      scanIndex += 1;
      continue;
    }
    if (candidate.codeGroupId !== codeGroupId) {
      break;
    }
    if (params.debug.enabled) {
      params.debug.appendLineText(candidate.text);
    }
    lastCodeFragmentText = candidate.text;
    scanIndex += 1;
  }
  const remainingWidthBeforeWholeGroupAccept = state.remainingWidth;
  acceptCodeFragmentState({
    state,
    item,
    codeGroupId,
    remainingWidthBeforeAccept: remainingWidthBeforeWholeGroupAccept,
    remainingWidthAfterAccept: Math.max(0, state.remainingWidth - params.wholeCodeGroupWidth),
    lastFragmentText: lastCodeFragmentText,
    lastFragmentEndedWithHyphen: lastCodeFragmentText.endsWith("-"),
    lastFragmentEndedWithPathDelimiter: /[\\/]+$/.test(lastCodeFragmentText),
    shouldLimitCurrentCodeGroupToFirstFragment: params.shouldLimitCurrentCodeGroupToFirstFragment,
    shouldTrackWeakProseStartCodeGroup: false,
    shouldForceSoftBreakWeakProseContinuationWrap: false,
    maxWidth: params.maxWidth,
  });
  state.itemIndex = scanIndex;
  state.pendingSpaceWidth = 0;
  return {
    action: "continue",
    state,
    forcedFreshWholeCodeGroupIndex: null,
  };
}
