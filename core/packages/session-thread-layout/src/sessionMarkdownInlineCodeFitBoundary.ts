import { layoutNextLine } from "@chenglou/pretext";
import { isSealedInlineCodeFragment } from "./inlineCodeFragments";
import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import {
  INLINE_CODE_ENGINE_PROSE_START_FOLLOWING_FRAGMENT_SLACK_PX,
  INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX,
  allowsChromiumDottedBoundaryHang,
  isShortExtensionPathLikeFragment,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeProseStartSeamGuardPx,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  resolveInlineCodeWrapChromeWidth,
  shouldBreakBeforePathDelimiterNearFitContinuation,
  shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment,
  type InlineCodeBoundaryFit,
} from "./sessionMarkdownInlineCodeFitRules";
import { LINE_START_CURSOR, cursorsMatch } from "./sessionMarkdownMeasurementCore";

type InlineSegmentItem = Extract<PreparedInlineLayoutItem, { kind: "segment" }>;

export function createInlineCodeBoundaryFitMeasurer(params: {
  items: readonly PreparedInlineLayoutItem[];
  maxWidth: number;
  wholeCodeGroupInlineWidths: ReadonlyMap<number, number>;
  isPathLikeContinuationItem: (item: InlineSegmentItem) => boolean;
}) {
  const { items, maxWidth, wholeCodeGroupInlineWidths, isPathLikeContinuationItem } = params;
const measureCodeGroupFitWithinWidth = (
    startIndex: number,
    availableWidth: number,
    startsAtLineStart: boolean,
    allowLeadingHang = true,
  ): InlineCodeBoundaryFit => {
    const firstItem = items[startIndex];
    if (firstItem?.kind !== "segment" || firstItem.codeGroupId == null) {
      return {
        consumedWidth: 0,
        endedAtGroupEnd: false,
        endedInsideFragment: false,
        lastFragmentText: null,
        nextFragmentText: null,
        nextStartsAfterCodeWhitespace: false,
      };
    }
    const codeGroupId = firstItem.codeGroupId;
    const wholeCodeGroupWidth = wholeCodeGroupInlineWidths.get(startIndex) ?? 0;
    const allowCurrentLineLeadingHang =
      allowLeadingHang &&
      browserAllowsInlineCodeLeadingHang() &&
      !startsAtLineStart &&
      firstItem.isFirstCodeGroupFragment &&
      firstItem.codeGroupStartsAfterText &&
      wholeCodeGroupWidth > availableWidth + 0.01;
    let remainingWidth = Math.max(
      1,
      availableWidth -
        resolveInlineCodeProseStartSeamGuardPx({
          startsAtLineStart,
          item: firstItem,
        }),
    );
    let lineHasContent = !startsAtLineStart;
    let pendingSpaceWidth = 0;
    let chargedChrome = false;
    let lastFragmentText: string | null = null;
    let consumedWidth = 0;
    let usedChromiumDottedPathBoundaryContinuation = false;

    for (let index = startIndex; index < items.length; index += 1) {
      const item = items[index]!;
      if (item.kind === "hardBreak") {
        return {
          consumedWidth,
          endedAtGroupEnd: true,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: null,
          nextStartsAfterCodeWhitespace: false,
        };
      }
      if (item.kind === "space") {
        if (item.codeGroupId !== codeGroupId) {
          return {
            consumedWidth,
            endedAtGroupEnd: true,
            endedInsideFragment: false,
            lastFragmentText,
            nextFragmentText: null,
            nextStartsAfterCodeWhitespace: false,
          };
        }
        if (lineHasContent) {
          pendingSpaceWidth = item.width;
        }
        continue;
      }
      if (item.codeGroupId !== codeGroupId) {
        return {
          consumedWidth,
          endedAtGroupEnd: true,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: null,
          nextStartsAfterCodeWhitespace: false,
        };
      }

      if (
        lineHasContent &&
        firstItem.codeGroupHasDottedPath &&
        lastFragmentText?.endsWith("-") &&
        isPathLikeContinuationItem(item)
      ) {
        const continuationFit = measureCodeGroupFitWithinWidth(index, remainingWidth, false, allowLeadingHang);
        if (!codeGroupFitEndsAtFriendlyBoundary(continuationFit)) {
          return {
            consumedWidth,
            endedAtGroupEnd: false,
            endedInsideFragment: false,
            lastFragmentText,
            nextFragmentText: item.text,
            nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          };
        }
      }
      const reservedWidth =
        (lineHasContent ? pendingSpaceWidth : 0) +
        resolveInlineCodeWrapChromeWidth({
          allowLeadingHang: allowCurrentLineLeadingHang,
          chromeWidth: item.chromeWidth,
          codeGroupHasWhitespace: item.codeGroupHasWhitespace,
          codeGroupStartsAfterText: item.codeGroupStartsAfterText,
          chargedChrome,
          isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
          prefersFreshLineStart: item.prefersFreshLineStart,
          lineHasContent,
          startsAtLineStart,
        });
      const continuationSlackPx = resolveInlineCodeContinuationFitSlackPx({
        lineHasContent,
        atLineBreakBoundary: true,
        sameCodeGroupContinuation: lineHasContent,
        startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        lastFragmentEndedWithDot: lastFragmentText?.endsWith(".") ?? false,
        lastFragmentEndedWithHyphen: lastFragmentText?.endsWith("-") ?? false,
        lastFragmentEndedWithPathDelimiter: /[\\/]+$/.test(lastFragmentText ?? ""),
        item,
      });
      const shouldAcceptChromiumPathTailContinuation =
        browserAllowsInlineCodeLeadingHang() &&
        lineHasContent &&
        /[\\/]+$/.test(lastFragmentText ?? "") &&
        !item.isSealedInlineCodeFragment &&
        !item.startsAfterCodeWhitespace &&
        reservedWidth + item.fullWidth + INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX <=
          remainingWidth + continuationSlackPx + 0.01;
      const nextSameCodeGroupItem = items[index + 1];
      const splitDottedStemTailWouldOverflowCurrentLine =
        availableWidth > maxWidth * 0.85 &&
        lineHasContent &&
        firstItem.codeGroupStartsAfterText &&
        firstItem.codeGroupHasTrailingText &&
        lastFragmentText?.endsWith("/") === true &&
        !firstItem.text.includes(".") &&
        item.text.endsWith(".") &&
        nextSameCodeGroupItem?.kind === "segment" &&
        nextSameCodeGroupItem.codeGroupId === codeGroupId &&
        isPathLikeContinuationItem(nextSameCodeGroupItem) &&
        reservedWidth + item.fullWidth + nextSameCodeGroupItem.fullWidth >
          remainingWidth + continuationSlackPx + 0.01;
      if (splitDottedStemTailWouldOverflowCurrentLine) {
        return {
          consumedWidth,
          endedAtGroupEnd: false,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: item.text,
          nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        };
      }
      if (
        shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment({
          lineHasContent,
          startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          reservedWidth,
          remainingWidth,
          fragmentWidth: item.fullWidth,
          slackPx: resolveInlineCodeWhitespaceSeparatedFragmentSlackPx({
            lineHasContent,
            startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
            fragmentText: item.text,
          }),
        })
      ) {
        return {
          consumedWidth,
          endedAtGroupEnd: false,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: item.text,
          nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        };
      }
      if (
        shouldBreakBeforePathDelimiterNearFitContinuation({
          lineHasContent,
          sameCodeGroupContinuation: lineHasContent,
          lastFragmentEndedWithPathDelimiter: /[\\/]+$/.test(lastFragmentText ?? ""),
          startsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          isSealedInlineCodeFragment: item.isSealedInlineCodeFragment,
          reservedWidth,
          remainingWidth,
          fragmentWidth: item.fullWidth,
        })
      ) {
        return {
          consumedWidth,
          endedAtGroupEnd: false,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: item.text,
          nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        };
      }
      if (
        lineHasContent &&
        !startsAtLineStart &&
        !browserAllowsInlineCodeLeadingHang() &&
        !firstItem.codeGroupHasWhitespace &&
        firstItem.isFirstCodeGroupFragment &&
        firstItem.codeGroupStartsAfterText &&
        !firstItem.codeGroupStartsAfterStyledTextSeam &&
        isPathLikeContinuationItem(item) &&
        Math.max(0, remainingWidth - reservedWidth - item.fullWidth) <
          INLINE_CODE_ENGINE_PROSE_START_FOLLOWING_FRAGMENT_SLACK_PX
      ) {
        return {
          consumedWidth,
          endedAtGroupEnd: false,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: item.text,
          nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        };
      }
      const sealedBoundary =
        lineHasContent && lastFragmentText != null && isSealedInlineCodeFragment(lastFragmentText);
      const boundaryRemainingWidth = remainingWidth + continuationSlackPx;
      const sealedBoundaryOverflow =
        sealedBoundary &&
        !shouldAcceptChromiumPathTailContinuation &&
        reservedWidth + item.fullWidth > boundaryRemainingWidth + 0.01;
      const lastFragmentIsShortExtensionPath = isShortExtensionPathLikeFragment(lastFragmentText);
      const canRelaxChromiumDottedPathBoundary =
        sealedBoundaryOverflow &&
        !usedChromiumDottedPathBoundaryContinuation &&
        browserAllowsInlineCodeLeadingHang() &&
        firstItem.codeGroupStartsAfterText &&
        !firstItem.text.includes(".") &&
        !lastFragmentIsShortExtensionPath &&
        item.codeGroupHasDottedPath &&
        !item.text.includes("/") &&
        !item.text.includes("\\") &&
        (item.text.includes(".") || item.isPathTailFragment) &&
        allowsChromiumDottedBoundaryHang({
          boundaryRemainingWidth,
          chromeWidth: item.chromeWidth,
          fullWidth: reservedWidth + item.fullWidth,
        });
      if (sealedBoundaryOverflow && !canRelaxChromiumDottedPathBoundary) {
        return {
          consumedWidth,
          endedAtGroupEnd: false,
          endedInsideFragment: false,
          lastFragmentText,
          nextFragmentText: item.text,
          nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
        };
      }
      if (canRelaxChromiumDottedPathBoundary) {
        usedChromiumDottedPathBoundaryContinuation = true;
      }
      if (item.isSealedInlineCodeFragment && canRelaxChromiumDottedPathBoundary) {
        remainingWidth = Math.max(0, remainingWidth - reservedWidth - item.fullWidth);
        consumedWidth += reservedWidth + item.fullWidth;
        lineHasContent = true;
        chargedChrome = true;
        pendingSpaceWidth = 0;
        lastFragmentText = item.text;
        continue;
      }
      if (shouldAcceptChromiumPathTailContinuation) {
        remainingWidth = Math.max(0, remainingWidth - reservedWidth - item.fullWidth);
        consumedWidth += reservedWidth + item.fullWidth;
        lineHasContent = true;
        chargedChrome = true;
        pendingSpaceWidth = 0;
        lastFragmentText = item.text;
        continue;
      }

      if (item.isSealedInlineCodeFragment) {
        const fullWidth = reservedWidth + item.fullWidth;
        if (fullWidth > remainingWidth + continuationSlackPx + 0.01) {
          return {
            consumedWidth,
            endedAtGroupEnd: false,
            endedInsideFragment: false,
            lastFragmentText,
            nextFragmentText: item.text,
            nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          };
        }
        remainingWidth = Math.max(0, remainingWidth - fullWidth);
        consumedWidth += fullWidth;
      } else {
        const availableLineWidth = Math.max(1, remainingWidth - reservedWidth);
        const line = layoutNextLine(item.prepared, LINE_START_CURSOR, availableLineWidth);
        if (line == null || cursorsMatch(LINE_START_CURSOR, line.end)) {
          return {
            consumedWidth,
            endedAtGroupEnd: false,
            endedInsideFragment: true,
            lastFragmentText,
            nextFragmentText: item.text,
            nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          };
        }
        remainingWidth = Math.max(0, remainingWidth - reservedWidth - line.width);
        consumedWidth += reservedWidth + line.width;
        if (!cursorsMatch(line.end, item.endCursor)) {
          return {
            consumedWidth,
            endedAtGroupEnd: false,
            endedInsideFragment: true,
            lastFragmentText,
            nextFragmentText: item.text,
            nextStartsAfterCodeWhitespace: item.startsAfterCodeWhitespace,
          };
        }
      }

      lineHasContent = true;
      chargedChrome = true;
      pendingSpaceWidth = 0;
      lastFragmentText = item.text;
    }

    return {
      consumedWidth,
      endedAtGroupEnd: true,
      endedInsideFragment: false,
      lastFragmentText,
      nextFragmentText: null,
      nextStartsAfterCodeWhitespace: false,
    };
  };

const codeGroupFitEndsAtFriendlyBoundary = (fit: InlineCodeBoundaryFit): boolean => {
    if (fit.endedInsideFragment) {
      return false;
    }
    if (fit.endedAtGroupEnd) {
      return true;
    }
    if (fit.nextStartsAfterCodeWhitespace) {
      return true;
    }
    const lastFragmentText = fit.lastFragmentText ?? "";
    return isSealedInlineCodeFragment(lastFragmentText) && fit.nextFragmentText != null;
  };


  return {
    codeGroupFitEndsAtFriendlyBoundary,
    measureCodeGroupFitWithinWidth,
  };
}
