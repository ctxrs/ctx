import { describe, expect, it } from "vitest";
import { shouldMarkEmptySessionSwitchRendered } from "./sessionViewVisibleSwitch";

describe("shouldMarkEmptySessionSwitchRendered", () => {
  it("settles an active loaded session with no transcript rows", () => {
    expect(
      shouldMarkEmptySessionSwitchRendered({
        isActive: true,
        stateLoaded: true,
        listItemCount: 0,
      }),
    ).toBe(true);
  });

  it("waits for transcript rendering when rows exist", () => {
    expect(
      shouldMarkEmptySessionSwitchRendered({
        isActive: true,
        stateLoaded: true,
        listItemCount: 1,
      }),
    ).toBe(false);
  });
});
