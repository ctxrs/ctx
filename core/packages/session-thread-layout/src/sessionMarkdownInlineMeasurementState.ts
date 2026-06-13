import type { LayoutCursor } from "@chenglou/pretext";

export type InlineMeasurementLineState = {
  cursor: LayoutCursor | null;
  itemIndex: number;
  remainingWidth: number;
  pendingSpaceWidth: number;
  lineHasContent: boolean;
  lineOnlyCodeGroupId: number | null;
  lastAcceptedCodeGroupId: number | null;
  lineCurrentCodeGroupStartFragmentText: string | null;
  lineCurrentCodeGroupStartedNearFresh: boolean;
  lineCurrentCodeGroupLimitToFirstFragment: boolean;
  lineLastCodeFragmentText: string | null;
  lineLastCodeFragmentEndedWithHyphen: boolean;
  lineLastCodeFragmentEndedWithPathDelimiter: boolean;
  lineStartedWithContinuedCode: boolean;
  lineStartedWithCollapsedSoftBreakPlainText: boolean;
  lineAcceptedPlainAfterContinuedCode: boolean;
  lineAcceptedSoftBreakProseAfterInlineCode: boolean;
  lineSoftBreakProseAfterInlineCodeGuardPx: number;
  lineSoftBreakProseGuardCodeGroupId: number | null;
  lineForceSoftBreakWeakProseContinuationCodeGroupId: number | null;
  lineWeakProseStartCodeGroupId: number | null;
  lineUsedChromiumDottedPathBoundaryContinuation: boolean;
  lineDecoratedTextSegmentCount: number;
  lineHasSoftHyphenText: boolean;
  lineSawInlineCode: boolean;
  lineTailAfterInlineCodeIsPunctuationOnly: boolean;
  chargedCodeGroups: Set<number>;
};

export function createInlineMeasurementLineState(params: {
  cursor: LayoutCursor | null;
  itemIndex: number;
  maxWidth: number;
}): InlineMeasurementLineState {
  return {
    cursor: params.cursor,
    itemIndex: params.itemIndex,
    remainingWidth: params.maxWidth,
    pendingSpaceWidth: 0,
    lineHasContent: false,
    lineOnlyCodeGroupId: null,
    lastAcceptedCodeGroupId: null,
    lineCurrentCodeGroupStartFragmentText: null,
    lineCurrentCodeGroupStartedNearFresh: false,
    lineCurrentCodeGroupLimitToFirstFragment: false,
    lineLastCodeFragmentText: null,
    lineLastCodeFragmentEndedWithHyphen: false,
    lineLastCodeFragmentEndedWithPathDelimiter: false,
    lineStartedWithContinuedCode: false,
    lineStartedWithCollapsedSoftBreakPlainText: false,
    lineAcceptedPlainAfterContinuedCode: false,
    lineAcceptedSoftBreakProseAfterInlineCode: false,
    lineSoftBreakProseAfterInlineCodeGuardPx: 0,
    lineSoftBreakProseGuardCodeGroupId: null,
    lineForceSoftBreakWeakProseContinuationCodeGroupId: null,
    lineWeakProseStartCodeGroupId: null,
    lineUsedChromiumDottedPathBoundaryContinuation: false,
    lineDecoratedTextSegmentCount: 0,
    lineHasSoftHyphenText: false,
    lineSawInlineCode: false,
    lineTailAfterInlineCodeIsPunctuationOnly: false,
    chargedCodeGroups: new Set<number>(),
  };
}
