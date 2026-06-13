import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import type { InlineCodeBoundaryFit, InlineCodeTrailingPlainInfo } from "./sessionMarkdownInlineCodeFit";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import {
  containsStrongRtlText,
  isPunctuationOnlySeamText,
} from "./sessionTextTokenClassifier";

const INLINE_CODE_NEAR_WHOLE_GROUP_FRESH_LINE_THRESHOLD_PX = 0.5;
const INLINE_CODE_SOFT_BREAK_PROSE_CODE_START_RATIO_THRESHOLD = 0.8;
const INLINE_CODE_FIRST_SLICE_ONLY_MIN_WIDTH_PX = 90;
const INLINE_CODE_FIRST_SLICE_ONLY_START_RATIO_THRESHOLD = 0.4;
const INLINE_CODE_FIRST_SLICE_ONLY_WITH_FOLLOWING_INLINE_CODE_START_RATIO_THRESHOLD = 0.55;
const INLINE_CODE_WEBKIT_WEAK_FIRST_SLICE_FRESH_LINE_START_RATIO_THRESHOLD = 0.3;

type InlineSegmentItem = Extract<PreparedInlineLayoutItem, { kind: "segment" }>;

export type InlineCodeStartDecision = {
  currentLineStartFitRatio: number;
  currentLineCodeStartFitIsReadable: boolean;
  currentLineCodeStartFitIsStrong: boolean;
  shouldTrackWeakProseStartCodeGroup: boolean;
  shouldForceSoftBreakWeakProseContinuationWrap: boolean;
  shouldBreakForPreferredStart: boolean;
  shouldBreakForSoftBreakProseCodeStart: boolean;
  shouldBreakForInlineTailPunctuationCodeStart: boolean;
  shouldBreakForStyledTailCodeStart: boolean;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldAllowChromiumPartialDottedPathStart: boolean;
  shouldAllowChromiumAttachedTrailingPlainPartialFit: boolean;
  shouldAllowEnginePathLikeTrailingPlainWrap: boolean;
  shouldBreakForAttachedTrailingPlainStart: boolean;
  shouldBreakForWeakFirstSliceFreshLineStart: boolean;
};

