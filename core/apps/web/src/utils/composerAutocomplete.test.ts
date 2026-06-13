import { describe, expect, it } from "vitest";
import {
  applyComposerAutocompleteCompletion,
  detectComposerAutocompleteToken,
} from "./composerAutocomplete";

describe("detectComposerAutocompleteToken", () => {
  it("detects slash token anywhere", () => {
    const text = "please run /rev";
    const cursor = text.length;
    const tok = detectComposerAutocompleteToken(text, cursor);
    expect(tok).toEqual({
      kind: "slash",
      start: "please run ".length,
      end: text.length,
      query: "rev",
    });
  });

  it("detects @ token anywhere", () => {
    const text = "check @src/pa please";
    const cursor = "check @src/pa".length;
    const tok = detectComposerAutocompleteToken(text, cursor);
    expect(tok?.kind).toBe("at");
    expect(tok?.query).toBe("src/pa");
  });

  it("does not trigger inside URLs or emails", () => {
    expect(detectComposerAutocompleteToken("http://example.com", 10)).toBeNull();
    expect(detectComposerAutocompleteToken("foo@bar.com", 5)).toBeNull();
  });

  it("ignores double-trigger tokens", () => {
    expect(detectComposerAutocompleteToken("// comment", 2)).toBeNull();
    expect(detectComposerAutocompleteToken("@@test", 2)).toBeNull();
  });
});

describe("applyComposerAutocompleteCompletion", () => {
  it("replaces the token range and moves cursor", () => {
    const text = "please run /rev now";
    const cursor = "please run /rev".length;
    const tok = detectComposerAutocompleteToken(text, cursor)!;
    const out = applyComposerAutocompleteCompletion(text, tok, "/review");
    expect(out.nextText).toBe("please run /review now");
    expect(out.nextCursor).toBe("please run /review".length);
  });

  it("adds a trailing space when needed", () => {
    const text = "look @src/pa";
    const cursor = text.length;
    const tok = detectComposerAutocompleteToken(text, cursor)!;
    const out = applyComposerAutocompleteCompletion(text, tok, "@src/pages/SessionPage.tsx");
    expect(out.nextText.endsWith(" ")).toBe(true);
  });
});
