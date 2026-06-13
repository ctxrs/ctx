import {
  buildPreparedContentKey,
  getPreparedTextWithSegments,
  measureSingleLineLayout,
  segmentGraphemes,
} from "./sessionTextMeasurement";

export const SOFT_HYPHEN = "\u00ad";
const VISIBLE_HYPHEN = "-";
export const SOFT_HYPHEN_CURRENT_LINE_FIT_GUARD_PX = 1;
export const ZERO_WIDTH_SPACE = "\u200b";

export function normalizeCollapsedPlainTextLineText(text: string): string {
  return text.replace(/\u00a0/g, " ").replace(/\r\n?/g, "\n").replace(/\n/g, " ");
}

function stripSoftHyphens(text: string): string {
  return text.replaceAll(SOFT_HYPHEN, "");
}

export function stripDiscretionaryBreakMarkers(text: string): string {
  return stripSoftHyphens(text).replaceAll(ZERO_WIDTH_SPACE, "");
}

export function measureSingleLineTextWidth(params: {
  cacheKey: string;
  text: string;
  font: string;
}): number {
  const prepared = getPreparedTextWithSegments(params.cacheKey, params.text, params.font, "normal");
  return Math.max(0, measureSingleLineLayout(prepared)?.width ?? 0);
}

export function findLargestCollapsedPlainTextPrefixThatFits(params: {
  cacheKeyPrefix: string;
  text: string;
  font: string;
  width: number;
}): {
  prefixWidth: number;
  remainder: string;
} {
  const graphemes = segmentGraphemes(params.text);
  if (graphemes.length === 0) {
    return { prefixWidth: 0, remainder: "" };
  }

  let bestCount = 1;
  let bestWidth = measureSingleLineTextWidth({
    cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:prefix:1`, graphemes[0]!),
    text: graphemes[0]!,
    font: params.font,
  });
  let low = 1;
  let high = graphemes.length;

  while (low <= high) {
    const count = Math.floor((low + high) / 2);
    const prefix = graphemes.slice(0, count).join("");
    const width = measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:prefix:${count}`, prefix),
      text: prefix,
      font: params.font,
    });
    if (width <= params.width + 0.01) {
      bestCount = count;
      bestWidth = width;
      low = count + 1;
      continue;
    }
    high = count - 1;
  }

  return {
    prefixWidth: bestWidth,
    remainder: graphemes.slice(bestCount).join(""),
  };
}

function measureExplicitBreakFit(params: {
  cacheKeyPrefix: string;
  text: string;
  marker: string;
  visibleSuffix: string;
  font: string;
  width: number;
}): {
  consumedText: string;
  consumedWidth: number;
  remainder: string;
} | null {
  const fragments = params.text.split(params.marker);
  if (fragments.length <= 1) {
    return null;
  }

  let bestFit: {
    consumedText: string;
    consumedWidth: number;
    remainder: string;
  } | null = null;

  for (let breakIndex = 1; breakIndex < fragments.length; breakIndex += 1) {
    const consumedText = `${fragments.slice(0, breakIndex).join("")}${params.visibleSuffix}`;
    const consumedWidth = measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:explicit-break:${breakIndex}`, consumedText),
      text: consumedText,
      font: params.font,
    });
    if (consumedWidth > params.width + 0.01) {
      break;
    }
    bestFit = {
      consumedText,
      consumedWidth,
      remainder: fragments.slice(breakIndex).join(params.marker),
    };
  }

  return bestFit;
}

export function measureSoftHyphenBreakFit(params: {
  cacheKeyPrefix: string;
  text: string;
  font: string;
  width: number;
}) {
  return measureExplicitBreakFit({
    ...params,
    marker: SOFT_HYPHEN,
    visibleSuffix: VISIBLE_HYPHEN,
  });
}

export function measureZeroWidthSpaceBreakFit(params: {
  cacheKeyPrefix: string;
  text: string;
  font: string;
  width: number;
}) {
  return measureExplicitBreakFit({
    ...params,
    marker: ZERO_WIDTH_SPACE,
    visibleSuffix: "",
  });
}

export function measureImplicitWordBreakFit(params: {
  cacheKeyPrefix: string;
  segments: readonly string[];
  font: string;
  width: number;
}): {
  consumedText: string;
  consumedWidth: number;
  remainder: string;
} | null {
  if (params.segments.length <= 1) {
    return null;
  }

  let bestFit: {
    consumedText: string;
    consumedWidth: number;
    remainder: string;
  } | null = null;

  for (let breakIndex = 1; breakIndex < params.segments.length; breakIndex += 1) {
    const consumedText = params.segments.slice(0, breakIndex).join("");
    const consumedWidth = measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:implicit-break:${breakIndex}`, consumedText),
      text: consumedText,
      font: params.font,
    });
    if (consumedWidth > params.width + 0.01) {
      break;
    }
    bestFit = {
      consumedText,
      consumedWidth,
      remainder: params.segments.slice(breakIndex).join(""),
    };
  }

  return bestFit;
}

function isPlainTextDelimitedWrapCandidate(text: string): boolean {
  return text.includes("://") || /[\/\\?&=]/.test(text);
}

export function splitPlainTextWrapFragments(text: string): string[] {
  if (!isPlainTextDelimitedWrapCandidate(text)) {
    return [text];
  }

  const fragments: string[] = [];
  let current = "";
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index]!;
    current += character;
    if (character === ":" && text.slice(index + 1, index + 3) === "//") {
      current += "//";
      index += 2;
      fragments.push(current);
      current = "";
      continue;
    }
    if (
      character === "-" ||
      character === "." ||
      character === "/" ||
      character === "\\" ||
      character === "?" ||
      character === "&" ||
      character === "="
    ) {
      fragments.push(current);
      current = "";
    }
  }
  if (current.length > 0) {
    fragments.push(current);
  }

  return fragments.length > 0 ? fragments : [text];
}

