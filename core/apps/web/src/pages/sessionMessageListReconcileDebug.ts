import type { WorkbenchListItem } from "./SessionPage.types";
import { debugItemSummary } from "./sessionMessageListDataDebug";

type Params = {
  devEnabled: boolean;
  showDebug: boolean;
  sessionId: string;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentIds: string[];
  nextIds: string[];
  currentLen: number;
  nextLen: number;
  prefixLen: number;
  suffixLen: number;
  deleteCount: number;
  insertDataLength: number;
  anchorId: string | null;
  anchorIndex: number;
  historyExpected: boolean;
  stickToBottom: boolean;
  suppressIdDiffLogs: boolean;
};

const resolveMapMode = (stickToBottom: boolean, anchorIndex: number): "mapWithAnchor" | "map:auto" | "map" => {
  if (!stickToBottom && anchorIndex >= 0) return "mapWithAnchor";
  return stickToBottom ? "map:auto" : "map";
};

export function logSessionMessageListReconcileDebug({
  devEnabled,
  showDebug,
  sessionId,
  current,
  next,
  currentIds,
  nextIds,
  currentLen,
  nextLen,
  prefixLen,
  suffixLen,
  deleteCount,
  insertDataLength,
  anchorId,
  anchorIndex,
  historyExpected,
  stickToBottom,
  suppressIdDiffLogs,
}: Params) {
  if (!devEnabled || !showDebug) return;

  const nextIdSet = new Set(nextIds);
  const currentIdSet = new Set(currentIds);
  const missingFromNext: string[] = [];
  for (const id of currentIds) if (!nextIdSet.has(id)) missingFromNext.push(id);

  const addedInNext: string[] = [];
  for (const id of nextIds) if (!currentIdSet.has(id)) addedInNext.push(id);

  if (!suppressIdDiffLogs && missingFromNext.length > 0) {
    const currentById = new Map(current.map((item) => [item.id, item] as const));
    // eslint-disable-next-line no-console
    console.warn("[MessageList][ids:missing-from-next]", {
      sessionId,
      count: missingFromNext.length,
      sample: missingFromNext.slice(0, 12).map((id) => debugItemSummary(currentById.get(id) ?? { id })),
    });
  }

  if (!suppressIdDiffLogs && addedInNext.length > 0) {
    const nextByIdLocal = new Map(next.map((item) => [item.id, item] as const));
    // eslint-disable-next-line no-console
    console.debug("[MessageList][ids:added-in-next]", {
      sessionId,
      count: addedInNext.length,
      sample: addedInNext.slice(0, 8).map((id) => debugItemSummary(nextByIdLocal.get(id) ?? { id })),
    });
  }

  if (!suppressIdDiffLogs && missingFromNext.length === 0 && addedInNext.length === 0) {
    // eslint-disable-next-line no-console
    console.warn("[MessageList][ids:reorder-only]", { sessionId, currentLen, nextLen, prefixLen, suffixLen });
  }

  const payload = {
    sessionId,
    nextLen,
    currentLen,
    prefixLen,
    suffixLen,
    deleteCount,
    insertLen: insertDataLength,
    anchorId,
    anchorIndex,
    mapMode: resolveMapMode(stickToBottom, anchorIndex),
  };

  if (historyExpected) {
    // eslint-disable-next-line no-console
    console.warn("[MessageList][history:mixed-update]", payload);
    return;
  }

  // eslint-disable-next-line no-console
  console.debug("[MessageList][data:reconcile]", {
    ...payload,
    stickToBottom,
  });
}
