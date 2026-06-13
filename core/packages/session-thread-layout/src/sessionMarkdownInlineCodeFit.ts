import { layoutNextLine } from "@chenglou/pretext";
import { browserAllowsInlineCodeLeadingHang } from "./sessionMarkdownBrowserProfile";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import { createInlineCodeBoundaryFitMeasurer } from "./sessionMarkdownInlineCodeFitBoundary";
import {
  isShortExtensionPathLikeFragment,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  resolveInlineCodeWrapChromeWidth,
  shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment,
  type InlineCodeTrailingPlainInfo,
} from "./sessionMarkdownInlineCodeFitRules";
import { LINE_START_CURSOR, cursorsMatch } from "./sessionMarkdownMeasurementCore";

type InlineSegmentItem = Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
export {
  allowsChromiumDottedBoundaryHang,
  INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
  INLINE_CODE_PATH_DELIMITER_CONTINUATION_MIN_SPARE_PX,
  isShortExtensionPathLikeFragment,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeProseStartSeamGuardPx,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  resolveInlineCodeWrapChromeWidth,
  shouldApplyInlineCodeSoftBreakTextStartGuard,
  shouldBreakBeforePartialDottedCallContinuation,
  shouldBreakBeforePartialDottedStemPathTailContinuation,
  shouldBreakBeforePartialSealedDottedPathContinuation,
  shouldBreakBeforePathDelimiterNearFitContinuation,
  shouldBreakBeforeWhitespaceSeparatedInlineCodeFragment,
} from "./sessionMarkdownInlineCodeFitRules";
export type {
  InlineCodeBoundaryFit,
  InlineCodeTrailingPlainInfo,
} from "./sessionMarkdownInlineCodeFitRules";

