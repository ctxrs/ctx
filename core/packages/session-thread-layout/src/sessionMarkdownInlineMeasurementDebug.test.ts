import type { PreparedTextWithSegments } from "@chenglou/pretext";
import { describe, expect, it } from "vitest";
import type { SessionMarkdownInlineRun } from "./sessionMarkdownContract";
import {
  serializeSessionMarkdownInlineMeasurementItems,
  type SessionMarkdownDebugWindow,
  shouldEnableSessionMarkdownInlineCodeDebug,
} from "./sessionMarkdownInlineMeasurementDebug";
import type { PreparedInlineLayoutItem } from "./sessionMarkdownInlineLayout";

describe("sessionMarkdownInlineMeasurementDebug", () => {
  it("enables inline-code debug only when the target and width gates match", () => {
    const runs: SessionMarkdownInlineRun[] = [
      { kind: "text", text: "prefix ", style: "body", deleted: false },
      { kind: "inlineCode", text: "ctx task list", parts: ["ctx", " ", "task", " ", "list"] },
    ];

    expect(
      shouldEnableSessionMarkdownInlineCodeDebug({
        debugWindow: {
          __ctxInlineCodeDebugTarget: "ctx task list",
          __ctxInlineCodeDebugWidth: 540,
        } as SessionMarkdownDebugWindow,
        runs,
        width: 540,
      }),
    ).toBe(true);

    expect(
      shouldEnableSessionMarkdownInlineCodeDebug({
        debugWindow: {
          __ctxInlineCodeDebugTarget: "ctx task list",
          __ctxInlineCodeDebugWidth: 620,
        } as SessionMarkdownDebugWindow,
        runs,
        width: 540,
      }),
    ).toBe(false);

    expect(
      shouldEnableSessionMarkdownInlineCodeDebug({
        debugWindow: {
          __ctxInlineCodeDebugTarget: "missing",
          __ctxInlineCodeDebugWidth: 540,
        } as SessionMarkdownDebugWindow,
        runs,
        width: 540,
      }),
    ).toBe(false);
  });

  it("serializes prepared inline items into the published debug payload shape", () => {
    const items: PreparedInlineLayoutItem[] = [
      {
        kind: "segment",
        allowsBreakWord: false,
        codeGroupId: 1,
        codeGroupHasDottedPath: true,
        codeGroupHasWhitespace: false,
        codeGroupHasTrailingText: true,
        codeGroupIsOnlyInlineCodeInSegment: false,
        codeGroupStartsAfterText: true,
        codeGroupStartsAfterStyledTextSeam: false,
        codePartStartsAfterWhitespace: false,
        chromeWidth: 14,
        endCursor: { segmentIndex: 0, graphemeIndex: 4 },
        fullWidth: 42,
        isFirstCodeGroupFragment: true,
        startsAfterCodeWhitespace: false,
        isFirstPathFragmentAfterHyphenRun: false,
        isPathTailFragment: false,
        isSealedInlineCodeFragment: false,
        minStartTextWidth: 18,
        prefersFreshLineStart: false,
        prefersFreshLineStartWithoutLeadingHang: false,
        startsAfterInlineCodeSeam: false,
        startsAfterCollapsedSoftBreak: false,
        startsAfterPathLikeInlineCodeSeam: false,
        startsStyledTextAfterInlineCodeSeam: false,
        startsAfterStyledTextSeam: false,
        startsStyledTextAfterBodySeam: false,
        font: "12px sans-serif",
        hasTrailingStyledText: false,
        hasTrailingInlineCode: true,
        isDecoratedText: false,
        prepared: { segments: ["path"] } as unknown as PreparedTextWithSegments,
        text: "path",
      },
      {
        kind: "space",
        width: 6,
        codeGroupId: 1,
        text: " ",
      },
      { kind: "hardBreak" },
    ];

    expect(serializeSessionMarkdownInlineMeasurementItems(items)).toEqual([
      {
        kind: "segment",
        codeGroupHasDottedPath: true,
        text: "path",
        chromeWidth: 14,
        fullWidth: 42,
        minStartTextWidth: 18,
        codeGroupHasTrailingText: true,
        codeGroupStartsAfterText: true,
        codeGroupStartsAfterStyledTextSeam: false,
        isFirstCodeGroupFragment: true,
        isSealedInlineCodeFragment: false,
        startsAfterCodeWhitespace: false,
        startsAfterStyledTextSeam: false,
        startsAfterInlineCodeSeam: false,
        startsStyledTextAfterBodySeam: false,
        startsAfterCollapsedSoftBreak: false,
        startsAfterPathLikeInlineCodeSeam: false,
        startsStyledTextAfterInlineCodeSeam: false,
        hasTrailingInlineCode: true,
      },
      {
        kind: "space",
        text: " ",
      },
      {
        kind: "hardBreak",
        text: "",
      },
    ]);
  });
});
