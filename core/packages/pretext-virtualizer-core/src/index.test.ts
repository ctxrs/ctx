import { describe, expect, it } from "vitest";
import { createPretextVirtualizerCore } from "./index";

type FixtureItem = {
  id: string;
  layoutRevision: number;
  plannedHeight: number;
};

const makeItems = (...plannedHeights: number[]): FixtureItem[] =>
  plannedHeights.map((plannedHeight, index) => ({
    id: `item-${index + 1}`,
    layoutRevision: 0,
    plannedHeight,
  }));

const createCore = (items: readonly FixtureItem[]) =>
  createPretextVirtualizerCore<FixtureItem>({
    initialItems: items,
    getPlannedLayout: (item) => ({ height: item.plannedHeight }),
    getId: (item) => item.id,
    getLayoutRevision: (item) => item.layoutRevision,
    viewportHeight: 100,
    viewportWidth: 320,
    overscanPx: 0,
  });

describe("createPretextVirtualizerCore", () => {
  it("prefers the first meaningfully visible row when capturing an anchor", () => {
    const core = createCore(makeItems(40, 64, 72));

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 30,
    });

    expect(core.getAnchor()).toEqual({
      kind: "item",
      id: "item-2",
      index: 1,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });

  it("falls back to the first non-sliver visible row when no row is meaningfully visible", () => {
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => ({ height: item.plannedHeight }),
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 8,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 8,
      width: 320,
      scrollTop: 38,
    });

    expect(core.getAnchor()).toEqual({
      kind: "item",
      id: "item-2",
      index: 1,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });

  it("preserves an item anchor when prepending older rows", () => {
    const core = createCore(makeItems(40, 64, 72));

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 40,
    });

    const snapshot = core.prependItems([
      { id: "older-1", layoutRevision: 0, plannedHeight: 24 },
      { id: "older-2", layoutRevision: 0, plannedHeight: 36 },
    ]);

    expect(snapshot.scrollTop).toBe(100);
    expect(snapshot.anchor).toEqual({
      kind: "item",
      id: "item-2",
      index: 3,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });

  it("keeps the viewport pinned to bottom when appending while bottom-following", () => {
    const core = createCore(makeItems(40, 64, 72));

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 76,
    });

    const snapshot = core.appendItems([{ id: "item-4", layoutRevision: 0, plannedHeight: 48 }]);

    expect(snapshot.anchor).toEqual({ kind: "bottom" });
    expect(snapshot.scrollTop).toBe(124);
    expect(snapshot.visibleItems.at(-1)?.id).toBe("item-4");
  });

  it("selects the visible window from the first intersecting row instead of the list head", () => {
    const core = createCore(new Array(20).fill(null).map((_, index) => ({
      id: `item-${index + 1}`,
      layoutRevision: 0,
      plannedHeight: 40,
    })));

    const snapshot = core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 410,
    });

    expect(snapshot.visibleItems.map((item) => item.id)).toEqual([
      "item-11",
      "item-12",
      "item-13",
    ]);
  });

  it("measures only appended rows when growing the tail", () => {
    let measureCalls = 0;
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => {
        measureCalls += 1;
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 76,
    });
    expect(measureCalls).toBe(3);

    core.appendItems([{ id: "item-4", layoutRevision: 0, plannedHeight: 48 }]);

    expect(measureCalls).toBe(4);
  });

  it("measures only prepended rows when growing history", () => {
    let measureCalls = 0;
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => {
        measureCalls += 1;
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 40,
    });
    expect(measureCalls).toBe(3);

    core.prependItems([
      { id: "older-1", layoutRevision: 0, plannedHeight: 24 },
      { id: "older-2", layoutRevision: 0, plannedHeight: 36 },
    ]);

    expect(measureCalls).toBe(5);
  });

  it("captures an item anchor instead of bottom when detached near the tail", () => {
    const core = createCore(makeItems(40, 64, 72, 48));

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 116,
    });

    expect(core.getAnchor()).toEqual({ kind: "bottom" });
    expect(core.getAnchor("detached")).toEqual({
      kind: "item",
      id: "item-3",
      index: 2,
      offsetPx: 12,
      offsetRatio: 12 / 72,
    });
  });

  it("restores an item anchor by ratio when the row height changes", () => {
    const core = createCore(makeItems(40, 64, 72));
    const anchor = {
      kind: "item" as const,
      id: "item-2",
      index: 1,
      offsetPx: 0,
      offsetRatio: 0.5,
    };

    const snapshot = core.restoreAnchor(anchor, "ratio");

    expect(snapshot.scrollTop).toBe(72);

    const grownItems = makeItems(40, 120, 72).map((item) =>
      item.id === "item-2" ? { ...item, layoutRevision: 1 } : item,
    );
    const grownSnapshot = core.syncItems(grownItems, anchor);
    const restoredSnapshot = core.restoreAnchor(anchor, "ratio");

    expect(grownSnapshot.scrollTop).toBe(40);
    expect(restoredSnapshot.scrollTop).toBe(100);
  });

  it("remeasures only rows whose layout revision changed during sync", () => {
    let measureCalls = 0;
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => {
        measureCalls += 1;
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 0,
    });
    expect(measureCalls).toBe(3);

    core.syncItems(
      makeItems(40, 64, 72).map((item) =>
        item.id === "item-2" ? { ...item, layoutRevision: 1, plannedHeight: 96 } : { ...item },
      ),
    );

    expect(measureCalls).toBe(4);
    expect(core.getHeightForIndex(1)).toBe(96);
    expect(core.getOffsetForIndex(2)).toBe(136);
  });

  it("does not remeasure stable rows when synced items only change by reference", () => {
    let measureCalls = 0;
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => {
        measureCalls += 1;
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 0,
    });
    expect(measureCalls).toBe(3);

    core.syncItems(makeItems(40, 64, 72).map((item) => ({ ...item })));

    expect(measureCalls).toBe(3);
  });

  it("patches same-sequence items by explicit remeasure ids", () => {
    let measureCalls = 0;
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item) => {
        measureCalls += 1;
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 0,
    });
    expect(measureCalls).toBe(3);

    core.patchItems(
      makeItems(40, 96, 72).map((item) =>
        item.id === "item-2" ? { ...item, layoutRevision: 1 } : { ...item },
      ),
      ["item-2"],
      ["item-2"],
    );

    expect(measureCalls).toBe(4);
    expect(core.getHeightForIndex(1)).toBe(96);
    expect(core.getOffsetForIndex(2)).toBe(136);
  });

  it("recomputes planned heights when the width bucket changes", () => {
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72),
      getPlannedLayout: (item, viewport) => ({
        height: viewport.widthBucket === "w5" ? item.plannedHeight : item.plannedHeight * 2,
      }),
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      widthBucketSize: 64,
      overscanPx: 0,
    });

    const narrow = core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 0,
    });
    const wide = core.syncViewport({
      height: 100,
      width: 640,
      scrollTop: 0,
    });

    expect(narrow.widthBucket).toBe("w5");
    expect(narrow.totalHeight).toBe(176);
    expect(wide.widthBucket).toBe("w10");
    expect(wide.totalHeight).toBe(352);
    expect(wide.visibleItems[0]?.height).toBe(80);
  });

  it("reuses prefix and suffix heights when rows are inserted in the middle", () => {
    const measuredIds: string[] = [];
    const core = createPretextVirtualizerCore<FixtureItem>({
      initialItems: makeItems(40, 64, 72, 80),
      getPlannedLayout: (item) => {
        measuredIds.push(item.id);
        return { height: item.plannedHeight };
      },
      getId: (item) => item.id,
      getLayoutRevision: (item) => item.layoutRevision,
      viewportHeight: 100,
      viewportWidth: 320,
      overscanPx: 0,
    });

    core.syncViewport({
      height: 100,
      width: 320,
      scrollTop: 0,
    });
    measuredIds.length = 0;

    const snapshot = core.syncItems([
      { id: "item-1", layoutRevision: 0, plannedHeight: 40 },
      { id: "inserted-1", layoutRevision: 0, plannedHeight: 48 },
      { id: "inserted-2", layoutRevision: 0, plannedHeight: 52 },
      { id: "item-2", layoutRevision: 0, plannedHeight: 64 },
      { id: "item-3", layoutRevision: 0, plannedHeight: 72 },
      { id: "item-4", layoutRevision: 0, plannedHeight: 80 },
    ]);

    expect(measuredIds).toEqual(["inserted-1", "inserted-2"]);
    expect(snapshot.totalHeight).toBe(356);
    expect(snapshot.visibleItems.map((item) => item.id)).toEqual([
      "item-1",
      "inserted-1",
      "inserted-2",
    ]);
    expect(core.getOffsetForIndex(3)).toBe(140);
    expect(core.getOffsetForIndex(5)).toBe(276);
  });

  it("resolves offsets and heights by index against the deterministic layout", () => {
    const core = createCore(makeItems(40, 64, 72));

    expect(core.getOffsetForIndex(0)).toBe(0);
    expect(core.getOffsetForIndex(1)).toBe(40);
    expect(core.getOffsetForIndex(2)).toBe(104);
    expect(core.getHeightForIndex(0)).toBe(40);
    expect(core.getHeightForIndex(1)).toBe(64);
    expect(core.getHeightForIndex(2)).toBe(72);
  });
});
