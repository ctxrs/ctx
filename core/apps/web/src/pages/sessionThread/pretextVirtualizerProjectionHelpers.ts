import {
  createPretextVirtualizerCore,
  type PretextVirtualizerLogicalAnchor,
  type PretextVirtualizerSnapshot,
} from "@pretext-virtualizer/core";
import type {
  PretextVirtualizerItemAlign,
  PretextVirtualizerItemLocation,
} from "@pretext-virtualizer/interface";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";

export function resolveScrollTopForLocation(
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  location: PretextVirtualizerItemLocation,
  core: ReturnType<typeof createPretextVirtualizerCore<WorkbenchListItem>>,
  itemCount: number,
): number {
  if (snapshot.visibleItems.length === 0 && location.index === "LAST") {
    return Math.max(0, snapshot.totalHeight - snapshot.viewportHeight);
  }
  const totalHeight = snapshot.totalHeight;
  const viewportHeight = snapshot.viewportHeight;
  const maxScrollTop = Math.max(0, totalHeight - viewportHeight);
  const rawIndex = location.index === "LAST" ? Math.max(0, itemCount - 1) : location.index;
  const targetIndex = Math.max(0, Math.min(rawIndex, Math.max(0, itemCount - 1)));
  const top = core.getOffsetForIndex(targetIndex);
  const height = core.getHeightForIndex(targetIndex);
  const align: PretextVirtualizerItemAlign = location.align ?? "start";
  if (align === "end") {
    return Math.max(0, Math.min(maxScrollTop, top + height - viewportHeight));
  }
  if (align === "center") {
    return Math.max(0, Math.min(maxScrollTop, top + height / 2 - viewportHeight / 2));
  }
  return Math.max(0, Math.min(maxScrollTop, top));
}

export function approximateIndexForLocation(
  items: readonly WorkbenchListItem[],
  location: PretextVirtualizerItemLocation,
): number {
  if (items.length === 0) return 0;
  if (location.index === "LAST") return items.length - 1;
  return Math.max(0, Math.min(location.index, items.length - 1));
}

export function haveSameItemRefs(
  current: readonly WorkbenchListItem[],
  next: readonly WorkbenchListItem[],
): boolean {
  if (current === next) return true;
  if (current.length !== next.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index] !== next[index]) return false;
  }
  return true;
}

export function haveSameLayoutInputs(
  current: readonly WorkbenchListItem[],
  next: readonly WorkbenchListItem[],
  getLayoutRevision: (item: WorkbenchListItem) => string | number,
): boolean {
  if (current === next) return true;
  if (current.length !== next.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    const currentItem = current[index];
    const nextItem = next[index];
    if (!currentItem || !nextItem) return false;
    if (currentItem.id !== nextItem.id) return false;
    if (getLayoutRevision(currentItem) !== getLayoutRevision(nextItem)) return false;
  }
  return true;
}

export function haveSameItemIds(
  current: readonly WorkbenchListItem[],
  next: readonly WorkbenchListItem[],
): boolean {
  if (current === next) return true;
  if (current.length !== next.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index]?.id !== next[index]?.id) return false;
  }
  return true;
}

function nextItemsEndWithPreviousItems(
  previousItems: readonly WorkbenchListItem[],
  nextItems: readonly WorkbenchListItem[],
): boolean {
  if (previousItems.length === 0) return false;
  if (nextItems.length <= previousItems.length) return false;
  const prefixLen = nextItems.length - previousItems.length;
  for (let index = 0; index < previousItems.length; index += 1) {
    if (nextItems[prefixLen + index]?.id !== previousItems[index]?.id) {
      return false;
    }
  }
  return true;
}

function nextItemsEndWithStablePreviousItems(
  previousItems: readonly WorkbenchListItem[],
  nextItems: readonly WorkbenchListItem[],
): boolean {
  if (!nextItemsEndWithPreviousItems(previousItems, nextItems)) {
    return false;
  }
  const prefixLen = nextItems.length - previousItems.length;
  for (let index = 0; index < previousItems.length; index += 1) {
    if (nextItems[prefixLen + index] !== previousItems[index]) {
      return false;
    }
  }
  return true;
}

function previousItemsEndWithNextItems(
  previousItems: readonly WorkbenchListItem[],
  nextItems: readonly WorkbenchListItem[],
): boolean {
  if (nextItems.length === 0) return false;
  if (previousItems.length <= nextItems.length) return false;
  const prefixLen = previousItems.length - nextItems.length;
  for (let index = 0; index < nextItems.length; index += 1) {
    if (previousItems[prefixLen + index]?.id !== nextItems[index]?.id) {
      return false;
    }
  }
  return true;
}

function containsItemId(
  items: readonly WorkbenchListItem[],
  itemId: string,
): boolean {
  return items.some((item) => item.id === itemId);
}

