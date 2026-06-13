import { describe, expect, it } from "vitest";

import { shouldCloseMobileSidebarSwipe } from "./mobileSidebarGesture";

describe("shouldCloseMobileSidebarSwipe", () => {
  it("accepts a clear left swipe", () => {
    expect(
      shouldCloseMobileSidebarSwipe(
        { clientX: 340, clientY: 120 },
        { clientX: 250, clientY: 130 },
      ),
    ).toBe(true);
  });

  it("rejects short horizontal movement", () => {
    expect(
      shouldCloseMobileSidebarSwipe(
        { clientX: 340, clientY: 120 },
        { clientX: 300, clientY: 124 },
      ),
    ).toBe(false);
  });

  it("rejects vertical scrolling", () => {
    expect(
      shouldCloseMobileSidebarSwipe(
        { clientX: 340, clientY: 120 },
        { clientX: 270, clientY: 210 },
      ),
    ).toBe(false);
  });

  it("rejects right swipes", () => {
    expect(
      shouldCloseMobileSidebarSwipe(
        { clientX: 250, clientY: 120 },
        { clientX: 340, clientY: 122 },
      ),
    ).toBe(false);
  });
});