export function measurePlainTextDelimitedTokenFit(params: {
  cacheKeyPrefix: string;
  text: string;
  fragments: readonly string[];
  font: string;
  width: number;
  allowPartialFragment: boolean;
  allowPartialAfterConsumedText?: boolean;
}): {
  consumedText: string;
  consumedWidth: number;
  remainder: string;
} {
  let remainingWidth = Math.max(1, params.width);
  let consumedText = "";
  let consumedWidth = 0;

  for (let index = 0; index < params.fragments.length; index += 1) {
    const fragment = params.fragments[index]!;
    const nextFragment = params.fragments[index + 1] ?? null;
    const fragmentWidth = measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:fragment:${index}`, fragment),
      text: fragment,
      font: params.font,
    });

    if (fragmentWidth <= remainingWidth + 0.01) {
      if (fragment.endsWith("=") && nextFragment != null) {
        const queryPairWidth = measureSingleLineTextWidth({
          cacheKey: buildPreparedContentKey(
            `${params.cacheKeyPrefix}:fragment-pair:${index}`,
            `${fragment}${nextFragment}`,
          ),
          text: `${fragment}${nextFragment}`,
          font: params.font,
        });
        if (queryPairWidth > remainingWidth + 0.01) {
          return {
            consumedText,
            consumedWidth,
            remainder: `${fragment}${params.fragments.slice(index + 1).join("")}`,
          };
        }
      }
      consumedText += fragment;
      consumedWidth += fragmentWidth;
      remainingWidth = Math.max(0, remainingWidth - fragmentWidth);
      continue;
    }

    if (
      !params.allowPartialFragment ||
      (consumedText.length > 0 && params.allowPartialAfterConsumedText !== true)
    ) {
      return {
        consumedText,
        consumedWidth,
        remainder: `${fragment}${params.fragments.slice(index + 1).join("")}`,
      };
    }

    const fittingPrefix = findLargestCollapsedPlainTextPrefixThatFits({
      cacheKeyPrefix: `${params.cacheKeyPrefix}:fragment:${index}`,
      text: fragment,
      font: params.font,
      width: remainingWidth,
    });
    const consumedFragmentText = fragment.slice(0, fragment.length - fittingPrefix.remainder.length);
    consumedText += consumedFragmentText;
    consumedWidth += fittingPrefix.prefixWidth;
    return {
      consumedText,
      consumedWidth,
      remainder: `${fittingPrefix.remainder}${params.fragments.slice(index + 1).join("")}`,
    };
  }

  return {
    consumedText,
    consumedWidth,
    remainder: "",
  };
}

export function snapDelimitedUrlContinuationFit(params: {
  cacheKeyPrefix: string;
  word: string;
  fit: {
    consumedText: string;
    consumedWidth: number;
    remainder: string;
  };
  font: string;
}): {
  consumedText: string;
  consumedWidth: number;
  remainder: string;
} {
  if (!params.word.includes("://") || params.fit.remainder.length === 0) {
    return params.fit;
  }

  const snappedPrefix = snapUrlContinuationPrefix(params.fit.consumedText);
  if (snappedPrefix.length === 0 || snappedPrefix.length === params.fit.consumedText.length) {
    return params.fit;
  }

  return {
    consumedText: snappedPrefix,
    consumedWidth: measureSingleLineTextWidth({
      cacheKey: buildPreparedContentKey(`${params.cacheKeyPrefix}:snapped`, snappedPrefix),
      text: snappedPrefix,
      font: params.font,
    }),
    remainder: params.word.slice(snappedPrefix.length),
  };
}

export function resolvePlainTextDelimitedStartRatioThreshold(text: string): number {
  return text.includes("://") ? 0.35 : 0.55;
}

function snapUrlContinuationPrefix(prefix: string): string {
  if (prefix.length === 0) {
    return prefix;
  }
  for (let index = prefix.length - 1; index >= 0; index -= 1) {
    const character = prefix[index]!;
    if (character === "-" || character === "/" || character === "?" || character === "&" || character === ".") {
      return prefix.slice(0, index + 1);
    }
  }
  return prefix;
}

export function lineEndsWithUrlQueryPair(text: string): boolean {
  const normalized = text.trim();
  if (normalized.length === 0) {
    return false;
  }
  return /(?:^|[/?&])[^/\s?&=]+=\S+$/.test(normalized);
}

export function isPathLikeDelimitedWord(text: string): boolean {
  return !text.includes("://") && /[./\\]/.test(text);
}

export function isAbsolutePathLikeDelimitedWord(text: string): boolean {
  return !text.includes("://") && /^[/\\]/.test(text);
}

export function acceptsAbsolutePathContinuationOnCurrentLine(consumedText: string): boolean {
  return consumedText.replace(/^[/\\]+/, "").length >= 4;
}

export function acceptsUrlContinuationOnCurrentLine(params: {
  word: string;
  consumedText: string;
  currentFitRatio: number;
}): boolean {
  if (params.consumedText.length === 0) {
    return false;
  }
  if (params.consumedText.includes("?")) {
    return true;
  }
  const threshold = Math.max(0.45, resolvePlainTextDelimitedStartRatioThreshold(params.word));
  return params.currentFitRatio >= threshold && params.consumedText.includes("-");
}
