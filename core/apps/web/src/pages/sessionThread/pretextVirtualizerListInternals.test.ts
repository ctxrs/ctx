import { createPretextVirtualizerCore } from "@pretext-virtualizer/core";
import type { PretextVirtualizerLogicalAnchor, PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import { describe, expect, it, vi } from "vitest";
import type { WorkbenchListItem } from "../SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import {
  resolveHistoryPrependAnchorOverride,
  resolveLocalizedAnchorOverride,
  syncSnapshotForProjectionOp,
  resolveViewportTopAnchorOverride,
} from "./pretextVirtualizerProjectionHelpers";

function makeMessage(id: string): Extract<WorkbenchListItem, { kind: "message" }> {
  return {
    kind: "message",
    id,
    role: "user",
    content: id,
    attachments: [],
    created_at: "2026-04-15T00:00:00.000Z",
  };
}

function createCore(
  items: readonly WorkbenchListItem[],
  heights: Record<string, number>,
) {
  return createPretextVirtualizerCore<WorkbenchListItem>({
    initialItems: items,
    getPlannedLayout: (item) => ({ height: heights[item.id] ?? 40 }),
    getId: (item) => item.id,
    getLayoutRevision: () => 0,
    viewportHeight: 100,
    viewportWidth: 320,
    overscanPx: 0,
  });
}

function makeSnapshot(
  visibleItems: PretextVirtualizerSnapshot<WorkbenchListItem>["visibleItems"],
  scrollTop = 100,
): PretextVirtualizerSnapshot<WorkbenchListItem> {
  return {
    scrollTop,
    viewportHeight: 300,
    viewportWidth: 900,
    totalHeight: 2000,
    widthBucket: "w14",
    anchor: { kind: "bottom" },
    visibleItems,
  };
}

const projectionOp: WorkbenchThreadProjectionOp = {
  kind: "toggle_expansion",
  projectionRevision: 1,
  changedItemIds: ["changed"],
  remeasureItemIds: ["changed"],
};

describe("resolveLocalizedAnchorOverride", () => {
  it("keeps the fallback anchor when the interacted row starts below the viewport top", () => {
    const snapshot = makeSnapshot([
      {
        id: "top",
        index: 0,
        item: makeMessage("top"),
        layoutRevision: "top",
        top: 80,
        height: 60,
        widthBucket: "w14",
      },
      {
        id: "changed",
        index: 1,
        item: makeMessage("changed"),
        layoutRevision: "changed",
        top: 220,
        height: 140,
        widthBucket: "w14",
      },
    ]);
    const fallback: PretextVirtualizerLogicalAnchor = {
      kind: "item",
      id: "top",
      index: 0,
      offsetPx: 20,
      offsetRatio: 20 / 60,
    };

    expect(resolveLocalizedAnchorOverride(snapshot, projectionOp, "changed", fallback)).toEqual(fallback);
  });

  it("anchors to the interacted row when that row already overlaps the viewport top", () => {
    const snapshot = makeSnapshot([
      {
        id: "changed",
        index: 0,
        item: makeMessage("changed"),
        layoutRevision: "changed",
        top: 60,
        height: 140,
        widthBucket: "w14",
      },
    ]);
    const fallback: PretextVirtualizerLogicalAnchor = { kind: "bottom" };

    expect(resolveLocalizedAnchorOverride(snapshot, projectionOp, "changed", fallback)).toEqual({
      kind: "item",
      id: "changed",
      index: 0,
      offsetPx: 40,
      offsetRatio: 40 / 140,
    });
  });
});

describe("resolveViewportTopAnchorOverride", () => {
  it("anchors to the first item that actually intersects the viewport top", () => {
    const snapshot = makeSnapshot([
      {
        id: "overscan",
        index: 0,
        item: makeMessage("overscan"),
        layoutRevision: "overscan",
        top: 20,
        height: 60,
        widthBucket: "w14",
      },
      {
        id: "sliver",
        index: 1,
        item: makeMessage("sliver"),
        layoutRevision: "sliver",
        top: 80,
        height: 40,
        widthBucket: "w14",
      },
      {
        id: "next",
        index: 2,
        item: makeMessage("next"),
        layoutRevision: "next",
        top: 120,
        height: 80,
        widthBucket: "w14",
      },
    ]);
    const fallback: PretextVirtualizerLogicalAnchor = { kind: "bottom" };

    expect(resolveViewportTopAnchorOverride(snapshot, fallback)).toEqual({
      kind: "item",
      id: "sliver",
      index: 1,
      offsetPx: 20,
      offsetRatio: 0.5,
    });
  });
});

