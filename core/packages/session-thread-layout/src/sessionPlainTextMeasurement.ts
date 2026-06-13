import {
  SESSION_TEXT_MEASUREMENT_CACHE_LIMIT,
  buildPreparedContentKey,
  measureCollapsedSpaceWidth,
  normalizeHeight,
  pruneCache,
  segmentImplicitWordBreaks,
} from "./sessionTextMeasurement";
import {
  SOFT_HYPHEN,
  SOFT_HYPHEN_CURRENT_LINE_FIT_GUARD_PX,
  ZERO_WIDTH_SPACE,
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
  normalizeCollapsedPlainTextLineText,
  resolvePlainTextDelimitedStartRatioThreshold,
  snapDelimitedUrlContinuationFit,
  splitPlainTextWrapFragments,
  stripDiscretionaryBreakMarkers,
} from "./sessionPlainTextMeasurementWrap";
import {
  resolveContinuedPlainTextPlacement,
  resolveFreshPlainTextPlacement,
  type PlainTextPlacement,
} from "./sessionPlainTextMeasurementPlacement";

type SessionPlainTextDebugWindow = Window & {
  __ctxForcePlainTextDebug?: boolean;
  __ctxPlainTextDebugTarget?: string;
  __ctxPlainTextDebugWidth?: number;
  __ctxPlainTextDebug?: {
    lineCount: number;
    lines: string[];
    lineWidths: number[];
    text: string;
    width: number;
  };
};

