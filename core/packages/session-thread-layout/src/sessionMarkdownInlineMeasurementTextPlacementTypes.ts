import type { LayoutCursor, LayoutLine } from "@chenglou/pretext";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import type { SessionMarkdownInlineCodeContinuationDecision, SessionMarkdownInlineCodeSegmentSeamAdjustment } from "./sessionMarkdownInlineMeasurementDebug";
import type { InlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";

export type InlineTextPlacementDebug = {
  enabled: boolean;
  continuationDecisions: SessionMarkdownInlineCodeContinuationDecision[];
  segmentSeamAdjustments: SessionMarkdownInlineCodeSegmentSeamAdjustment[];
  appendLineText: (text: string) => void;
};

export type InlineTextPlacementResult = {
  action: "continue" | "break";
  state: InlineMeasurementLineState;
};

export type InlineTextPlacementParams = {
  item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  state: InlineMeasurementLineState;
  startCursor: LayoutCursor;
  sliceStartsAtItemStart: boolean;
  reservedWidth: number;
  guardedRemainingWidth: number;
  currentLineWhitespaceContinuationGuardPx: number;
  currentLineStyledSeamGuardPx: number;
  currentLineStyledAfterInlineCodeClusterGuardPx: number;
  currentLineStyledBodyStartGuardPx: number;
  currentLineInlineCodeTailTextSeamGuardPx: number;
  currentLineInlineCodeSoftBreakTextStartGuardPx: number;
  currentLineFitSlackPx: number;
  currentLineNearFitLeadingHangPx: number;
  canDropLeadingCollapsedSpaceAtWrap: boolean;
  maxWidth: number;
  debug: InlineTextPlacementDebug;
};

export type InlineTextPlacementWidths = {
  availableWidth: number;
  availableWidthWithoutLeadingSpace: number;
  inlineCodeTailWholeSegmentFitAllowancePx: number;
  wholeSegmentAvailableWidth: number;
  wholeSegmentFitsCurrentLine: boolean;
  codeSegmentAvailableWidth: number;
};

export type InlineTextPlacementLineResolution = {
  line: LayoutLine | null;
  lineSegmentText: string | null;
};
