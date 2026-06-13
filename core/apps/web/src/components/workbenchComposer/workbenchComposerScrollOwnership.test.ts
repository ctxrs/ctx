import { describe, expect, it } from "vitest";

import {
  normalizeComposerWheelDeltaY,
  resolveComposerWheelTarget,
} from "./workbenchComposerScrollOwnership";

describe("normalizeComposerWheelDeltaY", () => {
  it("keeps pixel-mode wheel deltas unchanged", () => {
    expect(normalizeComposerWheelDeltaY(120, 0, 18)).toBe(120);
  });

  it("converts line-mode deltas with the supplied line height", () => {
    expect(normalizeComposerWheelDeltaY(3, 1, 18)).toBe(54);
  });
});

describe("resolveComposerWheelTarget", () => {
  it("keeps the gesture inside the composer when it can still scroll upward", () => {
    expect(
      resolveComposerWheelTarget(
        {
          scrollTop: 48,
          clientHeight: 220,
          scrollHeight: 520,
        },
        -120,
      ),
    ).toBe("composer");
  });

  it("hands the gesture to the transcript when the composer is already at the top edge", () => {
    expect(
      resolveComposerWheelTarget(
        {
          scrollTop: 0,
          clientHeight: 220,
          scrollHeight: 520,
        },
        -120,
      ),
    ).toBe("transcript");
  });

  it("keeps the gesture inside the composer when it can still scroll downward", () => {
    expect(
      resolveComposerWheelTarget(
        {
          scrollTop: 40,
          clientHeight: 220,
          scrollHeight: 520,
        },
        120,
      ),
    ).toBe("composer");
  });

  it("hands the gesture to the transcript when the composer is already at the bottom edge", () => {
    expect(
      resolveComposerWheelTarget(
        {
          scrollTop: 300,
          clientHeight: 220,
          scrollHeight: 520,
        },
        120,
      ),
    ).toBe("transcript");
  });

  it("hands the gesture to the transcript when the composer has no inner overflow", () => {
    expect(
      resolveComposerWheelTarget(
        {
          scrollTop: 0,
          clientHeight: 220,
          scrollHeight: 220,
        },
        120,
      ),
    ).toBe("transcript");
  });
});
