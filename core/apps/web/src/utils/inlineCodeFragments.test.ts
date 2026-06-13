import { describe, expect, it } from "vitest";
import { splitInlineCodeFragments } from "./inlineCodeFragments";

describe("splitInlineCodeFragments", () => {
  it("coalesces short slash stems into stable path fragments", () => {
    expect(splitInlineCodeFragments("table/pages/inline-code/blockquote/sessionMarkdownMeasurement.ts")).toEqual([
      "table/pages/",
      "inline-",
      "code/blockquote/",
      "sessionMarkdownMeasurement.",
      "ts",
    ]);
  });

  it("preserves hyphen boundaries while still coalescing short slash stems", () => {
    expect(
      splitInlineCodeFragments(
        "sessionThread/sessionThreadDomMeasurement.tsx/inline-code/pages/pretextVirtualizerRowLayout.ts/web",
      ),
    ).toEqual([
      "sessionThread/",
      "sessionThreadDomMeasurement.",
      "tsx/inline-",
      "code/pages/",
      "pretextVirtualizerRowLayout.",
      "ts/web",
    ]);
  });

  it("keeps short hyphenated path prefixes atomic with the following path fragment", () => {
    expect(
      splitInlineCodeFragments(
        "turn-header/fixtures/sessionMarkdownMeasurement.ts/src/blockquote/pretextVirtualizerRowLayout.ts/core",
      ),
    ).toEqual([
      "turn-header/",
      "fixtures/",
      "sessionMarkdownMeasurement.",
      "ts/src/blockquote/",
      "pretextVirtualizerRowLayout.",
      "ts/core",
    ]);
    expect(splitInlineCodeFragments("pages/apps/inline-code/blockquote/turn-header/fixtures/web")).toEqual([
      "pages/apps/",
      "inline-",
      "code/blockquote/",
      "turn-header/",
      "fixtures/",
      "web",
    ]);
  });

  it("does not merge a short slash stem into a long filename fragment", () => {
    expect(splitInlineCodeFragments("apps/e2e/e2e/core/web/pretextVirtualizerRowLayout.ts")).toEqual([
      "apps/e2e/",
      "e2e/core/",
      "web/",
      "pretextVirtualizerRowLayout.",
      "ts",
    ]);
  });

  it("merges short hyphenated path tails that would otherwise diverge across browsers", () => {
    expect(splitInlineCodeFragments("pages/apps/turn-header")).toEqual([
      "pages/apps/",
      "turn-header",
    ]);
  });
});
