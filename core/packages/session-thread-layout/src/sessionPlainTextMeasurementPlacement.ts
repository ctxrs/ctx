import {
  SOFT_HYPHEN_CURRENT_LINE_FIT_GUARD_PX,
  acceptsAbsolutePathContinuationOnCurrentLine,
  acceptsUrlContinuationOnCurrentLine,
  findLargestCollapsedPlainTextPrefixThatFits,
  isAbsolutePathLikeDelimitedWord,
  isPathLikeDelimitedWord,
  lineEndsWithUrlQueryPair,
  measureImplicitWordBreakFit,
  measurePlainTextDelimitedTokenFit,
  measureSingleLineTextWidth,
  measureSoftHyphenBreakFit,
  measureZeroWidthSpaceBreakFit,
  resolvePlainTextDelimitedStartRatioThreshold,
  snapDelimitedUrlContinuationFit,
  splitPlainTextWrapFragments,
} from "./sessionPlainTextMeasurementWrap";
import { buildPreparedContentKey } from "./sessionTextMeasurement";

export type PlainTextPlacement = {
  consumedText: string;
  consumedWidth: number;
  remainder: string | null;
  remainderFragments: string[] | null;
  prependSpace: boolean;
  flushAfterPlacement: boolean;
  advanceWordIndex: boolean;
};

export type PlainTextPlacementResolution =
  | {
      kind: "placed";
      placement: PlainTextPlacement;
    }
  | {
      kind: "break-line";
    };

function placed(placement: PlainTextPlacement): PlainTextPlacementResolution {
  return { kind: "placed", placement };
}

