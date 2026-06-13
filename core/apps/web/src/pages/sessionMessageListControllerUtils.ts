import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";

export const HISTORY_PREFETCH_MIN_PX = 600;
export const HISTORY_PREFETCH_VIEWPORT_MULTIPLIER = 2;
export const HISTORY_PREFETCH_MAX_PX = 1600;
export const VISIBLE_ITEM_MARGIN_PX = 4;

export type HistoryPrependTailReconcilePlan = {
  prefixLen: number;
  overlapLen: number;
  deleteOffset: number;
  deleteCount: number;
  insertStart: number;
  insertCount: number;
  suffixLen: number;
};

export function computeHistoryPrefetchThresholdPx(visibleListHeight: number): number {
  const normalizedHeight = Number.isFinite(visibleListHeight) ? Math.max(0, visibleListHeight) : 0;
  const target = Math.max(HISTORY_PREFETCH_MIN_PX, normalizedHeight * HISTORY_PREFETCH_VIEWPORT_MULTIPLIER);
  return Math.min(HISTORY_PREFETCH_MAX_PX, target);
}

export function shouldUseRawListItems(params: {
  stickToBottom: boolean;
  pendingHistory: boolean;
  loadingOlder: boolean;
}): boolean {
  const { stickToBottom, pendingHistory, loadingOlder } = params;
  return !stickToBottom || pendingHistory || loadingOlder;
}

export function pickAnchorIdsFromScroller(scroller: HTMLElement | null): {
  topId: string | null;
  anchorId: string | null;
} {
  if (!scroller) {
    return { topId: null, anchorId: null };
  }
  const scrollerRect = scroller.getBoundingClientRect();
  const items = Array.from(scroller.querySelectorAll<HTMLElement>("[role=\"listitem\"]"));
  const visibleIds: string[] = [];

  for (const item of items) {
    const rect = item.getBoundingClientRect();
    if (rect.bottom <= scrollerRect.top + VISIBLE_ITEM_MARGIN_PX) continue;
    if (rect.top >= scrollerRect.bottom - VISIBLE_ITEM_MARGIN_PX) continue;
    const itemId = item.getAttribute("data-thread-item-id");
    if (!itemId) continue;
    visibleIds.push(itemId);
  }

  if (visibleIds.length === 0) {
    return { topId: null, anchorId: null };
  }
  const middleIndex = Math.floor(visibleIds.length / 2);
  return {
    topId: visibleIds[0] ?? null,
    anchorId: visibleIds[middleIndex] ?? visibleIds[0] ?? null,
  };
}

export function pickAnchorIdsFromRange(range: WorkbenchListItem[]): {
  topId: string | null;
  anchorId: string | null;
} {
  const topId = range[0]?.id ?? null;
  const middleIndex = range.length > 0 ? Math.floor(range.length / 2) : 0;
  const anchorId = range[middleIndex]?.id ?? topId;
  return { topId, anchorId };
}

export function isExactContiguousIdWindow(
  currentIds: readonly string[],
  nextIds: readonly string[],
  startIndex: number,
): boolean {
  if (startIndex < 0) return false;
  if (currentIds.length === 0) return false;
  if (startIndex + currentIds.length > nextIds.length) return false;
  for (let index = 0; index < currentIds.length; index += 1) {
    if (nextIds[startIndex + index] !== currentIds[index]) {
      return false;
    }
  }
  return true;
}

export function countContiguousOverlapFromStart(
  currentIds: readonly string[],
  nextIds: readonly string[],
  startIndex: number,
): number {
  if (startIndex < 0) return 0;
  if (currentIds.length === 0) return 0;
  if (startIndex >= nextIds.length) return 0;
  const maxLen = Math.min(currentIds.length, nextIds.length - startIndex);
  let overlapLen = 0;
  while (overlapLen < maxLen && nextIds[startIndex + overlapLen] === currentIds[overlapLen]) {
    overlapLen += 1;
  }
  return overlapLen;
}

export function trimTrailingAppendsWhileScrolledUp<T extends { id: string }>(
  currentIds: readonly string[],
  nextItems: readonly T[],
): T[] {
  if (currentIds.length === 0) return [...nextItems];
  const nextIds = nextItems.map((item) => item.id);
  const firstCurrentId = currentIds[0];
  const startIndex = firstCurrentId ? nextIds.indexOf(firstCurrentId) : -1;
  if (!isExactContiguousIdWindow(currentIds, nextIds, startIndex)) return [...nextItems];
  const lastCurrentIndex = startIndex + currentIds.length - 1;
  if (lastCurrentIndex >= nextItems.length - 1) return [...nextItems];
  return nextItems.slice(0, lastCurrentIndex + 1);
}

