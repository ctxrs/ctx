import type { SessionMarkdownInlineRun } from "./sessionMarkdownContract";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";

export type SessionMarkdownInlineCodeStartDecision = {
  pendingSpaceWidth: number;
  preferredStartWidth: number;
  dottedPathClusterWidth: number;
  wholeCodeGroupWidth: number;
  remainingWidth: number;
  currentLineConsumedWidth: number;
  currentLineStartFitRatio: number;
  currentLineCodeStartFitIsReadable: boolean;
  currentLineCodeStartFitIsStrong: boolean;
  shouldBreakForSoftBreakProseCodeStart: boolean;
  shouldBreakForInlineTailPunctuationCodeStart: boolean;
  shouldBreakForStyledTailCodeStart: boolean;
  shouldLimitCurrentCodeGroupToFirstFragment: boolean;
  shouldBreakForAttachedTrailingPlainStart: boolean;
  shouldBreak: boolean;
  text: string;
};

export type SessionMarkdownInlineCodeWhitespaceDecision = {
  lineHasContent: boolean;
  reservedWidth: number;
  remainingWidth: number;
  guardedRemainingWidth: number;
  fragmentWidth: number;
  slackPx?: number;
  shouldBreak: boolean;
  text: string;
};

export type SessionMarkdownInlineCodeSealedContinuationDecision = {
  canRelaxChromiumDottedPathBoundary: boolean;
  currentCodeGroupStartFragmentText: string | null;
  fullWidth: number;
  guardedRemainingWidth: number;
  remainingWidth?: number;
  continuationSlackPx?: number;
  lastFragmentIsShortExtensionPath: boolean;
  lastFragmentText: string | null;
  sameCodeGroupContinuation: boolean;
  sealedBoundaryOverflow: boolean;
  acceptedFragment?: boolean;
  overflowedAcceptedFragment?: boolean;
  shouldBreakBeforePartialSealedDottedPathFragment: boolean;
  text: string;
};

export type SessionMarkdownInlineCodeContinuationDecision = {
  text: string;
  reservedWidth: number;
  remainingWidth: number;
  guardedRemainingWidth: number;
  availableLineWidth?: number;
  lineWidth?: number;
  fullWidth: number;
  currentLineFitSlackPx: number;
  lineLastCodeFragmentText: string | null;
  lineLastCodeFragmentEndedWithPathDelimiter: boolean;
  lineLastCodeFragmentEndedWithHyphen: boolean;
  acceptedWholeFragment?: boolean;
  brokeBeforeFragment?: boolean;
};

export type SessionMarkdownInlineCodeSegmentSeamAdjustment = {
  type:
    | "no-progress-advance"
    | "no-progress-drop"
    | "whitespace-only-break"
    | "whitespace-only-advance"
    | "inline-code-whole-fit";
  lineHasContent: boolean;
  text: string;
  reservedWidth?: number;
  availableWidth?: number;
  wholeSegmentAvailableWidth?: number;
  inlineCodeTailWholeSegmentFitAllowancePx?: number;
  fullWidth?: number;
  lineStartedWithContinuedCode?: boolean;
  startsAfterCollapsedSoftBreak?: boolean;
  startsStyledTextAfterInlineCodeSeam?: boolean;
  allowed?: boolean;
};

export type SessionMarkdownInlineCodeDebugItem =
  | { kind: "hardBreak"; text: "" }
  | { kind: "space"; text: string }
  | {
      kind: "segment";
      codeGroupHasDottedPath: boolean;
      text: string;
      chromeWidth: number;
      fullWidth: number;
      minStartTextWidth: number;
      codeGroupHasTrailingText: boolean;
      codeGroupStartsAfterText: boolean;
      codeGroupStartsAfterStyledTextSeam: boolean;
      isFirstCodeGroupFragment: boolean;
      isSealedInlineCodeFragment: boolean;
      startsAfterCodeWhitespace: boolean;
      startsAfterStyledTextSeam: boolean;
      startsAfterInlineCodeSeam: boolean;
      startsStyledTextAfterBodySeam: boolean;
      startsAfterCollapsedSoftBreak: boolean;
      startsAfterPathLikeInlineCodeSeam: boolean;
      startsStyledTextAfterInlineCodeSeam: boolean;
      hasTrailingInlineCode: boolean;
    };

export type SessionMarkdownInlineCodeDebugPayload = {
  lines: string[];
  startDecisions: SessionMarkdownInlineCodeStartDecision[];
  whitespaceDecisions: SessionMarkdownInlineCodeWhitespaceDecision[];
  continuationDecisions: SessionMarkdownInlineCodeContinuationDecision[];
  sealedContinuationDecisions: SessionMarkdownInlineCodeSealedContinuationDecision[];
  segmentSeamAdjustments: SessionMarkdownInlineCodeSegmentSeamAdjustment[];
  items: SessionMarkdownInlineCodeDebugItem[];
  width: number;
};

export type SessionMarkdownDebugWindow = Window & {
  __ctxForceInlineCodeDebug?: boolean;
  __ctxInlineCodeDebugTarget?: string;
  __ctxInlineCodeDebugWidth?: number;
  __ctxInlineCodeDebug?: SessionMarkdownInlineCodeDebugPayload;
};