describe("resolveHistoryPrependAnchorOverride", () => {
  it("uses the top-edge visible item when the scroller is still top-pinned", () => {
    const snapshot = makeSnapshot(
      [
        {
          id: "older-1",
          index: 0,
          item: makeMessage("older-1"),
          layoutRevision: "older-1",
          top: 0,
          height: 40,
          widthBucket: "w14",
        },
        {
          id: "older-2",
          index: 1,
          item: makeMessage("older-2"),
          layoutRevision: "older-2",
          top: 40,
          height: 80,
          widthBucket: "w14",
        },
      ],
      0,
    );
    const fallback: PretextVirtualizerLogicalAnchor = {
      kind: "item",
      id: "older-2",
      index: 1,
      offsetPx: 0,
      offsetRatio: 0,
    };

    expect(resolveHistoryPrependAnchorOverride(snapshot, fallback)).toEqual({
      kind: "item",
      id: "older-1",
      index: 0,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });

  it("preserves the current viewport-top item even after the user has moved away from the top edge", () => {
    const snapshot = makeSnapshot(
      [
        {
          id: "sliver",
          index: 0,
          item: makeMessage("sliver"),
          layoutRevision: "sliver",
          top: 240,
          height: 40,
          widthBucket: "w14",
        },
        {
          id: "anchor",
          index: 1,
          item: makeMessage("anchor"),
          layoutRevision: "anchor",
          top: 280,
          height: 120,
          widthBucket: "w14",
        },
      ],
      260,
    );
    const fallback: PretextVirtualizerLogicalAnchor = {
      kind: "item",
      id: "anchor",
      index: 1,
      offsetPx: 20,
      offsetRatio: 20 / 120,
    };

    expect(resolveHistoryPrependAnchorOverride(snapshot, fallback)).toEqual({
      kind: "item",
      id: "sliver",
      index: 0,
      offsetPx: 20,
      offsetRatio: 0.5,
    });
  });
});

describe("syncSnapshotForProjectionOp", () => {
  it("preserves the active anchor across prepend_history updates", () => {
    const previousItems = [makeMessage("item-1"), makeMessage("item-2"), makeMessage("item-3")];
    const nextItems = [
      makeMessage("older-1"),
      makeMessage("older-2"),
      ...previousItems,
    ];
    const heights = {
      "older-1": 24,
      "older-2": 36,
      "item-1": 40,
      "item-2": 64,
      "item-3": 72,
    };
    const core = createCore(previousItems, heights);
    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 40,
    });

    const snapshot = syncSnapshotForProjectionOp({
      core,
      items: nextItems,
      projectionOp: {
        kind: "prepend_history",
        projectionRevision: 1,
        changedItemIds: ["older-1", "older-2"],
        remeasureItemIds: ["older-1", "older-2"],
      },
      previousItems,
    });

    expect(snapshot.scrollTop).toBe(100);
    expect(snapshot.anchor).toEqual({
      kind: "item",
      id: "item-2",
      index: 3,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });

  it("uses prependItems without a full sync when the retained suffix is exact", () => {
    const previousItems = [makeMessage("item-1"), makeMessage("item-2"), makeMessage("item-3")];
    const nextItems = [makeMessage("older-1"), ...previousItems];
    const core = createCore(previousItems, {
      "older-1": 24,
      "item-1": 40,
      "item-2": 64,
      "item-3": 72,
    });
    const prependSpy = vi.spyOn(core, "prependItems");
    const syncSpy = vi.spyOn(core, "syncItems");

    syncSnapshotForProjectionOp({
      core,
      items: nextItems,
      projectionOp: {
        kind: "prepend_history",
        projectionRevision: 1,
        changedItemIds: ["older-1"],
        remeasureItemIds: ["older-1"],
      },
      previousItems,
    });

    expect(prependSpy).toHaveBeenCalledTimes(1);
    expect(syncSpy).not.toHaveBeenCalled();
  });

  it("falls back to full sync when prepended history also mutates the retained suffix", () => {
    const previousItems = [
      makeMessage("item-1"),
      { ...makeMessage("item-2"), content: "old payload" },
      makeMessage("item-3"),
    ];
    const nextItems = [
      makeMessage("older-1"),
      makeMessage("older-2"),
      previousItems[0]!,
      { ...previousItems[1]!, content: "new payload" },
      previousItems[2]!,
    ];
    const core = createCore(previousItems, {
      "older-1": 24,
      "older-2": 36,
      "item-1": 40,
      "item-2": 64,
      "item-3": 72,
    });
    const prependSpy = vi.spyOn(core, "prependItems");
    const syncSpy = vi.spyOn(core, "syncItems");
    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 40,
    });

    const snapshot = syncSnapshotForProjectionOp({
      core,
      items: nextItems,
      projectionOp: {
        kind: "prepend_history",
        projectionRevision: 1,
        changedItemIds: ["older-1", "older-2"],
        remeasureItemIds: ["older-1", "older-2"],
      },
      previousItems,
    });

    const refreshedItem = snapshot.visibleItems.find((item) => item.id === "item-2")?.item;
    expect(prependSpy).not.toHaveBeenCalled();
    expect(syncSpy).toHaveBeenCalledTimes(1);
    expect(refreshedItem && refreshedItem.kind === "message" ? refreshedItem.content : null).toBe("new payload");
  });

  it("preserves prepended history across a shorter bounded suffix reconcile", () => {
    const previousItems = [
      makeMessage("older-1"),
      makeMessage("older-2"),
      makeMessage("item-1"),
      { ...makeMessage("item-2"), content: "old payload" },
      makeMessage("item-3"),
    ];
    const nextItems = [
      previousItems[2]!,
      { ...previousItems[3]!, content: "new payload" },
      previousItems[4]!,
    ];
    const core = createCore(previousItems, {
      "older-1": 24,
      "older-2": 36,
      "item-1": 40,
      "item-2": 64,
      "item-3": 72,
    });
    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 100,
    });

    const snapshot = syncSnapshotForProjectionOp({
      core,
      items: nextItems,
      projectionOp: {
        kind: "reconcile",
        projectionRevision: 2,
        changedItemIds: ["item-2"],
        remeasureItemIds: ["item-2"],
      },
      previousItems,
    });

    expect(snapshot.totalHeight).toBe(236);
    expect(snapshot.anchor).toEqual({
      kind: "item",
      id: "item-2",
      index: 3,
      offsetPx: 0,
      offsetRatio: 0,
    });
    const refreshedItem = snapshot.visibleItems.find((item) => item.id === "item-2")?.item;
    expect(refreshedItem && refreshedItem.kind === "message" ? refreshedItem.content : null).toBe("new payload");
  });

  it("ignores shorter detached updates that omit the anchored history item", () => {
    const previousItems = [
      makeMessage("older-1"),
      makeMessage("older-2"),
      makeMessage("anchor-item"),
      makeMessage("item-2"),
      makeMessage("item-3"),
    ];
    const nextItems = [
      makeMessage("item-2"),
      makeMessage("item-3"),
    ];
    const core = createCore(previousItems, {
      "older-1": 24,
      "older-2": 36,
      "anchor-item": 40,
      "item-2": 64,
      "item-3": 72,
    });
    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 100,
    });

    const snapshot = syncSnapshotForProjectionOp({
      core,
      items: nextItems,
      projectionOp: {
        kind: "reconcile",
        projectionRevision: 3,
        changedItemIds: ["item-2", "item-3"],
        remeasureItemIds: ["item-2", "item-3"],
      },
      previousItems,
      anchorOverride: {
        kind: "item",
        id: "anchor-item",
        index: 2,
        offsetPx: 0,
        offsetRatio: 0,
      },
    });

    expect(snapshot.totalHeight).toBe(236);
    expect(snapshot.anchor).toEqual({
      kind: "item",
      id: "anchor-item",
      index: 2,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });
});