export function createInlineCodeFitPlanner(params: {
  items: readonly PreparedInlineLayoutItem[];
  maxWidth: number;
}) {
  const { items, maxWidth } = params;
  const preferredCodeGroupStartWidths = new Map<number, number>();
  const dottedPathCodeGroupStartClusterWidths = new Map<number, number>();
  const wholeCodeGroupInlineWidths = new Map<number, number>();

  const isPathLikeContinuationItem = (item: InlineSegmentItem): boolean =>
    item.text.includes("/") || item.text.includes("\\") || item.isPathTailFragment;

  const findTrailingPlainSegmentInfo = (startIndex: number): InlineCodeTrailingPlainInfo => {
    const firstItem = items[startIndex];
    if (firstItem?.kind !== "segment" || firstItem.codeGroupId == null) {
      return {
        width: 0,
        text: "",
        hasFollowingInlineCode: false,
        isDecoratedText: false,
        startsAfterCollapsedSoftBreak: false,
      };
    }
    const codeGroupId = firstItem.codeGroupId;
    for (let index = startIndex + 1; index < items.length; index += 1) {
      const item = items[index]!;
      if (item.kind === "hardBreak") {
        return {
          width: 0,
          text: "",
          hasFollowingInlineCode: false,
          isDecoratedText: false,
          startsAfterCollapsedSoftBreak: false,
        };
      }
      if (item.kind === "space") {
        continue;
      }
      if (item.codeGroupId === codeGroupId) {
        continue;
      }
      if (item.codeGroupId != null) {
        return {
          width: 0,
          text: "",
          hasFollowingInlineCode: false,
          isDecoratedText: false,
          startsAfterCollapsedSoftBreak: false,
        };
      }
      if (item.text.trim().length === 0) {
        continue;
      }
      let hasFollowingInlineCode = false;
      for (let nextIndex = index + 1; nextIndex < items.length; nextIndex += 1) {
        const nextItem = items[nextIndex]!;
        if (nextItem.kind === "hardBreak") {
          break;
        }
        if (nextItem.kind === "space") {
          continue;
        }
        if (nextItem.startsAfterCollapsedSoftBreak) {
          break;
        }
        if (nextItem.codeGroupId != null) {
          hasFollowingInlineCode = true;
          break;
        }
      }
      return {
        width: item.fullWidth,
        text: item.text,
        hasFollowingInlineCode,
        isDecoratedText: item.isDecoratedText,
        startsAfterCollapsedSoftBreak: item.startsAfterCollapsedSoftBreak,
      };
    }
    return {
      width: 0,
      text: "",
      hasFollowingInlineCode: false,
      isDecoratedText: false,
      startsAfterCollapsedSoftBreak: false,
    };
  };

  const measureAttachedTrailingPlainWidth = (startIndex: number): number => {
    return findTrailingPlainSegmentInfo(startIndex).width;
  };

  const measureCodeGroupTrailingPlainWidth = (startIndex: number): number => {
    return measureCodeGroupTrailingPlainInfo(startIndex).width;
  };

  const measureCodeGroupTrailingPlainInfo = (startIndex: number): InlineCodeTrailingPlainInfo => {
    return findTrailingPlainSegmentInfo(startIndex);
  };

  const shouldPreserveSealedInlineCodeBoundary = (params: {
    sealedBoundary: boolean;
    reservedWidth: number;
    remainingWidth: number;
    item: InlineSegmentItem;
    lastFragmentText?: string | null;
  }): boolean => {
    const lastFragmentIsShortExtensionPath = isShortExtensionPathLikeFragment(params.lastFragmentText);
    const allowChromiumDottedPathBoundaryContinuation =
      browserAllowsInlineCodeLeadingHang() &&
      params.item.codeGroupStartsAfterText &&
      !lastFragmentIsShortExtensionPath &&
      params.item.codeGroupHasDottedPath &&
      (params.item.text.includes(".") || params.item.isPathTailFragment);
    return (
      params.sealedBoundary &&
      !allowChromiumDottedPathBoundaryContinuation &&
      params.reservedWidth + params.item.fullWidth > params.remainingWidth + 0.01
    );
  };

  const measurePreferredCodeGroupStartWidth = (startIndex: number): number => {
    const firstItem = items[startIndex];
    if (firstItem?.kind !== "segment" || firstItem.codeGroupId == null) {
      return 0;
    }
    const codeGroupId = firstItem.codeGroupId;
    let lineWidth = 0;
    let remainingWidth = maxWidth;
    let lineHasContent = firstItem.codeGroupStartsAfterText;
    let pendingSpaceWidth = 0;
    let chargedChrome = false;
    let lastFragmentText: string | null = null;

    for (let index = startIndex; index < items.length; index += 1) {
      const item = items[index]!;
      if (item.kind === "hardBreak") {
        break;
      }
      if (item.kind === "space") {
        if (item.codeGroupId !== codeGroupId) {
          break;
        }
        if (lineHasContent) {
          pendingSpaceWidth = item.width;
        }
        continue;
      }
      if (item.codeGroupId !== codeGroupId) {
        break;
      }

      const reservedWidth =
        (lineHasContent ? pendingSpaceWidth : 0) +
        resolveInlineCodeWrapChromeWidth({
          chromeWidth: item.chromeWidth,
          codeGroupHasWhitespace: item.codeGroupHasWhitespace,
          codeGroupStartsAfterText: item.codeGroupStartsAfterText,
          chargedChrome,
          isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
          prefersFreshLineStart: item.prefersFreshLineStart,
          lineHasContent,
          startsAtLineStart: !lineHasContent,
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
        break;
      }
      const availableWidth = Math.max(1, remainingWidth - reservedWidth);
      if (item.isSealedInlineCodeFragment) {
        const fullWidth = reservedWidth + item.fullWidth;
        if (lineHasContent && fullWidth > remainingWidth + continuationSlackPx + 0.01) {
          break;
        }
        const overflowed = fullWidth > remainingWidth + 0.01;
        lineWidth += fullWidth;
        remainingWidth = overflowed ? 0 : Math.max(0, remainingWidth - fullWidth);
        chargedChrome = true;
        lineHasContent = true;
        pendingSpaceWidth = 0;
        lastFragmentText = item.text;
        if (overflowed) {
          break;
        }
        continue;
      }

      const line = layoutNextLine(item.prepared, LINE_START_CURSOR, availableWidth);
      if (line == null || cursorsMatch(LINE_START_CURSOR, line.end)) {
        break;
      }

      lineWidth += reservedWidth + line.width;
      remainingWidth = Math.max(0, remainingWidth - reservedWidth - line.width);
      chargedChrome = true;
      lineHasContent = true;
      pendingSpaceWidth = 0;
      lastFragmentText = item.text;
      if (!cursorsMatch(line.end, item.endCursor)) {
        break;
      }
    }

    return lineWidth;
  };

  const measureDottedPathCodeGroupStartClusterWidth = (startIndex: number): number => {
    const firstItem = items[startIndex];
    if (firstItem?.kind !== "segment" || firstItem.codeGroupId == null) {
      return 0;
    }

    let lineWidth = 0;
    let pendingSpaceWidth = 0;
    let lineHasContent = firstItem.codeGroupStartsAfterText;
    let chargedChrome = false;
    let sawPathLikeFragment = false;
    let sawDottedStem = false;

    for (let index = startIndex; index < items.length; index += 1) {
      const item = items[index]!;
      if (item.kind === "hardBreak" || item.kind === "space" || item.codeGroupId !== firstItem.codeGroupId) {
        break;
      }

      lineWidth +=
        (lineHasContent ? pendingSpaceWidth : 0) +
        resolveInlineCodeWrapChromeWidth({
          chromeWidth: item.chromeWidth,
          codeGroupHasWhitespace: item.codeGroupHasWhitespace,
          codeGroupStartsAfterText: item.codeGroupStartsAfterText,
          chargedChrome,
          isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
          prefersFreshLineStart: item.prefersFreshLineStart,
          lineHasContent,
          startsAtLineStart: !lineHasContent,
        }) +
        item.fullWidth;
      lineHasContent = true;
      chargedChrome = true;
      pendingSpaceWidth = 0;
      sawPathLikeFragment ||= item.text.includes("/") || item.text.includes("\\") || item.isPathTailFragment;
      sawDottedStem ||= item.text.endsWith(".");
    }

    return sawPathLikeFragment && sawDottedStem ? lineWidth : 0;
  };

  const measureWholeCodeGroupInlineWidth = (startIndex: number): number => {
    const firstItem = items[startIndex];
    if (firstItem?.kind !== "segment" || firstItem.codeGroupId == null) {
      return 0;
    }
    const codeGroupId = firstItem.codeGroupId;
    let lineWidth = 0;
    let pendingSpaceWidth = 0;
    let lineHasContent = firstItem.codeGroupStartsAfterText;
    let chargedChrome = false;

    for (let index = startIndex; index < items.length; index += 1) {
      const item = items[index]!;
      if (item.kind === "hardBreak") {
        break;
      }
      if (item.kind === "space") {
        if (item.codeGroupId !== codeGroupId) {
          break;
        }
        if (lineHasContent) {
          pendingSpaceWidth = item.width;
        }
        continue;
      }
      if (item.codeGroupId !== codeGroupId) {
        break;
      }

      lineWidth +=
        (lineHasContent ? pendingSpaceWidth : 0) +
        resolveInlineCodeWrapChromeWidth({
          chromeWidth: item.chromeWidth,
          codeGroupHasWhitespace: item.codeGroupHasWhitespace,
          codeGroupStartsAfterText: item.codeGroupStartsAfterText,
          chargedChrome,
          isFirstCodeGroupFragment: item.isFirstCodeGroupFragment,
          prefersFreshLineStart: item.prefersFreshLineStart,
          lineHasContent,
          startsAtLineStart: !lineHasContent,
        }) +
        item.fullWidth;
      lineHasContent = true;
      chargedChrome = true;
      pendingSpaceWidth = 0;
    }

    return lineWidth;
  };

  const { measureCodeGroupFitWithinWidth, codeGroupFitEndsAtFriendlyBoundary } =
    createInlineCodeBoundaryFitMeasurer({
      items,
      maxWidth,
      wholeCodeGroupInlineWidths,
      isPathLikeContinuationItem,
    });

  for (let index = 0; index < items.length; index += 1) {
    const item = items[index]!;
    if (item.kind === "segment" && item.codeGroupId != null && item.isFirstCodeGroupFragment) {
      preferredCodeGroupStartWidths.set(index, measurePreferredCodeGroupStartWidth(index));
      dottedPathCodeGroupStartClusterWidths.set(index, measureDottedPathCodeGroupStartClusterWidth(index));
      wholeCodeGroupInlineWidths.set(index, measureWholeCodeGroupInlineWidth(index));
    }
  }

  return {
    codeGroupFitEndsAtFriendlyBoundary,
    dottedPathCodeGroupStartClusterWidths,
    measureAttachedTrailingPlainWidth,
    measureCodeGroupTrailingPlainWidth,
    measureCodeGroupTrailingPlainInfo,
    measureCodeGroupFitWithinWidth,
    preferredCodeGroupStartWidths,
    shouldPreserveSealedInlineCodeBoundary,
    wholeCodeGroupInlineWidths,
  };
}
