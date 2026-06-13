import {
  useCallback,
  useEffect,
  useLayoutEffect,
  type KeyboardEvent,
  type MouseEvent,
  type MutableRefObject,
} from "react";
import type { PretextVirtualizerLogicalAnchor, PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import type { PretextVirtualizerItemLocation } from "@pretext-virtualizer/interface";
import type { WorkbenchListItem } from "../SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import type { SessionPretextRuntimeRecord } from "./pretextSessionRuntimeCache";
import { readSessionPretextRuntimePreparedState, SESSION_PRETEXT_BOTTOM_THRESHOLD_PX } from "./pretextSessionRuntimeCache";
import { shouldRestoreBottomOnViewportResize } from "./pretextFollowBottom";
import {
  approximateIndexForLocation,
  haveSameItemIds,
  haveSameLayoutInputs,
  resolveInteractionItemId,
  resolveScrollTopForLocation,
} from "./pretextVirtualizerProjectionHelpers";
import { buildVisibleProjectionUpdatePlan } from "./pretextVirtualizerProjectionController";
import { PRETEXT_VIRTUALIZER_INITIAL_BOTTOM_LOCATION } from "../../state/pretextVirtualizerViewportState";
import {
  addPretextPerfBucket,
  hashPretextPerfValue,
  incrementPretextPerfCounter,
} from "../../utils/pretextPerfDiagnostics";

const BOTTOM_THRESHOLD_PX = SESSION_PRETEXT_BOTTOM_THRESHOLD_PX;

export type SessionThreadPretextInteractionCaptureHandlers = {
  handleClickCapture: (event: MouseEvent<HTMLDivElement>) => void;
  handleKeyDownCapture: (event: KeyboardEvent<HTMLDivElement>) => void;
};

function resolveActiveChangedItemId(params: {
  changedItemIds: readonly string[];
  lastInteractedItemId: string | null;
}): string | null {
  const { changedItemIds, lastInteractedItemId } = params;
  if (lastInteractedItemId && changedItemIds.includes(lastInteractedItemId)) {
    return lastInteractedItemId;
  }
  if (typeof document === "undefined") {
    return null;
  }
  const activeElement = document.activeElement;
  if (!(activeElement instanceof HTMLElement)) {
    return null;
  }
  const owner = activeElement.closest<HTMLElement>("[data-thread-item-id]");
  const ownerId = owner?.dataset.threadItemId ?? null;
  if (!ownerId) {
    return null;
  }
  return changedItemIds.includes(ownerId) ? ownerId : null;
}

export function useSessionThreadPretextLifecycleController(params: {
  core: SessionPretextRuntimeRecord["core"];
  runtime: SessionPretextRuntimeRecord;
  listItems: readonly WorkbenchListItem[];
  initialLocation: PretextVirtualizerItemLocation | null | undefined;
  runtimeUiStateLayoutRevision: string;
  runtimeUiStateLayoutKeyRef: MutableRefObject<string | null>;
  containerRef: MutableRefObject<HTMLDivElement | null>;
  listItemsRef: MutableRefObject<readonly WorkbenchListItem[]>;
  followBottomRef: MutableRefObject<boolean>;
  lastSyncedItemCountRef: MutableRefObject<number>;
  lastAtBottomRef: MutableRefObject<boolean | null>;
  snapshotRef: MutableRefObject<PretextVirtualizerSnapshot<WorkbenchListItem> | null>;
  lastAppliedUiStateLayoutRevisionRef: MutableRefObject<string | null>;
  lastAppliedProjectionOpRef: MutableRefObject<string | null>;
  lastInteractedItemIdRef: MutableRefObject<string | null>;
  pendingProgrammaticTopRef: MutableRefObject<number | null>;
  pendingProgrammaticBehaviorRef: MutableRefObject<ScrollBehavior>;
  pendingRestoreRef: MutableRefObject<boolean>;
  threadProjectionOp: WorkbenchThreadProjectionOp;
  applySnapshotToDom: (
    nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
    options?: {
      behavior?: ScrollBehavior;
      followBottom?: boolean;
      nextItems?: readonly WorkbenchListItem[];
    },
  ) => void;
  commitRuntimeSnapshot: (
    nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
    nextItems: readonly WorkbenchListItem[],
  ) => void;
  scheduleScrollbarUpdate: () => void;
  scrollToOffset: (scrollTop: number, behavior?: ScrollBehavior) => void;
  syncFromDom: (scrollTopOverride?: number) => PretextVirtualizerSnapshot<WorkbenchListItem>;
}): SessionThreadPretextInteractionCaptureHandlers {
  const {
    applySnapshotToDom,
    commitRuntimeSnapshot,
    containerRef,
    core,
    followBottomRef,
    initialLocation,
    lastAppliedProjectionOpRef,
    lastAppliedUiStateLayoutRevisionRef,
    lastAtBottomRef,
    lastInteractedItemIdRef,
    lastSyncedItemCountRef,
    listItems,
    listItemsRef,
    pendingProgrammaticBehaviorRef,
    pendingProgrammaticTopRef,
    pendingRestoreRef,
    runtime,
    runtimeUiStateLayoutKeyRef,
    runtimeUiStateLayoutRevision,
    scheduleScrollbarUpdate,
    scrollToOffset,
    snapshotRef,
    syncFromDom,
    threadProjectionOp,
  } = params;

  const handleClickCapture = useCallback((event: MouseEvent<HTMLDivElement>) => {
    lastInteractedItemIdRef.current = resolveInteractionItemId(event.target);
  }, [lastInteractedItemIdRef]);

  const handleKeyDownCapture = useCallback((event: KeyboardEvent<HTMLDivElement>) => {
    lastInteractedItemIdRef.current = resolveInteractionItemId(event.target);
  }, [lastInteractedItemIdRef]);

  useLayoutEffect(() => {
    followBottomRef.current =
      initialLocation?.index === "LAST" && (initialLocation.align ?? "start") === "end";
    pendingProgrammaticTopRef.current = null;
    pendingProgrammaticBehaviorRef.current = "auto";
    pendingRestoreRef.current = false;
    lastAtBottomRef.current = null;
    lastSyncedItemCountRef.current = listItemsRef.current.length;
    lastAppliedUiStateLayoutRevisionRef.current = runtimeUiStateLayoutRevision;
    lastAppliedProjectionOpRef.current = null;
    const currentItems = listItemsRef.current;
    const scroller = containerRef.current;
    if (!scroller) {
      syncFromDom();
      return;
    }
    const preparedState = readSessionPretextRuntimePreparedState(runtime);
    const preparedLayoutMismatch =
      preparedState.layoutKey == null ||
      preparedState.layoutKey !== runtimeUiStateLayoutKeyRef.current;
    const preparedWidthMismatch =
      scroller.clientWidth > 0 && preparedState.snapshot.viewportWidth !== scroller.clientWidth;
    let baseSnapshot = core.syncViewport({
      height: scroller.clientHeight,
      width: scroller.clientWidth,
      scrollTop: scroller.scrollTop,
    });
    lastSyncedItemCountRef.current = preparedState.listItems.length;
    if (
      preparedLayoutMismatch ||
      preparedWidthMismatch ||
      !haveSameLayoutInputs(preparedState.listItems, currentItems, runtime.callbacks.getLayoutRevision)
    ) {
      incrementPretextPerfCounter("pretext_full_relayout_calls");
      incrementPretextPerfCounter("pretext_full_relayout_item_count", currentItems.length);
      addPretextPerfBucket(
        "pretext_full_relayout_reason",
        preparedLayoutMismatch
          ? "visible:mount-layout-key-mismatch"
          : preparedWidthMismatch
            ? "visible:mount-width-mismatch"
            : "visible:mount-sync-items",
      );
      const initialAnchor = followBottomRef.current
        ? ({ kind: "bottom" } satisfies PretextVirtualizerLogicalAnchor)
        : null;
      baseSnapshot = haveSameItemIds(preparedState.listItems, currentItems)
        ? core.patchItems(
            currentItems,
            currentItems.map((item) => item.id),
            currentItems.map((item) => item.id),
            initialAnchor,
          )
        : core.syncItems(currentItems, initialAnchor);
      lastSyncedItemCountRef.current = currentItems.length;
    }
    commitRuntimeSnapshot(baseSnapshot, currentItems);
    if (followBottomRef.current) {
      applySnapshotToDom(core.restoreAnchor({ kind: "bottom" }), {
        behavior: "auto",
        followBottom: true,
        nextItems: currentItems,
      });
    } else {
      const targetIndex = approximateIndexForLocation(
        listItemsRef.current,
        initialLocation ?? PRETEXT_VIRTUALIZER_INITIAL_BOTTOM_LOCATION,
      );
      const targetTop = resolveScrollTopForLocation(
        baseSnapshot,
        {
          ...(initialLocation ?? PRETEXT_VIRTUALIZER_INITIAL_BOTTOM_LOCATION),
          index: targetIndex,
        },
        core,
        listItemsRef.current.length,
      );
      scrollToOffset(targetTop, initialLocation?.behavior ?? "auto");
    }
  }, [
    applySnapshotToDom,
    commitRuntimeSnapshot,
    core,
    initialLocation,
    runtime,
    scrollToOffset,
    syncFromDom,
  ]);

  useLayoutEffect(() => {
    const scroller = containerRef.current;
    if (!scroller) {
      return;
    }
    const preparedState = readSessionPretextRuntimePreparedState(runtime);
    const activeChangedItemId = resolveActiveChangedItemId({
      changedItemIds: threadProjectionOp.changedItemIds,
      lastInteractedItemId: lastInteractedItemIdRef.current,
    });
    const updatePlan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems,
      threadProjectionOp,
      runtimeUiStateLayoutRevision,
      lastAppliedUiStateLayoutRevision: lastAppliedUiStateLayoutRevisionRef.current,
      lastAppliedProjectionOpKey: lastAppliedProjectionOpRef.current,
      viewport: {
        width: scroller.clientWidth,
        height: scroller.clientHeight,
        scrollTop: scroller.scrollTop,
      },
      followBottom: followBottomRef.current,
      atBottom: lastAtBottomRef.current === true,
      activeChangedItemId,
      getLayoutRevision: runtime.callbacks.getLayoutRevision,
      bottomThresholdPx: BOTTOM_THRESHOLD_PX,
    });
    if (updatePlan.kind === "noop") {
      return;
    }
    if (updatePlan.changeKind === "full-relayout") {
      incrementPretextPerfCounter("pretext_full_relayout_calls");
      incrementPretextPerfCounter("pretext_full_relayout_item_count", listItems.length);
      addPretextPerfBucket("pretext_full_relayout_reason", updatePlan.reason);
    } else {
      incrementPretextPerfCounter("pretext_localized_patch_calls");
      incrementPretextPerfCounter(
        "pretext_localized_patch_item_count",
        threadProjectionOp.remeasureItemIds.length,
      );
      addPretextPerfBucket("pretext_localized_patch_kind", threadProjectionOp.kind);
    }
    if (updatePlan.uiStateChanged) {
      addPretextPerfBucket(
        "pretext_ui_state_revision",
        hashPretextPerfValue(runtimeUiStateLayoutRevision),
      );
    }
    pendingRestoreRef.current = true;
    incrementPretextPerfCounter("pretext_visible_sync_items_calls");
    addPretextPerfBucket("pretext_visible_sync_items_kind", threadProjectionOp.kind);
    followBottomRef.current = updatePlan.shouldFollowBottom;
    lastSyncedItemCountRef.current = listItems.length;
    applySnapshotToDom(updatePlan.nextSnapshot, {
      behavior: "auto",
      followBottom: updatePlan.shouldFollowBottom,
      nextItems: listItems,
    });
    lastInteractedItemIdRef.current = null;
    pendingRestoreRef.current = false;
    lastAppliedUiStateLayoutRevisionRef.current = runtimeUiStateLayoutRevision;
    lastAppliedProjectionOpRef.current = updatePlan.projectionOpKey;
  }, [
    applySnapshotToDom,
    core,
    listItems,
    runtime,
    runtimeUiStateLayoutRevision,
    threadProjectionOp,
  ]);

  useEffect(() => {
    const scroller = containerRef.current;
    if (!scroller) {
      return;
    }
    let lastWidth = scroller.clientWidth;
    let lastHeight = scroller.clientHeight;
    let resizeFrameId: number | null = null;
    const processResize = () => {
      resizeFrameId = null;
      const nextWidth = scroller.clientWidth;
      const nextHeight = scroller.clientHeight;
      const sizeChanged = nextWidth !== lastWidth || nextHeight !== lastHeight;
      if (!sizeChanged) {
        return;
      }
      lastWidth = nextWidth;
      lastHeight = nextHeight;
      scheduleScrollbarUpdate();
      const previousSnapshot = snapshotRef.current;
      if (!previousSnapshot) {
        return;
      }
      const shouldRestoreBottom = shouldRestoreBottomOnViewportResize(sizeChanged, {
        followBottom: followBottomRef.current,
        atBottom: lastAtBottomRef.current === true,
      });
      if (sizeChanged) {
        if (nextWidth !== previousSnapshot.viewportWidth) {
          incrementPretextPerfCounter("pretext_full_relayout_calls");
          incrementPretextPerfCounter(
            "pretext_full_relayout_item_count",
            listItemsRef.current.length,
          );
          addPretextPerfBucket("pretext_full_relayout_reason", "visible:resize-width");
        }
        core.syncViewport({
          height: nextHeight,
          width: nextWidth,
          scrollTop: scroller.scrollTop,
        });
        if (nextWidth !== previousSnapshot.viewportWidth) {
          core.syncItems(
            listItemsRef.current,
            shouldRestoreBottom
              ? { kind: "bottom" }
              : previousSnapshot.anchor.kind === "item"
                ? previousSnapshot.anchor
                : null,
          );
        }
        if (!shouldRestoreBottom && previousSnapshot.anchor.kind === "item") {
          followBottomRef.current = false;
          applySnapshotToDom(core.restoreAnchor(previousSnapshot.anchor, "ratio"), {
            behavior: "auto",
            followBottom: false,
          });
          return;
        }
      }
      if (shouldRestoreBottom) {
        followBottomRef.current = true;
        applySnapshotToDom(core.restoreAnchor({ kind: "bottom" }), {
          behavior: "auto",
          followBottom: true,
        });
        return;
      }
      syncFromDom();
    };
    const observer = new ResizeObserver(() => {
      if (resizeFrameId != null) {
        cancelAnimationFrame(resizeFrameId);
      }
      resizeFrameId = requestAnimationFrame(processResize);
    });
    observer.observe(scroller);
    return () => {
      observer.disconnect();
      if (resizeFrameId != null) {
        cancelAnimationFrame(resizeFrameId);
      }
    };
  }, [
    applySnapshotToDom,
    core,
    scheduleScrollbarUpdate,
    syncFromDom,
  ]);

  return {
    handleClickCapture,
    handleKeyDownCapture,
  };
}
