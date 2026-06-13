import type { MutableRefObject } from "react";
import type { WorkbenchListItem } from "./SessionPage.types";
import { debugItemSummary } from "./sessionMessageListDataDebug";
import { applyStableListUpdate } from "./sessionMessageListStableUpdate";
import {
  computeHistoryPrependTailReconcilePlan,
  isExactContiguousIdWindow,
} from "./sessionMessageListControllerUtils";
import type {
  MessageListMethods,
  SessionMessageListReconcileParams as Params,
} from "./sessionMessageListReconcileTypes";

function applyPrependDrivenHistoryUpdate({
  methods,
  current,
  retainedNext,
  prefix,
  suffix,
  stickToBottom,
  appendBehavior,
}: {
  methods: MessageListMethods;
  current: WorkbenchListItem[];
  retainedNext: WorkbenchListItem[];
  prefix: WorkbenchListItem[];
  suffix: WorkbenchListItem[];
  stickToBottom: boolean;
  appendBehavior: Params["appendBehavior"];
}) {
  applyStableListUpdate({
    methods,
    current,
    next: retainedNext,
    prefix,
    suffix,
    stickToBottom,
    anchorIndex: -1,
    appendBehavior,
    allowAnchorMap: false,
  });
}

type ExpectedHistoryArgs = Pick<
  Params,
  | "sessionId"
  | "showDebug"
  | "methodsRef"
  | "reconcileEpochRef"
  | "stickToBottomRef"
  | "renderedAnchorIdRef"
  | "historyExpectedRef"
  | "historyRequestedAtTopRef"
  | "historyRequestedAnchorIdRef"
  | "appendBehavior"
  | "recordDebugSnapshot"
  | "startFlashProbe"
> & {
  methods: MessageListMethods;
  reconcileEpoch: number;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentIds: string[];
  nextIds: string[];
  currentLen: number;
  effectiveNextLen: number;
};

export function tryApplyExpectedHistoryUpdate({
  sessionId,
  showDebug,
  methods,
  methodsRef,
  reconcileEpochRef,
  reconcileEpoch,
  current,
  next,
  currentIds,
  nextIds,
  currentLen,
  effectiveNextLen,
  stickToBottomRef,
  renderedAnchorIdRef,
  historyExpectedRef,
  historyRequestedAtTopRef,
  historyRequestedAnchorIdRef,
  appendBehavior,
  recordDebugSnapshot,
  startFlashProbe,
}: ExpectedHistoryArgs): boolean {
  if (!historyExpectedRef.current || currentLen <= 0 || effectiveNextLen < currentLen) return false;

  const wasAtTop = historyRequestedAtTopRef.current;
  const requestedAnchorId = historyRequestedAnchorIdRef.current;
  const firstId = current[0]?.id ?? null;
  const lastId = current[currentLen - 1]?.id ?? null;
  const firstIndex = firstId ? next.findIndex((it) => it.id === firstId) : -1;
  const lastIndex = lastId ? next.findIndex((it) => it.id === lastId) : -1;
  const exactContiguousWindow = isExactContiguousIdWindow(currentIds, nextIds, firstIndex);
  if (firstIndex >= 0 && lastIndex >= firstIndex && exactContiguousWindow) {
    const retainedNext = next.slice(firstIndex, lastIndex + 1);
    let retainedMatchesCurrent = retainedNext.length === currentLen;
    if (retainedMatchesCurrent) {
      for (let index = 0; index < currentLen; index += 1) {
        if (retainedNext[index]?.id !== current[index]?.id) {
          retainedMatchesCurrent = false;
          break;
        }
      }
    }
    if (!retainedMatchesCurrent) {
      historyExpectedRef.current = false;
    } else {
      const currentIdSet = new Set(current.map((it) => it.id));
      const prefix = next.slice(0, firstIndex).filter((it) => !currentIdSet.has(it.id));
      const suffix = next.slice(lastIndex + 1).filter((it) => !currentIdSet.has(it.id));
      warnForMissingHistoryIds({ sessionId, showDebug, current, next });
      startFlashProbe("history:extend", {
        currentLen,
        nextLen: effectiveNextLen,
        prefixLen: prefix.length,
        suffixLen: suffix.length,
        firstIndex,
        lastIndex,
        requestedAnchorId,
        wasAtTop,
      });

      applyPrependDrivenHistoryUpdate({
        methods,
        current,
        retainedNext,
        prefix,
        suffix,
        stickToBottom: stickToBottomRef.current,
        appendBehavior,
      });

      historyExpectedRef.current = false;
      historyRequestedAtTopRef.current = false;
      historyRequestedAnchorIdRef.current = null;
      recordDebugSnapshot("history:extend", {
        prefixLen: prefix.length,
        suffixLen: suffix.length,
        firstIndex,
        lastIndex,
        nextLen: effectiveNextLen,
        currentLen,
        requestedAnchorId,
        wasAtTop,
      });
      if (import.meta.env.DEV && showDebug) {
        // eslint-disable-next-line no-console
        console.debug("[MessageList][history:extend]", {
          sessionId,
          prefixLen: prefix.length,
          suffixLen: suffix.length,
          firstIndex,
          lastIndex,
          nextLen: effectiveNextLen,
          currentLen,
        });
      }
      return true;
    }
  }

  return tryApplyMixedHistoryPlan({
    sessionId,
    showDebug,
    methods,
    methodsRef,
    reconcileEpochRef,
    reconcileEpoch,
    current,
    next,
    currentIds,
    nextIds,
    currentLen,
    effectiveNextLen,
    firstIndex,
    lastIndex,
    requestedAnchorId,
    wasAtTop,
    stickToBottomRef,
    renderedAnchorIdRef,
    historyExpectedRef,
    historyRequestedAtTopRef,
    historyRequestedAnchorIdRef,
    appendBehavior,
    recordDebugSnapshot,
    startFlashProbe,
  });
}

