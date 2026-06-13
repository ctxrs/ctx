import { describe, expect, it } from "vitest";
import { resolveContinuedPlainTextPlacement } from "./sessionPlainTextMeasurementPlacement";
import { SESSION_MARKDOWN_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";
import {
  segmentImplicitWordBreaks,
} from "./sessionTextMeasurement";
import {
  acceptsAbsolutePathContinuationOnCurrentLine,
  acceptsUrlContinuationOnCurrentLine,
  lineEndsWithUrlQueryPair,
  measureSoftHyphenBreakFit,
  measureZeroWidthSpaceBreakFit,
  resolvePlainTextDelimitedStartRatioThreshold,
} from "./sessionPlainTextMeasurementWrap";

const BODY_FONT =
  `${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontSizePx}px ${SESSION_MARKDOWN_MEASUREMENT_CONTRACT.typography.bodyFontFamily}`;

describe("sessionPlainTextMeasurement", () => {
  it("accepts a readable URL continuation on the current line", () => {
    expect(
      acceptsUrlContinuationOnCurrentLine({
        word: "https://example.com/assistant/streaming-tail/streaming-tail?ref=656",
        consumedText: "https://example.com/assistant/streaming-tail/",
        currentFitRatio: 0.72,
      }),
    ).toBe(true);
  });

  it("blocks path continuation after a URL query tail", () => {
    expect(lineEndsWithUrlQueryPair("https://example.com/chromium/docs/parity?ref=734")).toBe(true);
  });

  it("accepts only meaningful absolute path continuations on the current line", () => {
    expect(acceptsAbsolutePathContinuationOnCurrentLine("/Users")).toBe(true);
    expect(acceptsAbsolutePathContinuationOnCurrentLine("/U")).toBe(false);
  });

  it("uses implicit Thai word segments when available", () => {
    const segments = segmentImplicitWordBreaks("กรุงเทพคือสวยงามและต้อง");
    expect(segments.length).toBeGreaterThan(1);
    expect(segments.join("")).toBe("กรุงเทพคือสวยงามและต้อง");
  });

  it("uses soft hyphen break opportunities before grapheme slicing", () => {
    const fit = measureSoftHyphenBreakFit({
      cacheKeyPrefix: "plain-text-soft-hyphen",
      text: "super\u00adcalifragilistic",
      font: BODY_FONT,
      width: 60,
    });

    expect(fit).not.toBeNull();
    expect(fit?.consumedText.endsWith("-")).toBe(true);
    expect((fit?.remainder.length ?? 0) > 0).toBe(true);
  });

  it("keeps mid-line soft hyphen continuations when the word also fits on a fresh line", () => {
    const placement = resolveContinuedPlainTextPlacement({
      cacheKeyPrefix: "plain-text-soft-hyphen-public",
      currentLineText: "prefix",
      word: "super\u00adcalifragilistic",
      displayWord: "supercalifragilistic",
      font: BODY_FONT,
      availableWidth: 60,
      maxWidth: 200,
      wordFitsFreshLine: true,
      usesDelimitedWrapping: false,
      wordFragments: ["super\u00adcalifragilistic"],
      wordContainsSoftHyphen: true,
      wordContainsZeroWidthBreak: false,
      usesImplicitWordBreaking: false,
      implicitWordBreakSegments: ["supercalifragilistic"],
    });

    expect(placement).not.toBeNull();
    expect(placement?.kind).toBe("placed");
    if (placement?.kind === "placed") {
      expect(placement.placement.consumedText).toBe("super-");
      expect(placement.placement.flushAfterPlacement).toBe(true);
      expect(placement.placement.remainder).toBe("califragilistic");
    }
  });

  it("uses zero-width-space break opportunities before grapheme slicing", () => {
    const fit = measureZeroWidthSpaceBreakFit({
      cacheKeyPrefix: "plain-text-zero-width-space",
      text: "alpha\u200bbeta",
      font: BODY_FONT,
      width: 36,
    });

    expect(fit).not.toBeNull();
    expect(fit?.consumedText.length).toBeGreaterThan(0);
    expect((fit?.remainder.length ?? 0) > 0).toBe(true);
  });

  it("keeps mid-line zero-width-space continuations when the word also fits on a fresh line", () => {
    const placement = resolveContinuedPlainTextPlacement({
      cacheKeyPrefix: "plain-text-zero-width-public",
      currentLineText: "prefix",
      word: "alpha\u200bbeta",
      displayWord: "alphabeta",
      font: BODY_FONT,
      availableWidth: 36,
      maxWidth: 200,
      wordFitsFreshLine: true,
      usesDelimitedWrapping: false,
      wordFragments: ["alpha\u200bbeta"],
      wordContainsSoftHyphen: false,
      wordContainsZeroWidthBreak: true,
      usesImplicitWordBreaking: false,
      implicitWordBreakSegments: ["alphabeta"],
    });

    expect(placement).not.toBeNull();
    expect(placement?.kind).toBe("placed");
    if (placement?.kind === "placed") {
      expect(placement.placement.consumedText).toBe("alpha");
      expect(placement.placement.flushAfterPlacement).toBe(true);
      expect(placement.placement.remainder).toBe("beta");
    }
  });

  it("does not fall back to generic prefix slicing when the whole word fits on a fresh line", () => {
    const placement = resolveContinuedPlainTextPlacement({
      cacheKeyPrefix: "plain-text-break-line-over-prefix-slice",
      currentLineText: "prefix",
      word: "Deoxyribonucleic",
      displayWord: "Deoxyribonucleic",
      font: BODY_FONT,
      availableWidth: 45,
      maxWidth: 200,
      wordFitsFreshLine: true,
      usesDelimitedWrapping: false,
      wordFragments: ["Deoxyribonucleic"],
      wordContainsSoftHyphen: false,
      wordContainsZeroWidthBreak: false,
      usesImplicitWordBreaking: false,
      implicitWordBreakSegments: ["Deoxyribonucleic"],
    });

    expect(placement).not.toBeNull();
    expect(placement?.kind).toBe("break-line");
  });

  it("uses stricter current-line thresholds for non-URL delimited tokens", () => {
    expect(resolvePlainTextDelimitedStartRatioThreshold("https://example.com/path")).toBe(0.35);
    expect(resolvePlainTextDelimitedStartRatioThreshold("fixtures/worktrees/public-replay")).toBe(0.55);
  });
});
