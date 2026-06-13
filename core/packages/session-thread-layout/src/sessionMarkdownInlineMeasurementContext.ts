import type { LayoutCursor, PreparedTextWithSegments } from "@chenglou/pretext";
import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import {
  createInlineCodeFitPlanner,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeProseStartSeamGuardPx,
  shouldApplyInlineCodeSoftBreakTextStartGuard,
} from "./sessionMarkdownInlineCodeFit";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import { resolveInlineCodeStartDecision } from "./sessionMarkdownInlineStartDecisions";
import { segmentGraphemes } from "./sessionMarkdownMeasurementCore";
import { isPunctuationOnlySeamText } from "./sessionTextTokenClassifier";
import { SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX } from "./sessionThreadLayoutTokens";

export { isPunctuationOnlySeamText } from "./sessionTextTokenClassifier";

const INLINE_CODE_FRAGMENT_FIT_SLACK_PX = 0;
const INLINE_CODE_WHOLE_GROUP_FIT_SLACK_PX = 0;
const INLINE_CODE_WHITESPACE_CONTINUATION_GUARD_PX = 1;
const INLINE_CODE_WEAK_PROSE_START_CONTINUATION_GUARD_PX = 1;
export const INLINE_CODE_TAIL_TEXT_SEAM_GUARD_PX = 4;
export const INLINE_CODE_CONTINUED_TAIL_TEXT_SEAM_GUARD_PX =
  SESSION_THREAD_MARKDOWN_INLINE_CODE_FRAGMENT_CHROME_WIDTH_PX / 2;
const INLINE_CODE_SOFT_BREAK_TEXT_START_GUARD_RATIO = 1;
const INLINE_CODE_SOFT_BREAK_TEXT_START_GUARD_MAX_PX = 36;

export const STYLED_TEXT_SEAM_GUARD_PX = 0;
export const STYLED_TEXT_BODY_START_GUARD_PX = 4;
export const STYLED_TEXT_BODY_START_CURRENT_LINE_RATIO_THRESHOLD = 0.2;
export const STYLED_TEXT_AFTER_INLINE_CODE_CLUSTER_GUARD_PX = 2;

export function shouldDropLeadingCollapsedSpaceAtWrap(params: {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  codeGroupId: number | null;
  lineHasContent: boolean;
  cursor: LayoutCursor | null;
  pendingSpaceWidth: number;
}): boolean {
  return (
    params.codeGroupId == null &&
    params.lineHasContent &&
    params.cursor === null &&
    params.pendingSpaceWidth > 0 &&
    !params.item.startsAfterInlineCodeSeam &&
    !params.item.startsAfterStyledTextSeam &&
    !params.item.startsStyledTextAfterInlineCodeSeam &&
    !params.item.startsStyledTextAfterBodySeam
  );
}

export function isAtomicNonCodeTextSegment(text: string): boolean {
  const trimmed = text.trim();
  return trimmed.length > 0 && !/\s/.test(trimmed) && !isPunctuationOnlySeamText(trimmed);
}

export function isWhitespaceOnlyLineSlice(text: string): boolean {
  return text.trim().length === 0;
}

export function resolveInlineCodeSoftBreakTextStartGuardPx(minStartTextWidth: number): number {
  return Math.min(
    INLINE_CODE_SOFT_BREAK_TEXT_START_GUARD_MAX_PX,
    Math.max(
      INLINE_CODE_TAIL_TEXT_SEAM_GUARD_PX,
      minStartTextWidth * INLINE_CODE_SOFT_BREAK_TEXT_START_GUARD_RATIO,
    ),
  );
}

export function advancePreparedCursorOneGrapheme(
  prepared: PreparedTextWithSegments,
  cursor: LayoutCursor,
): LayoutCursor | null {
  const segment = prepared.segments[cursor.segmentIndex];
  if (segment == null) {
    return null;
  }
  const graphemeCount = segmentGraphemes(segment).length;
  if (cursor.graphemeIndex + 1 < graphemeCount) {
    return {
      segmentIndex: cursor.segmentIndex,
      graphemeIndex: cursor.graphemeIndex + 1,
    };
  }
  const nextSegmentIndex = cursor.segmentIndex + 1;
  if (nextSegmentIndex < prepared.segments.length) {
    return {
      segmentIndex: nextSegmentIndex,
      graphemeIndex: 0,
    };
  }
  return null;
}