type MixedHistoryPlanArgs = ExpectedHistoryArgs & {
  firstIndex: number;
  lastIndex: number;
  requestedAnchorId: string | null;
  wasAtTop: boolean;
};

function tryApplyMixedHistoryPlan({
  sessionId,
  showDebug,
  methods,
  methodsRef,
  reconcileEpochRef,
  reconcileEpoch,
  next,
  currentIds,
  nextIds,
  currentLen,
  effectiveNextLen,
  firstIndex,
  lastIndex,
  requestedAnchorId,
  wasAtTop,
  stickToBottomRef,
  renderedAnchorIdRef,
  historyExpectedRef,
  historyRequestedAtTopRef,
  historyRequestedAnchorIdRef,
  appendBehavior,
  recordDebugSnapshot,
  startFlashProbe,
}: MixedHistoryPlanArgs): boolean {
  const mixedHistoryPlan = computeHistoryPrependTailReconcilePlan({
    currentIds,
    nextIds,
    startIndex: firstIndex,
    anchorId: renderedAnchorIdRef.current,
  });
  if (!mixedHistoryPlan) {
    if (import.meta.env.DEV && showDebug && firstIndex >= 0 && lastIndex >= firstIndex) {
      // eslint-disable-next-line no-console
      console.debug("[MessageList][history:extend:skipped]", {
        sessionId,
        reason: "nonContiguousWindow",
        firstIndex,
        lastIndex,
        nextLen: effectiveNextLen,
        currentLen,
        requestedAnchorId,
        wasAtTop,
      });
    }
    return false;
  }

  const nextById = new Map(next.map((it) => [it.id, it] as const));
  const prefix = next.slice(0, mixedHistoryPlan.prefixLen);
  const insertData = next.slice(
    mixedHistoryPlan.insertStart,
    mixedHistoryPlan.insertStart + mixedHistoryPlan.insertCount,
  );

  startFlashProbe("history:prepend-tail-reconcile", {
    currentLen,
    nextLen: effectiveNextLen,
    prefixLen: mixedHistoryPlan.prefixLen,
    overlapLen: mixedHistoryPlan.overlapLen,
    deleteOffset: mixedHistoryPlan.deleteOffset,
    deleteCount: mixedHistoryPlan.deleteCount,
    insertLen: insertData.length,
    suffixLen: mixedHistoryPlan.suffixLen,
    requestedAnchorId,
    wasAtTop,
  });

  methods.data.prepend(prefix);
  requestAnimationFrame(() => {
    if (reconcileEpochRef.current !== reconcileEpoch) return;
    const liveMethods = methodsRef.current;
    if (!liveMethods) return;
    if (mixedHistoryPlan.deleteCount > 0 || insertData.length > 0) {
      liveMethods.data.batch(
        () => {
          if (mixedHistoryPlan.deleteCount > 0) {
            liveMethods.data.deleteRange(mixedHistoryPlan.deleteOffset, mixedHistoryPlan.deleteCount);
          }
          if (insertData.length > 0) {
            liveMethods.data.insert(insertData, mixedHistoryPlan.deleteOffset, appendBehavior);
          }
          liveMethods.data.map(
            (item) => nextById.get(item.id) ?? item,
            stickToBottomRef.current ? ("auto" as const) : undefined,
          );
        },
        appendBehavior,
      );
      return;
    }
    liveMethods.data.map(
      (item) => nextById.get(item.id) ?? item,
      stickToBottomRef.current ? ("auto" as const) : undefined,
    );
  });

  historyExpectedRef.current = false;
  historyRequestedAtTopRef.current = false;
  historyRequestedAnchorIdRef.current = null;
  recordDebugSnapshot("history:prepend-tail-reconcile", {
    prefixLen: mixedHistoryPlan.prefixLen,
    overlapLen: mixedHistoryPlan.overlapLen,
    deleteOffset: mixedHistoryPlan.deleteOffset,
    deleteCount: mixedHistoryPlan.deleteCount,
    insertLen: insertData.length,
    suffixLen: mixedHistoryPlan.suffixLen,
    nextLen: effectiveNextLen,
    currentLen,
    requestedAnchorId,
    wasAtTop,
  });
  if (import.meta.env.DEV && showDebug) {
    // eslint-disable-next-line no-console
    console.debug("[MessageList][history:prepend-tail-reconcile]", {
      sessionId,
      prefixLen: mixedHistoryPlan.prefixLen,
      overlapLen: mixedHistoryPlan.overlapLen,
      deleteOffset: mixedHistoryPlan.deleteOffset,
      deleteCount: mixedHistoryPlan.deleteCount,
      insertLen: insertData.length,
      suffixLen: mixedHistoryPlan.suffixLen,
      nextLen: effectiveNextLen,
      currentLen,
    });
  }
  return true;
}