export function resolveInlineCodeStartDecision(params: {
  codeGroupId: number | null;
  currentLineStartFragmentWidth: number;
  lineAcceptedPlainAfterContinuedCode: boolean;
  lineAcceptedSoftBreakProseAfterInlineCode: boolean;
  lineHasContent: boolean;
  lineSawInlineCode: boolean;
  lineSoftBreakProseGuardCodeGroupId: number | null;
  lineTailAfterInlineCodeIsPunctuationOnly: boolean;
  item: InlineSegmentItem;
  maxWidth: number;
  preferredStartWidth: number;
  remainingWidth: number;
  trailingPlainAfterCodeGroupStart: InlineCodeTrailingPlainInfo;
  wholeCodeGroupWidth: number;
  currentLineCodeFit: InlineCodeBoundaryFit | null;
  freshLineCodeFit: InlineCodeBoundaryFit | null;
  codeFitEndsAtFriendlyBoundary: boolean;
  freshLineFitEndsAtFriendlyBoundary: boolean;
}): InlineCodeStartDecision {
  const allowsInlineCodeLeadingHang = browserAllowsInlineCodeLeadingHang();
  const softBreakProseAppliesToCurrentCodeGroup =
    params.codeGroupId != null &&
    params.lineAcceptedSoftBreakProseAfterInlineCode &&
    (params.lineSoftBreakProseGuardCodeGroupId == null ||
      params.lineSoftBreakProseGuardCodeGroupId === params.codeGroupId);
  const currentLineStartFitRatio =
    params.currentLineCodeFit != null &&
    params.freshLineCodeFit != null &&
    params.freshLineCodeFit.consumedWidth > 0
      ? params.currentLineCodeFit.consumedWidth / params.freshLineCodeFit.consumedWidth
      : 1;
  const currentLineCodeStartFitIsReadable =
    params.currentLineCodeFit != null &&
    params.currentLineCodeFit.consumedWidth + 0.01 >= params.item.minStartTextWidth;
  const currentLineCodeStartFitIsStrong =
    params.currentLineCodeFit != null &&
    currentLineStartFitRatio >= 0.45 &&
    (params.codeFitEndsAtFriendlyBoundary || params.freshLineFitEndsAtFriendlyBoundary);
  const currentLineCodeStartFitIsFriendlyReadable =
    params.currentLineCodeFit != null &&
    currentLineCodeStartFitIsReadable &&
    params.codeFitEndsAtFriendlyBoundary;
  const effectiveTrailingPlainWidthAfterCodeGroupStart =
    params.trailingPlainAfterCodeGroupStart.startsAfterCollapsedSoftBreak
      ? 0
      : params.trailingPlainAfterCodeGroupStart.width;
  const effectiveTrailingPlainHasFollowingInlineCode =
    params.trailingPlainAfterCodeGroupStart.startsAfterCollapsedSoftBreak
      ? false
      : params.trailingPlainAfterCodeGroupStart.hasFollowingInlineCode;
  const trailingPlainIsOrdinaryProse =
    effectiveTrailingPlainWidthAfterCodeGroupStart > 0 &&
    !effectiveTrailingPlainHasFollowingInlineCode &&
    !isPunctuationOnlySeamText(params.trailingPlainAfterCodeGroupStart.text);
  const trailingPlainHasStrongRtlText = containsStrongRtlText(
    params.trailingPlainAfterCodeGroupStart.text,
  );
  const shouldAllowChromiumAttachedTrailingPlainPartialFit =
    allowsInlineCodeLeadingHang &&
    currentLineCodeStartFitIsFriendlyReadable &&
    params.currentLineCodeFit != null &&
    !params.currentLineCodeFit.endedAtGroupEnd;
  const shouldTrackWeakProseStartCodeGroup =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.codeGroupStartsAfterText &&
    !params.item.codeGroupHasWhitespace &&
    params.currentLineCodeFit != null &&
    ((!params.currentLineCodeFit.endedInsideFragment &&
      Math.abs(params.currentLineCodeFit.consumedWidth - params.currentLineStartFragmentWidth) <= 0.5 &&
      currentLineStartFitRatio < 0.3) ||
      (params.currentLineCodeFit.endedInsideFragment && !params.codeFitEndsAtFriendlyBoundary) ||
      (softBreakProseAppliesToCurrentCodeGroup &&
        currentLineStartFitRatio < INLINE_CODE_SOFT_BREAK_PROSE_CODE_START_RATIO_THRESHOLD) ||
      (params.item.prefersFreshLineStart &&
        params.item.codeGroupHasTrailingText &&
        params.currentLineCodeFit.consumedWidth + 0.01 < params.preferredStartWidth &&
        currentLineStartFitRatio < 0.75));
  const shouldForceSoftBreakWeakProseContinuationWrap =
    softBreakProseAppliesToCurrentCodeGroup &&
    currentLineStartFitRatio < INLINE_CODE_SOFT_BREAK_PROSE_CODE_START_RATIO_THRESHOLD;
  const shouldBreakForPreferredStart =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    !params.lineSawInlineCode &&
    !params.item.startsAfterCollapsedSoftBreak &&
    !params.lineAcceptedPlainAfterContinuedCode &&
    params.item.prefersFreshLineStart &&
    params.item.codeGroupStartsAfterText &&
    !params.item.codeGroupStartsAfterStyledTextSeam &&
    !params.item.codeGroupHasTrailingText &&
    !params.item.codeGroupHasWhitespace &&
    currentLineCodeStartFitIsStrong &&
    params.wholeCodeGroupWidth > params.remainingWidth + 0.01 &&
    params.wholeCodeGroupWidth <= params.maxWidth + 0.01 &&
    params.wholeCodeGroupWidth - params.remainingWidth <= INLINE_CODE_NEAR_WHOLE_GROUP_FRESH_LINE_THRESHOLD_PX;
  const shouldBreakForSoftBreakProseCodeStart =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    softBreakProseAppliesToCurrentCodeGroup &&
    params.preferredStartWidth > 0 &&
    params.currentLineCodeFit != null &&
    params.currentLineCodeFit.consumedWidth + 0.01 < params.preferredStartWidth &&
    currentLineStartFitRatio < INLINE_CODE_SOFT_BREAK_PROSE_CODE_START_RATIO_THRESHOLD;
  const shouldBreakForInlineTailPunctuationCodeStart =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.codeGroupStartsAfterText &&
    !(allowsInlineCodeLeadingHang && params.item.codeGroupHasWhitespace) &&
    !params.item.codeGroupHasTrailingText &&
    params.lineTailAfterInlineCodeIsPunctuationOnly &&
    params.preferredStartWidth > params.remainingWidth + 0.01 &&
    params.preferredStartWidth <= params.maxWidth + 0.01 &&
    !currentLineCodeStartFitIsStrong;
  const shouldBreakForStyledTailCodeStart =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.codeGroupStartsAfterStyledTextSeam &&
    params.preferredStartWidth > params.remainingWidth + 0.01 &&
    params.preferredStartWidth <= params.maxWidth + 0.01 &&
    !currentLineCodeStartFitIsReadable;
  const shouldLimitCurrentCodeGroupToFirstFragment =
    (!allowsInlineCodeLeadingHang &&
      params.lineHasContent &&
      params.codeGroupId != null &&
      params.item.isFirstCodeGroupFragment &&
      params.item.prefersFreshLineStart &&
      params.item.codeGroupStartsAfterText &&
      params.item.codeGroupHasTrailingText &&
      (params.item.codeGroupHasDottedPath ||
        params.item.text.includes("/") ||
        params.item.text.includes("\\") ||
        params.item.isPathTailFragment) &&
      !trailingPlainIsOrdinaryProse &&
      !params.item.codeGroupHasWhitespace &&
      params.item.fullWidth >= INLINE_CODE_FIRST_SLICE_ONLY_MIN_WIDTH_PX &&
      params.currentLineCodeFit != null &&
      params.currentLineCodeFit.consumedWidth + 0.01 < params.preferredStartWidth &&
      currentLineStartFitRatio <
        (effectiveTrailingPlainHasFollowingInlineCode
          ? INLINE_CODE_FIRST_SLICE_ONLY_WITH_FOLLOWING_INLINE_CODE_START_RATIO_THRESHOLD
          : INLINE_CODE_FIRST_SLICE_ONLY_START_RATIO_THRESHOLD)) ||
    (params.lineHasContent &&
      params.codeGroupId != null &&
      params.item.isFirstCodeGroupFragment &&
      params.item.codeGroupStartsAfterText &&
      params.item.codeGroupHasTrailingText &&
      !trailingPlainIsOrdinaryProse &&
      !params.item.codeGroupHasWhitespace &&
      !effectiveTrailingPlainHasFollowingInlineCode &&
      trailingPlainHasStrongRtlText &&
      currentLineCodeStartFitIsReadable &&
      params.currentLineCodeFit != null &&
      !params.currentLineCodeFit.endedInsideFragment &&
      params.currentLineCodeFit.consumedWidth + 0.01 < params.preferredStartWidth);
  const shouldAllowChromiumPartialDottedPathStart =
    allowsInlineCodeLeadingHang &&
    (params.currentLineCodeFit == null || params.currentLineCodeFit.consumedWidth <= 0.01) &&
    !params.item.codeGroupHasWhitespace &&
    params.item.codeGroupHasDottedPath &&
    (params.item.text.includes(".") || params.item.isPathTailFragment);
  const shouldAllowEnginePathLikeTrailingPlainWrap =
    !allowsInlineCodeLeadingHang &&
    !params.item.codeGroupHasWhitespace &&
    (params.item.codeGroupHasDottedPath ||
      params.item.text.includes("/") ||
      params.item.text.includes("\\") ||
      params.item.isPathTailFragment);
  const shouldBreakForAttachedTrailingPlainStart =
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    effectiveTrailingPlainWidthAfterCodeGroupStart > 0 &&
    !effectiveTrailingPlainHasFollowingInlineCode &&
    params.item.prefersFreshLineStart &&
    !params.trailingPlainAfterCodeGroupStart.isDecoratedText &&
    !isPunctuationOnlySeamText(params.trailingPlainAfterCodeGroupStart.text) &&
    params.preferredStartWidth + effectiveTrailingPlainWidthAfterCodeGroupStart >
      params.remainingWidth + 0.01 &&
    !shouldAllowEnginePathLikeTrailingPlainWrap &&
    !shouldAllowChromiumPartialDottedPathStart &&
    !shouldAllowChromiumAttachedTrailingPlainPartialFit &&
    (!currentLineCodeStartFitIsStrong ||
      (!allowsInlineCodeLeadingHang &&
        params.currentLineCodeFit != null &&
        params.currentLineCodeFit.endedAtGroupEnd &&
        params.item.codeGroupHasDottedPath &&
        (params.item.text.includes("/") || params.item.text.includes("\\") || params.item.isPathTailFragment)));
  const shouldBreakForWeakFirstSliceFreshLineStart =
    !allowsInlineCodeLeadingHang &&
    params.lineHasContent &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.prefersFreshLineStart &&
    params.item.codeGroupStartsAfterText &&
    params.item.codeGroupHasTrailingText &&
    !params.item.codeGroupHasWhitespace &&
    params.wholeCodeGroupWidth > 0 &&
    params.wholeCodeGroupWidth <= params.maxWidth + 0.01 &&
    params.currentLineCodeFit != null &&
    params.currentLineCodeFit.consumedWidth > 0.01 &&
    !shouldAllowEnginePathLikeTrailingPlainWrap &&
    shouldLimitCurrentCodeGroupToFirstFragment &&
    currentLineStartFitRatio < INLINE_CODE_WEBKIT_WEAK_FIRST_SLICE_FRESH_LINE_START_RATIO_THRESHOLD;

  return {
    currentLineStartFitRatio,
    currentLineCodeStartFitIsReadable,
    currentLineCodeStartFitIsStrong,
    shouldTrackWeakProseStartCodeGroup,
    shouldForceSoftBreakWeakProseContinuationWrap,
    shouldBreakForPreferredStart,
    shouldBreakForSoftBreakProseCodeStart,
    shouldBreakForInlineTailPunctuationCodeStart,
    shouldBreakForStyledTailCodeStart,
    shouldLimitCurrentCodeGroupToFirstFragment,
    shouldAllowChromiumPartialDottedPathStart,
    shouldAllowChromiumAttachedTrailingPlainPartialFit,
    shouldAllowEnginePathLikeTrailingPlainWrap,
    shouldBreakForAttachedTrailingPlainStart,
    shouldBreakForWeakFirstSliceFreshLineStart,
  };
}
