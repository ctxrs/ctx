import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import type {
  SessionMarkdownInlineCodeContinuationDecision,
  SessionMarkdownInlineCodeSealedContinuationDecision,
  SessionMarkdownInlineCodeWhitespaceDecision,
} from "./sessionMarkdownInlineMeasurementDebug";
import type { InlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";

const NON_PUNCTUATION_TRAILING_TEXT_PATTERN = /[\p{L}\p{N}]/u;

export type InlineCodePlacementDebug = {
  enabled: boolean;
  whitespaceDecisions: SessionMarkdownInlineCodeWhitespaceDecision[];
  continuationDecisions: SessionMarkdownInlineCodeContinuationDecision[];
  sealedContinuationDecisions: SessionMarkdownInlineCodeSealedContinuationDecision[];
  appendLineText: (text: string) => void;
};

export type InlineCodePlacementResult = {
  action: "continue" | "break" | "pass";
  state: InlineMeasurementLineState;
  forcedFreshWholeCodeGroupIndex: number | null;
};

export type InlineCodePlacementParams = {
  items: readonly PreparedInlineLayoutItem[];
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  state: InlineMeasurementLineState;
  codeGroupId: number;
  maxWidth: number;
  reservedWidth: number;
  wholeCodeGroupWidth: number;
  forcedFreshWholeCodeGroupIndex: number | null;
  currentLineCodeStartSeamGuardPx: number;
  currentLineFitSlackPx: number;
  currentLineWhitespaceContinuationGuardPx: number;
  currentLineWeakProseStartContinuationGuardPx: number;
  currentLineNearFitLeadingHangPx: number;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldTrackWeakProseStartCodeGroup: boolean;
  shouldForceSoftBreakWeakProseContinuationWrap: boolean;
  debug: InlineCodePlacementDebug;
};

export function hasNonPunctuationTrailingTextAfterCodeGroup(params: {
  codeGroupId: number;
  itemIndex: number;
  items: readonly PreparedInlineLayoutItem[];
}): boolean {
  for (let index = params.itemIndex + 1; index < params.items.length; index += 1) {
    const item = params.items[index]!;
    if (item.kind === "hardBreak") {
      return false;
    }
    if (item.kind === "space") {
      continue;
    }
    if (item.codeGroupId === params.codeGroupId) {
      continue;
    }
    if (item.codeGroupId != null) {
      return false;
    }
    return NON_PUNCTUATION_TRAILING_TEXT_PATTERN.test(item.text);
  }
  return false;
}

export function acceptCodeFragmentState(params: {
  state: InlineMeasurementLineState;
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  codeGroupId: number;
  remainingWidthBeforeAccept: number;
  remainingWidthAfterAccept: number;
  lastFragmentText: string;
  lastFragmentEndedWithHyphen: boolean;
  lastFragmentEndedWithPathDelimiter: boolean;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldTrackWeakProseStartCodeGroup: boolean;
  shouldForceSoftBreakWeakProseContinuationWrap: boolean;
  maxWidth: number;
}): void {
  const {
    state,
    item,
    codeGroupId,
    remainingWidthBeforeAccept,
    remainingWidthAfterAccept,
    lastFragmentText,
    lastFragmentEndedWithHyphen,
    lastFragmentEndedWithPathDelimiter,
    shouldLimitCurrentCodeGroupToFirstFragment,
    shouldTrackWeakProseStartCodeGroup,
    shouldForceSoftBreakWeakProseContinuationWrap,
    maxWidth,
  } = params;
  const lineHasContentBeforeAccept = state.lineHasContent;

  state.remainingWidth = remainingWidthAfterAccept;
  if (!lineHasContentBeforeAccept) {
    state.lineOnlyCodeGroupId = codeGroupId;
    state.lineStartedWithContinuedCode = !item.isFirstCodeGroupFragment;
  } else if (state.lineOnlyCodeGroupId !== codeGroupId) {
    state.lineOnlyCodeGroupId = null;
  }
  if (state.lastAcceptedCodeGroupId !== codeGroupId) {
    state.lineCurrentCodeGroupStartFragmentText = item.text;
    state.lineCurrentCodeGroupStartedNearFresh =
      lineHasContentBeforeAccept && remainingWidthBeforeAccept > maxWidth * 0.85;
    state.lineCurrentCodeGroupLimitToFirstFragment = shouldLimitCurrentCodeGroupToFirstFragment;
  }
  state.lineLastCodeFragmentText = lastFragmentText;
  state.lineLastCodeFragmentEndedWithHyphen = lastFragmentEndedWithHyphen;
  state.lineLastCodeFragmentEndedWithPathDelimiter = lastFragmentEndedWithPathDelimiter;
  state.lastAcceptedCodeGroupId = codeGroupId;
  state.lineTailAfterInlineCodeIsPunctuationOnly = false;
  state.lineHasContent = true;
  state.chargedCodeGroups.add(codeGroupId);
  if (state.lineAcceptedSoftBreakProseAfterInlineCode && state.lineSoftBreakProseGuardCodeGroupId == null) {
    state.lineSoftBreakProseGuardCodeGroupId = codeGroupId;
  }
  if (shouldTrackWeakProseStartCodeGroup) {
    state.lineWeakProseStartCodeGroupId = codeGroupId;
    if (shouldForceSoftBreakWeakProseContinuationWrap) {
      state.lineForceSoftBreakWeakProseContinuationCodeGroupId = codeGroupId;
    }
  }
}
