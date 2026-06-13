import { describe, expect, it } from "vitest";
import {
  LINE_START_CURSOR,
  buildPreparedContentKey,
  getPreparedTextWithSegments,
  measureSingleLineLayout,
  segmentGraphemes,
} from "./sessionMarkdownMeasurementCore";
import {
  advancePreparedCursorOneGrapheme,
  slicePreparedTextBetweenCursors,
} from "./sessionMarkdownInlineMeasurementContext";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";
import {
  backtrackBreakWordTokenContinuation,
  measureBreakWordLine,
  resolveInlineCodeTailWholeSegmentFitAllowancePx,
  shouldBreakBeforePunctuationOnlyContinuationTail,
  shouldEmergencyBreakAtomicTextSegment,
} from "./sessionMarkdownInlineMeasurementTextPlacement";

describe("sessionMarkdownInlineMeasurementTextPlacement", () => {
  const buildBreakWordItem = (text: string, overrides: Partial<Extract<PreparedInlineLayoutItem, { kind: "segment" }>> = {}) => {
    const font = "12px sans-serif";
    const prepared = getPreparedTextWithSegments(
      buildPreparedContentKey(`inline-break-word-item:${font}`, text),
      text,
      font,
      "normal",
    );
    const wholeLine = measureSingleLineLayout(prepared);
    expect(wholeLine).not.toBeNull();
    return {
      kind: "segment",
      prepared,
      font,
      text,
      allowsBreakWord: true,
      codeGroupId: null,
      codeGroupHasDottedPath: false,
      codeGroupHasWhitespace: false,
      codeGroupHasTrailingText: false,
      codeGroupIsOnlyInlineCodeInSegment: false,
      codeGroupStartsAfterText: false,
      codeGroupStartsAfterStyledTextSeam: false,
      codePartStartsAfterWhitespace: false,
      chromeWidth: 0,
      endCursor: wholeLine!.end,
      fullWidth: wholeLine!.width,
      hasTrailingInlineCode: false,
      hasTrailingStyledText: false,
      isDecoratedText: false,
      isFirstCodeGroupFragment: false,
      isFirstPathFragmentAfterHyphenRun: false,
      isPathTailFragment: false,
      isSealedInlineCodeFragment: false,
      minStartTextWidth: 0,
      prefersFreshLineStart: false,
      prefersFreshLineStartWithoutLeadingHang: false,
      startsAfterCodeWhitespace: false,
      startsAfterCollapsedSoftBreak: false,
      startsAfterInlineCodeSeam: true,
      startsAfterPathLikeInlineCodeSeam: false,
      startsAfterStyledTextSeam: false,
      startsStyledTextAfterBodySeam: false,
      startsStyledTextAfterInlineCodeSeam: false,
      ...overrides,
    } satisfies Extract<PreparedInlineLayoutItem, { kind: "segment" }>;
  };

  const advanceCursorByText = (
    item: Extract<PreparedInlineLayoutItem, { kind: "segment" }>,
    text: string,
  ) => {
    let cursor = LINE_START_CURSOR;
    for (const _grapheme of segmentGraphemes(text)) {
      const next = advancePreparedCursorOneGrapheme(item.prepared, cursor);
      expect(next).not.toBeNull();
      cursor = next!;
    }
    return cursor;
  };

  it("breaks before a punctuation-only tail slice after a continued path-like code fragment", () => {
    expect(
      shouldBreakBeforePunctuationOnlyContinuationTail({
        codeGroupId: null,
        startsAfterPathLikeInlineCodeSeam: true,
        lineHasContent: true,
        atItemStart: true,
        lineStartedWithContinuedCode: true,
        lineEndsAtItemEnd: false,
        lineSegmentText: ": ",
        segmentFullWidth: 3,
        availableWidth: 10,
      }),
    ).toBe(true);
  });

  it("breaks before a fully consumed punctuation-only continued path-like code tail that does not fit", () => {
    expect(
      shouldBreakBeforePunctuationOnlyContinuationTail({
        codeGroupId: null,
        startsAfterPathLikeInlineCodeSeam: true,
        lineHasContent: true,
        atItemStart: true,
        lineStartedWithContinuedCode: true,
        lineEndsAtItemEnd: true,
        lineSegmentText: ":",
        segmentFullWidth: 4,
        availableWidth: 1,
      }),
    ).toBe(true);
  });

  it("keeps a fully consumed punctuation-only continued path-like code tail when it fits", () => {
    expect(
      shouldBreakBeforePunctuationOnlyContinuationTail({
        codeGroupId: null,
        startsAfterPathLikeInlineCodeSeam: true,
        lineHasContent: true,
        atItemStart: true,
        lineStartedWithContinuedCode: true,
        lineEndsAtItemEnd: true,
        lineSegmentText: ":",
        segmentFullWidth: 4,
        availableWidth: 8,
      }),
    ).toBe(false);
  });

  it("does not break once the tail slice already carries following prose", () => {
    expect(
      shouldBreakBeforePunctuationOnlyContinuationTail({
        codeGroupId: null,
        startsAfterPathLikeInlineCodeSeam: true,
        lineHasContent: true,
        atItemStart: true,
        lineStartedWithContinuedCode: true,
        lineEndsAtItemEnd: false,
        lineSegmentText: ": thread delta ",
        segmentFullWidth: 80,
        availableWidth: 10,
      }),
    ).toBe(false);
  });

  it("does not break for ordinary non-path inline-code seams", () => {
    expect(
      shouldBreakBeforePunctuationOnlyContinuationTail({
        codeGroupId: null,
        startsAfterPathLikeInlineCodeSeam: false,
        lineHasContent: true,
        atItemStart: true,
        lineStartedWithContinuedCode: true,
        lineEndsAtItemEnd: false,
        lineSegmentText: ": ",
        segmentFullWidth: 3,
        availableWidth: 10,
      }),
    ).toBe(false);
  });

  it("does not reclaim inline-code tail seam allowance after a continued code line", () => {
    expect(
      resolveInlineCodeTailWholeSegmentFitAllowancePx({
        lineStartedWithContinuedCode: true,
        seamGuardPx: 4,
      }),
    ).toBe(0);
  });

  it("reclaims inline-code tail seam allowance for same-line code tails", () => {
    expect(
      resolveInlineCodeTailWholeSegmentFitAllowancePx({
        lineStartedWithContinuedCode: false,
        seamGuardPx: 4,
      }),
    ).toBe(4);
  });

  it("allows emergency break-word fallback for overwide atomic prose tokens", () => {
    expect(
      shouldEmergencyBreakAtomicTextSegment({
        item: {
          kind: "segment",
          allowsBreakWord: false,
          codeGroupId: null,
          codeGroupHasDottedPath: false,
          codeGroupHasWhitespace: false,
          codeGroupHasTrailingText: false,
          codeGroupIsOnlyInlineCodeInSegment: false,
          codeGroupStartsAfterText: false,
          codeGroupStartsAfterStyledTextSeam: false,
          codePartStartsAfterWhitespace: false,
          chromeWidth: 0,
          endCursor: { segmentIndex: 0, graphemeIndex: 0 },
          font: "12px sans-serif",
          fullWidth: 161.55,
          hasTrailingInlineCode: false,
          hasTrailingStyledText: false,
          isDecoratedText: false,
          isFirstCodeGroupFragment: false,
          isFirstPathFragmentAfterHyphenRun: false,
          isPathTailFragment: false,
          isSealedInlineCodeFragment: false,
          minStartTextWidth: 161.55,
          prefersFreshLineStart: false,
          prefersFreshLineStartWithoutLeadingHang: false,
          prepared: {} as never,
          startsAfterCodeWhitespace: false,
          startsAfterCollapsedSoftBreak: false,
          startsAfterInlineCodeSeam: false,
          startsAfterPathLikeInlineCodeSeam: false,
          startsAfterStyledTextSeam: false,
          startsStyledTextAfterBodySeam: false,
          startsStyledTextAfterInlineCodeSeam: false,
          text: "containerd/BuildKit/nerdctl",
        },
        maxWidth: 148,
      }),
    ).toBe(true);
  });

  it("does not emergency-break tokens that fit a fresh line", () => {
    expect(
      shouldEmergencyBreakAtomicTextSegment({
        item: {
          kind: "segment",
          allowsBreakWord: false,
          codeGroupId: null,
          codeGroupHasDottedPath: false,
          codeGroupHasWhitespace: false,
          codeGroupHasTrailingText: false,
          codeGroupIsOnlyInlineCodeInSegment: false,
          codeGroupStartsAfterText: false,
          codeGroupStartsAfterStyledTextSeam: false,
          codePartStartsAfterWhitespace: false,
          chromeWidth: 0,
          endCursor: { segmentIndex: 0, graphemeIndex: 0 },
          font: "12px sans-serif",
          fullWidth: 161.55,
          hasTrailingInlineCode: false,
          hasTrailingStyledText: false,
          isDecoratedText: false,
          isFirstCodeGroupFragment: false,
          isFirstPathFragmentAfterHyphenRun: false,
          isPathTailFragment: false,
          isSealedInlineCodeFragment: false,
          minStartTextWidth: 161.55,
          prefersFreshLineStart: false,
          prefersFreshLineStartWithoutLeadingHang: false,
          prepared: {} as never,
          startsAfterCodeWhitespace: false,
          startsAfterCollapsedSoftBreak: false,
          startsAfterInlineCodeSeam: false,
          startsAfterPathLikeInlineCodeSeam: false,
          startsAfterStyledTextSeam: false,
          startsStyledTextAfterBodySeam: false,
          startsStyledTextAfterInlineCodeSeam: false,
          text: "containerd/BuildKit/nerdctl",
        },
        maxWidth: 164,
      }),
    ).toBe(false);
  });

  it("measures the final grapheme remainder in a break-word segment", () => {
    const text = "containerd/BuildKit/nerdctl";
    const font = "12px sans-serif";
    const prepared = getPreparedTextWithSegments(
      buildPreparedContentKey(`inline-break-word-final:${font}`, text),
      text,
      font,
      "normal",
    );
    const wholeLine = measureSingleLineLayout(prepared);
    expect(wholeLine).not.toBeNull();
    let startCursor = LINE_START_CURSOR;
    for (const _grapheme of Array.from(text).slice(0, -1)) {
      const next = advancePreparedCursorOneGrapheme(prepared, startCursor);
      expect(next).not.toBeNull();
      startCursor = next!;
    }
    const line = measureBreakWordLine({
      item: {
        kind: "segment",
        prepared,
        font,
        endCursor: wholeLine!.end,
      } as never,
      startCursor,
      availableWidth: 20,
    });
    expect(line).not.toBeNull();
    expect(line?.end).toEqual(wholeLine!.end);
  });

  it("keeps ordinary inline-code seam prose attached when later slash prose is not path-sensitive", () => {
    const item = buildBreakWordItem(": agent render turn agent padding containerd/BuildKit/nerdctl");
    const lineEnd = advanceCursorByText(item, ": agent");
    const adjusted = backtrackBreakWordTokenContinuation({
      item,
      startCursor: LINE_START_CURSOR,
      line: { start: LINE_START_CURSOR, end: lineEnd, text: ": agent", width: 41.81201171875 },
      lineHasContent: true,
      lineTailAfterInlineCodeIsPunctuationOnly: false,
    });

    expect(adjusted.end).toEqual(lineEnd);
  });

  it("backtracks a partial slash token off a fresh continuation line", () => {
    const item = buildBreakWordItem(": agent render turn agent padding containerd/BuildKit/nerdctl");
    const startCursor = advanceCursorByText(item, ": agent render turn agent ");
    const lineEnd = advanceCursorByText(item, ": agent render turn agent padding containerd/");
    const adjusted = backtrackBreakWordTokenContinuation({
      item,
      startCursor,
      line: {
        start: startCursor,
        end: lineEnd,
        text: "padding containerd/",
        width: 146.37060546875,
      },
      lineHasContent: false,
      lineTailAfterInlineCodeIsPunctuationOnly: false,
    });

    const adjustedText = slicePreparedTextBetweenCursors(item.prepared, startCursor, adjusted.end);
    expect(adjustedText).toBe("padding ");
  });
});
