import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import {
  allowsChromiumDottedBoundaryHang,
  INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
  isShortExtensionPathLikeFragment,
  INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX,
  shouldBreakBeforePartialDottedCallContinuation,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  shouldBreakBeforePartialDottedStemPathTailContinuation,
  shouldBreakBeforePartialSealedDottedPathContinuation,
  shouldBreakBeforePathDelimiterNearFitContinuation,
  shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment,
} from "./sessionMarkdownInlineCodeFit";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import type {
  SessionMarkdownInlineCodeContinuationDecision,
  SessionMarkdownInlineCodeSealedContinuationDecision,
  SessionMarkdownInlineCodeWhitespaceDecision,
} from "./sessionMarkdownInlineMeasurementDebug";
import type { InlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";
import {
  type InlineCodePlacementParams,
  acceptCodeFragmentState,
  hasNonPunctuationTrailingTextAfterCodeGroup,
  type InlineCodePlacementDebug,
  type InlineCodePlacementResult,
} from "./sessionMarkdownInlineMeasurementCodePlacementShared";
import { acceptWholeInlineCodeFragment } from "./sessionMarkdownInlineCodePlacementAccept";
import { tryAcceptForcedFreshWholeCodeGroup } from "./sessionMarkdownInlineCodePlacementWholeGroup";
import { placeSealedInlineCodeSegment } from "./sessionMarkdownInlineMeasurementCodePlacementSealed";
export type {
  InlineCodePlacementParams,
  InlineCodePlacementResult,
} from "./sessionMarkdownInlineMeasurementCodePlacementShared";

const INLINE_CODE_HYPHEN_CONTINUATION_GUARD_PX = 2;

export function placeInlineCodeSegment(params: InlineCodePlacementParams): InlineCodePlacementResult {
  const state = { ...params.state };
  const { item, codeGroupId } = params;
  const shouldLimitCurrentCodeGroupToFirstFragment =
    params.shouldLimitCurrentCodeGroupToFirstFragment ||
    (browserAllowsInlineCodeLeadingHang() &&
      state.lineHasContent &&
      state.lineHasSoftHyphenText &&
      state.remainingWidth < params.maxWidth * 0.85 &&
      item.codeGroupStartsAfterText &&
      item.codeGroupHasDottedPath);

  const forcedWholeCodeGroupResult = tryAcceptForcedFreshWholeCodeGroup({
    ...params,
    state,
    shouldLimitCurrentCodeGroupToFirstFragment,
  });
  if (forcedWholeCodeGroupResult != null) {
    return forcedWholeCodeGroupResult;
  }

  const fullWidth = params.reservedWidth + item.fullWidth;
  const guardedRemainingWidth =
    state.remainingWidth -
    params.currentLineCodeStartSeamGuardPx -
    params.currentLineWhitespaceContinuationGuardPx -
    params.currentLineWeakProseStartContinuationGuardPx +
    params.currentLineFitSlackPx +
    params.currentLineNearFitLeadingHangPx;
  const hyphenContinuationGuardPx =
    !browserAllowsInlineCodeLeadingHang() &&
    state.lineLastCodeFragmentEndedWithHyphen &&
    !item.text.includes("/") &&
    !item.text.includes("\\") &&
    !item.isPathTailFragment
      ? INLINE_CODE_HYPHEN_CONTINUATION_GUARD_PX
      : 0;
  const shouldBreakBeforeHyphenTailContinuationWithTrailingPlain =
    !browserAllowsInlineCodeLeadingHang() &&
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId &&
    state.lineLastCodeFragmentEndedWithHyphen &&
    item.codeGroupHasTrailingText &&
    !item.text.includes("-") &&
    !item.text.includes("/") &&
    !item.text.includes("\\") &&
    !item.isPathTailFragment;
  const whitespaceSlackPx = resolveInlineCodeWhitespaceSeparatedFragmentSlackPx({
    lineHasContent: state.lineHasContent,
    startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
    fragmentText: item.text,
  });
  const shouldBreakBeforeWhitespaceSeparatedFragment =
    shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment({
      lineHasContent: state.lineHasContent,
      startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
      reservedWidth: params.reservedWidth,
      remainingWidth: guardedRemainingWidth,
      fragmentWidth: item.fullWidth,
      slackPx: whitespaceSlackPx,
    });
  const wholeFragmentFitTolerancePx = item.startsAfterCodeWhitespace ? whitespaceSlackPx : 0;

  if (params.debug.enabled && item.startsAfterCodeWhitespace) {
    params.debug.whitespaceDecisions.push({
      lineHasContent: state.lineHasContent,
      reservedWidth: params.reservedWidth,
      remainingWidth: state.remainingWidth,
      guardedRemainingWidth,
      fragmentWidth: item.fullWidth,
      slackPx: whitespaceSlackPx,
      shouldBreak: shouldBreakBeforeWhitespaceSeparatedFragment,
      text: item.text,
    });
  }
  if (shouldBreakBeforeWhitespaceSeparatedFragment) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (
    shouldBreakBeforePathDelimiterNearFitContinuation({
      lineHasContent: state.lineHasContent,
      sameCodeGroupContinuation: state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId,
      lastFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
      startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
      isSealedInlineCodeFragment: item.isSealedInlineCodeFragment,
      reservedWidth: params.reservedWidth,
      remainingWidth: state.remainingWidth,
      fragmentWidth: item.fullWidth,
    })
  ) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (
    state.lineHasContent &&
    state.lineCurrentCodeGroupLimitToFirstFragment &&
    state.lastAcceptedCodeGroupId === codeGroupId &&
    state.lineCurrentCodeGroupStartFragmentText != null &&
    state.lineCurrentCodeGroupStartFragmentText === state.lineLastCodeFragmentText
  ) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (
    state.lineHasContent &&
    state.lineForceSoftBreakWeakProseContinuationCodeGroupId === codeGroupId &&
    state.lastAcceptedCodeGroupId === codeGroupId
  ) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  const sealedBoundary =
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId &&
    (state.lineLastCodeFragmentEndedWithHyphen || state.lineLastCodeFragmentEndedWithPathDelimiter);
  const boundaryRemainingWidth =
    state.remainingWidth +
    params.currentLineFitSlackPx -
    hyphenContinuationGuardPx -
    params.currentLineWeakProseStartContinuationGuardPx;
  const shouldAcceptChromiumPathTailContinuation =
    browserAllowsInlineCodeLeadingHang() &&
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId &&
    state.lineLastCodeFragmentEndedWithPathDelimiter &&
    !item.isSealedInlineCodeFragment &&
    !item.startsAfterCodeWhitespace &&
    params.reservedWidth + item.fullWidth + INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX <=
      boundaryRemainingWidth + 0.01;
  const nextSameCodeGroupItem = params.items[state.itemIndex + 1];
  const splitDottedStemTailWouldOverflowCurrentLine =
    state.lineCurrentCodeGroupStartedNearFresh &&
    state.lineHasContent &&
    state.lastAcceptedCodeGroupId === codeGroupId &&
    state.lineLastCodeFragmentEndedWithPathDelimiter &&
    item.codeGroupStartsAfterText &&
    item.codeGroupHasTrailingText &&
    state.lineCurrentCodeGroupStartFragmentText != null &&
    !state.lineCurrentCodeGroupStartFragmentText.includes(".") &&
    item.text.endsWith(".") &&
    nextSameCodeGroupItem?.kind === "segment" &&
    nextSameCodeGroupItem.codeGroupId === codeGroupId &&
    (nextSameCodeGroupItem.text.includes("/") ||
      nextSameCodeGroupItem.text.includes("\\") ||
      nextSameCodeGroupItem.isPathTailFragment) &&
    params.reservedWidth + item.fullWidth + nextSameCodeGroupItem.fullWidth > boundaryRemainingWidth + 0.01;
  if (splitDottedStemTailWouldOverflowCurrentLine) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  const sealedBoundaryOverflow =
    sealedBoundary && params.reservedWidth + item.fullWidth > boundaryRemainingWidth + 0.01;
  const shouldBreakBeforePartialSealedDottedPathFragment =
    shouldBreakBeforePartialSealedDottedPathContinuation({
      currentCodeGroupStartFragmentText: state.lineCurrentCodeGroupStartFragmentText,
      fullWidth,
      guardedRemainingWidth,
      item,
      lastFragmentText: state.lineLastCodeFragmentText,
      sameCodeGroupContinuation: state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId,
    });
  const shouldBreakBeforePartialDottedStemPathTailFragment =
    shouldBreakBeforePartialDottedStemPathTailContinuation({
      fullWidth,
      guardedRemainingWidth,
      item,
      lastFragmentText: state.lineLastCodeFragmentText,
      sameCodeGroupContinuation: state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId,
    });
  const shouldBreakBeforePartialDottedCallFragment =
    shouldBreakBeforePartialDottedCallContinuation({
      fragmentWidth: item.fullWidth,
      fullWidth,
      guardedRemainingWidth,
      item,
      lastFragmentText: state.lineLastCodeFragmentText,
      maxWidth: params.maxWidth,
      minSparePx:
        !browserAllowsInlineCodeLeadingHang() &&
        item.codeGroupStartsAfterText &&
        item.codeGroupHasTrailingText &&
        hasNonPunctuationTrailingTextAfterCodeGroup({
          codeGroupId,
          itemIndex: state.itemIndex,
          items: params.items,
        })
          ? INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX
          : 0,
      sameCodeGroupContinuation: state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId,
    });
  const lastFragmentIsShortExtensionPath = isShortExtensionPathLikeFragment(state.lineLastCodeFragmentText);
  const canRelaxChromiumDottedPathBoundary =
    sealedBoundaryOverflow &&
    !state.lineUsedChromiumDottedPathBoundaryContinuation &&
    browserAllowsInlineCodeLeadingHang() &&
    item.codeGroupStartsAfterText &&
    state.lineCurrentCodeGroupStartFragmentText != null &&
    !state.lineCurrentCodeGroupStartFragmentText.includes(".") &&
    !lastFragmentIsShortExtensionPath &&
    item.codeGroupHasDottedPath &&
    !item.text.includes("/") &&
    !item.text.includes("\\") &&
    (item.text.includes(".") || item.isPathTailFragment) &&
    allowsChromiumDottedBoundaryHang({
      boundaryRemainingWidth,
      chromeWidth: item.chromeWidth,
      fullWidth,
    });
  const effectiveSealedBoundaryOverflow =
    sealedBoundaryOverflow && !shouldAcceptChromiumPathTailContinuation;

  if (shouldBreakBeforePartialDottedStemPathTailFragment) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (shouldBreakBeforePartialDottedCallFragment) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  if (params.debug.enabled && state.lineHasContent && item.isSealedInlineCodeFragment && state.lastAcceptedCodeGroupId === codeGroupId) {
    params.debug.sealedContinuationDecisions.push({
      canRelaxChromiumDottedPathBoundary,
      currentCodeGroupStartFragmentText: state.lineCurrentCodeGroupStartFragmentText,
      fullWidth,
      guardedRemainingWidth,
      remainingWidth: state.remainingWidth,
      continuationSlackPx: params.currentLineFitSlackPx,
      lastFragmentIsShortExtensionPath,
      lastFragmentText: state.lineLastCodeFragmentText,
      sameCodeGroupContinuation: state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId,
      sealedBoundaryOverflow: effectiveSealedBoundaryOverflow,
      shouldBreakBeforePartialSealedDottedPathFragment,
      text: item.text,
    });
  }
  if (effectiveSealedBoundaryOverflow && !canRelaxChromiumDottedPathBoundary) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (shouldBreakBeforePartialSealedDottedPathFragment) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }
  if (canRelaxChromiumDottedPathBoundary) {
    state.lineUsedChromiumDottedPathBoundaryContinuation = true;
  }
  if (item.isSealedInlineCodeFragment && canRelaxChromiumDottedPathBoundary) {
    const remainingWidthBeforeSealedFragmentAccept = state.remainingWidth;
    acceptCodeFragmentState({
      state,
      item,
      codeGroupId,
      remainingWidthBeforeAccept: remainingWidthBeforeSealedFragmentAccept,
      remainingWidthAfterAccept: Math.max(0, state.remainingWidth - fullWidth),
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
        overflowedAcceptedFragment: fullWidth > guardedRemainingWidth + 0.01,
        shouldBreakBeforePartialSealedDottedPathFragment,
        text: item.text,
      });
      params.debug.appendLineText(item.text);
    }
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  if (shouldAcceptChromiumPathTailContinuation) {
    return acceptWholeInlineCodeFragment({
      placement: params,
      state,
      fullWidth: params.reservedWidth + item.fullWidth,
      guardedRemainingWidth,
      shouldLimitCurrentCodeGroupToFirstFragment,
      shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
      shouldForceSoftBreakWeakProseContinuationWrap: params.shouldForceSoftBreakWeakProseContinuationWrap,
    });
  }

  const sealedPlacementResult = placeSealedInlineCodeSegment({
    state,
    item,
    codeGroupId,
    maxWidth: params.maxWidth,
    reservedWidth: params.reservedWidth,
    forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    currentLineFitSlackPx: params.currentLineFitSlackPx,
    currentLineWhitespaceContinuationGuardPx: params.currentLineWhitespaceContinuationGuardPx,
    currentLineWeakProseStartContinuationGuardPx: params.currentLineWeakProseStartContinuationGuardPx,
    shouldLimitCurrentCodeGroupToFirstFragment,
    shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
    shouldForceSoftBreakWeakProseContinuationWrap: params.shouldForceSoftBreakWeakProseContinuationWrap,
    debug: params.debug,
    fullWidth,
    guardedRemainingWidth,
    canRelaxChromiumDottedPathBoundary,
    effectiveSealedBoundaryOverflow,
    shouldBreakBeforePartialSealedDottedPathFragment,
    lastFragmentIsShortExtensionPath,
  });
  if (sealedPlacementResult != null) {
    return sealedPlacementResult;
  }

  if (fullWidth <= guardedRemainingWidth + wholeFragmentFitTolerancePx + 0.01) {
    return acceptWholeInlineCodeFragment({
      placement: params,
      state,
      fullWidth,
      guardedRemainingWidth,
      shouldLimitCurrentCodeGroupToFirstFragment,
      shouldTrackWeakProseStartCodeGroup: params.shouldTrackWeakProseStartCodeGroup,
      shouldForceSoftBreakWeakProseContinuationWrap: params.shouldForceSoftBreakWeakProseContinuationWrap,
    });
  }

  const shouldBreakBeforePartialFreshLineFittingCodeGroup =
    state.lineHasContent &&
    state.pendingSpaceWidth > 0 &&
    item.isFirstCodeGroupFragment &&
    item.codeGroupStartsAfterText &&
    !state.chargedCodeGroups.has(codeGroupId) &&
    params.wholeCodeGroupWidth > 0 &&
    params.wholeCodeGroupWidth <= params.maxWidth + 0.01;

  if (shouldBreakBeforePartialFreshLineFittingCodeGroup) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  if (params.debug.enabled && state.lineHasContent && state.lastAcceptedCodeGroupId === codeGroupId) {
    params.debug.continuationDecisions.push({
      text: item.text,
      reservedWidth: params.reservedWidth,
      remainingWidth: state.remainingWidth,
      guardedRemainingWidth,
      fullWidth,
      currentLineFitSlackPx: params.currentLineFitSlackPx,
      lineLastCodeFragmentText: state.lineLastCodeFragmentText,
      lineLastCodeFragmentEndedWithPathDelimiter: state.lineLastCodeFragmentEndedWithPathDelimiter,
      lineLastCodeFragmentEndedWithHyphen: state.lineLastCodeFragmentEndedWithHyphen,
      brokeBeforeFragment: item.codePartStartsAfterWhitespace && item.text.endsWith("-"),
    });
  }
  if (
    shouldBreakBeforeHyphenTailContinuationWithTrailingPlain ||
    state.lineHasContent &&
    ((!item.codePartStartsAfterWhitespace && item.text.endsWith("-")) ||
      (!item.codePartStartsAfterWhitespace &&
        item.isSealedInlineCodeFragment &&
        (item.text.includes("/") || item.text.includes("\\"))))
  ) {
    state.cursor = null;
    return {
      action: "break",
      state,
      forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
    };
  }

  return {
    action: "pass",
    state,
    forcedFreshWholeCodeGroupIndex: params.forcedFreshWholeCodeGroupIndex,
  };
}
