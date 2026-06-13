import type { LayoutCursor } from "@chenglou/pretext";
import type { SessionMarkdownInlineRun } from "./sessionMarkdownContract";
import {
  createInlineCodeFitPlanner,
  resolveInlineCodeWrapChromeWidth,
} from "./sessionMarkdownInlineCodeFit";
import { prepareInlineLayoutItems } from "./sessionMarkdownInlineLayout";
import { placeInlineCodeSegment } from "./sessionMarkdownInlineMeasurementCodePlacement";
import {
  createSessionMarkdownInlineMeasurementDebugProbe,
  type SessionMarkdownDebugWindow,
} from "./sessionMarkdownInlineMeasurementDebug";
import type { InlineWrapMode } from "./sessionMarkdownInlineLayout";
import { resolveInlineMeasurementDerivedContext } from "./sessionMarkdownInlineMeasurementContext";
import { createInlineMeasurementLineState } from "./sessionMarkdownInlineMeasurementState";
import { placeInlineTextSegment } from "./sessionMarkdownInlineMeasurementTextPlacement";
import { clampHeight, type TextBlockTypography } from "./sessionMarkdownMeasurementCore";

export function measureInlineRunsHeight(params: {
  runs: readonly SessionMarkdownInlineRun[];
  width: number;
  typography: TextBlockTypography;
  cacheKeyPrefix: string;
  wrapMode?: InlineWrapMode;
}): number {
  const maxWidth = Math.max(1, params.width);
  const items = prepareInlineLayoutItems(params);
  const debugProbe = createSessionMarkdownInlineMeasurementDebugProbe({
    debugWindow: typeof window !== "undefined" ? (window as SessionMarkdownDebugWindow) : null,
    runs: params.runs,
    width: params.width,
  });
  const debugInlineCode = debugProbe.enabled;
  const debugStartDecisions = debugProbe.startDecisions;
  const debugWhitespaceDecisions = debugProbe.whitespaceDecisions;
  const debugSealedContinuationDecisions = debugProbe.sealedContinuationDecisions;
  const debugContinuationDecisions = debugProbe.continuationDecisions;
  const debugSegmentSeamAdjustments = debugProbe.segmentSeamAdjustments;
  const {
    codeGroupFitEndsAtFriendlyBoundary,
    dottedPathCodeGroupStartClusterWidths,
    measureCodeGroupTrailingPlainInfo,
    measureCodeGroupTrailingPlainWidth,
    measureCodeGroupFitWithinWidth,
    preferredCodeGroupStartWidths,
    wholeCodeGroupInlineWidths,
  } = createInlineCodeFitPlanner({ items, maxWidth });
  let totalHeight = 0;
  let itemIndex = 0;
  let cursor: LayoutCursor | null = null;
  let forcedFreshWholeCodeGroupIndex: number | null = null;

  while (itemIndex < items.length) {
    let lineState = createInlineMeasurementLineState({
      cursor,
      itemIndex,
      maxWidth,
    });
    let forcedBreak = false;

    while (lineState.itemIndex < items.length) {
      const item = items[lineState.itemIndex]!;
      if (item.kind === "hardBreak") {
        lineState.itemIndex += 1;
        lineState.cursor = null;
        forcedBreak = true;
        break;
      }
      if (item.kind === "space") {
        lineState.itemIndex += 1;
        if (lineState.lineHasContent) {
          lineState.pendingSpaceWidth = item.width;
          if (debugInlineCode) {
            debugProbe.appendLineText(item.text);
          }
        }
        continue;
      }

      const codeGroupId = item.codeGroupId;
      const preferredStartWidth = preferredCodeGroupStartWidths.get(lineState.itemIndex) ?? 0;
      const dottedPathClusterWidth =
        dottedPathCodeGroupStartClusterWidths.get(lineState.itemIndex) ?? 0;
      const wholeCodeGroupWidth = wholeCodeGroupInlineWidths.get(lineState.itemIndex) ?? 0;
      const allowCurrentLineLeadingHang =
        !lineState.lineSawInlineCode ||
        lineState.lineAcceptedPlainAfterContinuedCode ||
        item.startsAfterCollapsedSoftBreak;
      const allowLeadingHangForCurrentLine =
        lineState.lineHasContent &&
        lineState.cursor === null &&
        codeGroupId != null &&
        item.isFirstCodeGroupFragment &&
        item.codeGroupStartsAfterText &&
        !lineState.lineStartedWithCollapsedSoftBreakPlainText &&
        wholeCodeGroupWidth > lineState.remainingWidth + 0.01;
      const chromeWidth =
        codeGroupId != null
          ? resolveInlineCodeWrapChromeWidth({
              allowLeadingHang: allowCurrentLineLeadingHang && allowLeadingHangForCurrentLine,
              chromeWidth: item.chromeWidth,
              codeGroupHasWhitespace: item.codeGroupHasWhitespace,
              codeGroupStartsAfterText: item.codeGroupStartsAfterText,
              chargedChrome: lineState.chargedCodeGroups.has(codeGroupId),
              isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
              prefersFreshLineStart: item.prefersFreshLineStart,
              lineHasContent: lineState.lineHasContent,
              startsAtLineStart: !lineState.lineHasContent,
            })
          : 0;
      const reservedWidth =
        (lineState.lineHasContent ? lineState.pendingSpaceWidth : 0) + chromeWidth;

      const {
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
        currentLineCodeFit,
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
        shouldBreakForAttachedTrailingPlainStart,
        shouldBreakForWeakFirstSliceFreshLineStart,
      } = resolveInlineMeasurementDerivedContext({
        item,
        itemIndex: lineState.itemIndex,
        codeGroupId,
        preferredStartWidth,
        wholeCodeGroupWidth,
        lineHasContent: lineState.lineHasContent,
        lineSawInlineCode: lineState.lineSawInlineCode,
        lineAcceptedPlainAfterContinuedCode: lineState.lineAcceptedPlainAfterContinuedCode,
        lineStartedWithContinuedCode: lineState.lineStartedWithContinuedCode,
        lineAcceptedSoftBreakProseAfterInlineCode:
          lineState.lineAcceptedSoftBreakProseAfterInlineCode,
        lineSoftBreakProseAfterInlineCodeGuardPx:
          lineState.lineSoftBreakProseAfterInlineCodeGuardPx,
        lineSoftBreakProseGuardCodeGroupId: lineState.lineSoftBreakProseGuardCodeGroupId,
        lineTailAfterInlineCodeIsPunctuationOnly:
          lineState.lineTailAfterInlineCodeIsPunctuationOnly,
        lineWeakProseStartCodeGroupId: lineState.lineWeakProseStartCodeGroupId,
        lineDecoratedTextSegmentCount: lineState.lineDecoratedTextSegmentCount,
        lastAcceptedCodeGroupId: lineState.lastAcceptedCodeGroupId,
        lineLastCodeFragmentText: lineState.lineLastCodeFragmentText,
        lineLastCodeFragmentEndedWithHyphen: lineState.lineLastCodeFragmentEndedWithHyphen,
        lineLastCodeFragmentEndedWithPathDelimiter:
          lineState.lineLastCodeFragmentEndedWithPathDelimiter,
        remainingWidth: lineState.remainingWidth,
        pendingSpaceWidth: lineState.pendingSpaceWidth,
        maxWidth,
        cursor: lineState.cursor,
        allowLeadingHangForCurrentLine,
        allowCurrentLineLeadingHang,
        chargedCodeGroups: lineState.chargedCodeGroups,
        chromeWidth,
        measureCodeGroupTrailingPlainWidth,
        measureCodeGroupTrailingPlainInfo,
        measureCodeGroupFitWithinWidth,
        codeGroupFitEndsAtFriendlyBoundary,
      });

      if (
        debugInlineCode &&
        lineState.lineHasContent &&
        lineState.cursor === null &&
        codeGroupId != null &&
        item.isFirstCodeGroupFragment &&
        prefersFreshLineStart
      ) {
        debugStartDecisions.push({
          pendingSpaceWidth: lineState.pendingSpaceWidth,
          preferredStartWidth,
          dottedPathClusterWidth,
          wholeCodeGroupWidth,
          remainingWidth: lineState.remainingWidth,
          currentLineConsumedWidth: currentLineCodeFit?.consumedWidth ?? 0,
          currentLineStartFitRatio,
          currentLineCodeStartFitIsReadable,
          currentLineCodeStartFitIsStrong,
          shouldBreakForSoftBreakProseCodeStart,
          shouldBreakForInlineTailPunctuationCodeStart,
          shouldBreakForStyledTailCodeStart,
          shouldLimitCurrentCodeGroupToFirstFragment,
          shouldBreakForAttachedTrailingPlainStart,
          shouldBreak:
            shouldBreakForSoftBreakProseCodeStart ||
            shouldBreakForPreferredStart ||
            shouldBreakForInlineTailPunctuationCodeStart ||
            shouldBreakForStyledTailCodeStart ||
            shouldBreakForAttachedTrailingPlainStart ||
            shouldBreakForWeakFirstSliceFreshLineStart,
          text: item.text,
        });
      }

      if (
        shouldBreakForSoftBreakProseCodeStart ||
        shouldBreakForPreferredStart ||
        shouldBreakForInlineTailPunctuationCodeStart ||
        shouldBreakForStyledTailCodeStart ||
        shouldBreakForAttachedTrailingPlainStart ||
        shouldBreakForWeakFirstSliceFreshLineStart
      ) {
        if (
          shouldBreakForInlineTailPunctuationCodeStart &&
          item.isFirstCodeGroupFragment &&
          codeGroupId != null &&
          !item.codeGroupHasTrailingText &&
          wholeCodeGroupWidth > 0 &&
          wholeCodeGroupWidth <= maxWidth + 0.01
        ) {
          forcedFreshWholeCodeGroupIndex = lineState.itemIndex;
        }
        lineState.cursor = null;
        break;
      }

      if (
        lineState.lineHasContent &&
        lineState.cursor === null &&
        codeGroupId != null &&
        item.isFirstCodeGroupFragment &&
        prefersFreshLineStart &&
        (currentLineCodeFit == null || currentLineCodeFit.consumedWidth <= 0) &&
        reservedWidth + item.fullWidth >
          lineState.remainingWidth + currentLineNearFitLeadingHangPx + 0.01
      ) {
        lineState.cursor = null;
        break;
      }

      if (lineState.lineHasContent && lineState.cursor === null && codeGroupId != null) {
        const fullWidth = reservedWidth + item.fullWidth;
        if (
          fullWidth > lineState.remainingWidth + 0.01 &&
          (prefersFreshLineStart || item.codeGroupHasDottedPath) &&
          lineState.remainingWidth < reservedWidth + item.minStartTextWidth - 0.01 &&
          !currentLineCodeStartFitIsReadable
        ) {
          lineState.cursor = null;
          break;
        }
      }

      if (lineState.cursor === null && codeGroupId != null) {
        const codePlacementResult = placeInlineCodeSegment({
          items,
          item,
          state: lineState,
          codeGroupId,
          maxWidth,
          reservedWidth,
          wholeCodeGroupWidth,
          forcedFreshWholeCodeGroupIndex,
          currentLineCodeStartSeamGuardPx,
          currentLineFitSlackPx,
          currentLineWhitespaceContinuationGuardPx,
          currentLineWeakProseStartContinuationGuardPx,
          currentLineNearFitLeadingHangPx,
          shouldLimitCurrentCodeGroupToFirstFragment,
          shouldTrackWeakProseStartCodeGroup,
          shouldForceSoftBreakWeakProseContinuationWrap,
          debug: {
            enabled: debugInlineCode,
            whitespaceDecisions: debugWhitespaceDecisions,
            continuationDecisions: debugContinuationDecisions,
            sealedContinuationDecisions: debugSealedContinuationDecisions,
            appendLineText: debugProbe.appendLineText,
          },
        });
        lineState = codePlacementResult.state;
        forcedFreshWholeCodeGroupIndex = codePlacementResult.forcedFreshWholeCodeGroupIndex;
        if (codePlacementResult.action === "continue") {
          continue;
        }
        if (codePlacementResult.action === "break") {
          break;
        }
      }

      const textPlacementResult = placeInlineTextSegment({
        item,
        state: lineState,
        startCursor,
        sliceStartsAtItemStart,
        reservedWidth,
        guardedRemainingWidth: lineState.remainingWidth,
        currentLineWhitespaceContinuationGuardPx,
        currentLineStyledSeamGuardPx,
        currentLineStyledAfterInlineCodeClusterGuardPx,
        currentLineStyledBodyStartGuardPx,
        currentLineInlineCodeTailTextSeamGuardPx,
        currentLineInlineCodeSoftBreakTextStartGuardPx,
        currentLineFitSlackPx,
        currentLineNearFitLeadingHangPx,
        canDropLeadingCollapsedSpaceAtWrap,
        maxWidth,
        debug: {
          enabled: debugInlineCode,
          continuationDecisions: debugContinuationDecisions,
          segmentSeamAdjustments: debugSegmentSeamAdjustments,
          appendLineText: debugProbe.appendLineText,
        },
      });
      lineState = textPlacementResult.state;
      if (textPlacementResult.action === "continue") {
        continue;
      }
      break;
    }

    if (
      lineState.lineHasContent &&
      lineState.cursor === null &&
      items[lineState.itemIndex]?.kind === "hardBreak"
    ) {
      lineState.itemIndex += 1;
      forcedBreak = true;
    }

    if (!lineState.lineHasContent && !forcedBreak) {
      break;
    }

    totalHeight += params.typography.lineHeight;
    itemIndex = lineState.itemIndex;
    cursor = lineState.cursor;
    if (debugInlineCode) {
      debugProbe.finishLine();
    }
  }

  debugProbe.publish(items);

  return clampHeight(totalHeight);
}
