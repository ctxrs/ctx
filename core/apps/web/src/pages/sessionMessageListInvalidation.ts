import type { MutableRefObject } from "react";
import type {
  ItemLocation,
  VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";

type Params = {
  sessionId: string;
  layoutRevision: string;
  current: WorkbenchListItem[];
  nextRaw: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentLen: number;
  effectiveNextLen: number;
  methods: VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>;
  initialLocation: ItemLocation;
  lastLayoutRevisionRef: MutableRefObject<string>;
  historyExpectedRef: MutableRefObject<boolean>;
  historyRequestedAtTopRef: MutableRefObject<boolean>;
  historyRequestedAnchorIdRef: MutableRefObject<string | null>;
  stickToBottomRef: MutableRefObject<boolean>;
  renderedTopIdRef: MutableRefObject<string | null>;
  renderedAnchorIdRef: MutableRefObject<string | null>;
  suppressIdDiffLogsRef: MutableRefObject<{ sessionId: string; remainingTicks: number } | null>;
  snapToBottom: (methods: VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>) => void;
  startFlashProbe: (cause: string, detail?: Record<string, unknown> | null) => void;
  recordDebugSnapshot: (cause: string, detail?: Record<string, unknown> | null) => void;
  logMessageListDebug: (label: string, detail: Record<string, unknown>) => void;
  showDebug: boolean;
};

const buildReplaceLocation = ({
  atBottom,
  next,
  renderedTopIdRef,
  renderedAnchorIdRef,
  initialLocation,
}: {
  atBottom: boolean;
  next: WorkbenchListItem[];
  renderedTopIdRef: MutableRefObject<string | null>;
  renderedAnchorIdRef: MutableRefObject<string | null>;
  initialLocation: ItemLocation;
}) => {
  const purgeAnchorId = atBottom ? null : renderedTopIdRef.current ?? renderedAnchorIdRef.current;
  const purgeAnchorIndex = purgeAnchorId ? next.findIndex((item) => item.id === purgeAnchorId) : -1;
  const replaceLocation: ItemLocation =
    atBottom
      ? { index: "LAST", align: "end" }
      : purgeAnchorIndex >= 0
        ? { index: purgeAnchorIndex, align: "start" }
        : initialLocation;
  return { purgeAnchorId, purgeAnchorIndex, replaceLocation };
};

export function applySessionMessageListInvalidation(params: Params): boolean {
  const {
    sessionId,
    layoutRevision,
    current,
    nextRaw,
    next,
    currentLen,
    effectiveNextLen,
    methods,
    initialLocation,
    lastLayoutRevisionRef,
    historyExpectedRef,
    historyRequestedAtTopRef,
    historyRequestedAnchorIdRef,
    stickToBottomRef,
    renderedTopIdRef,
    renderedAnchorIdRef,
    suppressIdDiffLogsRef,
    snapToBottom,
    startFlashProbe,
    recordDebugSnapshot,
    logMessageListDebug,
    showDebug,
  } = params;

  const applyReplaceWithPurgedSizes = (detail: Record<string, unknown>) => {
    const atBottom = stickToBottomRef.current;
    const { purgeAnchorId, purgeAnchorIndex, replaceLocation } = buildReplaceLocation({
      atBottom,
      next,
      renderedTopIdRef,
      renderedAnchorIdRef,
      initialLocation,
    });
    methods.cancelSmoothScroll();
    suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 1 };
    startFlashProbe("data:replace", {
      ...detail,
      nextLen: effectiveNextLen,
      currentLen,
      atBottom,
      purgeAnchorId,
      purgeAnchorIndex,
    });
    methods.data.replace(next, { initialLocation: replaceLocation, purgeItemSizes: true });
    if (atBottom) {
      snapToBottom(methods);
    }
    recordDebugSnapshot("data:replace", {
      ...detail,
      nextLen: effectiveNextLen,
      currentLen,
      atBottom,
      purgeAnchorId,
      purgeAnchorIndex,
    });
    logMessageListDebug("data:replace", {
      ...detail,
      nextLen: effectiveNextLen,
      currentLen,
      atBottom,
      purgeAnchorId,
      purgeAnchorIndex,
    });
  };

  if (currentLen === 0) {
    if (nextRaw.length === 0) return true;
    lastLayoutRevisionRef.current = layoutRevision;
    historyExpectedRef.current = false;
    methods.cancelSmoothScroll();
    suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 2 };
    methods.data.replace(nextRaw, { initialLocation, purgeItemSizes: true });
    snapToBottom(methods);
    recordDebugSnapshot("data:replace", {
      reason: "initialPopulation",
      nextLen: nextRaw.length,
      currentLen,
    });
    logMessageListDebug("data:replace", {
      reason: "initialPopulation",
      nextLen: nextRaw.length,
      currentLen,
    });
    return true;
  }

  if (effectiveNextLen === 0) {
    lastLayoutRevisionRef.current = layoutRevision;
    historyExpectedRef.current = false;
    methods.data.deleteRange(0, currentLen);
    recordDebugSnapshot("data:deleteRange", {
      offset: 0,
      count: currentLen,
    });
    if (import.meta.env.DEV && showDebug) {
      // eslint-disable-next-line no-console
      console.debug("[MessageList][data:deleteRange]", { sessionId, offset: 0, count: currentLen });
    }
    return true;
  }

  if (lastLayoutRevisionRef.current !== layoutRevision) {
    lastLayoutRevisionRef.current = layoutRevision;
    historyExpectedRef.current = false;
    historyRequestedAtTopRef.current = false;
    historyRequestedAnchorIdRef.current = null;
    applyReplaceWithPurgedSizes({ reason: "layoutRevisionChanged", layoutRevision });
    return true;
  }

  return false;
}