export function slicePreparedTextBetweenCursors(
  prepared: PreparedTextWithSegments,
  start: LayoutCursor,
  end: LayoutCursor,
): string {
  if (
    end.segmentIndex < start.segmentIndex ||
    (end.segmentIndex === start.segmentIndex && end.graphemeIndex <= start.graphemeIndex)
  ) {
    return "";
  }
  const parts: string[] = [];
  for (let segmentIndex = start.segmentIndex; segmentIndex <= end.segmentIndex; segmentIndex += 1) {
    const segment = prepared.segments[segmentIndex] ?? "";
    const graphemes = segmentGraphemes(segment);
    const startGraphemeIndex = segmentIndex === start.segmentIndex ? start.graphemeIndex : 0;
    const endGraphemeIndex = segmentIndex === end.segmentIndex ? end.graphemeIndex : graphemes.length;
    if (endGraphemeIndex > startGraphemeIndex) {
      parts.push(graphemes.slice(startGraphemeIndex, endGraphemeIndex).join(""));
    }
  }
  return parts.join("");
}

type InlineCodeFitPlanner = ReturnType<typeof createInlineCodeFitPlanner>;

export type InlineMeasurementDerivedContext = {
  currentLineCodeStartSeamGuardPx: number;
  currentLineFitSlackPx: number;
  currentLineWhitespaceContinuationGuardPx: number;
  currentLineWeakProseStartContinuationGuardPx: number;
  currentLineNearFitLeadingHangPx: number;
  sliceStartsAtItemStart: boolean;
  currentLineStyledSeamGuardPx: number;
  currentLineStyledAfterInlineCodeClusterGuardPx: number;
  currentLineInlineCodeTailTextSeamGuardPx: number;
  currentLineInlineCodeSoftBreakTextStartGuardPx: number;
  currentLineStyledBodyStartGuardPx: number;
  canDropLeadingCollapsedSpaceAtWrap: boolean;
  startCursor: LayoutCursor;
  trailingPlainWidthAfterCodeGroupStart: number;
  trailingPlainAfterCodeGroupStart: ReturnType<InlineCodeFitPlanner["measureCodeGroupTrailingPlainInfo"]>;
  currentLineCodeFit: ReturnType<InlineCodeFitPlanner["measureCodeGroupFitWithinWidth"]> | null;
  codeFitEndsAtFriendlyBoundary: boolean;
  freshLineCodeFit: ReturnType<InlineCodeFitPlanner["measureCodeGroupFitWithinWidth"]> | null;
  effectiveTrailingPlainWidthAfterCodeGroupStart: number;
  effectiveTrailingPlainHasFollowingInlineCode: boolean;
  freshLineFitEndsAtFriendlyBoundary: boolean;
  prefersFreshLineStart: boolean;
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

export function resolveInlineMeasurementDerivedContext(params: {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  itemIndex: number;
  codeGroupId: number | null;
  preferredStartWidth: number;
  wholeCodeGroupWidth: number;
  lineHasContent: boolean;
  lineSawInlineCode: boolean;
  lineAcceptedPlainAfterContinuedCode: boolean;
  lineStartedWithContinuedCode: boolean;
  lineAcceptedSoftBreakProseAfterInlineCode: boolean;
  lineSoftBreakProseAfterInlineCodeGuardPx: number;
  lineSoftBreakProseGuardCodeGroupId: number | null;
  lineTailAfterInlineCodeIsPunctuationOnly: boolean;
  lineWeakProseStartCodeGroupId: number | null;
  lineDecoratedTextSegmentCount: number;
  lastAcceptedCodeGroupId: number | null;
  lineLastCodeFragmentText: string | null;
  lineLastCodeFragmentEndedWithHyphen: boolean;
  lineLastCodeFragmentEndedWithPathDelimiter: boolean;
  remainingWidth: number;
  pendingSpaceWidth: number;
  maxWidth: number;
  cursor: LayoutCursor | null;
  allowLeadingHangForCurrentLine: boolean;
  allowCurrentLineLeadingHang: boolean;
  chargedCodeGroups: ReadonlySet<number>;
  chromeWidth: number;
  measureCodeGroupTrailingPlainWidth: InlineCodeFitPlanner["measureCodeGroupTrailingPlainWidth"];
  measureCodeGroupTrailingPlainInfo: InlineCodeFitPlanner["measureCodeGroupTrailingPlainInfo"];
  measureCodeGroupFitWithinWidth: InlineCodeFitPlanner["measureCodeGroupFitWithinWidth"];
  codeGroupFitEndsAtFriendlyBoundary: InlineCodeFitPlanner["codeGroupFitEndsAtFriendlyBoundary"];
}): InlineMeasurementDerivedContext {
  const currentLineStartSlackPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.codeGroupStartsAfterText
      ? INLINE_CODE_WHOLE_GROUP_FIT_SLACK_PX
      : INLINE_CODE_FRAGMENT_FIT_SLACK_PX;
  const currentLineWhitespaceContinuationSlackPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.item.startsAfterCodeWhitespace &&
    params.item.codeGroupStartsAfterText &&
    params.chargedCodeGroups.has(params.codeGroupId)
      ? INLINE_CODE_WHOLE_GROUP_FIT_SLACK_PX
      : 0;
  const currentLineCodeContinuationSlackPx = resolveInlineCodeContinuationFitSlackPx({
    lineHasContent: params.lineHasContent,
    atLineBreakBoundary: params.cursor === null,
    sameCodeGroupContinuation:
      params.codeGroupId != null && params.lastAcceptedCodeGroupId === params.codeGroupId,
    startsAfterCodeWhitespace: params.item.startsAfterCodeWhitespace,
    lastFragmentEndedWithDot:
      params.lineLastCodeFragmentText?.endsWith(".") ?? false,
    lastFragmentEndedWithHyphen: params.lineLastCodeFragmentEndedWithHyphen,
    lastFragmentEndedWithPathDelimiter: params.lineLastCodeFragmentEndedWithPathDelimiter,
    item: params.item,
  });
  const currentLineCodeStartSeamGuardPx = resolveInlineCodeProseStartSeamGuardPx({
    startsAtLineStart: !params.lineHasContent,
    item: params.item,
  });
  const currentLineFitSlackPx = Math.max(
    currentLineStartSlackPx,
    currentLineWhitespaceContinuationSlackPx,
    currentLineCodeContinuationSlackPx,
  );
  const currentLineWeakProseStartContinuationGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.lineWeakProseStartCodeGroupId === params.codeGroupId &&
    params.lastAcceptedCodeGroupId === params.codeGroupId
      ? currentLineCodeContinuationSlackPx +
        currentLineCodeStartSeamGuardPx +
        params.lineSoftBreakProseAfterInlineCodeGuardPx +
        INLINE_CODE_WEAK_PROSE_START_CONTINUATION_GUARD_PX
      : 0;
  const currentLineNearFitLeadingHangPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment &&
    params.item.codeGroupStartsAfterText &&
    (params.item.prefersFreshLineStart || params.item.codeGroupHasWhitespace) &&
    params.allowLeadingHangForCurrentLine &&
    params.allowCurrentLineLeadingHang
      ? params.item.chromeWidth / 2
      : 0;
  const sliceStartsAtItemStart = params.cursor === null;
  const currentLineWhitespaceContinuationGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.item.startsAfterCodeWhitespace &&
    params.item.codeGroupStartsAfterText
      ? INLINE_CODE_WHITESPACE_CONTINUATION_GUARD_PX
      : 0;
  const currentLineStyledSeamGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId == null &&
    params.item.startsAfterStyledTextSeam &&
    !params.item.hasTrailingStyledText &&
    !isPunctuationOnlySeamText(params.item.text)
      ? params.lineDecoratedTextSegmentCount >= 3
        ? STYLED_TEXT_SEAM_GUARD_PX * 2
        : STYLED_TEXT_SEAM_GUARD_PX
      : 0;
  const currentLineStyledAfterInlineCodeClusterGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId == null &&
    params.item.isDecoratedText &&
    params.lineSawInlineCode &&
    params.lineDecoratedTextSegmentCount > 0
      ? STYLED_TEXT_AFTER_INLINE_CODE_CLUSTER_GUARD_PX
      : 0;
  const currentLineInlineCodeTailTextSeamGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId == null &&
    params.item.startsAfterInlineCodeSeam &&
    params.lineStartedWithContinuedCode &&
    !params.item.startsAfterCollapsedSoftBreak &&
    browserAllowsInlineCodeLeadingHang()
      ? INLINE_CODE_CONTINUED_TAIL_TEXT_SEAM_GUARD_PX
      : 0;
  const currentLineInlineCodeSoftBreakTextStartGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId == null &&
    sliceStartsAtItemStart &&
    shouldApplyInlineCodeSoftBreakTextStartGuard({
      text: params.item.text,
      startsAfterCollapsedSoftBreak: params.item.startsAfterCollapsedSoftBreak,
      startsAfterPathLikeInlineCodeSeam: params.item.startsAfterPathLikeInlineCodeSeam,
      startsAfterInlineCodeSeam: params.item.startsAfterInlineCodeSeam,
      startsStyledTextAfterInlineCodeSeam: params.item.startsStyledTextAfterInlineCodeSeam,
      lastFragmentEndedWithPathDelimiter: params.lineLastCodeFragmentEndedWithPathDelimiter,
      lastFragmentEndedWithHyphen: params.lineLastCodeFragmentEndedWithHyphen,
    })
      ? resolveInlineCodeSoftBreakTextStartGuardPx(params.item.minStartTextWidth)
      : 0;
  const currentLineStyledBodyStartGuardPx =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId == null &&
    params.item.startsStyledTextAfterBodySeam
      ? STYLED_TEXT_BODY_START_GUARD_PX
      : 0;
  const canDropLeadingCollapsedSpaceAtWrap = shouldDropLeadingCollapsedSpaceAtWrap({
    item: params.item,
    codeGroupId: params.codeGroupId,
    lineHasContent: params.lineHasContent,
    cursor: params.cursor,
    pendingSpaceWidth: params.pendingSpaceWidth,
  });
  const startCursor = params.cursor ?? ({ segmentIndex: 0, graphemeIndex: 0 } satisfies LayoutCursor);
  const trailingPlainWidthAfterCodeGroupStart = params.measureCodeGroupTrailingPlainWidth(params.itemIndex);
  const trailingPlainAfterCodeGroupStart = params.measureCodeGroupTrailingPlainInfo(params.itemIndex);
  const currentLineCodeFit =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment
      ? params.measureCodeGroupFitWithinWidth(
          params.itemIndex,
          Math.max(
            1,
            params.remainingWidth -
              params.pendingSpaceWidth -
              currentLineCodeStartSeamGuardPx +
              currentLineFitSlackPx,
          ),
          false,
          params.allowCurrentLineLeadingHang,
        )
      : null;
  const codeFitEndsAtFriendlyBoundary =
    currentLineCodeFit != null && params.codeGroupFitEndsAtFriendlyBoundary(currentLineCodeFit);
  const freshLineCodeFit =
    params.lineHasContent &&
    params.cursor === null &&
    params.codeGroupId != null &&
    params.item.isFirstCodeGroupFragment
      ? params.measureCodeGroupFitWithinWidth(params.itemIndex, params.maxWidth, true)
      : null;
  const effectiveTrailingPlainWidthAfterCodeGroupStart =
    trailingPlainAfterCodeGroupStart.startsAfterCollapsedSoftBreak
      ? 0
      : trailingPlainWidthAfterCodeGroupStart;
  const effectiveTrailingPlainHasFollowingInlineCode =
    trailingPlainAfterCodeGroupStart.startsAfterCollapsedSoftBreak
      ? false
      : trailingPlainAfterCodeGroupStart.hasFollowingInlineCode;
  const freshLineFitEndsAtFriendlyBoundary =
    freshLineCodeFit != null && params.codeGroupFitEndsAtFriendlyBoundary(freshLineCodeFit);
  const prefersFreshLineStart = params.item.prefersFreshLineStart;
  const {
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
  } = resolveInlineCodeStartDecision({
    codeGroupId: params.codeGroupId,
    currentLineStartFragmentWidth: params.chromeWidth + params.item.fullWidth,
    lineAcceptedPlainAfterContinuedCode: params.lineAcceptedPlainAfterContinuedCode,
    lineAcceptedSoftBreakProseAfterInlineCode: params.lineAcceptedSoftBreakProseAfterInlineCode,
    lineHasContent: params.lineHasContent,
    lineSawInlineCode: params.lineSawInlineCode,
    lineSoftBreakProseGuardCodeGroupId: params.lineSoftBreakProseGuardCodeGroupId,
    lineTailAfterInlineCodeIsPunctuationOnly: params.lineTailAfterInlineCodeIsPunctuationOnly,
    item: params.item,
    maxWidth: params.maxWidth,
    preferredStartWidth: params.preferredStartWidth,
    remainingWidth: params.remainingWidth,
    trailingPlainAfterCodeGroupStart,
    wholeCodeGroupWidth: params.wholeCodeGroupWidth,
    currentLineCodeFit,
    freshLineCodeFit,
    codeFitEndsAtFriendlyBoundary,
    freshLineFitEndsAtFriendlyBoundary,
  });

  return {
    currentLineCodeStartSeamGuardPx,
    currentLineFitSlackPx,
    currentLineWhitespaceContinuationGuardPx,
    currentLineWeakProseStartContinuationGuardPx,
    currentLineNearFitLeadingHangPx,
    sliceStartsAtItemStart,
    currentLineStyledSeamGuardPx,
    currentLineStyledAfterInlineCodeClusterGuardPx,
    currentLineInlineCodeTailTextSeamGuardPx,
    currentLineInlineCodeSoftBreakTextStartGuardPx,
    currentLineStyledBodyStartGuardPx,
    canDropLeadingCollapsedSpaceAtWrap,
    startCursor,
    trailingPlainWidthAfterCodeGroupStart,
    trailingPlainAfterCodeGroupStart,
    currentLineCodeFit,
    codeFitEndsAtFriendlyBoundary,
    freshLineCodeFit,
    effectiveTrailingPlainWidthAfterCodeGroupStart,
    effectiveTrailingPlainHasFollowingInlineCode,
    freshLineFitEndsAtFriendlyBoundary,
    prefersFreshLineStart,
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
