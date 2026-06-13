import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkbenchListItem } from "./transcriptTypes";

const { prepareMock, prepareWithSegmentsMock, layoutMock, layoutNextLineMock } = vi.hoisted(() => ({
  prepareMock: vi.fn((text: string, font: string, options?: { whiteSpace?: "normal" | "pre-wrap" }) => ({
    text,
    font,
    options,
  })),
  prepareWithSegmentsMock: vi.fn((text: string, font: string, options?: { whiteSpace?: "normal" | "pre-wrap" }) => {
    const segments = text.match(/\s+|\S+/g) ?? [];
    const mono = font.toLowerCase().includes("mono");
    return {
      text,
      font,
      options,
      segments,
      widths: segments.map((segment) => (mono ? 8 : 6) * Math.max(1, segment.length)),
      kinds: segments.map((segment) => (/^\s+$/.test(segment) ? "space" : "text")),
      breakableWidths: segments.map((segment) =>
        /^\s+$/.test(segment)
          ? null
          : Array.from(segment).map(() => (mono ? 8 : 6)),
      ),
      breakablePrefixWidths: segments.map(() => null),
      lineEndFitAdvances: segments.map((segment) => (/^\s+$/.test(segment) ? 0 : (mono ? 8 : 6) * Math.max(1, segment.length))),
      lineEndPaintAdvances: segments.map((segment) => (/^\s+$/.test(segment) ? 0 : (mono ? 8 : 6) * Math.max(1, segment.length))),
      simpleLineWalkFastPath: true,
      segLevels: null,
      discretionaryHyphenWidth: 0,
      tabStopAdvance: 0,
      chunks: [],
    };
  }),
  layoutNextLineMock: vi.fn(
    (
      prepared: { text: string },
      start: { segmentIndex: number; graphemeIndex: number },
      maxWidth: number,
    ) => {
      const text = prepared.text ?? "";
      const startIndex = Math.max(0, start.segmentIndex);
      if (startIndex >= text.length) {
        return null;
      }
      const maxChars = Math.max(1, Math.floor(maxWidth / 10));
      let endIndex = Math.min(text.length, startIndex + maxChars);
      if (endIndex < text.length) {
        const lastSpace = text.lastIndexOf(" ", endIndex - 1);
        if (lastSpace >= startIndex) {
          endIndex = lastSpace + 1;
        }
      }
      if (endIndex <= startIndex) {
        endIndex = Math.min(text.length, startIndex + 1);
      }
      const lineText = text.slice(startIndex, endIndex);
      return {
        text: lineText,
        width: Math.max(1, lineText.length) * 6,
        start,
        end: { segmentIndex: endIndex, graphemeIndex: 0 },
      };
    },
  ),
  layoutMock: vi.fn((prepared: { text: string }, maxWidth: number, lineHeight: number) => ({
    height: Math.max(
      lineHeight,
      Math.ceil(Math.max(1, prepared.text.length) / Math.max(1, Math.floor(maxWidth / 10))) * lineHeight,
    ),
    lineCount: 1,
  })),
}));

vi.mock("@chenglou/pretext", () => ({
  prepare: prepareMock,
  prepareWithSegments: prepareWithSegmentsMock,
  layoutNextLine: layoutNextLineMock,
  layout: layoutMock,
}));