const plainTextBlockHeightCache = new Map<string, number>();
function measureCollapsedPlainTextLineHeight(params: {
  cacheKey: string;
  text: string;
  font: string;
  width: number;
  lineHeight: number;
}): number {
  const normalizedText = normalizeCollapsedPlainTextLineText(params.text);
  const words = normalizedText.match(/\S+/g) ?? [];
  if (words.length === 0) {
    return params.lineHeight;
  }
  const plainTextDebugWindow =
    typeof window !== "undefined" ? (window as SessionPlainTextDebugWindow) : null;
  const debugPlainText =
    (plainTextDebugWindow?.__ctxForcePlainTextDebug === true ||
      plainTextDebugWindow?.__ctxPlainTextDebugTarget === "*" ||
      plainTextDebugWindow?.__ctxPlainTextDebugTarget === normalizedText) &&
    (plainTextDebugWindow?.__ctxPlainTextDebugWidth == null ||
      plainTextDebugWindow.__ctxPlainTextDebugWidth === params.width);
  const debugLines: string[] = [];
  const debugLineWidths: number[] = [];
  let currentLineText = "";
  const appendLineText = (text: string, prefixSpace: boolean) => {
    if (prefixSpace && currentLineText.length > 0) {
      currentLineText += " ";
    }
    currentLineText += text;
  };
  const flushLine = () => {
    if (debugPlainText && currentLineText.length > 0) {
      debugLines.push(currentLineText);
      debugLineWidths.push(
        measureSingleLineTextWidth({
          cacheKey: buildPreparedContentKey(`${params.cacheKey}:debug-line:${debugLines.length}`, currentLineText),
          text: currentLineText,
          font: params.font,
        }),
      );
    }
    currentLineText = "";
  };

  const maxWidth = Math.max(1, params.width);
  const collapsedSpaceWidth = measureCollapsedSpaceWidth(params.font);
  let lineCount = 1;
  let lineHasContent = false;
  let remainingWidth = maxWidth;
  let wordIndex = 0;
  let remainder: string | null = null;
  let remainderFragments: string[] | null = null;
  const applyPlacement = (placement: PlainTextPlacement) => {
    remainingWidth = Math.max(0, remainingWidth - placement.consumedWidth);
    lineHasContent = true;
    appendLineText(placement.consumedText, placement.prependSpace);
    remainder = placement.remainder;
    remainderFragments = placement.remainderFragments;
    if (placement.advanceWordIndex) {
      wordIndex += 1;
    }
    if (placement.flushAfterPlacement && (wordIndex < words.length || remainder != null)) {
      flushLine();
      lineCount += 1;
      lineHasContent = false;
      remainingWidth = maxWidth;
    }
  };

  while (wordIndex < words.length || remainder != null) {
    const word: string = remainder ?? words[wordIndex]!;
    const displayWord = stripDiscretionaryBreakMarkers(word);
    const wordContainsSoftHyphen = word.includes(SOFT_HYPHEN);
    const wordContainsZeroWidthBreak = word.includes(ZERO_WIDTH_SPACE);
    const wordFragments = remainderFragments ?? splitPlainTextWrapFragments(word);
    const usesDelimitedWrapping = wordFragments.length > 1;
    const implicitWordBreakSegments =
      !usesDelimitedWrapping && !wordContainsSoftHyphen && !wordContainsZeroWidthBreak
        ? segmentImplicitWordBreaks(displayWord)
        : [displayWord];
    const usesImplicitWordBreaking = implicitWordBreakSegments.length > 1;
    const wordWidth = measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKey}:word:${wordIndex}`, displayWord),
      text: displayWord,
      font: params.font,
    });
    const reservedWidth = lineHasContent ? collapsedSpaceWidth : 0;

    if (lineHasContent && reservedWidth + wordWidth <= remainingWidth + 0.01) {
      remainingWidth = Math.max(0, remainingWidth - reservedWidth - wordWidth);
      appendLineText(displayWord, true);
      remainder = null;
      remainderFragments = null;
      wordIndex += 1;
      continue;
    }

    if (lineHasContent) {
      const availableWidth = Math.max(0, remainingWidth - reservedWidth);
      const wordFitsFreshLine = wordWidth <= maxWidth + 0.01;
      const continuedPlacement = resolveContinuedPlainTextPlacement({
        cacheKeyPrefix: `${params.cacheKey}:word:${wordIndex}`,
        currentLineText,
        word,
        displayWord,
        font: params.font,
        availableWidth,
        maxWidth,
        wordFitsFreshLine,
        usesDelimitedWrapping,
        wordFragments,
        wordContainsSoftHyphen,
        wordContainsZeroWidthBreak,
        usesImplicitWordBreaking,
        implicitWordBreakSegments,
      });
      if (continuedPlacement?.kind === "placed") {
        applyPlacement(continuedPlacement.placement);
        continue;
      }
      flushLine();
      lineCount += 1;
      lineHasContent = false;
      remainingWidth = maxWidth;
      continue;
    }

    applyPlacement(
      resolveFreshPlainTextPlacement({
        cacheKeyPrefix: `${params.cacheKey}:word:${wordIndex}`,
        wordIndex,
        word,
        displayWord,
        font: params.font,
        remainingWidth,
        wordWidth,
        usesDelimitedWrapping,
        wordFragments,
        wordContainsSoftHyphen,
        wordContainsZeroWidthBreak,
        usesImplicitWordBreaking,
        implicitWordBreakSegments,
      }),
    );
  }

  flushLine();
  if (debugPlainText && plainTextDebugWindow) {
    plainTextDebugWindow.__ctxPlainTextDebug = {
      lineCount,
      lines: debugLines,
      lineWidths: debugLineWidths,
      text: normalizedText,
      width: maxWidth,
    };
  }

  return lineCount * params.lineHeight;
}

export function clearSessionPlainTextMeasurementCaches(): void {
  plainTextBlockHeightCache.clear();
}

export function measureSessionPlainTextBlockHeight(params: {
  cacheKey: string;
  text: string;
  font: string;
  width: number;
  lineHeight: number;
}): number {
  const normalizedText = String(params.text ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[\r\f]/g, "\n");

  const measurementKey = `${params.cacheKey}:plain-text:${params.font}:${params.width}:${params.lineHeight}`;
  const cached = plainTextBlockHeightCache.get(measurementKey);
  if (cached != null) {
    return cached;
  }

  const measuredHeight = normalizeHeight(
    normalizedText.split("\n").reduce((sum, line, index) => {
      if (line.length === 0) {
        return sum + params.lineHeight;
      }
      return (
        sum +
        measureCollapsedPlainTextLineHeight({
          cacheKey: `${measurementKey}:line:${index}`,
          text: line,
          font: params.font,
          width: params.width,
          lineHeight: params.lineHeight,
        })
      );
    }, 0),
  );
  plainTextBlockHeightCache.set(measurementKey, measuredHeight);
  pruneCache(plainTextBlockHeightCache, SESSION_TEXT_MEASUREMENT_CACHE_LIMIT);
  return measuredHeight;
}
