import { layoutNextLine, type LayoutLine } from "@chenglou/pretext";
import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import { slicePreparedTextBetweenCursors } from "./sessionMarkdownInlineMeasurementContext";
import { LINE_START_CURSOR, cursorsMatch } from "./sessionMarkdownMeasurementCore";
import {
  acceptCodeFragmentState,
  type InlineCodePlacementDebug,
  type InlineCodePlacementResult,
} from "./sessionMarkdownInlineMeasurementCodePlacementShared";
import type { InlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";

const INLINE_CODE_CURRENT_LINE_START_RATIO_THRESHOLD = 0.35;

export function placeSealedInlineCodeSegment(params: {
  state: InlineMeasurementLineState;
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  codeGroupId: number;
  maxWidth: number;
  reservedWidth: number;
  forcedFreshWholeCodeGroupIndex: number | null;
  currentLineFitSlackPx: number;
  currentLineWhitespaceContinuationGuardPx: number;
  currentLineWeakProseStartContinuationGuardPx: number;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldTrackWeakProseStartCodeGroup: boolean;
  shouldForceSoftBreakWeakProseContinuationWrap: boolean;
  debug: InlineCodePlacementDebug;
  fullWidth: number;
  guardedRemainingWidth: number;
  canRelaxChromiumDottedPathBoundary: boolean;
  effectiveSealedBoundaryOverflow: boolean;
  shouldBreakBeforePartialSealedDottedPathFragment: boolean;
  lastFragmentIsShortExtensionPath: boolean;
}): InlineCodePlacementResult | null {
  const {
    state,
    item,
    codeGroupId,
    fullWidth,
    guardedRemainingWidth,
    canRelaxChromiumDottedPathBoundary,
    effectiveSealedBoundaryOverflow,
    shouldBreakBeforePartialSealedDottedPathFragment,
    lastFragmentIsShortExtensionPath,
    shouldLimitCurrentCodeGroupToFirstFragment,
  } = params;

  if (item.isSealedInlineCodeFragment) {
    if (state.lineHasContent && fullWidth > guardedRemainingWidth + 0.01) {
      const shouldBreakBeforeEnginePartialSealedPathContinuation =
        !browserAllowsInlineCodeLeadingHang() &&
        (item.text.includes("/") || item.text.includes("\\") || item.isPathTailFragment);
      if (shouldBreakBeforeEnginePartialSealedPathContinuation) {
        state.cursor = null;
        return {
          action: "break",
          state,
          forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
        };
      }
      const sealedContinuationLine: LayoutLine | null =
        state.chargedCodeGroups.has(codeGroupId)
          ? layoutNextLine(
              item.prepared,
              LINE_START_CURSOR,
              Math.max(1, state.remainingWidth - params.reservedWidth - params.currentLineWhitespaceContinuationGuardPx),
            )
          : null;
      const sealedContinuationFitRatio =
        sealedContinuationLine != null && item.fullWidth > 0
          ? sealedContinuationLine.width / item.fullWidth
          : 0;
      const sealedContinuationWouldOverflowCurrentLine =
        sealedContinuationLine != null &&
        !cursorsMatch(LINE_START_CURSOR, sealedContinuationLine.end) &&
        params.reservedWidth + sealedContinuationLine.width > guardedRemainingWidth + 0.01;
      const shouldBreakForEngineSealedOverflow =
        sealedContinuationWouldOverflowCurrentLine &&
        !browserAllowsInlineCodeLeadingHang() &&
        (item.text.includes("/") || item.text.includes("\\") || item.isPathTailFragment);
      if (
        sealedContinuationLine != null &&
        !cursorsMatch(LINE_START_CURSOR, sealedContinuationLine.end) &&
        !shouldBreakForEngineSealedOverflow &&
        sealedContinuationFitRatio >= INLINE_CODE_CURRENT_LINE_START_RATIO_THRESHOLD
      ) {
        const sealedContinuationConsumedWholeItem = cursorsMatch(
          sealedContinuationLine.end,
          item.endCursor,
        );
        const remainingWidthBeforeSealedContinuationAccept = state.remainingWidth;
        acceptCodeFragmentState({
          state,
          item,
          codeGroupId,
          remainingWidthBeforeAccept: remainingWidthBeforeSealedContinuationAccept,
          remainingWidthAfterAccept: Math.max(
            0,
            state.remainingWidth - params.reservedWidth - sealedContinuationLine.width,
          ),
          lastFragmentText: item.text,
          lastFragmentEndedWithHyphen: false,
          lastFragmentEndedWithPathDelimiter: false,
          shouldLimitCurrentCodeGroupToFirstFragment,
          shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
          shouldForceSoftBreakWeakProseContinuationWrap: params.shouldForceSoftBreakWeakProseContinuationWrap,
          maxWidth: params.maxWidth,
        });
        state.pendingSpaceWidth = 0;
        if (params.debug.enabled) {
          params.debug.sealedContinuationDecisions.push({
            canRelaxChromiumDottedPathBoundary,
            currentCodeGroupStartFragmentText: state.lineCurrentCodeGroupStartFragmentText,
            fullWidth,
            guardedRemainingWidth,
            remainingWidth: remainingWidthBeforeSealedContinuationAccept,
            continuationSlackPx: params.currentLineFitSlackPx,
            lastFragmentIsShortExtensionPath,
            lastFragmentText: state.lineLastCodeFragmentText,
            sameCodeGroupContinuation: state.lineHasContent,
            sealedBoundaryOverflow: effectiveSealedBoundaryOverflow,
            acceptedFragment: true,
            overflowedAcceptedFragment: false,
            shouldBreakBeforePartialSealedDottedPathFragment,
            text: item.text,
          });
          const segmentText = slicePreparedTextBetweenCursors(
            item.prepared,
            LINE_START_CURSOR,
            sealedContinuationLine.end,
          );
          params.debug.appendLineText(segmentText);
        }
        if (sealedContinuationConsumedWholeItem) {
          state.itemIndex += 1;
          state.cursor = null;
          return {
            action: "continue",
            state,
            forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
          };
        }
        state.cursor = sealedContinuationLine.end;
        return {
          action: "break",
          state,
          forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
        };
      }
      state.cursor = null;
      return {
        action: "break",
        state,
        forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
      };
    }

    const overflowed = fullWidth > guardedRemainingWidth + 0.01;
    const remainingWidthBeforeSealedFragmentAccept = state.remainingWidth;
    acceptCodeFragmentState({
      state,
      item,
      codeGroupId,
      remainingWidthBeforeAccept: remainingWidthBeforeSealedFragmentAccept,
      remainingWidthAfterAccept: overflowed ? 0 : Math.max(0, state.remainingWidth - fullWidth),
      lastFragmentText: item.text,
      lastFragmentEndedWithHyphen: item.text.endsWith("-"),
      lastFragmentEndedWithPathDelimiter: /[\\/]+$/.test(item.text),
      shouldLimitCurrentCodeGroupToFirstFragment,
      shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
      shouldForceSoftBreakWeakProseContinuationWrap: params.shouldForceSoftBreakWeakProseContinuationWrap,
      maxWidth: params.maxWidth,
    });
    state.itemIndex += 1;
    state.pendingSpaceWidth = 0;
    if (params.debug.enabled) {
      params.debug.sealedContinuationDecisions.push({
        canRelaxChromiumDottedPathBoundary,
        currentCodeGroupStartFragmentText: state.lineCurrentCodeGroupStartFragmentText,
        fullWidth,
        guardedRemainingWidth,
        remainingWidth: remainingWidthBeforeSealedFragmentAccept,
        continuationSlackPx: params.currentLineFitSlackPx,
        lastFragmentIsShortExtensionPath,
        lastFragmentText: state.lineLastCodeFragmentText,
        sameCodeGroupContinuation: state.lineHasContent,
        sealedBoundaryOverflow: effectiveSealedBoundaryOverflow,
        acceptedFragment: true,
        overflowedAcceptedFragment: overflowed,
        shouldBreakBeforePartialSealedDottedPathFragment,
        text: item.text,
      });
      params.debug.appendLineText(item.text);
    }
    if (overflowed) {
      state.cursor = null;
      return {
        action: "break",
        state,
        forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
      };
    }
    return {
      action: "continue",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }


  return null;
}