import {
  clearPretextVirtualizerRowLayoutCache,
  getPretextVirtualizerRowLayout,
} from "./pretextVirtualizerRowLayout";
import {
  clearSessionMarkdownMeasurementCaches,
  measureSessionMarkdownDocument,
} from "./sessionMarkdownMeasurement";
import { SESSION_THREAD_ROW_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";
import {
  allowsChromiumDottedBoundaryHang,
  INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
  resolveInlineCodeContinuationFitSlackPx,
  resolveInlineCodeWhitespaceSeparatedFragmentSlackPx,
  resolveInlineCodeWrapChromeWidth,
  shouldBreakBeforePartialDottedCallContinuation,
  shouldApplyInlineCodeSoftBreakTextStartGuard,
  shouldBreakBeforePathDelimiterNearFitContinuation,
} from "./sessionMarkdownInlineCodeFit";
import { measureSessionPlainTextBlockHeight } from "./sessionPlainTextMeasurement";
import {
  SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY,
  SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX,
  SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX,
  SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_LINE_HEIGHT_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_BORDER_WIDTH_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_BOTTOM_PX,
  SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_TOP_PX,
  SESSION_THREAD_TURN_HEADER_BUBBLE_BORDER_WIDTH_PX,
  SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_INLINE_PX,
  SESSION_THREAD_TURN_HEADER_COPY_GUTTER_PX,
  resolveSessionThreadContentWidth,
  resolveSessionThreadTurnHeaderTextWidth,
} from "./sessionThreadLayoutTokens";

describe("getPretextVirtualizerRowLayout", () => {
  beforeEach(() => {
    clearPretextVirtualizerRowLayoutCache();
    clearSessionMarkdownMeasurementCaches();
    prepareMock.mockClear();
    prepareWithSegmentsMock.mockClear();
    layoutNextLineMock.mockClear();
    layoutMock.mockClear();
  });

  it("reuses prepared text across width changes for thought rows", () => {
    const item: WorkbenchListItem = {
      kind: "thought",
      id: "thought-1",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content: "thinking aloud",
    };

    const first = getPretextVirtualizerRowLayout(item, 640, {});
    const second = getPretextVirtualizerRowLayout(item, 720, {});

    expect(first.height).toBeGreaterThan(0);
    expect(second.height).toBeGreaterThan(0);
    expect(first.height).toBe(
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.verticalChromePx +
        SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.typography.lineHeightPx,
    );
    expect(prepareMock).toHaveBeenCalledTimes(1);
    expect(layoutMock).toHaveBeenCalledTimes(2);
  });

  it("measures markdown-heavy assistant rows deterministically", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content: "# Heading\n\n- bullet",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 640, {});

    expect(result.height).toBeGreaterThan(40);
    expect(prepareMock.mock.calls.length + prepareWithSegmentsMock.mock.calls.length).toBeGreaterThan(0);
  });

  it("measures incomplete assistant markdown exactly like the completed row for the same content", () => {
    const partialItem: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-streaming-partial",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content: "Before\n\n- partial item",
      thought: "",
      is_complete: false,
    };
    const closedItem: WorkbenchListItem = {
      ...partialItem,
      id: "assistant-streaming-closed",
      is_complete: true,
    };

    const partial = getPretextVirtualizerRowLayout(partialItem, 640, {});
    const closed = getPretextVirtualizerRowLayout(closedItem, 640, {});

    expect(partial.height).toBeGreaterThan(0);
    expect(closed.height).toBe(partial.height);
    expect(prepareMock.mock.calls.length + prepareWithSegmentsMock.mock.calls.length).toBeGreaterThan(0);
  });

  it("measures markdown tables with deterministic width-sensitive heights", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-table-1",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content: [
        "| Day (UTC) | Daily active people | Return users |",
        "|---|---:|---:|",
        "| 2026-04-03 | 34 | 2 |",
        "| 2026-04-04 | 21 | `7` |",
      ].join("\n"),
      thought: "",
      is_complete: true,
    };

    const wide = getPretextVirtualizerRowLayout(item, 640, {});
    const narrow = getPretextVirtualizerRowLayout(item, 240, {});

    expect(wide.height).toBeGreaterThan(40);
    expect(narrow.height).toBeGreaterThan(wide.height);
    expect(prepareWithSegmentsMock).toHaveBeenCalled();
  });

  it("uses segmented monospace measurement for inline-code chips in assistant rows", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-inline-code",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content: "prefix `1180` suffix",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 180, {});

    expect(result.height).toBeGreaterThan(0);
    expect(prepareWithSegmentsMock).toHaveBeenCalled();
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) => text === "1180" && typeof font === "string" && font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
  });

  it("splits long hyphenated inline-code tokens into deterministic wrap fragments", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-inline-code-wrap-fragments",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content:
        "begin agent message with plain text and now `inline-thing-that-actually-gets-really-long-so-much-so-that-it-wraps-to-multiple-lines/core/apps/web/src/pages/sessionThread/sessionMarkdownMeasurement.ts` after the prose",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 620, {});

    expect(result.height).toBeGreaterThan(0);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "really-" && typeof font === "string" && font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "multiple-" &&
          typeof font === "string" &&
          font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "lines/core/" &&
          typeof font === "string" &&
          font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
  });

  it("splits pure path-like inline-code tokens at filename boundaries", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-inline-code-path-wrap-fragments",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      content:
        "prefix prose `sessionThreadDomMeasurement.tsx/sessionThreadDomMeasurement.tsx/apps/sessionThreadDomMeasurement.tsx/workbenchShell` suffix prose",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 472, {});

    expect(result.height).toBeGreaterThan(0);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "sessionThreadDomMeasurement." &&
          typeof font === "string" &&
          font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "tsx/apps/" &&
          typeof font === "string" &&
          font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
  });

  it("uses the shared body line-height for wrapped markdown lines with inline code", () => {
    const markdown = "`abcd/` `efgh/` `ijkl/`";

    const height = measureSessionMarkdownDocument(markdown, 50);

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("does not grant generic continuation slack after a sealed dotted fragment", () => {
    const slack = resolveInlineCodeContinuationFitSlackPx({
      lineHasContent: true,
      atLineBreakBoundary: true,
      sameCodeGroupContinuation: true,
      startsAfterCodeWhitespace: false,
      lastFragmentEndedWithDot: true,
      lastFragmentEndedWithHyphen: false,
      lastFragmentEndedWithPathDelimiter: false,
      item: {
        codeGroupHasDottedPath: false,
        codeGroupHasTrailingText: true,
        codeGroupStartsAfterText: true,
        codeGroupStartsAfterStyledTextSeam: false,
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        text: "disconnect()",
      },
    });

    expect(slack).toBe(0);
  });

  it("does not grant continuation slack after a hyphen-ended path fragment", () => {
    const slack = resolveInlineCodeContinuationFitSlackPx({
      lineHasContent: true,
      atLineBreakBoundary: true,
      sameCodeGroupContinuation: true,
      startsAfterCodeWhitespace: false,
      lastFragmentEndedWithDot: false,
      lastFragmentEndedWithHyphen: true,
      lastFragmentEndedWithPathDelimiter: false,
      item: {
        codeGroupHasDottedPath: true,
        codeGroupHasTrailingText: false,
        codeGroupStartsAfterText: true,
        codeGroupStartsAfterStyledTextSeam: false,
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        text: "code/e2e",
      },
    });

    expect(slack).toBe(0);
  });

  it("breaks path delimiter continuations that only fit by a subpixel margin", () => {
    expect(
      shouldBreakBeforePathDelimiterNearFitContinuation({
        lineHasContent: true,
        sameCodeGroupContinuation: true,
        lastFragmentEndedWithPathDelimiter: true,
        startsAfterCodeWhitespace: false,
        isSealedInlineCodeFragment: false,
        reservedWidth: 0,
        remainingWidth: 24.080078125,
        fragmentWidth: 23.47998046875,
      }),
    ).toBe(true);
  });

  it("keeps path delimiter continuations when they have enough spare width", () => {
    expect(
      shouldBreakBeforePathDelimiterNearFitContinuation({
        lineHasContent: true,
        sameCodeGroupContinuation: true,
        lastFragmentEndedWithPathDelimiter: true,
        startsAfterCodeWhitespace: false,
        isSealedInlineCodeFragment: false,
        reservedWidth: 0,
        remainingWidth: 25,
        fragmentWidth: 23.47998046875,
      }),
    ).toBe(false);
  });

  it("grants Chromium command-fragment slack for hyphen-ended package fragments after whitespace", () => {
    expect(
      resolveInlineCodeWhitespaceSeparatedFragmentSlackPx({
        lineHasContent: true,
        startsAfterCodeWhitespace: true,
        fragmentText: "ctx-",
      }),
    ).toBe(5);
  });

  it("breaks before partially fitting a dotted call continuation after a sealed dotted fragment", () => {
    const shouldBreak = shouldBreakBeforePartialDottedCallContinuation({
      fragmentWidth: 93.92,
      fullWidth: 93.92,
      guardedRemainingWidth: 88.96,
      item: {
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        text: "disconnect()",
      },
      lastFragmentText: "ConnectionManager.",
      maxWidth: 472,
      sameCodeGroupContinuation: true,
    });

    expect(shouldBreak).toBe(true);
  });

  it("keeps strict dotted-call continuations off the previous line when only a tiny spare margin remains", () => {
    const shouldBreak = shouldBreakBeforePartialDottedCallContinuation({
      fragmentWidth: 70.43994140625,
      fullWidth: 70.43994140625,
      guardedRemainingWidth: 73.7275390625,
      item: {
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        text: "tasksById",
      },
      lastFragmentText: "workspaceSnapshot.",
      maxWidth: 402,
      minSparePx: INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
      sameCodeGroupContinuation: true,
    });

    expect(shouldBreak).toBe(true);
  });

  it("does not apply the long dotted-call spare guard to decimals or short dotted names", () => {
    const nearFit = {
      fragmentWidth: 23.47998046875,
      fullWidth: 23.47998046875,
      guardedRemainingWidth: 26,
      item: {
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        text: "65s",
      },
      maxWidth: 597,
      minSparePx: INLINE_CODE_DOTTED_CALL_CONTINUATION_MIN_SPARE_PX,
      sameCodeGroupContinuation: true,
    };

    expect(
      shouldBreakBeforePartialDottedCallContinuation({
        ...nearFit,
        lastFragmentText: "0.",
      }),
    ).toBe(false);
    expect(
      shouldBreakBeforePartialDottedCallContinuation({
        ...nearFit,
        lastFragmentText: "Promise.",
        item: {
          ...nearFit.item,
          text: "all",
        },
      }),
    ).toBe(false);
  });

  it("treats markdown hard breaks as forced line breaks in wide paragraphs", () => {
    const markdown = [
      "Short version:  ",
      "the next system should answer not just which title wins.  ",
      "It should answer which article shape wins.",
    ].join("\n");

    const height = measureSessionMarkdownDocument(markdown, 1600);

    expect(height).toBe(Math.round(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 3 * 16) / 16);
  });

  it("uses styled inline runs when bold markdown changes line breaking", () => {
    const markdown =
      "**Most predictive overall, but not usable directly pre-submit** These are still the strongest overall signal family.";

    const height = measureSessionMarkdownDocument(markdown, 340);

    expect(prepareWithSegmentsMock).toHaveBeenCalled();
    expect(height).toBeGreaterThan(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("measures strong inline markdown with the browser-matching heavier font weight", () => {
    measureSessionMarkdownDocument("prefix **strong fragment** suffix", 260);

    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          typeof text === "string" &&
          text.includes("strong") &&
          typeof font === "string" &&
          font.includes("700"),
      ),
    ).toBe(true);
  });

  it("does not reuse prepared mixed-inline segments across different markdown documents", () => {
    const first = measureSessionMarkdownDocument("`supercalifragilisticexpialidocious` tail", 120);
    const second = measureSessionMarkdownDocument("`x` tail", 120);

    expect(first).toBeGreaterThan(second);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "supercalifragilisticexpialidocious" &&
          typeof font === "string" &&
          font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) => text === "x" && typeof font === "string" && font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
  });

  it("keeps wrapped inline-code prose heights deterministic", () => {
    const wrappedCode =
      "inline-thing-that-actually-gets-really-long-so-much-so-that-it-wraps-to-multiple-lines/core/apps/web/src/pages/sessionThread/sessionMarkdownMeasurement.ts";
    const withTrailingProse = measureSessionMarkdownDocument(
      `begin agent message with plain text and now \`${wrappedCode}\` after the prose`,
      620,
    );
    const withoutTrailingProse = measureSessionMarkdownDocument(
      `begin agent message with plain text and now \`${wrappedCode}\``,
      620,
    );

    expect(withTrailingProse % SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX).toBe(0);
    expect(withoutTrailingProse % SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX).toBe(0);
    expect(withTrailingProse).toBeGreaterThanOrEqual(withoutTrailingProse);
  });

  it("keeps sealed inline-code fragments atomic when they exceed the available line width", () => {
    const height = measureSessionMarkdownDocument("`web/pretextVirtualizerRowLayout.ts`", 80);

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("fits consecutive sealed inline-code fragments by text width instead of painted chip chrome", () => {
    const height = measureSessionMarkdownDocument("`apps/e2e/e2e/core/web/pretextVirtualizerRowLayout.ts`", 150);

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps the first visual slice of a path-like code group on the prose line when it fits", () => {
    const height = measureSessionMarkdownDocument(
      "Entry command command `turn-header/workbenchShell/blockquote/workbenchShell/src/pages/core`",
      382.48,
    );

    expect(height).toBe(Math.round(2 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps dotted path tail fragments on the continuation line when they still fit", () => {
    const height = measureSessionMarkdownDocument(
      "composer `sessionThread/src/apps/web/inline-code/pretextVirtualizerRowLayout.ts`",
      382.48,
    );

    expect(height).toBe(Math.round(2 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("allows a styled-seam path-like code group to keep its first readable slice on the current line", () => {
    const height = measureSessionMarkdownDocument(
      "Session padding probe *render session padding* `workbenchShell/sessionThread/e2e/workbenchShell/turn-header/web` ~~fragment~~ `cargo test -p ctx-store` deterministic summary parity ~~session virtualizer~~.",
      518.64,
    );

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("starts a near-fitting path-like code group on a fresh line after prose pressure", () => {
    const height = measureSessionMarkdownDocument(
      [
        "Layout composer fragment padding `sessionMarkdownMeasurement.ts/src/sessionThreadDomMeasurement.tsx/web/blockquote/workbenchShell/workbenchShell` **command inline** `cargo test -p ctx-store` ⚙️ 測試 佈局 [agent fragment](https://example.com/transcript/chromium?ref=812) *summary fragment*. `apps/fixtures/src/src/sessionMarkdownMeasurement.ts/pages`",
        "Inline stream browser context **thread layout padding** 🙂 段落 換行 fragment header probe session summary token ~~inline~~; `git rev-parse HEAD`",
        "Render deterministic [command session deterministic](https://example.com/docs/inline-code/measurement?ref=150) *token virtualizer* ~~agent shell~~ session parity shell inline fragment; `e2e/turn-header/workbenchShell/table`",
        "Virtualizer probe render 🙂 段落 換行 *composer browser render* session browser header layout command session layout parity command: `sessionThread/table/web/workbenchShell/apps/blockquote`",
      ].join("\n"),
      518.64,
    );

    expect(height).toBe(Math.round(11 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("does not apply the leading-hang chrome discount to fresh-line path-like code groups", () => {
    expect(
      resolveInlineCodeWrapChromeWidth({
        chromeWidth: 14,
        codeGroupHasWhitespace: false,
        codeGroupStartsAfterText: true,
        chargedChrome: false,
        isFirstCodeGroupFragment: true,
        prefersFreshLineStart: true,
        lineHasContent: false,
        startsAtLineStart: true,
      }),
    ).toBe(14);
  });

  it("uses half chrome for a path-like continuation line start in chromium-style wraps", () => {
    expect(
      resolveInlineCodeWrapChromeWidth({
        chromeWidth: 14,
        codeGroupHasWhitespace: false,
        codeGroupStartsAfterText: true,
        chargedChrome: false,
        isFirstCodeGroupFragment: false,
        prefersFreshLineStart: false,
        lineHasContent: false,
        startsAtLineStart: true,
      }),
    ).toBe(7);
  });

  it("applies the leading-hang chrome discount to whitespace-bearing command chips after prose", () => {
    expect(
      resolveInlineCodeWrapChromeWidth({
        allowLeadingHang: true,
        chromeWidth: 14,
        codeGroupHasWhitespace: true,
        codeGroupStartsAfterText: true,
        chargedChrome: false,
        isFirstCodeGroupFragment: true,
        prefersFreshLineStart: false,
        lineHasContent: true,
        startsAtLineStart: false,
      }),
    ).toBe(7);
  });

  it("caps chromium dotted boundary hangs to one inline-code chrome width", () => {
    expect(
      allowsChromiumDottedBoundaryHang({
        boundaryRemainingWidth: 17.34,
        chromeWidth: 14,
        fullWidth: 31.31,
      }),
    ).toBe(true);
    expect(
      allowsChromiumDottedBoundaryHang({
        boundaryRemainingWidth: 17.34,
        chromeWidth: 14,
        fullWidth: 32,
      }),
    ).toBe(false);
  });

  it("keeps a trailing styled list-item segment on the prose line at the WebKit seam", () => {
    const height = measureSessionMarkdownDocument(
      "- Browser agent **deterministic session shell** header marker pretext probe *fragment fragment*.",
      588,
    );

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX);
  });

  it("treats soft newlines inside mixed inline paragraphs like collapsed spaces", () => {
    const withSoftNewline = measureSessionMarkdownDocument(
      "before mixed prose\nand `inline-code-token/with/path` after the wrap",
      260,
    );
    const withSpace = measureSessionMarkdownDocument(
      "before mixed prose and `inline-code-token/with/path` after the wrap",
      260,
    );

    expect(withSoftNewline).toBe(withSpace);
  });

  it("skips the collapsed soft-break prose guard after a friendly path-like inline-code boundary", () => {
    expect(
      shouldApplyInlineCodeSoftBreakTextStartGuard({
        text: "friendly prose",
        startsAfterCollapsedSoftBreak: true,
        startsAfterPathLikeInlineCodeSeam: true,
        startsAfterInlineCodeSeam: true,
        startsStyledTextAfterInlineCodeSeam: false,
        lastFragmentEndedWithPathDelimiter: false,
        lastFragmentEndedWithHyphen: false,
      }),
    ).toBe(false);
    expect(
      shouldApplyInlineCodeSoftBreakTextStartGuard({
        text: "friendly prose",
        startsAfterCollapsedSoftBreak: true,
        startsAfterPathLikeInlineCodeSeam: true,
        startsAfterInlineCodeSeam: true,
        startsStyledTextAfterInlineCodeSeam: false,
        lastFragmentEndedWithPathDelimiter: true,
        lastFragmentEndedWithHyphen: false,
      }),
    ).toBe(true);
    expect(
      shouldApplyInlineCodeSoftBreakTextStartGuard({
        text: "friendly prose",
        startsAfterCollapsedSoftBreak: true,
        startsAfterPathLikeInlineCodeSeam: true,
        startsAfterInlineCodeSeam: false,
        startsStyledTextAfterInlineCodeSeam: true,
        lastFragmentEndedWithPathDelimiter: false,
        lastFragmentEndedWithHyphen: false,
      }),
    ).toBe(false);
  });

  it("does not add a phantom line when collapsed soft-break prose follows a wrapped inline-code path", () => {
    const height = measureSessionMarkdownDocument(
      [
        "Marker summary layout `cargo test -p codex-crp` browser shell agent header summary 🙂 段落 換行 ~~summary~~ *thread buffer thread*. `table/web/table/workbenchShell/fixtures/fixtures/pretextVirtualizerRowLayout.ts`",
        "Pretext summary agent fragment fragment thread entry probe deterministic `git diff --stat`; `core/pages/inline-code/pages/inline-code/apps`",
      ].join("\n"),
      445.04,
    );

    expect(height).toBe(Math.round(6 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps punctuation-only tails attached to a wrapped path-like code group without requiring another inline code chip", () => {
    const height = measureSessionMarkdownDocument(
      "Delta header virtualizer browser layout virtualizer delta turn deterministic buffer **context parity** `table/sessionMarkdownMeasurement.ts/blockquote/table/blockquote/fixtures/inline-code`:",
      673.2,
    );

    expect(height).toBe(Math.round(2 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("does not force a fresh-line code start for decorated trailing prose after a path-like code group", () => {
    const height = measureSessionMarkdownDocument(
      "- Agent token marker `src/src/pretextVirtualizerRowLayout.ts/core/blockquote` *padding entry* **marker stream**:",
      620,
    );

    expect(height).toBe(Math.round(2 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps trailing prose on the same line when it fully fits after a long path-like code group", () => {
    const height = measureSessionMarkdownDocument(
      "Paragraph with `alpha-beta-gamma-delta/ctx/path/one/with/a/very/long/suffix` and prose after the wrap threshold.",
      788,
    );

    expect(height).toBe(Math.round(1 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("moves an overflowing sealed path fragment to the next line inside list items", () => {
    const height = measureSessionMarkdownDocument(
      "- Command fragment [stream layout](https://example.com/docs/parity/webkit?ref=298) *agent virtualizer* ~~inline~~ `pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e` render parity composer probe render header render summary;",
      588,
    );

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("moves an overflowing sealed path fragment before trailing prose in plain paragraphs", () => {
    const height = measureSessionMarkdownDocument(
      "Command fragment [stream layout](https://example.com/docs/parity/webkit?ref=298) *agent virtualizer* ~~inline~~ `pages/fixtures/sessionMarkdownMeasurement.ts/sessionMarkdownMeasurement.ts/blockquote/inline-code/e2e` render parity composer probe render header render summary;",
      564,
    );

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("breaks before a dotted path tail fragment that would only partially fit on the prose line", () => {
    const height = measureSessionMarkdownDocument(
      "threshold render deterministic context `src/src/pretextVirtualizerRowLayout.ts/core/blockquote` layout browser layout line delta context before the final browser-width pass lands.",
      620,
    );

    expect(height).toBe(Math.round(3 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps a path-tail final inline fragment on the decorated prose line in chromium-style wraps", () => {
    const height = measureSessionMarkdownDocument(
      "Probe layout browser `turn-header/sessionMarkdownMeasurement.ts/pages/inline-code/turn-header` context command virtualizer fragment 🧪 測試 佈局 ~~virtualizer~~. `blockquote/fixtures/sessionThread/e2e`",
      416,
    );

    expect(height).toBe(Math.round(4 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("keeps a whitespace-bearing command chip on the punctuation seam line when chromium does", () => {
    const height = measureSessionMarkdownDocument(
      "Buffer session summary ~~turn~~ ⚙️ 你好 世界 `core/blockquote/sessionThreadDomMeasurement.tsx/sessionThreadDomMeasurement.tsx` ~~command~~ ~~command command~~ **context**: `workbenchShell/fixtures/turn-header/table` Stream browser agent ⚙️ 測試 佈局 header deterministic thread summary buffer entry context ⚙️ 你好 世界 `web/fixtures/core/fixtures`. `pnpm -C core/apps/web test:e2e:pretext:corpus:webkit`",
      382.48,
    );

    expect(height).toBe(Math.round(8 * SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 16) / 16);
  });

  it("packs URL-heavy plain text lines as whitespace-separated turn-header tokens", () => {
    const line =
      "https://example.com/transcript/inline-code/transcript/inline-code?ref=293 pretextVirtualizerRowLayout.ts/fixtures/fixtures/core/workbenchShell/core/pages.";
    measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-plain-url",
      text: line,
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 220,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(
      prepareMock.mock.calls.some(([text]) => text === line),
    ).toBe(false);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text, font]) =>
          text === "https://example.com/transcript/inline-code/transcript/inline-code?ref=293" &&
          typeof font === "string" &&
          !font.toLowerCase().includes("mono"),
      ),
    ).toBe(true);
    expect(
      prepareWithSegmentsMock.mock.calls.some(
        ([text]) => typeof text === "string" && text.startsWith("https://"),
      ),
    ).toBe(true);
  });

  it("wraps whitespace-separated turn-header tokens before breaking inside a long token", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-word-wrap-before-break",
      text: "alpha betagamma",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 70,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("breaks overlong turn-header tokens at grapheme boundaries on a fresh line", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-plain-command",
      text: "supercalifragilistic",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 60,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
    expect(
      prepareWithSegmentsMock.mock.calls.some(([text]) => text === "supercalif"),
    ).toBe(true);
  });

  it("continues overlong turn-header tokens on the current line when break-word is required", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-break-word-continuation",
      text: "alpha supercalifragilistic",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 100,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("prefers implicit Thai word breaks over grapheme slicing for no-space scripts", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-thai-implicit-word-breaks",
      text: "กรุงเทพคือสวยงามและต้อง",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 48,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 4);
  });

  it("continues delimited turn-header path tokens on the current line when break-word is required", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-delimited-break-word-continuation",
      text: "alpha beta/gamma",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 60,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("continues a turn-header URL token on the current line after prose when break-word is required", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-url-after-prose-continuation",
      text: "Message summary context https://example.com/assistant/streaming-tail/streaming-tail?ref=656 e2e/fixtures/src/web/pages/turn-header.",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 550,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("keeps a meaningful URL prefix on the current line after prose at the WebKit seam width", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-url-after-prose-webkit-seam",
      text: "Message summary context https://example.com/assistant/streaming-tail/streaming-tail?ref=656 e2e/fixtures/src/web/pages/turn-header.",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 470,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("pushes insufficient URL continuations after prose onto a fresh line in turn headers", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-url-after-prose-fresh-line-threshold",
      text: "Composer turn header pretext session https://example.com/transcript/transcript/docs/streaming-tail?ref=972 fixtures/web/fixtures/web.",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 550,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 3);
  });

  it("starts a path token on a fresh line after a wrapped URL query tail", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-url-query-tail-path-wrap",
      text: "Entry buffer layout https://example.com/chromium/docs/parity?ref=734 sessionMarkdownMeasurement.ts/pretextVirtualizerRowLayout.ts/pages/table.",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 402,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 4);
  });

  it("keeps a readable path prefix on the current line after prose in turn headers", () => {
    const height = measureSessionPlainTextBlockHeight({
      cacheKey: "turn-header-path-after-prose-webkit-seam",
      text: "Follow-up line with fixtures/worktrees/public-replay/root-alpha/session-thread path pressure.",
      font: `${SESSION_THREAD_MARKDOWN_BODY_FONT_SIZE_PX}px ${SESSION_THREAD_MARKDOWN_BODY_FONT_FAMILY}`,
      width: 470,
      lineHeight: SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX,
    });

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });

  it("wraps trailing plain text after styled seams at the WebKit threshold", () => {
    const markdown =
      "Turn summary layout summary [command fragment](https://example.com/parity/inline-code/virtualizer/virtualizer?ref=774) *stream marker marker* **layout** ~~context layout~~ browser layout session delta.";

    const height = measureSessionMarkdownDocument(markdown, 756);

    expect(height).toBe(SESSION_THREAD_MARKDOWN_BODY_LINE_HEIGHT_PX * 2);
  });


  it("includes fenced code block border chrome in deterministic markdown height", () => {
    const markdown = "```ts\nconst value = 1;\n```";

    const height = measureSessionMarkdownDocument(markdown, 400);

    const expected =
      10 +
      SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_TOP_PX +
      SESSION_THREAD_MARKDOWN_CODE_BLOCK_PADDING_BOTTOM_PX +
      SESSION_THREAD_MARKDOWN_CODE_BLOCK_BORDER_WIDTH_PX * 2 +
      SESSION_THREAD_MARKDOWN_CODE_BLOCK_LINE_HEIGHT_PX;
    expect(height).toBe(Math.round(expected * 16) / 16);
  });

  it("accounts for expansion state and attachments in turn header height", () => {
    const item: WorkbenchListItem = {
      kind: "turn_header",
      id: "header-item-1",
      header: {
        id: "header-1",
        created_at: "2026-04-05T00:00:00Z",
        content: "Header",
        plain_text: "Header\nwith several lines\nof content\nthat can collapse",
        attachments: [
          { kind: "image_ref", blob_id: "blob-1", mime_type: "image/png", name: "a.png" },
          { kind: "image_ref", blob_id: "blob-2", mime_type: "image/png", name: "b.png" },
        ],
      },
    };

    const collapsed = getPretextVirtualizerRowLayout(item, 640, {
      expandedTurnHeaders: { "header-1": false },
    });
    const expanded = getPretextVirtualizerRowLayout(item, 640, {
      expandedTurnHeaders: { "header-1": true },
    });

    expect(expanded.height).toBeGreaterThan(collapsed.height);
  });

  it("measures URL-heavy turn headers with the whitespace-token packer", () => {
    const item: WorkbenchListItem = {
      kind: "turn_header",
      id: "header-item-url",
      header: {
        id: "header-url",
        created_at: "2026-04-05T00:00:00Z",
        content:
          "- https://example.com/a/really/long/path/that/keeps/wrapping?token=12345 should not drift when wrapped inside the turn header bubble.",
        plain_text:
          "- https://example.com/a/really/long/path/that/keeps/wrapping?token=12345 should not drift when wrapped inside the turn header bubble.",
        attachments: [],
      },
    };

    const result = getPretextVirtualizerRowLayout(item, 620, {
      expandedTurnHeaders: { "header-url": true },
    });

    expect(result.height).toBeGreaterThan(0);
    expect(
      prepareMock.mock.calls.some(([text]) => text === item.header.plain_text),
    ).toBe(false);
    expect(
      prepareWithSegmentsMock.mock.calls.some(([text]) => text === "https://example.com/a/really/long/path/that/keeps/wrapping?token=12345"),
    ).toBe(true);
    expect(prepareWithSegmentsMock.mock.calls.some(([text]) => text === "https://")).toBe(false);
  });

  it("budgets turn-header text width using bubble border, padding, and copy gutter", () => {
    const viewportWidth = 620;

    expect(resolveSessionThreadTurnHeaderTextWidth(viewportWidth)).toBe(
      resolveSessionThreadContentWidth(viewportWidth) -
        SESSION_THREAD_TURN_HEADER_BUBBLE_BORDER_WIDTH_PX * 2 -
        SESSION_THREAD_TURN_HEADER_BUBBLE_PADDING_INLINE_PX * 2 -
        SESSION_THREAD_TURN_HEADER_COPY_GUTTER_PX,
    );
  });

  it("keeps tool groups compact until expanded", () => {
    const item: WorkbenchListItem = {
      kind: "tool_group",
      id: "tool-group-1",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      updated_at: "2026-04-05T00:00:30Z",
      tool_total: 2,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 2,
      tool_failed: 0,
      tools: [
        {
          kind: "tool",
          id: "tool-1",
          created_at: "2026-04-05T00:00:00Z",
          updated_at: "2026-04-05T00:00:10Z",
          tool_call_id: "call-1",
          tool_kind: "execute",
          title: "Run pwd",
          status: "completed",
          locations: [],
          input: { command: "pwd" },
          output_text: "",
          raw: {},
          updates_seen: 1,
        },
      ],
      thought: "I should inspect the repository first.",
    };

    const collapsed = getPretextVirtualizerRowLayout(item, 640, {});
    const expanded = getPretextVirtualizerRowLayout(item, 640, {
      expandedTurnDetailsById: { "turn-1": true },
    });

    const expectedThoughtHeight =
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtTitle.heightPx +
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtBody.chromeHeightPx +
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtBody.typography.lineHeightPx;
    const expectedExpandedHeight =
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.rowHeightPx +
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.groupGapPx +
      (SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.rowHeightPx +
        SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.groupGapPx +
        expectedThoughtHeight);

    expect(collapsed.height).toBe(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.rowHeightPx);
    expect(expanded.height).toBe(expectedExpandedHeight);
    expect(expanded.height).toBeGreaterThan(collapsed.height);
  });

  it("reserves deterministic space for ask-user-question cards", () => {
    const item: WorkbenchListItem = {
      kind: "ask_user_question",
      id: "ask-1",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      tool_call_id: "tool-call-1",
      answered: false,
      input: {
        questions: [
          {
            header: "Priority",
            question: "Which option should I choose?",
            options: [
              { label: "Fast", description: "Get it done quickly." },
              { label: "Careful", description: "Spend more time validating." },
            ],
            allowOther: true,
          },
        ],
      },
    };

    const result = getPretextVirtualizerRowLayout(item, 640, {});

    expect(result.height).toBe(
      Math.round((SESSION_THREAD_ASK_USER_MARGIN_VERTICAL_PX + SESSION_THREAD_ASK_USER_SHELL_HEIGHT_PX) * 16) / 16,
    );
  });

  it("keeps ask-user-question height fixed after the row is answered", () => {
    const pendingItem: WorkbenchListItem = {
      kind: "ask_user_question",
      id: "ask-2",
      turn_id: "turn-1",
      created_at: "2026-04-05T00:00:00Z",
      tool_call_id: "tool-call-2",
      answered: false,
      input: {
        questions: [
          {
            header: "Priority",
            question: "Which option should I choose?",
            options: [{ label: "Fast", description: "Get it done quickly." }],
            allowOther: true,
          },
        ],
      },
    };
    const answeredItem: WorkbenchListItem = {
      ...pendingItem,
      answered: true,
      answers: { "Which option should I choose?": "Fast" },
      outcome: "submitted",
    };

    const pending = getPretextVirtualizerRowLayout(pendingItem, 640, {});
    const answered = getPretextVirtualizerRowLayout(answeredItem, 640, {});

    expect(answered.height).toBe(pending.height);
  });

  it("prefers an assistant row-height override over deterministic measurement", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-override",
      turn_id: "turn-1",
      created_at: "2026-04-25T00:00:00Z",
      content: "Long inline code follow-up with `ctx-main #42` and exact release notes.",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 788, {
      measurementHooks: {
        resolveRowHeightOverride: () => 83,
      },
    });

    expect(result.height).toBe(83);
  });

  it("uses assistant browser-authoritative row measurement when provided", () => {
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-measured",
      turn_id: "turn-1",
      created_at: "2026-04-25T00:00:00Z",
      content: "Release note with `origin/main` and more wrapping prose.",
      thought: "",
      is_complete: true,
    };

    const result = getPretextVirtualizerRowLayout(item, 788, {
      measurementHooks: {
        measureRowHeight: (request) =>
          request.kind === "assistant-row"
            ? { status: "measured", height: 117 }
            : { status: "miss" },
      },
    });

    expect(result.height).toBe(117);
  });

  it("uses text measurement hooks for plain-text message rows", () => {
    const item: WorkbenchListItem = {
      kind: "message",
      id: "message-text-hook",
      role: "user",
      content: "Please continue with the rollout notes.",
      attachments: [],
      created_at: "2026-04-25T00:00:00Z",
    };

    const result = getPretextVirtualizerRowLayout(item, 788, {
      measurementHooks: {
        measureTextHeight: (request) =>
          request.kind === "message-text"
            ? { status: "measured", height: 73 }
            : { status: "miss" },
      },
    });

    expect(result.height).toBe(
      Math.round(
        (SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.message.rowPaddingBlockPx * 2 +
          SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.message.roleLineHeightPx +
          SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.message.bubblePaddingBlockPx * 2 +
          SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.message.bubbleBorderWidthPx * 2 +
          73) *
          16,
      ) / 16,
    );
  });
});