export function resolveContinuedPlainTextPlacement(params: {
  cacheKeyPrefix: string;
  currentLineText: string;
  word: string;
  displayWord: string;
  font: string;
  availableWidth: number;
  maxWidth: number;
  wordFitsFreshLine: boolean;
  usesDelimitedWrapping: boolean;
  wordFragments: string[];
  wordContainsSoftHyphen: boolean;
  wordContainsZeroWidthBreak: boolean;
  usesImplicitWordBreaking: boolean;
  implicitWordBreakSegments: string[];
}): PlainTextPlacementResolution | null {
  if (params.usesDelimitedWrapping) {
    const isUrlLikeWord = params.word.includes("://");
    const isAbsolutePathLikeWord = isAbsolutePathLikeDelimitedWord(params.word);
    if ((isUrlLikeWord || isAbsolutePathLikeWord || !params.wordFitsFreshLine) && params.availableWidth > 0.01) {
      const currentFit = snapDelimitedUrlContinuationFit({
        cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
        word: params.word,
        font: params.font,
        fit: measurePlainTextDelimitedTokenFit({
          cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
          text: params.word,
          fragments: params.wordFragments,
          font: params.font,
          width: params.availableWidth,
          allowPartialFragment: true,
          allowPartialAfterConsumedText: true,
        }),
      });
      const freshFit = measurePlainTextDelimitedTokenFit({
        cacheKeyPrefix: `${params.cacheKeyPrefix}:fresh`,
        text: params.word,
        fragments: params.wordFragments,
        font: params.font,
        width: params.maxWidth,
        allowPartialFragment: true,
        allowPartialAfterConsumedText: params.word.includes("://"),
      });
      const currentFitRatio =
        freshFit.consumedWidth > 0 ? currentFit.consumedWidth / freshFit.consumedWidth : 1;
      const startRatioThreshold = resolvePlainTextDelimitedStartRatioThreshold(params.word);
      const blocksPathContinuationAfterQueryTail =
        lineEndsWithUrlQueryPair(params.currentLineText) && isPathLikeDelimitedWord(params.word);
      const acceptsCurrentDelimitedContinuation = blocksPathContinuationAfterQueryTail
        ? false
        : isUrlLikeWord
          ? acceptsUrlContinuationOnCurrentLine({
              word: params.word,
              consumedText: currentFit.consumedText,
              currentFitRatio,
            })
          : isAbsolutePathLikeWord
            ? acceptsAbsolutePathContinuationOnCurrentLine(currentFit.consumedText)
            : currentFitRatio >= startRatioThreshold;
      if (currentFit.consumedText.length > 0 && acceptsCurrentDelimitedContinuation) {
        return placed({
          consumedText: currentFit.consumedText,
          consumedWidth: currentFit.consumedWidth,
          remainder: currentFit.remainder.length > 0 ? currentFit.remainder : null,
          remainderFragments:
            currentFit.remainder.length > 0
              ? splitPlainTextWrapFragments(currentFit.remainder)
              : null,
          prependSpace: true,
          flushAfterPlacement: true,
          advanceWordIndex: currentFit.remainder.length === 0,
        });
      }
    }
  }
  if (!params.usesDelimitedWrapping && params.availableWidth > 0.01) {
    const softHyphenFit =
      params.wordContainsSoftHyphen
        ? measureSoftHyphenBreakFit({
            cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
            text: params.word,
            font: params.font,
            width: Math.max(1, params.availableWidth - SOFT_HYPHEN_CURRENT_LINE_FIT_GUARD_PX),
          })
        : null;
    if (softHyphenFit != null) {
      return placed({
        consumedText: softHyphenFit.consumedText,
        consumedWidth: softHyphenFit.consumedWidth,
        remainder: softHyphenFit.remainder.length > 0 ? softHyphenFit.remainder : null,
        remainderFragments: null,
        prependSpace: true,
        flushAfterPlacement: true,
        advanceWordIndex: softHyphenFit.remainder.length === 0,
      });
    }
    const zeroWidthBreakFit =
      params.wordContainsZeroWidthBreak
        ? measureZeroWidthSpaceBreakFit({
            cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
            text: params.word,
            font: params.font,
            width: params.availableWidth,
          })
        : null;
    if (zeroWidthBreakFit != null) {
      return placed({
        consumedText: zeroWidthBreakFit.consumedText,
        consumedWidth: zeroWidthBreakFit.consumedWidth,
        remainder: zeroWidthBreakFit.remainder.length > 0 ? zeroWidthBreakFit.remainder : null,
        remainderFragments: null,
        prependSpace: true,
        flushAfterPlacement: true,
        advanceWordIndex: zeroWidthBreakFit.remainder.length === 0,
      });
    }
    if (params.wordFitsFreshLine) {
      return { kind: "break-line" };
    }
    const implicitWordBreakFit =
      params.usesImplicitWordBreaking
        ? measureImplicitWordBreakFit({
            cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
            segments: params.implicitWordBreakSegments,
            font: params.font,
            width: params.availableWidth,
          })
        : null;
    if (implicitWordBreakFit != null) {
      return placed({
        consumedText: implicitWordBreakFit.consumedText,
        consumedWidth: implicitWordBreakFit.consumedWidth,
        remainder: implicitWordBreakFit.remainder.length > 0 ? implicitWordBreakFit.remainder : null,
        remainderFragments: null,
        prependSpace: true,
        flushAfterPlacement: true,
        advanceWordIndex: implicitWordBreakFit.remainder.length === 0,
      });
    }
    const fittingPrefix = findLargestCollapsedPlainTextPrefixThatFits({
      cacheKeyPrefix: `${params.cacheKeyPrefix}:continued`,
      text: params.displayWord,
      font: params.font,
      width: params.availableWidth,
    });
    if (fittingPrefix.prefixWidth > 0) {
      return placed({
        consumedText: params.displayWord.slice(
          0,
          params.displayWord.length - fittingPrefix.remainder.length,
        ),
        consumedWidth: fittingPrefix.prefixWidth,
        remainder: fittingPrefix.remainder.length > 0 ? fittingPrefix.remainder : null,
        remainderFragments: null,
        prependSpace: true,
        flushAfterPlacement: true,
        advanceWordIndex: fittingPrefix.remainder.length === 0,
      });
    }
  }
  return { kind: "break-line" };
}