export function findSharedItemSizeCacheKeyChanges<T extends { id: string }>(
  currentItems: readonly T[],
  nextItems: readonly T[],
  getItemSizeCacheKey: (item: T) => string | null,
): { count: number; sampleIds: string[] } {
  if (currentItems.length === 0 || nextItems.length === 0) {
    return { count: 0, sampleIds: [] };
  }
  const nextById = new Map(nextItems.map((item) => [item.id, item] as const));
  let count = 0;
  const sampleIds: string[] = [];
  for (const currentItem of currentItems) {
    const nextItem = nextById.get(currentItem.id);
    if (!nextItem) continue;
    const currentKey = getItemSizeCacheKey(currentItem);
    const nextKey = getItemSizeCacheKey(nextItem);
    if (currentKey == null && nextKey == null) continue;
    if (currentKey === nextKey) continue;
    count += 1;
    if (sampleIds.length < 8) sampleIds.push(currentItem.id);
  }
  return { count, sampleIds };
}

export function haveSameItemIdSequence<T extends { id: string }>(
  currentItems: readonly T[],
  nextItems: readonly T[],
): boolean {
  if (currentItems.length !== nextItems.length) return false;
  for (let index = 0; index < currentItems.length; index += 1) {
    if (currentItems[index]?.id !== nextItems[index]?.id) {
      return false;
    }
  }
  return true;
}

export function shouldReplaceBottomLockedStructuralUpdate(params: {
  stickToBottom: boolean;
  currentLen: number;
  nextLen: number;
  prefixLen: number;
  suffixLen: number;
  deleteCount: number;
  insertCount: number;
}): boolean {
  const { stickToBottom, currentLen, nextLen, prefixLen, suffixLen, deleteCount, insertCount } = params;
  if (!stickToBottom) return false;
  if (currentLen <= 0 || nextLen <= 0) return false;
  const isPureAppend = prefixLen === currentLen;
  const isPurePrepend = suffixLen === currentLen;
  if (isPureAppend || isPurePrepend) return false;
  if (prefixLen === 0 && suffixLen === 0) return true;

  const replacedCount = Math.max(deleteCount, insertCount);
  const sharedLen = Math.min(currentLen, nextLen);
  const replacesLargeVisiblePortion =
    sharedLen > 0 && replacedCount >= 32 && replacedCount >= Math.floor(sharedLen / 2);
  if (replacesLargeVisiblePortion) return true;

  const replacesMostOfVisibleList =
    replacedCount >= 64 && sharedLen > 0 && replacedCount >= Math.floor(sharedLen / 2);
  if (replacesMostOfVisibleList) return true;

  return currentLen >= 32 && nextLen >= currentLen * 2;
}

export function assertWholeListPurgeAllowed(params: {
  reason: string;
  threadOp?: WorkbenchThreadProjectionOp | null;
}): void {
  const kind = params.threadOp?.kind ?? null;
  if (!kind || kind === "replace_session" || kind === "reconcile") return;

  const message = `[MessageList] full-list purge is reserved for replace_session/reconcile (reason=${params.reason}, op=${kind})`;
  if (import.meta.env.DEV || import.meta.env.MODE === "test") {
    throw new Error(message);
  }
  // eslint-disable-next-line no-console
  console.error(message);
}

export function computeHistoryPrependTailReconcilePlan(params: {
  currentIds: readonly string[];
  nextIds: readonly string[];
  startIndex: number;
  anchorId: string | null;
}): HistoryPrependTailReconcilePlan | null {
  const { currentIds, nextIds, startIndex, anchorId } = params;
  if (!anchorId) return null;
  if (startIndex <= 0) return null;

  const overlapLen = countContiguousOverlapFromStart(currentIds, nextIds, startIndex);
  if (overlapLen <= 0 || overlapLen >= currentIds.length) return null;

  const anchorIndex = currentIds.indexOf(anchorId);
  if (anchorIndex < 0 || anchorIndex >= overlapLen) return null;

  const currentTailLen = currentIds.length - overlapLen;
  const nextTailStart = startIndex + overlapLen;
  const nextTailLen = nextIds.length - nextTailStart;

  let suffixLen = 0;
  while (
    suffixLen < currentTailLen &&
    suffixLen < nextTailLen &&
    currentIds[currentIds.length - 1 - suffixLen] === nextIds[nextIds.length - 1 - suffixLen]
  ) {
    suffixLen += 1;
  }

  return {
    prefixLen: startIndex,
    overlapLen,
    deleteOffset: startIndex + overlapLen,
    deleteCount: currentTailLen - suffixLen,
    insertStart: nextTailStart,
    insertCount: nextTailLen - suffixLen,
    suffixLen,
  };
}
