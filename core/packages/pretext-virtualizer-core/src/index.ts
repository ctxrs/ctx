import type {
  PretextVirtualizerAnchorCaptureMode,
  PretextVirtualizerComputedLayout,
  PretextVirtualizerCore,
  PretextVirtualizerCoreOptions,
  PretextVirtualizerLogicalAnchor,
  PretextVirtualizerSnapshot,
  PretextVirtualizerVisibleItem,
  PretextVirtualizerWidthBucket,
  PretextVirtualizerAnchorRestoreMode,
} from "./types";

export type {
  PretextVirtualizerLayoutRevision,
  PretextVirtualizerWidthBucket,
  PretextVirtualizerAnchorRestoreMode,
  PretextVirtualizerPlannedLayout,
  PretextVirtualizerLogicalAnchor,
  PretextVirtualizerVisibleItem,
  PretextVirtualizerSnapshot,
  PretextVirtualizerDiagnosticEvent,
  PretextVirtualizerCoreOptions,
} from "./types";

const DEFAULT_OVERSCAN_PX = 320;
const DEFAULT_BOTTOM_THRESHOLD_PX = 16;
const DEFAULT_WIDTH_BUCKET_SIZE = 64;
const MIN_VISIBLE_ANCHOR_PX = 4;
const MIN_MEANINGFUL_VISIBLE_ANCHOR_PX = 24;

const normalizeHeight = (value: number): number =>
  Number.isFinite(value) && value > 0 ? Math.max(1, Math.round(value * 16) / 16) : 1;

const normalizeSize = (value: number): number =>
  Number.isFinite(value) && value > 0 ? value : 0;

const clamp = (value: number, min: number, max: number): number =>
  Math.max(min, Math.min(max, value));

export const createWidthBucket = (
  viewportWidth: number,
  widthBucketSize = DEFAULT_WIDTH_BUCKET_SIZE,
): PretextVirtualizerWidthBucket => {
  const normalizedWidth = normalizeSize(viewportWidth);
  const normalizedBucketSize = Math.max(1, Math.round(widthBucketSize));
  return `w${Math.floor(normalizedWidth / normalizedBucketSize)}`;
};