export function shouldEnableSessionMarkdownInlineCodeDebug(params: {
  debugWindow: SessionMarkdownDebugWindow | null;
  runs: readonly SessionMarkdownInlineRun[];
  width: number;
}): boolean {
  const { debugWindow } = params;
  const forceInlineCodeDebug = debugWindow?.__ctxForceInlineCodeDebug === true;
  const inlineCodeDebugTarget = debugWindow?.__ctxInlineCodeDebugTarget ?? null;
  const debugInlineCodeWidthMatches =
    debugWindow?.__ctxInlineCodeDebugWidth == null ||
    debugWindow.__ctxInlineCodeDebugWidth === params.width;

  return (
    debugInlineCodeWidthMatches &&
    (forceInlineCodeDebug ||
      (inlineCodeDebugTarget != null &&
        params.runs.some(
          (run) =>
            run.kind === "inlineCode" &&
            (inlineCodeDebugTarget === "*" ||
              run.text === inlineCodeDebugTarget ||
              run.text.includes(inlineCodeDebugTarget)),
        )))
  );
}

export function serializeSessionMarkdownInlineMeasurementItems(
  items: readonly PreparedInlineLayoutItem[],
): SessionMarkdownInlineCodeDebugItem[] {
  return items.map((item) => {
    if (item.kind === "segment") {
      return {
        kind: item.kind,
        codeGroupHasDottedPath: item.codeGroupHasDottedPath,
        text: item.text,
        chromeWidth: item.chromeWidth,
        fullWidth: item.fullWidth,
        minStartTextWidth: item.minStartTextWidth,
        codeGroupHasTrailingText: item.codeGroupHasTrailingText,
        codeGroupStartsAfterText: item.codeGroupStartsAfterText,
        codeGroupStartsAfterStyledTextSeam: item.codeGroupStartsAfterStyledTextSeam,
        isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
        isSealedInlineCodeFragment: item.isSealedInlineCodeFragment,
        startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        startsAfterStyledTextSeam: item.startsAfterStyledTextSeam,
        startsAfterInlineCodeSeam: item.startsAfterInlineCodeSeam,
        startsStyledTextAfterBodySeam: item.startsStyledTextAfterBodySeam,
        startsAfterCollapsedSoftBreak: item.startsAfterCollapsedSoftBreak,
        startsAfterPathLikeInlineCodeSeam: item.startsAfterPathLikeInlineCodeSeam,
        startsStyledTextAfterInlineCodeSeam: item.startsStyledTextAfterInlineCodeSeam,
        hasTrailingInlineCode: item.hasTrailingInlineCode,
      };
    }
    if (item.kind === "space") {
      return {
        kind: item.kind,
        text: item.text,
      };
    }
    return {
      kind: item.kind,
      text: "",
    };
  });
}

export type SessionMarkdownInlineMeasurementDebugProbe = {
  enabled: boolean;
  startDecisions: SessionMarkdownInlineCodeStartDecision[];
  whitespaceDecisions: SessionMarkdownInlineCodeWhitespaceDecision[];
  continuationDecisions: SessionMarkdownInlineCodeContinuationDecision[];
  sealedContinuationDecisions: SessionMarkdownInlineCodeSealedContinuationDecision[];
  segmentSeamAdjustments: SessionMarkdownInlineCodeSegmentSeamAdjustment[];
  appendLineText: (text: string) => void;
  finishLine: () => void;
  publish: (items: readonly PreparedInlineLayoutItem[]) => void;
};

export function createSessionMarkdownInlineMeasurementDebugProbe(params: {
  debugWindow: SessionMarkdownDebugWindow | null;
  runs: readonly SessionMarkdownInlineRun[];
  width: number;
}): SessionMarkdownInlineMeasurementDebugProbe {
  const enabled = shouldEnableSessionMarkdownInlineCodeDebug(params);
  const lines: string[] = [];
  const startDecisions: SessionMarkdownInlineCodeStartDecision[] = [];
  const whitespaceDecisions: SessionMarkdownInlineCodeWhitespaceDecision[] = [];
  const continuationDecisions: SessionMarkdownInlineCodeContinuationDecision[] = [];
  const sealedContinuationDecisions: SessionMarkdownInlineCodeSealedContinuationDecision[] = [];
  const segmentSeamAdjustments: SessionMarkdownInlineCodeSegmentSeamAdjustment[] = [];
  let currentLine = "";

  if (!enabled) {
    return {
      enabled,
      startDecisions,
      whitespaceDecisions,
      continuationDecisions,
      sealedContinuationDecisions,
      segmentSeamAdjustments,
      appendLineText: () => undefined,
      finishLine: () => undefined,
      publish: () => undefined,
    };
  }

  return {
    enabled,
    startDecisions,
    whitespaceDecisions,
    continuationDecisions,
    sealedContinuationDecisions,
    segmentSeamAdjustments,
    appendLineText: (text) => {
      currentLine += text;
    },
    finishLine: () => {
      lines.push(currentLine);
      currentLine = "";
    },
    publish: (items) => {
      if (!params.debugWindow) {
        return;
      }
      params.debugWindow.__ctxInlineCodeDebug = {
        lines,
        startDecisions,
        whitespaceDecisions,
        continuationDecisions,
        sealedContinuationDecisions,
        segmentSeamAdjustments,
        items: serializeSessionMarkdownInlineMeasurementItems(items),
        width: params.width,
      };
    },
  };
}