export function syncSnapshotForProjectionOp({
  core,
  items,
  projectionOp,
  previousItems,
  anchorOverride,
}: {
  core: ReturnType<typeof createPretextVirtualizerCore<WorkbenchListItem>>;
  items: readonly WorkbenchListItem[];
  projectionOp: WorkbenchThreadProjectionOp;
  previousItems: readonly WorkbenchListItem[];
  anchorOverride?: PretextVirtualizerLogicalAnchor | null;
}): PretextVirtualizerSnapshot<WorkbenchListItem> {
  if (
    projectionOp.kind !== "replace_session" &&
    anchorOverride?.kind === "item" &&
    previousItems.length > items.length &&
    containsItemId(previousItems, anchorOverride.id) &&
    !containsItemId(items, anchorOverride.id)
  ) {
    return core.syncItems(previousItems, anchorOverride);
  }

  if (
    projectionOp.kind !== "replace_session" &&
    previousItemsEndWithNextItems(previousItems, items)
  ) {
    const preservedPrefixLen = previousItems.length - items.length;
    return core.syncItems(
      [...previousItems.slice(0, preservedPrefixLen), ...items],
      anchorOverride,
    );
  }

  const changedCount = projectionOp.changedItemIds.length;
  switch (projectionOp.kind) {
    case "replace_session":
      return core.replaceItems(items, anchorOverride);
    case "append_stream":
      if (changedCount > 0 && items.length === previousItems.length + changedCount) {
        return core.appendItems(items.slice(items.length - changedCount), anchorOverride);
      }
      if (changedCount > 0 && haveSameItemIds(previousItems, items)) {
        return core.patchItems(
          items,
          projectionOp.changedItemIds,
          projectionOp.remeasureItemIds,
          anchorOverride,
        );
      }
      return core.syncItems(items, anchorOverride);
    case "prepend_history":
      if (nextItemsEndWithStablePreviousItems(previousItems, items)) {
        const prependCount = items.length - previousItems.length;
        return core.prependItems(items.slice(0, prependCount), anchorOverride);
      }
      return core.syncItems(items, anchorOverride);
    case "hydrate_tools":
    case "terminalize_turn":
    case "toggle_expansion":
      return core.patchItems(
        items,
        projectionOp.changedItemIds,
        projectionOp.remeasureItemIds,
        anchorOverride,
      );
    default:
      return core.syncItems(items, anchorOverride);
  }
}

export function isLocalizedProjectionOp(kind: WorkbenchThreadProjectionOp["kind"]): boolean {
  return (
    kind === "append_stream" ||
    kind === "hydrate_tools" ||
    kind === "terminalize_turn" ||
    kind === "toggle_expansion"
  );
}

export function createVisibleItemAnchor(
  visibleItem: PretextVirtualizerSnapshot<WorkbenchListItem>["visibleItems"][number],
  scrollTop: number,
): PretextVirtualizerLogicalAnchor {
  const maxOffsetPx = Math.max(0, visibleItem.height - 1);
  const offsetPx = Math.max(0, Math.min(scrollTop - visibleItem.top, maxOffsetPx));
  return {
    kind: "item",
    id: visibleItem.id,
    index: visibleItem.index,
    offsetPx,
    offsetRatio: visibleItem.height > 0 ? offsetPx / visibleItem.height : 0,
  };
}

export function resolveViewportTopAnchorOverride(
  currentSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  fallback: PretextVirtualizerLogicalAnchor,
): PretextVirtualizerLogicalAnchor {
  const viewportTop = currentSnapshot.scrollTop;
  const viewportBottom = viewportTop + currentSnapshot.viewportHeight;
  const topVisibleItem = currentSnapshot.visibleItems.find((visibleItem) => {
    const itemBottom = visibleItem.top + visibleItem.height;
    return itemBottom > viewportTop && visibleItem.top < viewportBottom;
  });
  if (!topVisibleItem) {
    return fallback;
  }
  return createVisibleItemAnchor(topVisibleItem, currentSnapshot.scrollTop);
}

export function resolveHistoryPrependAnchorOverride(
  currentSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  fallback: PretextVirtualizerLogicalAnchor,
): PretextVirtualizerLogicalAnchor {
  if (fallback.kind === "bottom") {
    return fallback;
  }
  return resolveViewportTopAnchorOverride(currentSnapshot, fallback);
}

export function resolveLocalizedAnchorOverride(
  currentSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  projectionOp: WorkbenchThreadProjectionOp,
  activeChangedItemId: string | null,
  fallback: PretextVirtualizerLogicalAnchor,
): PretextVirtualizerLogicalAnchor {
  if (
    projectionOp.changedItemIds.length === 0 ||
    projectionOp.kind === "replace_session" ||
    activeChangedItemId == null
  ) {
    return fallback;
  }
  const viewportTop = currentSnapshot.scrollTop;
  const viewportBottom = viewportTop + currentSnapshot.viewportHeight;
  const visibleChangedItem = currentSnapshot.visibleItems.find((visibleItem) => {
    if (visibleItem.id !== activeChangedItemId) return false;
    const itemBottom = visibleItem.top + visibleItem.height;
    return itemBottom > viewportTop && visibleItem.top < viewportBottom;
  });
  if (!visibleChangedItem) {
    return fallback;
  }
  if (visibleChangedItem.top > viewportTop) {
    return fallback;
  }
  return createVisibleItemAnchor(visibleChangedItem, currentSnapshot.scrollTop);
}

export function resolveInteractionItemId(target: EventTarget | null): string | null {
  if (!(target instanceof Element)) return null;
  const owner = target.closest<HTMLElement>("[data-thread-item-id]");
  return owner?.dataset.threadItemId ?? null;
}