export const createPretextVirtualizerCore = <Item,>({
  initialItems = [],
  getPlannedLayout,
  getId,
  getLayoutRevision,
  overscanPx = DEFAULT_OVERSCAN_PX,
  bottomThresholdPx = DEFAULT_BOTTOM_THRESHOLD_PX,
  widthBucketSize = DEFAULT_WIDTH_BUCKET_SIZE,
  viewportHeight = 0,
  viewportWidth = 0,
  onDiagnosticEvent,
}: PretextVirtualizerCoreOptions<Item>): PretextVirtualizerCore<Item> => {
  const state = {
    items: [...initialItems],
    viewportHeight: normalizeSize(viewportHeight),
    viewportWidth: normalizeSize(viewportWidth),
    scrollTop: 0,
  };
  let layoutCache: PretextVirtualizerComputedLayout<Item> | null = null;

  const invalidateLayout = () => {
    layoutCache = null;
  };

  const buildHeightEntry = (
    item: Item,
    width: number,
    widthBucket: `w${number}`,
  ): PretextVirtualizerComputedLayout<Item>["heights"][number] => ({
    id: getId(item),
    item,
    layoutRevision: getLayoutRevision(item),
    height: normalizeHeight(getPlannedLayout(item, { width, widthBucket }).height),
  });

  const recomputeOffsetsFrom = (
    layout: PretextVirtualizerComputedLayout<Item>,
    startIndex: number,
  ) => {
    let runningTop =
      startIndex > 0
        ? (layout.offsets[startIndex - 1] ?? 0) + (layout.heights[startIndex - 1]?.height ?? 0)
        : 0;
    for (let index = startIndex; index < layout.heights.length; index += 1) {
      layout.offsets[index] = runningTop;
      runningTop += layout.heights[index]!.height;
    }
    layout.totalHeight = runningTop;
  };

  const computeLayout = (): PretextVirtualizerComputedLayout<Item> => {
    if (layoutCache) return layoutCache;
    const widthBucket = createWidthBucket(state.viewportWidth, widthBucketSize);
    const heights = state.items.map((item) => buildHeightEntry(item, state.viewportWidth, widthBucket));
    const offsets = new Array<number>(heights.length);
    let runningTop = 0;
    for (let index = 0; index < heights.length; index += 1) {
      offsets[index] = runningTop;
      runningTop += heights[index]!.height;
    }
    layoutCache = {
      widthBucket,
      heights,
      offsets,
      totalHeight: runningTop,
    };
    return layoutCache;
  };

  const getMaxScrollTop = (totalHeight: number): number =>
    Math.max(0, totalHeight - normalizeSize(state.viewportHeight));

  const clampScrollTop = (scrollTop: number, totalHeight: number): number =>
    clamp(Number.isFinite(scrollTop) ? scrollTop : 0, 0, getMaxScrollTop(totalHeight));

  const findFirstIndexWithBottomAfter = (
    layout: PretextVirtualizerComputedLayout<Item>,
    offset: number,
  ): number => {
    const itemCount = layout.heights.length;
    if (itemCount === 0) {
      return 0;
    }
    let low = 0;
    let high = itemCount;
    while (low < high) {
      const middle = (low + high) >> 1;
      const bottom = (layout.offsets[middle] ?? 0) + (layout.heights[middle]?.height ?? 0);
      if (bottom <= offset) {
        low = middle + 1;
      } else {
        high = middle;
      }
    }
    return clamp(low, 0, Math.max(0, itemCount - 1));
  };

  const captureAnchor = (
    layout = computeLayout(),
    scrollTop = state.scrollTop,
    mode: PretextVirtualizerAnchorCaptureMode = "default",
  ): PretextVirtualizerLogicalAnchor => {
    const normalizedScrollTop = clampScrollTop(scrollTop, layout.totalHeight);
    const bottomOffsetPx = layout.totalHeight - (normalizedScrollTop + state.viewportHeight);
    if (mode === "default" && bottomOffsetPx <= bottomThresholdPx) {
      return { kind: "bottom" };
    }
    const viewportBottom = normalizedScrollTop + state.viewportHeight;
    let fallbackAnchor: PretextVirtualizerLogicalAnchor | null = null;
    const startIndex = findFirstIndexWithBottomAfter(layout, normalizedScrollTop);
    for (let index = startIndex; index < layout.heights.length; index += 1) {
      const entry = layout.heights[index]!;
      const top = layout.offsets[index]!;
      const bottom = top + entry.height;
      if (bottom <= normalizedScrollTop) continue;
      const visibleHeight = Math.min(bottom, viewportBottom) - Math.max(top, normalizedScrollTop);
      if (visibleHeight <= MIN_VISIBLE_ANCHOR_PX) continue;
      const offsetPx = clamp(normalizedScrollTop - top, 0, Math.max(0, entry.height - 1));
      const nextAnchor: PretextVirtualizerLogicalAnchor = {
        kind: "item",
        id: entry.id,
        index,
        offsetPx,
        offsetRatio: entry.height > 0 ? offsetPx / entry.height : 0,
      };
      if (visibleHeight >= MIN_MEANINGFUL_VISIBLE_ANCHOR_PX) {
        return nextAnchor;
      }
      fallbackAnchor ??= nextAnchor;
    }
    return fallbackAnchor ?? { kind: "bottom" };
  };

  const createSnapshot = (): PretextVirtualizerSnapshot<Item> => {
    const layout = computeLayout();
    state.scrollTop = clampScrollTop(state.scrollTop, layout.totalHeight);
    const visibleTop = Math.max(0, state.scrollTop - overscanPx);
    const visibleBottom = state.scrollTop + state.viewportHeight + overscanPx;
    const visibleItems: PretextVirtualizerVisibleItem<Item>[] = [];
    const startIndex = findFirstIndexWithBottomAfter(layout, visibleTop);
    for (let index = startIndex; index < layout.heights.length; index += 1) {
      const entry = layout.heights[index]!;
      const top = layout.offsets[index]!;
      const bottom = top + entry.height;
      if (bottom < visibleTop) continue;
      if (top > visibleBottom) break;
      visibleItems.push({
        id: entry.id,
        index,
        item: entry.item,
        layoutRevision: entry.layoutRevision,
        top,
        height: entry.height,
        widthBucket: layout.widthBucket,
      });
    }
    return {
      scrollTop: state.scrollTop,
      viewportHeight: state.viewportHeight,
      viewportWidth: state.viewportWidth,
      totalHeight: layout.totalHeight,
      widthBucket: layout.widthBucket,
      anchor: captureAnchor(layout, state.scrollTop),
      visibleItems,
    };
  };

  const emitDiagnostic = (
    type: string,
    snapshot: PretextVirtualizerSnapshot<Item>,
    detail?: Record<string, unknown> | null,
  ) => {
    onDiagnosticEvent?.({ type, snapshot, detail });
  };

  const restoreAnchorIntoState = (
    anchor: PretextVirtualizerLogicalAnchor,
    mode: PretextVirtualizerAnchorRestoreMode = "offset",
  ): PretextVirtualizerSnapshot<Item> => {
    const layout = computeLayout();
    if (anchor.kind === "bottom") {
      state.scrollTop = getMaxScrollTop(layout.totalHeight);
      const snapshot = createSnapshot();
      emitDiagnostic("restore:bottom", snapshot, { anchorKind: "bottom" });
      return snapshot;
    }
    const resolvedIndex = layout.heights.findIndex((entry) => entry.id === anchor.id);
    const targetIndex = resolvedIndex >= 0 ? resolvedIndex : clamp(anchor.index, 0, Math.max(0, layout.heights.length - 1));
    const target = layout.heights[targetIndex];
    if (!target) {
      const snapshot = createSnapshot();
      emitDiagnostic("restore:missing", snapshot, { anchorKind: "item", missingId: anchor.id });
      return snapshot;
    }
    const top = layout.offsets[targetIndex] ?? 0;
    const targetOffset =
      mode === "ratio"
        ? clamp(anchor.offsetRatio * target.height, 0, Math.max(0, target.height - 1))
        : clamp(anchor.offsetPx, 0, Math.max(0, target.height - 1));
    state.scrollTop = clampScrollTop(top + targetOffset, layout.totalHeight);
    const snapshot = createSnapshot();
    emitDiagnostic("restore:item", snapshot, {
      anchorKind: "item",
      anchorId: anchor.id,
      targetIndex,
      mode,
    });
    return snapshot;
  };

  const preserveAnchorAcrossItems = (
    mutate: () => void,
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ): PretextVirtualizerSnapshot<Item> => {
    const retainedAnchor = anchorOverride ?? createSnapshot().anchor;
    mutate();
    return restoreAnchorIntoState(retainedAnchor, "offset");
  };

  const replaceAllItems = (nextItems: readonly Item[]) => {
    state.items = [...nextItems];
    invalidateLayout();
  };

  const appendIntoLayout = (items: readonly Item[]) => {
    const layout = computeLayout();
    const nextEntries = items.map((item) => buildHeightEntry(item, state.viewportWidth, layout.widthBucket));
    let runningTop = layout.totalHeight;
    for (const entry of nextEntries) {
      layout.offsets.push(runningTop);
      layout.heights.push(entry);
      runningTop += entry.height;
    }
    layout.totalHeight = runningTop;
    state.items = [...state.items, ...items];
  };

  const prependIntoLayout = (items: readonly Item[]) => {
    const layout = computeLayout();
    const nextEntries = items.map((item) => buildHeightEntry(item, state.viewportWidth, layout.widthBucket));
    const prefixHeight = nextEntries.reduce((sum, entry) => sum + entry.height, 0);
    let runningTop = 0;
    const prefixOffsets = new Array<number>(nextEntries.length);
    for (let index = 0; index < nextEntries.length; index += 1) {
      prefixOffsets[index] = runningTop;
      runningTop += nextEntries[index]!.height;
    }
    layout.heights = [...nextEntries, ...layout.heights];
    layout.offsets = [
      ...prefixOffsets,
      ...layout.offsets.map((offset) => offset + prefixHeight),
    ];
    layout.totalHeight += prefixHeight;
    state.items = [...items, ...state.items];
  };

  const syncStableItems = (items: readonly Item[]): boolean => {
    const layout = computeLayout();
    if (layout.heights.length !== items.length) return false;

    let firstHeightChangeIndex: number | null = null;
    for (let index = 0; index < items.length; index += 1) {
      const nextItem = items[index]!;
      const existing = layout.heights[index]!;
      const nextId = getId(nextItem);
      if (existing.id !== nextId) return false;
      const nextLayoutRevision = getLayoutRevision(nextItem);
      if (existing.layoutRevision === nextLayoutRevision) {
        existing.item = nextItem;
        continue;
      }
      const nextHeight = normalizeHeight(
        getPlannedLayout(nextItem, { width: state.viewportWidth, widthBucket: layout.widthBucket }).height,
      );
      if (existing.height !== nextHeight && firstHeightChangeIndex == null) {
        firstHeightChangeIndex = index;
      }
      existing.item = nextItem;
      existing.layoutRevision = nextLayoutRevision;
      existing.height = nextHeight;
    }

    state.items = [...items];
    if (firstHeightChangeIndex != null) {
      recomputeOffsetsFrom(layout, firstHeightChangeIndex);
    }
    return true;
  };

  const patchStableItems = (
    items: readonly Item[],
    changedItemIds: readonly string[],
    remeasureItemIds: readonly string[],
  ): boolean => {
    const layout = computeLayout();
    if (layout.heights.length !== items.length) return false;
    const changedIds = new Set(changedItemIds);
    const remeasureIds = new Set(remeasureItemIds);

    let firstHeightChangeIndex: number | null = null;
    for (let index = 0; index < items.length; index += 1) {
      const nextItem = items[index]!;
      const existing = layout.heights[index]!;
      const nextId = getId(nextItem);
      if (existing.id !== nextId) {
        return false;
      }

      const nextLayoutRevision = getLayoutRevision(nextItem);
      const shouldRemeasure =
        remeasureIds.has(nextId) ||
        existing.layoutRevision !== nextLayoutRevision;
      if (shouldRemeasure) {
        const nextHeight = normalizeHeight(
          getPlannedLayout(nextItem, { width: state.viewportWidth, widthBucket: layout.widthBucket }).height,
        );
        if (existing.height !== nextHeight && firstHeightChangeIndex == null) {
          firstHeightChangeIndex = index;
        }
        existing.item = nextItem;
        existing.layoutRevision = nextLayoutRevision;
        existing.height = nextHeight;
        continue;
      }

      if (changedIds.has(nextId) || existing.item !== nextItem) {
        existing.item = nextItem;
      }
      if (existing.layoutRevision !== nextLayoutRevision) {
        existing.layoutRevision = nextLayoutRevision;
      }
    }

    state.items = [...items];
    if (firstHeightChangeIndex != null) {
      recomputeOffsetsFrom(layout, firstHeightChangeIndex);
    }
    return true;
  };

  const syncDiffWindowItems = (items: readonly Item[]): boolean => {
    const layout = computeLayout();
    const previousLength = layout.heights.length;
    const nextLength = items.length;
    if (previousLength === 0) return false;

    let prefixLength = 0;
    const sharedPrefixLimit = Math.min(previousLength, nextLength);
    while (
      prefixLength < sharedPrefixLimit &&
      layout.heights[prefixLength]?.id === getId(items[prefixLength]!)
    ) {
      prefixLength += 1;
    }

    let suffixLength = 0;
    const sharedSuffixLimit = Math.min(previousLength - prefixLength, nextLength - prefixLength);
    while (
      suffixLength < sharedSuffixLimit &&
      layout.heights[previousLength - 1 - suffixLength]?.id === getId(items[nextLength - 1 - suffixLength]!)
    ) {
      suffixLength += 1;
    }

    if (prefixLength === previousLength && previousLength === nextLength) {
      return syncStableItems(items);
    }
    if (prefixLength === 0 && suffixLength === 0) {
      return false;
    }

    const widthBucket = layout.widthBucket;
    const nextHeights = new Array<PretextVirtualizerComputedLayout<Item>["heights"][number]>(nextLength);
    let firstHeightChangeIndex: number | null = null;

    for (let index = 0; index < prefixLength; index += 1) {
      const nextItem = items[index]!;
      const existing = layout.heights[index]!;
      const nextLayoutRevision = getLayoutRevision(nextItem);
      if (existing.layoutRevision === nextLayoutRevision) {
        nextHeights[index] = { ...existing, item: nextItem };
        continue;
      }
      nextHeights[index] = buildHeightEntry(nextItem, state.viewportWidth, widthBucket);
      if (existing.height !== nextHeights[index]!.height && firstHeightChangeIndex == null) {
        firstHeightChangeIndex = index;
      }
    }

    const middleStart = prefixLength;
    const middleEnd = nextLength - suffixLength;
    for (let index = middleStart; index < middleEnd; index += 1) {
      nextHeights[index] = buildHeightEntry(items[index]!, state.viewportWidth, widthBucket);
    }

    for (let suffixIndex = 0; suffixIndex < suffixLength; suffixIndex += 1) {
      const nextIndex = nextLength - suffixLength + suffixIndex;
      const existingIndex = previousLength - suffixLength + suffixIndex;
      const nextItem = items[nextIndex]!;
      const existing = layout.heights[existingIndex]!;
      const nextLayoutRevision = getLayoutRevision(nextItem);
      if (existing.layoutRevision === nextLayoutRevision) {
        nextHeights[nextIndex] = { ...existing, item: nextItem };
        continue;
      }
      nextHeights[nextIndex] = buildHeightEntry(nextItem, state.viewportWidth, widthBucket);
      if (existing.height !== nextHeights[nextIndex]!.height && firstHeightChangeIndex == null) {
        firstHeightChangeIndex = nextIndex;
      }
    }

    const nextOffsets = new Array<number>(nextLength);
    for (let index = 0; index < Math.min(prefixLength, layout.offsets.length, nextOffsets.length); index += 1) {
      nextOffsets[index] = layout.offsets[index] ?? 0;
    }

    layout.heights = nextHeights;
    layout.offsets = nextOffsets;
    state.items = [...items];

    const offsetRecomputeIndex =
      firstHeightChangeIndex != null
        ? Math.min(firstHeightChangeIndex, middleStart)
        : middleStart;
    recomputeOffsetsFrom(layout, Math.max(0, Math.min(offsetRecomputeIndex, nextLength)));
    return true;
  };

  return {
    getSnapshot: () => createSnapshot(),
    getAnchor: (mode = "default") => {
      const layout = computeLayout();
      return captureAnchor(layout, state.scrollTop, mode);
    },
    syncViewport: ({ height, width, scrollTop }) => {
      state.viewportHeight = normalizeSize(height);
      const normalizedWidth = normalizeSize(width);
      if (normalizedWidth !== state.viewportWidth) {
        state.viewportWidth = normalizedWidth;
        invalidateLayout();
      } else {
        state.viewportWidth = normalizedWidth;
      }
      const layout = computeLayout();
      state.scrollTop = clampScrollTop(scrollTop, layout.totalHeight);
      const snapshot = createSnapshot();
      emitDiagnostic("viewport:sync", snapshot, {
        viewportHeight: state.viewportHeight,
        viewportWidth: state.viewportWidth,
      });
      return snapshot;
    },
    replaceItems: (items, anchorOverride) => {
      const snapshot = preserveAnchorAcrossItems(() => {
        replaceAllItems(items);
      }, anchorOverride);
      emitDiagnostic("items:replace", snapshot, {
        itemCount: items.length,
        anchorOverrideKind: anchorOverride?.kind ?? null,
      });
      return snapshot;
    },
    appendItems: (items, anchorOverride) => {
      const snapshot = preserveAnchorAcrossItems(() => {
        appendIntoLayout(items);
      }, anchorOverride);
      emitDiagnostic("items:append", snapshot, {
        itemCount: state.items.length,
        deltaCount: items.length,
        anchorOverrideKind: anchorOverride?.kind ?? null,
      });
      return snapshot;
    },
    prependItems: (items, anchorOverride) => {
      const snapshot = preserveAnchorAcrossItems(() => {
        prependIntoLayout(items);
      }, anchorOverride);
      emitDiagnostic("items:prepend", snapshot, {
        itemCount: state.items.length,
        deltaCount: items.length,
        anchorOverrideKind: anchorOverride?.kind ?? null,
      });
      return snapshot;
    },
    syncItems: (items, anchorOverride) => {
      const snapshot = preserveAnchorAcrossItems(() => {
        if (!syncStableItems(items) && !syncDiffWindowItems(items)) {
          replaceAllItems(items);
        }
      }, anchorOverride);
      emitDiagnostic("items:sync", snapshot, {
        itemCount: items.length,
        anchorOverrideKind: anchorOverride?.kind ?? null,
      });
      return snapshot;
    },
    patchItems: (items, changedItemIds, remeasureItemIds, anchorOverride) => {
      const snapshot = preserveAnchorAcrossItems(() => {
        if (!patchStableItems(items, changedItemIds, remeasureItemIds)) {
          if (!syncStableItems(items) && !syncDiffWindowItems(items)) {
            replaceAllItems(items);
          }
        }
      }, anchorOverride);
      emitDiagnostic("items:patch", snapshot, {
        itemCount: items.length,
        changedItemCount: changedItemIds.length,
        remeasureItemCount: remeasureItemIds.length,
        anchorOverrideKind: anchorOverride?.kind ?? null,
      });
      return snapshot;
    },
    restoreAnchor: (anchor, mode = "offset") => restoreAnchorIntoState(anchor, mode),
    getOffsetForIndex: (index) => {
      const layout = computeLayout();
      const clampedIndex = clamp(index, 0, Math.max(0, layout.offsets.length - 1));
      return layout.offsets[clampedIndex] ?? 0;
    },
    getHeightForIndex: (index) => {
      const layout = computeLayout();
      const clampedIndex = clamp(index, 0, Math.max(0, layout.heights.length - 1));
      return layout.heights[clampedIndex]?.height ?? 0;
    },
  };
};
