import { describe, expect, it } from "vitest";
import {
  containsStrongRtlText,
  isCompactPathTailContinuationAnchor,
  isCompactSlashDelimitedSeamAnchor,
  isHyphenatedTextBreakToken,
  isPathLikeColonLeadText,
  isPlainSlashDelimitedTextFragment,
  isPunctuationOnlySeamText,
  isShortDottedNumericPrefixFragment,
  isSlashDelimitedTextBreakToken,
  shouldPreferWholeInlineTokenFreshLine,
  splitHyphenatedTextBreakToken,
  splitLeadingHyphenTextBreakToken,
  splitUrlLikeTextBreakToken,
} from "./sessionTextTokenClassifier";

describe("sessionTextTokenClassifier", () => {
  it("recognizes punctuation-only seam text and strong rtl text", () => {
    expect(isPunctuationOnlySeamText(": /")).toBe(false);
    expect(isPunctuationOnlySeamText("::")).toBe(true);
    expect(containsStrongRtlText("hello")).toBe(false);
    expect(containsStrongRtlText("مرحبا")).toBe(true);
  });

  it("recognizes compact path-tail continuation anchors", () => {
    expect(isCompactPathTailContinuationAnchor("tsx/apps")).toBe(true);
    expect(isCompactPathTailContinuationAnchor("tsx/turn-header")).toBe(false);
    expect(isCompactPathTailContinuationAnchor("corpus:webkit")).toBe(false);
  });

  it("recognizes compact slash-delimited seam anchors", () => {
    expect(isCompactSlashDelimitedSeamAnchor("foo/bar/baz")).toBe(true);
    expect(isCompactSlashDelimitedSeamAnchor("foo/bar.ts/baz")).toBe(false);
  });

  it("classifies slash-delimited text fragments without treating urls as fragments", () => {
    expect(isPlainSlashDelimitedTextFragment("daemon/cli")).toBe(true);
    expect(isPlainSlashDelimitedTextFragment("https://example.com")).toBe(false);
    expect(isSlashDelimitedTextBreakToken("daemon/cli")).toBe(true);
    expect(isSlashDelimitedTextBreakToken("/absolute/path")).toBe(false);
  });

  it("splits hyphenated tokens at break-friendly boundaries", () => {
    expect(isHyphenatedTextBreakToken("turn-header-layout")).toBe(true);
    expect(splitHyphenatedTextBreakToken("turn-header-layout")).toEqual([
      "turn-",
      "header-",
      "layout",
    ]);
    expect(splitLeadingHyphenTextBreakToken("-webkit-layout")).toEqual([
      "-",
      "webkit-",
      "layout",
    ]);
  });

  it("splits url-like tokens into browser-friendly fragments", () => {
    expect(splitUrlLikeTextBreakToken("https://ctx.dev/foo-bar?x=y")).toEqual([
      "https://",
      "ctx.",
      "dev/",
      "foo-",
      "bar?",
      "x=",
      "y",
    ]);
  });

  it("prefers fresh-line whole-token starts only for dense non-path identifiers", () => {
    expect(shouldPreferWholeInlineTokenFreshLine("EXAMPLE_TOKEN", 6)).toBe(true);
    expect(shouldPreferWholeInlineTokenFreshLine("cli/run", 6)).toBe(false);
    expect(shouldPreferWholeInlineTokenFreshLine("short", 6)).toBe(false);
  });

  it("keeps short dotted numeric prefixes out of dotted-path heuristics", () => {
    expect(isShortDottedNumericPrefixFragment("1.")).toBe(true);
    expect(isShortDottedNumericPrefixFragment("12.")).toBe(true);
    expect(isShortDottedNumericPrefixFragment("10.5")).toBe(false);
    expect(isShortDottedNumericPrefixFragment("build.")).toBe(false);
  });

  it("detects path-like colon leads without over-matching prose", () => {
    expect(isPathLikeColonLeadText("src/pages/sessionThread:")).toBe(true);
    expect(isPathLikeColonLeadText("Note:")).toBe(false);
  });
});
