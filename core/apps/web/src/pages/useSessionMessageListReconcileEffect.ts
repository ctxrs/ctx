import { useLayoutEffect } from "react";
import type { ItemLocation } from "@virtuoso.dev/message-list";
import { runSessionMessageListDevValidation } from "./sessionMessageListDevValidation";
import { logSessionMessageListReconcileDebug } from "./sessionMessageListReconcileDebug";
import { applyStructuralStableListUpdate } from "./sessionMessageListStableUpdate";
import {
  assertWholeListPurgeAllowed,
  findSharedItemSizeCacheKeyChanges,
  haveSameItemIdSequence,
  shouldReplaceBottomLockedStructuralUpdate,
  trimTrailingAppendsWhileScrolledUp,
} from "./sessionMessageListControllerUtils";
import type { SessionMessageListReconcileParams as Params } from "./sessionMessageListReconcileTypes";
import {
  tryApplyExpectedHistoryUpdate,
  tryApplyPurePrependUpdate,
} from "./sessionMessageListReconcileHistory";
import {
  tryApplyPureAppendUpdate,
  tryApplySameLengthUpdate,
} from "./sessionMessageListReconcileStable";

export function useSessionMessageListReconcileEffect({
  sessionId,
  isActive,
  listItems,
  visibleListItems,
  loadingOlder,
  deferTrailingAppends,
  layoutRevision,
  itemSizeCacheKey,
  threadOp,
  showDebug,
  initialLocation,
  appendBehavior,
  methodsRef,
  lastSessionIdRef,
  contractViolationLoggedRef,
  lastScrollLocationRef,
  lastAtBottomRef,
  lastListOffsetRef,
  stickToBottomRef,
  renderedAnchorIdRef,
  renderedTopIdRef,
  firstListItemIdRef,
  pendingHistoryRef,
  historyExpectedRef,
  historyRequestedAtTopRef,
  historyRequestedAnchorIdRef,
  lastLayoutRevisionRef,
  reconcileEpochRef,
  suppressIdDiffLogsRef,
  setLoadingOlder,
  setDeferTrailingAppends,
  snapToBottom,
  recordDebugSnapshot,
  startFlashProbe,
  logMessageListDebug,
}: Params) {
  useLayoutEffect(() => {
    if (!isActive) return;
    const methods = methodsRef.current;
    if (!methods) return;
    const reconcileEpoch = ++reconcileEpochRef.current;

    const nextRaw = listItems;
    let next = visibleListItems;
    const current = methods.data.get();
    const sessionChanged = lastSessionIdRef.current !== sessionId;

    if (!sessionChanged) {
      const suppress = suppressIdDiffLogsRef.current;
      if (suppress && suppress.sessionId === sessionId && suppress.remainingTicks > 0) {
        suppress.remainingTicks -= 1;
        if (suppress.remainingTicks <= 0) suppressIdDiffLogsRef.current = null;
      }
    }

    runSessionMessageListDevValidation({
      sessionId,
      showDebug,
      nextRaw,
      current,
      next,
      contractViolationLoggedRef,
    });

    if (sessionChanged) {
      lastSessionIdRef.current = sessionId;
      lastLayoutRevisionRef.current = layoutRevision;
      pendingHistoryRef.current = false;
      historyExpectedRef.current = false;
      historyRequestedAtTopRef.current = false;
      historyRequestedAnchorIdRef.current = null;
      setLoadingOlder(false);
      if (deferTrailingAppends) setDeferTrailingAppends(false);
      lastScrollLocationRef.current = null;
      lastListOffsetRef.current = null;
      stickToBottomRef.current = true;
      lastAtBottomRef.current = true;
      renderedAnchorIdRef.current = null;
      renderedTopIdRef.current = null;
      firstListItemIdRef.current = null;
      methods.cancelSmoothScroll();
      suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 3 };
      methods.data.replace(next);
      snapToBottom(methods);
      recordDebugSnapshot("data:replace", {
        reason: "sessionChanged",
        nextLen: next.length,
        currentLen: current.length,
      });
      logMessageListDebug("data:replace", {
        reason: "sessionChanged",
        nextLen: next.length,
        currentLen: current.length,
      });
      return;
    }

    const nextLen = next.length;
    const currentLen = current.length;
    const currentIds = current.map((it) => it.id);
    const shouldDeferTrailingAppends =
      currentLen > 0 &&
      !stickToBottomRef.current &&
      (deferTrailingAppends ||
        historyExpectedRef.current ||
        pendingHistoryRef.current ||
        loadingOlder);
    if (shouldDeferTrailingAppends) {
      const trimmedNext = trimTrailingAppendsWhileScrolledUp(currentIds, next);
      if (trimmedNext.length !== next.length) {
        next = trimmedNext;
        if (!deferTrailingAppends) setDeferTrailingAppends(true);
        recordDebugSnapshot("data:deferTrailingAppends", {
          nextLen,
          trimmedLen: trimmedNext.length,
        });
        logMessageListDebug("data:deferTrailingAppends", {
          nextLen,
          trimmedLen: trimmedNext.length,
          currentLen,
        });
      }
    }
    const nextIds = next.map((it) => it.id);
    const effectiveNextLen = next.length;
    const layoutRevisionChanged = lastLayoutRevisionRef.current !== layoutRevision;
    const sameIdSequence = haveSameItemIdSequence(current, next);
    const hasLocalizedThreadOp =
      Boolean(threadOp) &&
      threadOp?.kind !== "noop" &&
      threadOp?.kind !== "replace_session" &&
      sameIdSequence;

    if (currentLen === 0) {
      if (next.length === 0) return;
      lastLayoutRevisionRef.current = layoutRevision;
      historyExpectedRef.current = false;
      methods.cancelSmoothScroll();
      suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 2 };
      methods.data.replace(next, { initialLocation, purgeItemSizes: false });
      snapToBottom(methods);
      recordDebugSnapshot("data:replace", {
        reason: "initialPopulation",
        nextLen: next.length,
        currentLen,
      });
      logMessageListDebug("data:replace", {
        reason: "initialPopulation",
        nextLen: next.length,
        currentLen,
      });
      return;
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
      return;
    }

    if (layoutRevisionChanged) {
      lastLayoutRevisionRef.current = layoutRevision;
      historyExpectedRef.current = false;
      historyRequestedAtTopRef.current = false;
      historyRequestedAnchorIdRef.current = null;
      if (threadOp && threadOp.kind !== "replace_session") {
        recordDebugSnapshot("data:layout-op", {
          reason: threadOp?.kind ?? "unknown",
          nextLen: effectiveNextLen,
          currentLen,
          remeasureCount: threadOp?.remeasureItemIds.length ?? 0,
        });
      } else {
        const atBottom = stickToBottomRef.current;
        const purgeAnchorId = atBottom ? null : renderedTopIdRef.current ?? renderedAnchorIdRef.current;
        const purgeAnchorIndex = purgeAnchorId ? next.findIndex((item) => item.id === purgeAnchorId) : -1;
        const replaceLocation: ItemLocation =
          atBottom
            ? initialLocation
            : purgeAnchorIndex >= 0
              ? { index: purgeAnchorIndex, align: "start" }
              : initialLocation;
        assertWholeListPurgeAllowed({ reason: "layoutRevisionChanged", threadOp });
        methods.cancelSmoothScroll();
        suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 1 };
        startFlashProbe("data:replace", {
          reason: "layoutRevisionChanged",
          layoutRevision,
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
          reason: "layoutRevisionChanged",
          layoutRevision,
          nextLen: effectiveNextLen,
          currentLen,
          atBottom,
          purgeAnchorId,
          purgeAnchorIndex,
        });
        logMessageListDebug("data:replace", {
          reason: "layoutRevisionChanged",
          layoutRevision,
          nextLen: effectiveNextLen,
          currentLen,
          atBottom,
          purgeAnchorId,
          purgeAnchorIndex,
        });
        return;
      }
    }

    const sizeCacheKeyChanges = sameIdSequence
      ? findSharedItemSizeCacheKeyChanges(current, next, itemSizeCacheKey)
      : { count: 0, sampleIds: [] as string[] };
    if (sizeCacheKeyChanges.count > 0 && !threadOp) {
      const atBottom = stickToBottomRef.current;
      const purgeAnchorId = atBottom ? null : renderedTopIdRef.current ?? renderedAnchorIdRef.current;
      const purgeAnchorIndex = purgeAnchorId ? next.findIndex((item) => item.id === purgeAnchorId) : -1;
      const replaceLocation: ItemLocation =
        atBottom
          ? initialLocation
          : purgeAnchorIndex >= 0
            ? { index: purgeAnchorIndex, align: "start" }
            : initialLocation;
      assertWholeListPurgeAllowed({ reason: "sizeCacheKeyChanged", threadOp });
      methods.cancelSmoothScroll();
      suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 1 };
      startFlashProbe("data:replace", {
        reason: "sizeCacheKeyChanged",
        changedCount: sizeCacheKeyChanges.count,
        changedSampleIds: sizeCacheKeyChanges.sampleIds,
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
        reason: "sizeCacheKeyChanged",
        changedCount: sizeCacheKeyChanges.count,
        changedSampleIds: sizeCacheKeyChanges.sampleIds,
        nextLen: effectiveNextLen,
        currentLen,
        atBottom,
        purgeAnchorId,
        purgeAnchorIndex,
      });
      logMessageListDebug("data:replace", {
        reason: "sizeCacheKeyChanged",
        changedCount: sizeCacheKeyChanges.count,
        changedSampleIds: sizeCacheKeyChanges.sampleIds,
        nextLen: effectiveNextLen,
        currentLen,
        atBottom,
        purgeAnchorId,
        purgeAnchorIndex,
      });
      return;
    }

    if (
      tryApplyExpectedHistoryUpdate({
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
      })
    ) {
      return;
    }

    if (
      tryApplyPurePrependUpdate({
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
      })
    ) {
      return;
    }

    if (
      tryApplyPureAppendUpdate({
        sessionId,
        methods,
        current,
        next,
        currentLen,
        effectiveNextLen,
        stickToBottomRef,
        renderedAnchorIdRef,
        appendBehavior,
        snapToBottom,
        recordDebugSnapshot,
        logMessageListDebug,
      })
    ) {
      return;
    }

    if (
      tryApplySameLengthUpdate({
        sessionId,
        showDebug,
        methods,
        current,
        next,
        currentLen,
        effectiveNextLen,
        stickToBottomRef,
        renderedAnchorIdRef,
        renderedTopIdRef,
        appendBehavior,
        snapToBottom,
        recordDebugSnapshot,
        startFlashProbe,
        logMessageListDebug,
        hasLocalizedThreadOp,
        threadOp,
      })
    ) {
      return;
    }

    let prefixLen = 0;
    while (
      prefixLen < currentLen &&
      prefixLen < effectiveNextLen &&
      currentIds[prefixLen] === nextIds[prefixLen]
    ) {
      prefixLen += 1;
    }
    let suffixLen = 0;
    while (
      suffixLen < currentLen - prefixLen &&
      suffixLen < effectiveNextLen - prefixLen &&
      currentIds[currentLen - 1 - suffixLen] === nextIds[effectiveNextLen - 1 - suffixLen]
    ) {
      suffixLen += 1;
    }

    const deleteCount = currentLen - prefixLen - suffixLen;
    const insertData = next.slice(prefixLen, effectiveNextLen - suffixLen);
    const anchorId = renderedAnchorIdRef.current;
    const anchorIndex = anchorId ? next.findIndex((it) => it.id === anchorId) : -1;

    const suppress = suppressIdDiffLogsRef.current;
    const suppressIdDiffLogs = Boolean(
      suppress && suppress.sessionId === sessionId && suppress.remainingTicks > 0,
    );
    const historyExpected = historyExpectedRef.current;
    if (historyExpected) {
      historyExpectedRef.current = false;
    }
    logSessionMessageListReconcileDebug({
      devEnabled: import.meta.env.DEV,
      showDebug,
      sessionId,
      current,
      next,
      currentIds,
      nextIds,
      currentLen,
      nextLen: effectiveNextLen,
      prefixLen,
      suffixLen,
      deleteCount,
      insertDataLength: insertData.length,
      anchorId,
      anchorIndex,
      historyExpected,
      stickToBottom: stickToBottomRef.current,
      suppressIdDiffLogs,
    });

    const replaceBottomLockedStructuralUpdate = shouldReplaceBottomLockedStructuralUpdate({
      stickToBottom: stickToBottomRef.current,
      currentLen,
      nextLen: effectiveNextLen,
      prefixLen,
      suffixLen,
      deleteCount,
      insertCount: insertData.length,
    });
    if (replaceBottomLockedStructuralUpdate) {
      assertWholeListPurgeAllowed({ reason: "bottomLockedStructuralReconcile", threadOp });
      methods.cancelSmoothScroll();
      suppressIdDiffLogsRef.current = { sessionId, remainingTicks: 1 };
      startFlashProbe("data:replace", {
        reason: "bottomLockedStructuralReconcile",
        currentLen,
        nextLen: effectiveNextLen,
        prefixLen,
        suffixLen,
        deleteCount,
        insertLen: insertData.length,
        anchorId,
        anchorIndex,
        historyExpected,
        stickToBottom: stickToBottomRef.current,
      });
      methods.data.replace(next, { initialLocation, purgeItemSizes: true });
      snapToBottom(methods);
      recordDebugSnapshot("data:replace", {
        reason: "bottomLockedStructuralReconcile",
        nextLen: effectiveNextLen,
        currentLen,
        prefixLen,
        suffixLen,
        deleteCount,
        insertLen: insertData.length,
        anchorId,
        anchorIndex,
        historyExpected,
        stickToBottom: stickToBottomRef.current,
      });
      logMessageListDebug("data:replace", {
        reason: "bottomLockedStructuralReconcile",
        nextLen: effectiveNextLen,
        currentLen,
        prefixLen,
        suffixLen,
        deleteCount,
        insertLen: insertData.length,
        anchorId,
        anchorIndex,
        historyExpected,
        stickToBottom: stickToBottomRef.current,
      });
      return;
    }

    startFlashProbe("data:reconcile", {
      currentLen,
      nextLen: effectiveNextLen,
      prefixLen,
      suffixLen,
      deleteCount,
      insertLen: insertData.length,
      anchorId,
      anchorIndex,
      historyExpected,
      stickToBottom: stickToBottomRef.current,
    });

    const updateResult = applyStructuralStableListUpdate({
      methods,
      current,
      next,
      prefixLen,
      suffixLen,
      stickToBottom: stickToBottomRef.current,
      anchorIndex,
      appendBehavior,
      forceRemeasureItemIds: hasLocalizedThreadOp ? (threadOp?.remeasureItemIds ?? []) : [],
    });
    if (stickToBottomRef.current) {
      snapToBottom(methods);
    }
    recordDebugSnapshot("data:reconcile", {
      nextLen: effectiveNextLen,
      currentLen,
      prefixLen,
      suffixLen,
      deleteCount,
      insertLen: insertData.length,
      anchorId,
      anchorIndex,
      stickToBottom: stickToBottomRef.current,
      changedSpans: updateResult.changedSpans,
    });
  }, [
    appendBehavior,
    contractViolationLoggedRef,
    deferTrailingAppends,
    firstListItemIdRef,
    historyExpectedRef,
    historyRequestedAnchorIdRef,
    historyRequestedAtTopRef,
    initialLocation,
    isActive,
    itemSizeCacheKey,
    lastAtBottomRef,
    lastLayoutRevisionRef,
    lastListOffsetRef,
    lastScrollLocationRef,
    layoutRevision,
    listItems,
    loadingOlder,
    logMessageListDebug,
    methodsRef,
    pendingHistoryRef,
    reconcileEpochRef,
    recordDebugSnapshot,
    renderedAnchorIdRef,
    renderedTopIdRef,
    sessionId,
    setDeferTrailingAppends,
    setLoadingOlder,
    showDebug,
    snapToBottom,
    startFlashProbe,
    stickToBottomRef,
    suppressIdDiffLogsRef,
    threadOp,
    visibleListItems,
  ]);
}
