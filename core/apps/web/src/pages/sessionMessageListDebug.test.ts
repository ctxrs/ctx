import { beforeEach, describe, expect, it } from "vitest";
import {
  recordSessionMessageListFlashTrace,
  type SessionMessageListFlashSample,
} from "./sessionMessageListDebug";

function sample(
  atMs: number,
  overrides: Partial<SessionMessageListFlashSample>,
): SessionMessageListFlashSample {
  return {
    atMs,
    scrollTop: 100,
    clientHeight: 400,
    scrollHeight: 500,
    maxScrollTop: 100,
    distanceFromMaxScrollPx: 0,
    blankTailPx: 0,
    firstItemId: "row-1",
    firstItemTopPx: -20,
    firstItemBottomPx: 20,
    lastItemId: "row-last",
    lastItemTopPx: 360,
    lastItemBottomPx: 400,
    renderedTopId: "row-1",
    renderedAnchorId: "row-anchor",
    ...overrides,
  };
}

describe("recordSessionMessageListFlashTrace", () => {
  beforeEach(() => {
    window.__wbSessionMessageListDebug = {
      seq: 0,
      entries: [],
      flashSeq: 0,
      flashTraces: [],
      rowSizeMismatchSeq: 0,
      rowSizeMismatches: [],
    };
  });

  it("does not flag monotonic bottom-locked replace settling as snapback", () => {
    const trace = recordSessionMessageListFlashTrace({
      sessionId: "session-1",
      cause: "data:replace",
      startedAtMs: 0,
      finishedAtMs: 120,
      layoutShiftCount: 0,
      samples: [
        sample(0, {}),
        sample(20, {
          scrollTop: 1000,
          scrollHeight: 1400,
          maxScrollTop: 1000,
          firstItemTopPx: -1800,
          firstItemBottomPx: -1760,
        }),
        sample(40, {
          scrollTop: 1000,
          scrollHeight: 1400,
          maxScrollTop: 1000,
          firstItemTopPx: -1800,
          firstItemBottomPx: -1760,
        }),
        sample(80, {
          scrollTop: 1000,
          scrollHeight: 1400,
          maxScrollTop: 1000,
          firstItemTopPx: -24,
          firstItemBottomPx: 16,
        }),
      ],
      detail: { reason: "bottomLockedStructuralReconcile" },
    });

    expect(trace?.snapbackDetected).toBe(false);
  });

  it("still flags snapback when bottom drifts away and settles back", () => {
    const trace = recordSessionMessageListFlashTrace({
      sessionId: "session-1",
      cause: "data:replace",
      startedAtMs: 0,
      finishedAtMs: 120,
      layoutShiftCount: 0,
      samples: [
        sample(0, {}),
        sample(20, { distanceFromMaxScrollPx: 120, blankTailPx: 120 }),
        sample(40, { distanceFromMaxScrollPx: 120, blankTailPx: 120 }),
        sample(80, { distanceFromMaxScrollPx: 0, blankTailPx: 0 }),
      ],
      detail: { reason: "bottomLockedStructuralReconcile" },
    });

    expect(trace?.snapbackDetected).toBe(true);
  });
});
