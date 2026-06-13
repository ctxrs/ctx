import type { WorkbenchListItem } from "./SessionPage.types";

export function splitWorkbenchListItemsByGroup(params: {
  listItems: WorkbenchListItem[];
  groupRanges: ReadonlyMap<string, { start: number; end: number }>;
  liveGroupKey: string | null;
}): {
  historyListItems: WorkbenchListItem[];
  liveTailItems: WorkbenchListItem[];
} {
  const { listItems, groupRanges, liveGroupKey } = params;
  if (!liveGroupKey) {
    return { historyListItems: listItems, liveTailItems: [] };
  }
  const liveRange = groupRanges.get(liveGroupKey);
  if (!liveRange) {
    return { historyListItems: listItems, liveTailItems: [] };
  }
  const start = Math.max(0, liveRange.start);
  if (start >= listItems.length) {
    return { historyListItems: listItems, liveTailItems: [] };
  }
  return {
    historyListItems: listItems.slice(0, start),
    liveTailItems: listItems.slice(start),
  };
}
