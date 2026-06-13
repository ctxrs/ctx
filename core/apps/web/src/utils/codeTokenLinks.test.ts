import { describe, expect, it } from "vitest";
import { isAbsolutePath, parseFileRefToken, parseUrlToken, splitWhitespaceTokens } from "./codeTokenLinks";

describe("splitWhitespaceTokens", () => {
  it("preserves whitespace segments", () => {
    expect(splitWhitespaceTokens("a b\tc\n")).toEqual(["a", " ", "b", "\t", "c", "\n"]);
  });
});

describe("parseFileRefToken", () => {
  it("accepts explicit relative paths", () => {
    expect(parseFileRefToken("./foo/bar.ts")).toEqual({ path: "./foo/bar.ts" });
    expect(parseFileRefToken("../foo/bar.ts")).toEqual({ path: "../foo/bar.ts" });
  });

  it("accepts paths with slashes", () => {
    expect(parseFileRefToken("core/apps/web/src/App.tsx")).toEqual({
      path: "core/apps/web/src/App.tsx",
    });
  });

  it("accepts dot and tilde paths", () => {
    expect(parseFileRefToken(".")).toEqual({ path: "." });
    expect(parseFileRefToken("..")).toEqual({ path: ".." });
    expect(parseFileRefToken("~")).toEqual({ path: "~" });
  });

  it("parses line and column suffixes when path cues exist", () => {
    expect(parseFileRefToken("foo/bar.ts:12:3")).toEqual({
      path: "foo/bar.ts",
      line: 12,
      col: 3,
    });
    expect(parseFileRefToken("foo/bar.ts#L9C2")).toEqual({
      path: "foo/bar.ts",
      line: 9,
      col: 2,
    });
  });

  it("rejects ambiguous tokens without path cues", () => {
    expect(parseFileRefToken("App.tsx")).toBeNull();
    expect(parseFileRefToken("1.2.3")).toBeNull();
    expect(parseFileRefToken("foo:12")).toBeNull();
  });

  it("ignores urls", () => {
    expect(parseFileRefToken("https://example.com/foo")).toBeNull();
  });
});

describe("parseUrlToken", () => {
  it("accepts http and https urls", () => {
    expect(parseUrlToken("http://127.0.0.1:54321")).toEqual({ url: "http://127.0.0.1:54321" });
    expect(parseUrlToken("https://example.com/foo")).toEqual({ url: "https://example.com/foo" });
  });

  it("rejects non-http schemes", () => {
    expect(parseUrlToken("ctx://open?path=%2Ftmp%2Ffoo")).toBeNull();
    expect(parseUrlToken("file:///tmp/foo")).toBeNull();
  });
});

describe("isAbsolutePath", () => {
  it("detects unix, tilde, and windows absolute paths", () => {
    expect(isAbsolutePath("/tmp/foo")).toBe(true);
    expect(isAbsolutePath("~/foo")).toBe(true);
    expect(isAbsolutePath("C:\\repo\\foo")).toBe(true);
    expect(isAbsolutePath("C:/repo/foo")).toBe(true);
    expect(isAbsolutePath("repo/foo")).toBe(false);
  });
});
