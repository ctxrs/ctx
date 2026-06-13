import { describe, expect, it } from "vitest";
import {
  assertWholeListPurgeAllowed,
  computeHistoryPrependTailReconcilePlan,
  computeHistoryPrefetchThresholdPx,
  countContiguousOverlapFromStart,
  findSharedItemSizeCacheKeyChanges,
  haveSameItemIdSequence,
  isExactContiguousIdWindow,
  pickAnchorIdsFromRange,
  pickAnchorIdsFromScroller,
  shouldReplaceBottomLockedStructuralUpdate,
  shouldUseRawListItems,
  trimTrailingAppendsWhileScrolledUp,
} from "./sessionMessageListControllerUtils";
import type { WorkbenchListItem } from "./SessionPage.types";

describe("sessionMessageListControllerUtils", () => {
  it("prefetches history earlier than one viewport but caps the threshold", () => {
    expect(computeHistoryPrefetchThresholdPx(0)).toBe(600);
    expect(computeHistoryPrefetchThresholdPx(300)).toBe(600);
    expect(computeHistoryPrefetchThresholdPx(900)).toBe(1600);
    expect(computeHistoryPrefetchThresholdPx(1400)).toBe(1600);
  });

  it("uses raw list items while scrolled away from bottom or during history loading", () => {
    expect(
      shouldUseRawListItems({
        stickToBottom: true,
        pendingHistory: false,
        loadingOlder: false,
      }),
    ).toBe(false);
    expect(
      shouldUseRawListItems({
        stickToBottom: false,
        pendingHistory: false,
        loadingOlder: false,
      }),
    ).toBe(true);
    expect(
      shouldUseRawListItems({
        stickToBottom: true,
        pendingHistory: true,
        loadingOlder: false,
      }),
    ).toBe(true);
    expect(
      shouldUseRawListItems({
        stickToBottom: true,
        pendingHistory: false,
        loadingOlder: true,
      }),
    ).toBe(true);
  });

  it("falls back to range anchoring when DOM anchoring is unavailable", () => {
    expect(pickAnchorIdsFromRange([])).toEqual({ topId: null, anchorId: null });
    expect(
      pickAnchorIdsFromRange([
        { id: "a" },
        { id: "b" },
        { id: "c" },
      ] as unknown as WorkbenchListItem[]),
    ).toEqual({ topId: "a", anchorId: "b" });
  });

  it("anchors using the actually visible DOM rows", () => {
    const scroller = document.createElement("div");
    document.body.appendChild(scroller);

    Object.defineProperty(scroller, "getBoundingClientRect", {
      value: () => ({
        top: 100,
        bottom: 500,
        left: 0,
        right: 0,
        width: 0,
        height: 400,
        x: 0,
        y: 100,
        toJSON: () => ({}),
      }),
    });

    const rows = [
      { id: "offscreen", top: 20, bottom: 90 },
      { id: "first-visible", top: 110, bottom: 180 },
      { id: "middle-visible", top: 190, bottom: 260 },
      { id: "last-visible", top: 270, bottom: 340 },
      { id: "below", top: 505, bottom: 575 },
    ];

    for (const row of rows) {
      const el = document.createElement("div");
      el.setAttribute("role", "listitem");
      el.setAttribute("data-thread-item-id", row.id);
      Object.defineProperty(el, "getBoundingClientRect", {
        value: () => ({
          top: row.top,
          bottom: row.bottom,
          left: 0,
          right: 0,
          width: 0,
          height: row.bottom - row.top,
          x: 0,
          y: row.top,
          toJSON: () => ({}),
        }),
      });
      scroller.appendChild(el);
    }

    expect(pickAnchorIdsFromScroller(scroller)).toEqual({
      topId: "first-visible",
      anchorId: "middle-visible",
    });
  });

  it("only treats exact contiguous ranges as history-extend windows", () => {
    expect(isExactContiguousIdWindow(["b", "c"], ["a", "b", "c", "d"], 1)).toBe(true);
    expect(isExactContiguousIdWindow(["b", "x"], ["a", "b", "c", "d"], 1)).toBe(false);
    expect(isExactContiguousIdWindow(["b", "c"], ["a", "b", "x", "c", "d"], 1)).toBe(false);
    expect(isExactContiguousIdWindow(["b", "c"], ["a", "b"], 1)).toBe(false);
    expect(isExactContiguousIdWindow([], ["a", "b"], 0)).toBe(false);
  });

  it("counts the preserved leading overlap inside a prepended next window", () => {
    expect(countContiguousOverlapFromStart(["b", "c", "d"], ["x", "y", "b", "c", "q"], 2)).toBe(2);
    expect(countContiguousOverlapFromStart(["b", "c", "d"], ["x", "y", "b", "c", "d"], 2)).toBe(3);
    expect(countContiguousOverlapFromStart(["b", "c"], ["x"], 1)).toBe(0);
    expect(countContiguousOverlapFromStart([], ["x", "y"], 0)).toBe(0);
  });

  it("plans a history prepend plus volatile-tail reconcile when the anchor stays in preserved overlap", () => {
    expect(
      computeHistoryPrependTailReconcilePlan({
        currentIds: ["b", "c", "tail-old-1", "tail-old-2", "z"],
        nextIds: ["a", "b", "c", "tail-new-1", "tail-new-2", "z", "live-new"],
        startIndex: 1,
        anchorId: "c",
      }),
    ).toEqual({
      prefixLen: 1,
      overlapLen: 2,
      deleteOffset: 3,
      deleteCount: 3,
      insertStart: 3,
      insertCount: 4,
      suffixLen: 0,
    });
  });

  it("rejects volatile-tail history plans when the anchor would be inside replaced content", () => {
    expect(
      computeHistoryPrependTailReconcilePlan({
        currentIds: ["b", "c", "tail-old-1", "tail-old-2", "z"],
        nextIds: ["a", "b", "c", "tail-new-1", "tail-new-2", "z", "live-new"],
        startIndex: 1,
        anchorId: "tail-old-1",
      }),
    ).toBeNull();
    expect(
      computeHistoryPrependTailReconcilePlan({
        currentIds: ["b", "c"],
        nextIds: ["a", "b", "c"],
        startIndex: 1,
        anchorId: "b",
      }),
    ).toBeNull();
  });

  it("trims deferred trailing appends when current ids are still a contiguous window", () => {
    expect(
      trimTrailingAppendsWhileScrolledUp(
        ["b", "c"],
        [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "live-1" }, { id: "live-2" }],
      ),
    ).toEqual([{ id: "a" }, { id: "b" }, { id: "c" }]);

    expect(
      trimTrailingAppendsWhileScrolledUp(
        ["a", "b"],
        [{ id: "a" }, { id: "b" }, { id: "live-1" }],
      ),
    ).toEqual([{ id: "a" }, { id: "b" }]);
  });

  it("does not trim when current ids are missing or no longer contiguous", () => {
    expect(
      trimTrailingAppendsWhileScrolledUp(
        ["b", "c"],
        [{ id: "a" }, { id: "b" }, { id: "x" }, { id: "c" }, { id: "live-1" }],
      ),
    ).toEqual([{ id: "a" }, { id: "b" }, { id: "x" }, { id: "c" }, { id: "live-1" }]);

    expect(
      trimTrailingAppendsWhileScrolledUp(
        ["missing"],
        [{ id: "a" }, { id: "b" }, { id: "live-1" }],
      ),
    ).toEqual([{ id: "a" }, { id: "b" }, { id: "live-1" }]);
  });

  it("detects size-cache key changes only for shared ids", () => {
    const current = [
      { id: "a", key: "stable-a" },
      { id: "b", key: null },
      { id: "c", key: "stable-c" },
    ];
    const next = [
      { id: "older", key: "older" },
      { id: "a", key: "stable-a-2" },
      { id: "b", key: "stable-b" },
      { id: "c", key: "stable-c" },
      { id: "live", key: "live" },
    ];

    expect(findSharedItemSizeCacheKeyChanges(current, next, (item) => item.key)).toEqual({
      count: 2,
      sampleIds: ["a", "b"],
    });
  });

  it("ignores pure prepends when shared ids keep the same size-cache key", () => {
    const current = [
      { id: "b", key: "stable-b" },
      { id: "c", key: "stable-c" },
    ];
    const next = [
      { id: "a", key: "stable-a" },
      { id: "b", key: "stable-b" },
      { id: "c", key: "stable-c" },
      { id: "live", key: null },
    ];

    expect(findSharedItemSizeCacheKeyChanges(current, next, (item) => item.key)).toEqual({
      count: 0,
      sampleIds: [],
    });
  });

  it("only treats identical id order as a pure size-cache invalidation case", () => {
    expect(haveSameItemIdSequence([{ id: "a" }, { id: "b" }], [{ id: "a" }, { id: "b" }])).toBe(true);
    expect(haveSameItemIdSequence([{ id: "a" }, { id: "b" }], [{ id: "a" }, { id: "c" }])).toBe(false);
    expect(haveSameItemIdSequence([{ id: "a" }, { id: "b" }], [{ id: "x" }, { id: "a" }, { id: "b" }])).toBe(false);
  });

  it("replaces bottom-locked mixed structural updates instead of reconciling them", () => {
    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 393,
        nextLen: 1099,
        prefixLen: 179,
        suffixLen: 1,
        deleteCount: 213,
        insertCount: 919,
      }),
    ).toBe(true);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: false,
        currentLen: 393,
        nextLen: 1099,
        prefixLen: 179,
        suffixLen: 1,
        deleteCount: 213,
        insertCount: 919,
      }),
    ).toBe(false);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 10,
        nextLen: 12,
        prefixLen: 10,
        suffixLen: 0,
        deleteCount: 0,
        insertCount: 2,
      }),
    ).toBe(false);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 231,
        nextLen: 231,
        prefixLen: 92,
        suffixLen: 1,
        deleteCount: 138,
        insertCount: 138,
      }),
    ).toBe(true);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 286,
        nextLen: 393,
        prefixLen: 0,
        suffixLen: 0,
        deleteCount: 286,
        insertCount: 393,
      }),
    ).toBe(true);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 51,
        nextLen: 16,
        prefixLen: 0,
        suffixLen: 1,
        deleteCount: 50,
        insertCount: 15,
      }),
    ).toBe(true);

    expect(
      shouldReplaceBottomLockedStructuralUpdate({
        stickToBottom: true,
        currentLen: 40,
        nextLen: 40,
        prefixLen: 18,
        suffixLen: 18,
        deleteCount: 4,
        insertCount: 4,
      }),
    ).toBe(false);
  });
});

describe("assertWholeListPurgeAllowed", () => {
  it("allows a full purge for replace_session", () => {
    expect(() =>
      assertWholeListPurgeAllowed({
        reason: "replace_session",
        threadOp: {
          kind: "replace_session",
          projectionRevision: 1,
          changedItemIds: ["row-1"],
          remeasureItemIds: ["row-1"],
        },
      }),
    ).not.toThrow();
  });

  it("allows a full purge for reconcile", () => {
    expect(() =>
      assertWholeListPurgeAllowed({
        reason: "bottomLockedStructuralReconcile",
        threadOp: {
          kind: "reconcile",
          projectionRevision: 3,
          changedItemIds: ["row-3"],
          remeasureItemIds: ["row-3"],
        },
      }),
    ).not.toThrow();
  });

  it("throws when a localized op attempts to use a full purge", () => {
    expect(() =>
      assertWholeListPurgeAllowed({
        reason: "terminalize_turn",
        threadOp: {
          kind: "terminalize_turn",
          projectionRevision: 2,
          changedItemIds: ["row-2"],
          remeasureItemIds: ["row-2"],
        },
      }),
    ).toThrow(/full-list purge is reserved for replace_session\/reconcile/i);
  });
});