export function resolveFreshPlainTextPlacement(params: {
  cacheKeyPrefix: string;
  wordIndex: number;
  word: string;
  displayWord: string;
  font: string;
  remainingWidth: number;
  wordWidth: number;
  usesDelimitedWrapping: boolean;
  wordFragments: string[];
  wordContainsSoftHyphen: boolean;
  wordContainsZeroWidthBreak: boolean;
  usesImplicitWordBreaking: boolean;
  implicitWordBreakSegments: string[];
}): PlainTextPlacement {
  if (params.usesDelimitedWrapping) {
    const fittingPrefix = measurePlainTextDelimitedTokenFit({
      cacheKeyPrefix: `${params.cacheKeyPrefix}:fresh`,
      text: params.word,
      fragments: params.wordFragments,
      font: params.font,
      width: params.remainingWidth,
      allowPartialFragment: true,
    });
    return {
      consumedText: fittingPrefix.consumedText,
      consumedWidth: fittingPrefix.consumedWidth,
      remainder: fittingPrefix.remainder.length > 0 ? fittingPrefix.remainder : null,
      remainderFragments:
        fittingPrefix.remainder.length > 0
          ? splitPlainTextWrapFragments(fittingPrefix.remainder)
          : null,
      prependSpace: false,
      flushAfterPlacement: fittingPrefix.remainder.length > 0,
      advanceWordIndex: fittingPrefix.remainder.length === 0,
    };
  }

  if (params.wordWidth <= params.remainingWidth + 0.01) {
    return {
      consumedText: params.displayWord,
      consumedWidth: params.wordWidth,
      remainder: null,
      remainderFragments: null,
      prependSpace: false,
      flushAfterPlacement: false,
      advanceWordIndex: true,
    };
  }

  const softHyphenFreshFit =
    params.wordContainsSoftHyphen
      ? measureSoftHyphenBreakFit({
          cacheKeyPrefix: `${params.cacheKeyPrefix}:fresh`,
          text: params.word,
          font: params.font,
          width: params.remainingWidth,
        })
      : null;
  if (softHyphenFreshFit != null) {
    return {
      consumedText: softHyphenFreshFit.consumedText,
      consumedWidth: softHyphenFreshFit.consumedWidth,
      remainder: softHyphenFreshFit.remainder.length > 0 ? softHyphenFreshFit.remainder : null,
      remainderFragments: null,
      prependSpace: false,
      flushAfterPlacement: true,
      advanceWordIndex: softHyphenFreshFit.remainder.length === 0,
    };
  }

  const zeroWidthFreshFit =
    params.wordContainsZeroWidthBreak
      ? measureZeroWidthSpaceBreakFit({
          cacheKeyPrefix: `${params.cacheKeyPrefix}:fresh`,
          text: params.word,
          font: params.font,
          width: params.remainingWidth,
        })
      : null;
  if (zeroWidthFreshFit != null) {
    return {
      consumedText: zeroWidthFreshFit.consumedText,
      consumedWidth: zeroWidthFreshFit.consumedWidth,
      remainder: zeroWidthFreshFit.remainder.length > 0 ? zeroWidthFreshFit.remainder : null,
      remainderFragments: null,
      prependSpace: false,
      flushAfterPlacement: true,
      advanceWordIndex: zeroWidthFreshFit.remainder.length === 0,
    };
  }

  const implicitWordFreshFit =
    params.usesImplicitWordBreaking
      ? measureImplicitWordBreakFit({
          cacheKeyPrefix: `${params.cacheKeyPrefix}:fresh`,
          segments: params.implicitWordBreakSegments,
          font: params.font,
          width: params.remainingWidth,
        })
      : null;
  if (implicitWordFreshFit != null) {
    return {
      consumedText: implicitWordFreshFit.consumedText,
      consumedWidth: implicitWordFreshFit.consumedWidth,
      remainder: implicitWordFreshFit.remainder.length > 0 ? implicitWordFreshFit.remainder : null,
      remainderFragments: null,
      prependSpace: false,
      flushAfterPlacement: true,
      advanceWordIndex: implicitWordFreshFit.remainder.length === 0,
    };
  }

  const fittingPrefix = findLargestCollapsedPlainTextPrefixThatFits({
    cacheKeyPrefix: params.cacheKeyPrefix,
    text: params.displayWord,
    font: params.font,
    width: params.remainingWidth,
  });
  return {
    consumedText: params.displayWord.slice(
      0,
      params.displayWord.length - fittingPrefix.remainder.length,
    ),
    consumedWidth: fittingPrefix.prefixWidth,
    remainder: fittingPrefix.remainder.length > 0 ? fittingPrefix.remainder : null,
    remainderFragments: null,
    prependSpace: false,
    flushAfterPlacement: true,
    advanceWordIndex: fittingPrefix.remainder.length === 0,
  };
}

export function measureNextWordWidth(params: {
  cacheKeyPrefix: string;
  nextWord: string | null;
  font: string;
}): number {
  if (params.nextWord == null) {
    return 0;
  }
  return measureSingleLineTextWidth({
    cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:next-word`, params.nextWord),
    text: params.nextWord,
    font: params.font,
  });
}
