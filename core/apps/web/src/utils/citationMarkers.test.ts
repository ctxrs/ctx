import { describe, expect, it } from "vitest";
import { stripCitationMarkers } from "./citationMarkers";

describe("stripCitationMarkers", () => {
  it("removes well-formed citation markers", () => {
    const input = `hello citeturn2search5 world`;
    expect(stripCitationMarkers(input)).toBe("hello  world");
  });

  it("removes stray private-use glyphs", () => {
    const input = `a\uE200b\uE202c\uE201d`;
    // Strip removes the whole marker envelope (including payload), not just the wrapper glyphs.
    expect(stripCitationMarkers(input)).toBe("ad");
  });

  it("removes unterminated citation markers without leaking payload", () => {
    const input = `hello \uE200cite\uE202turn0search3`;
    expect(stripCitationMarkers(input)).toBe("hello ");
  });
});