type PurePrependArgs = Pick<
  Params,
  | "sessionId"
  | "showDebug"
  | "stickToBottomRef"
  | "renderedAnchorIdRef"
  | "historyExpectedRef"
  | "historyRequestedAtTopRef"
  | "historyRequestedAnchorIdRef"
  | "appendBehavior"
  | "recordDebugSnapshot"
  | "startFlashProbe"
> & {
  methods: MessageListMethods;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentLen: number;
  effectiveNextLen: number;
};

export function tryApplyPurePrependUpdate({
  sessionId,
  showDebug,
  methods,
  current,
  next,
  currentLen,
  effectiveNextLen,
  stickToBottomRef,
  renderedAnchorIdRef,
  historyExpectedRef,
  historyRequestedAtTopRef,
  historyRequestedAnchorIdRef,
  appendBehavior,
  recordDebugSnapshot,
  startFlashProbe,
}: PurePrependArgs): boolean {
  if (effectiveNextLen <= currentLen) return false;
  for (let i = 0; i < currentLen; i += 1) {
    if (next[effectiveNextLen - currentLen + i]?.id !== current[i]?.id) {
      return false;
    }
  }

  const wasAtTop = historyRequestedAtTopRef.current;
  const requestedAnchorId = historyRequestedAnchorIdRef.current;
  const prefix = next.slice(0, effectiveNextLen - currentLen);
  const anchorId = renderedAnchorIdRef.current;
  const anchorIndex = anchorId ? next.findIndex((it) => it.id === anchorId) : -1;

  startFlashProbe("data:prepend", {
    currentLen,
    nextLen: effectiveNextLen,
    prefixLen: prefix.length,
    anchorId,
    anchorIndex,
    requestedAnchorId,
    wasAtTop,
  });

  applyPrependDrivenHistoryUpdate({
    methods,
    current,
    retainedNext: next.slice(effectiveNextLen - currentLen),
    prefix,
    suffix: [],
    stickToBottom: stickToBottomRef.current,
    appendBehavior,
  });

  recordDebugSnapshot("data:prepend", {
    prefixLen: prefix.length,
    nextLen: effectiveNextLen,
    currentLen,
    anchorId,
    anchorIndex,
    requestedAnchorId,
    wasAtTop,
  });
  if (import.meta.env.DEV && showDebug) {
    // eslint-disable-next-line no-console
    console.debug("[MessageList][data:prepend]", {
      sessionId,
      prefixLen: prefix.length,
      nextLen: effectiveNextLen,
      currentLen,
      anchorId,
      anchorIndex,
    });
  }
  if (historyExpectedRef.current) {
    historyExpectedRef.current = false;
    historyRequestedAtTopRef.current = false;
    historyRequestedAnchorIdRef.current = null;
    if (import.meta.env.DEV && showDebug) {
      // eslint-disable-next-line no-console
      console.debug("[MessageList][history:applied]", { sessionId, prefixLen: prefix.length });
    }
  }
  return true;
}

function warnForMissingHistoryIds({
  sessionId,
  showDebug,
  current,
  next,
}: {
  sessionId: string;
  showDebug: boolean;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
}) {
  if (!import.meta.env.DEV || !showDebug) return;
  const nextIdSet = new Set(next.map((it) => it.id));
  const missingFromNext: string[] = [];
  for (const it of current) if (!nextIdSet.has(it.id)) missingFromNext.push(it.id);
  if (missingFromNext.length === 0) return;
  const currentById = new Map(current.map((it) => [it.id, it] as const));
  // eslint-disable-next-line no-console
  console.warn("[MessageList][history:extend][ids:missing-from-next]", {
    sessionId,
    count: missingFromNext.length,
    sample: missingFromNext.slice(0, 12).map((id) => debugItemSummary(currentById.get(id) ?? { id })),
  });
}
