import {
  memo,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
} from "react";
import { type PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import { PRETEXT_VIRTUALIZER_INITIAL_BOTTOM_LOCATION } from "../state/pretextVirtualizerViewportState";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import {
  collectWorkbenchToolGroupExpansionIds,
  getWorkbenchMessageListLayoutRevision,
  type WorkbenchMessageListUiState,
} from "./sessionMessageListItemIdentity";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import {
  buildSessionPretextRuntimeSourceKey,
  buildSessionPretextRuntimeLayoutKey,
  getOrCreateSessionPretextRuntime,
  SESSION_PRETEXT_BOTTOM_THRESHOLD_PX,
} from "./sessionThread/pretextSessionRuntimeCache";
import { createSessionThreadPretextVirtualizerMethods } from "./sessionThread/pretextVirtualizerMethods";
import { MeasuredPretextRow } from "./sessionThread/pretextVirtualizerMeasuredRow";
import {
  writePretextAssistantHeightOverride,
  writePretextMessageHeightOverride,
} from "./sessionThread/pretextRowMeasurementOverrides";
import { usePretextTranscriptScrollbar } from "./sessionThread/usePretextTranscriptScrollbar";
import { useSessionThreadPretextLifecycleController } from "./sessionThread/useSessionThreadPretextLifecycleController";
import { useSessionThreadPretextScrollController } from "./sessionThread/useSessionThreadPretextScrollController";
import { getWorkbenchMessageLayoutState } from "./sessionThread/transcriptRowLayoutModel";
import type { SessionThreadPretextVirtualizerListProps } from "./SessionThreadMessageList.pretextVirtualizer.types";
import {
  createInitialSessionThreadPretextSnapshot,
  isBottomOpenLocation,
} from "./sessionThread/pretextVirtualizerDisplayState";
import {
  commitSessionThreadRuntimeSnapshot,
  getRenderedItemsFromSnapshot,
} from "./sessionThread/pretextVirtualizerRuntimeState";
import { usePretextActivationRestore } from "./sessionThread/usePretextActivationRestore";

const BOTTOM_THRESHOLD_PX = SESSION_PRETEXT_BOTTOM_THRESHOLD_PX;
const JUMP_TO_LATEST_THRESHOLD_PX = 200;

export const SessionThreadPretextVirtualizerList = memo(function SessionThreadPretextVirtualizerList({
  style,
  sessionId,
  isActive,
  listItems,
  sourceKey,
  threadProjectionOp,
  initialLocation = PRETEXT_VIRTUALIZER_INITIAL_BOTTOM_LOCATION,
  itemContent,
  itemKey,
  context,
  onScroll,
  onRenderedDataChange,
  onAtBottomChange,
  onDiagnosticEvent,
  methodsRef,
  shortSizeAlign = "top",
}: SessionThreadPretextVirtualizerListProps) {
  const listItemsRef = useRef(listItems);
  const onScrollRef = useRef(onScroll);
  const onRenderedDataChangeRef = useRef(onRenderedDataChange);
  const onAtBottomChangeRef = useRef(onAtBottomChange);
  const onDiagnosticEventRef = useRef(onDiagnosticEvent);
  const runtimeSourceKeyRef = useRef<string | null>(null);
  const runtimeUiStateLayoutKeyRef = useRef<string | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const followBottomRef = useRef(true);
  const lastScrollTopRef = useRef(0);
  const lastSyncedItemCountRef = useRef(listItems.length);
  const lastAtBottomRef = useRef<boolean | null>(null);
  const snapshotRef = useRef<PretextVirtualizerSnapshot<WorkbenchListItem> | null>(null);
  const lastAppliedUiStateLayoutRevisionRef = useRef<string | null>(null);
  const lastAppliedProjectionOpRef = useRef<string | null>(null);
  const lastInteractedItemIdRef = useRef<string | null>(null);
  const pendingProgrammaticTopRef = useRef<number | null>(null);
  const pendingProgrammaticBehaviorRef = useRef<ScrollBehavior>("auto");
  const pendingRestoreRef = useRef(false);
  const pendingHeightCorrectionKeysRef = useRef(new Set<string>());
  listItemsRef.current = listItems;
  onScrollRef.current = onScroll;
  onRenderedDataChangeRef.current = onRenderedDataChange;
  onAtBottomChangeRef.current = onAtBottomChange;
  onDiagnosticEventRef.current = onDiagnosticEvent;

  const runtimeUiState = useMemo<WorkbenchMessageListUiState>(
    () => ({
      expandedTurnHeaders: { ...(context.expandedTurnHeaders ?? {}) },
      expandedTurnDetailsById: { ...(context.expandedTurnDetailsById ?? {}) },
      expandedToolById: { ...(context.expandedToolById ?? {}) },
      expandedMessageById: { ...(context.expandedMessageById ?? {}) },
      turnToolsLoading: [...(context.turnToolsLoading ?? [])],
      verbosity: context.verbosity,
    }),
    [
      context.expandedMessageById,
      context.expandedToolById,
      context.expandedTurnDetailsById,
      context.expandedTurnHeaders,
      context.turnToolsLoading,
      context.verbosity,
    ],
  );
  const runtimeToolGroupExpansionIds = useMemo(
    () => collectWorkbenchToolGroupExpansionIds(listItems),
    [listItems],
  );
  const runtimeUiStateLayoutRevision = useMemo(
    () =>
      getWorkbenchMessageListLayoutRevision(runtimeUiState, {
        toolExpansionIds: runtimeToolGroupExpansionIds,
      }),
    [runtimeToolGroupExpansionIds, runtimeUiState],
  );
  const runtimeUiStateLayoutKey = useMemo(
    () =>
      buildSessionPretextRuntimeLayoutKey({
        uiState: runtimeUiState,
        listItems,
      }),
    [listItems, runtimeUiState],
  );
  const runtimeSourceKey = useMemo(
    () => sourceKey ?? buildSessionPretextRuntimeSourceKey(listItems, runtimeUiState),
    [listItems, runtimeUiState, sourceKey],
  );
  runtimeSourceKeyRef.current = runtimeSourceKey;
  runtimeUiStateLayoutKeyRef.current = runtimeUiStateLayoutKey;
  const runtime = useMemo(
    () =>
      getOrCreateSessionPretextRuntime(sessionId, {
        uiState: runtimeUiState,
        listItems,
        onDiagnosticEvent: (event) => {
          onDiagnosticEventRef.current?.(event);
        },
      }),
    [listItems, runtimeUiState, sessionId],
  );
  const core = runtime.core;
  if (lastAppliedUiStateLayoutRevisionRef.current == null) {
    lastAppliedUiStateLayoutRevisionRef.current = runtimeUiStateLayoutRevision;
  }

  const createInitialSnapshot = useCallback(
    () =>
      createInitialSessionThreadPretextSnapshot({
        runtime,
        sessionId,
        listItems,
        uiState: runtimeUiState,
        sourceKey: runtimeSourceKey,
        layoutKey: runtimeUiStateLayoutKey,
      }),
    [listItems, runtime, runtimeSourceKey, runtimeUiState, runtimeUiStateLayoutKey, sessionId],
  );
  const requireBottomAlignmentForDisplay = isBottomOpenLocation(initialLocation);
  const {
    scrollbarActive,
    scrollbarDragging,
    scrollbarNeeded,
    scrollbarThumbRef,
    scrollbarTrackRef,
    handleScrollbarMouseLeave,
    handleScrollbarThumbPointerDown,
    handleScrollbarThumbPointerMove,
    handleScrollbarThumbPointerUp,
    handleScrollbarTrackPointerDown,
    scheduleScrollbarUpdate,
    showScrollbarTemporarily,
  } = usePretextTranscriptScrollbar({
    containerRef,
    followBottomRef,
  });

  const emitRenderedData = useCallback((nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>) => {
    onRenderedDataChangeRef.current?.(getRenderedItemsFromSnapshot(nextSnapshot, listItemsRef.current));
  }, []);

  const emitAtBottomChange = useCallback((atBottom: boolean) => {
    onAtBottomChangeRef.current?.(atBottom);
  }, []);

  const emitScroll = useCallback(
    (payload: {
      listOffset: number;
      visibleListHeight: number;
      bottomOffset: number;
    }) => {
      onScrollRef.current?.(payload);
    },
    [],
  );

  const commitRuntimeSnapshot = useCallback(
    (nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>, nextItems: readonly WorkbenchListItem[]) => {
      commitSessionThreadRuntimeSnapshot(runtime, nextSnapshot, nextItems, {
        sourceKey: runtimeSourceKeyRef.current,
        layoutKey: runtimeUiStateLayoutKeyRef.current,
      });
    },
    [runtime],
  );

  const scrollControllerParams = useMemo(
    () => ({
      core,
      createInitialSnapshot,
      initialListCount: listItems.length,
      requireBottomAlignmentForDisplay,
      bottomThresholdPx: BOTTOM_THRESHOLD_PX,
      jumpToLatestThresholdPx: JUMP_TO_LATEST_THRESHOLD_PX,
      containerRef,
      listItemsRef,
      followBottomRef,
      lastScrollTopRef,
      lastAtBottomRef,
      snapshotRef,
      pendingProgrammaticTopRef,
      pendingProgrammaticBehaviorRef,
      commitRuntimeSnapshot,
      emitRenderedData,
      emitAtBottomChange,
      emitScroll,
      scheduleScrollbarUpdate,
      showScrollbarTemporarily,
    }),
    [
      commitRuntimeSnapshot,
      core,
      emitAtBottomChange,
      emitRenderedData,
      emitScroll,
      listItems.length,
      requireBottomAlignmentForDisplay,
      scheduleScrollbarUpdate,
      showScrollbarTemporarily,
    ],
  );

  const {
    snapshot,
    surfaceReady,
    showJumpToLatest,
    applySnapshotToDom,
    syncFromDom,
    restoreBottom,
    scrollToOffset,
    scrollToItem,
    handleScroll,
    handleWheel,
  } = useSessionThreadPretextScrollController(scrollControllerParams);

  const handleAssistantHeightMismatch = useCallback(
    (item: Extract<WorkbenchListItem, { kind: "assistant" }>, actualHeight: number, plannedHeight: number) => {
      if (Math.abs(actualHeight - plannedHeight) <= 1) return;
      const scroller = containerRef.current;
      if (!scroller) return;
      const changed = writePretextAssistantHeightOverride({
        sessionId,
        item,
        viewportWidth: scroller.clientWidth,
        height: actualHeight,
      });
      if (!changed) return;

      const correctionKey = `${item.id}:${Math.max(1, Math.round(scroller.clientWidth))}`;
      if (pendingHeightCorrectionKeysRef.current.has(correctionKey)) return;
      pendingHeightCorrectionKeysRef.current.add(correctionKey);

      requestAnimationFrame(() => {
        pendingHeightCorrectionKeysRef.current.delete(correctionKey);
        const currentScroller = containerRef.current;
        if (!currentScroller) return;
        const currentItems = listItemsRef.current;
        if (!currentItems.some((candidate) => candidate.id === item.id && candidate.kind === "assistant")) {
          return;
        }
        const nextSnapshot = core.patchItems(
          currentItems,
          [item.id],
          [item.id],
          followBottomRef.current ? { kind: "bottom" } : null,
        );
        commitRuntimeSnapshot(nextSnapshot, currentItems);
        applySnapshotToDom(nextSnapshot, {
          behavior: "auto",
          followBottom: followBottomRef.current,
          nextItems: currentItems,
        });
        scheduleScrollbarUpdate();
      });
    },
    [applySnapshotToDom, commitRuntimeSnapshot, core, scheduleScrollbarUpdate, sessionId],
  );

  const handleMessageHeightMismatch = useCallback(
    (item: Extract<WorkbenchListItem, { kind: "message" }>, actualHeight: number, plannedHeight: number) => {
      if (Math.abs(actualHeight - plannedHeight) <= 1) return;
      const scroller = containerRef.current;
      if (!scroller) return;
      const layout = getWorkbenchMessageLayoutState(item, runtimeUiState.expandedMessageById);
      const changed = writePretextMessageHeightOverride({
        sessionId,
        item,
        viewportWidth: scroller.clientWidth,
        layout,
        height: actualHeight,
      });
      if (!changed) return;

      const correctionKey = `${item.id}:${Math.max(1, Math.round(scroller.clientWidth))}`;
      if (pendingHeightCorrectionKeysRef.current.has(correctionKey)) return;
      pendingHeightCorrectionKeysRef.current.add(correctionKey);

      requestAnimationFrame(() => {
        pendingHeightCorrectionKeysRef.current.delete(correctionKey);
        const currentScroller = containerRef.current;
        if (!currentScroller) return;
        const currentItems = listItemsRef.current;
        if (!currentItems.some((candidate) => candidate.id === item.id && candidate.kind === "message")) {
          return;
        }
        const nextSnapshot = core.patchItems(
          currentItems,
          [item.id],
          [item.id],
          followBottomRef.current ? { kind: "bottom" } : null,
        );
        commitRuntimeSnapshot(nextSnapshot, currentItems);
        applySnapshotToDom(nextSnapshot, {
          behavior: "auto",
          followBottom: followBottomRef.current,
          nextItems: currentItems,
        });
        scheduleScrollbarUpdate();
      });
    },
    [
      applySnapshotToDom,
      commitRuntimeSnapshot,
      core,
      runtimeUiState.expandedMessageById,
      scheduleScrollbarUpdate,
      sessionId,
    ],
  );

  const handleWheelEvent = useCallback(
    (event: React.WheelEvent<HTMLDivElement>) => {
      handleWheel(event.deltaY);
    },
    [handleWheel],
  );

  const { handleClickCapture, handleKeyDownCapture } = useSessionThreadPretextLifecycleController({
    core,
    runtime,
    listItems,
    initialLocation,
    runtimeUiStateLayoutRevision,
    runtimeUiStateLayoutKeyRef,
    containerRef,
    listItemsRef,
    followBottomRef,
    lastSyncedItemCountRef,
    lastAtBottomRef,
    snapshotRef,
    lastAppliedUiStateLayoutRevisionRef,
    lastAppliedProjectionOpRef,
    lastInteractedItemIdRef,
    pendingProgrammaticTopRef,
    pendingProgrammaticBehaviorRef,
    pendingRestoreRef,
    threadProjectionOp,
    applySnapshotToDom,
    commitRuntimeSnapshot,
    scheduleScrollbarUpdate,
    scrollToOffset,
    syncFromDom,
  });

  usePretextActivationRestore({
    containerRef,
    followBottomRef,
    initialLocation,
    isActive,
    restoreBottom,
    syncFromDom,
  });

  const pretextVirtualizerMethods = useMemo(
    () =>
      createSessionThreadPretextVirtualizerMethods({
        applySnapshotToDom,
        containerRef,
        pendingProgrammaticBehaviorRef,
        pendingProgrammaticTopRef,
        restoreAnchorSnapshot: (anchor) => {
          pendingRestoreRef.current = true;
          const nextSnapshot = core.restoreAnchor(anchor);
          pendingRestoreRef.current = false;
          return nextSnapshot;
        },
        restoreBottom,
        scrollToItemFn: scrollToItem,
        scrollToOffsetFn: scrollToOffset,
      }),
    [applySnapshotToDom, core, restoreBottom, scrollToItem, scrollToOffset],
  );

  useLayoutEffect(() => {
    if (!methodsRef) return;
    methodsRef.current = pretextVirtualizerMethods;
    return () => {
      if (methodsRef.current === pretextVirtualizerMethods) {
        methodsRef.current = null;
      }
    };
  }, [methodsRef, pretextVirtualizerMethods]);

  useLayoutEffect(() => {
    scheduleScrollbarUpdate();
  }, [scheduleScrollbarUpdate, snapshot.scrollTop, snapshot.totalHeight, snapshot.viewportHeight]);

  const bottomOffsetPx = Math.max(0, snapshot.totalHeight - (snapshot.scrollTop + snapshot.viewportHeight));
  const shortThreadOffsetPx =
    shortSizeAlign === "bottom" && snapshot.totalHeight < snapshot.viewportHeight
      ? snapshot.viewportHeight - snapshot.totalHeight
      : 0;
  const innerHeight = Math.max(snapshot.totalHeight + shortThreadOffsetPx, snapshot.viewportHeight);
  const renderedItems = snapshot.visibleItems;

  return (
    <div
      className="wb-pretext-transcript-shell wb-thread-stack wb-thread-scroller--message-list"
      style={{ position: "relative", minWidth: 0 }}
      onMouseLeave={handleScrollbarMouseLeave}
    >
      <div
        ref={containerRef}
        style={{
          ...style,
          position: "absolute",
          inset: 0,
          width: "auto",
          minWidth: 0,
          visibility: surfaceReady ? "visible" : "hidden",
        }}
        className="wb-thread-scroller"
        role="list"
        onClickCapture={handleClickCapture}
        onKeyDownCapture={handleKeyDownCapture}
        onScroll={handleScroll}
        onWheel={handleWheelEvent}
        data-pretext-virtualizer-list="1"
        data-pretext-virtualizer-snapshot-scroll-top={String(Math.round(snapshot.scrollTop))}
        data-pretext-virtualizer-snapshot-first-index={String(snapshot.visibleItems[0]?.index ?? -1)}
        data-pretext-virtualizer-snapshot-last-index={String(snapshot.visibleItems.at(-1)?.index ?? -1)}
        data-pretext-virtualizer-rendered-first-index={String(renderedItems[0]?.index ?? -1)}
        data-pretext-virtualizer-rendered-last-index={String(renderedItems.at(-1)?.index ?? -1)}
        data-pretext-virtualizer-programmatic-pending={pendingProgrammaticTopRef.current != null ? "1" : "0"}
        data-pretext-virtualizer-pending-restore={pendingRestoreRef.current ? "1" : "0"}
      >
        <div
          className="wb-thread-list"
          data-pretext-virtualizer-content="1"
          style={{ position: "relative", height: `${innerHeight}px` }}
        >
          {renderedItems.map((visibleItem) => {
            const latestItem = listItemsRef.current[visibleItem.index];
            const currentItem = latestItem?.id === visibleItem.id ? latestItem : visibleItem.item;
            return (
            <div
              key={itemKey(currentItem)}
              data-pretext-virtualizer-row-shell="1"
              style={{
                position: "absolute",
                top: `${visibleItem.top + shortThreadOffsetPx}px`,
                left: 0,
                right: 0,
                width: "100%",
              }}
            >
              <div
                className="wb-pretext-virtualizer-row"
                data-pretext-virtualizer-row="1"
                data-pretext-virtualizer-item-id={currentItem.id}
                data-pretext-virtualizer-planned-height={String(visibleItem.height)}
              >
                <MeasuredPretextRow
                  id={visibleItem.id}
                  itemKind={currentItem.kind}
                  itemKey={itemKey(currentItem)}
                  plannedHeight={visibleItem.height}
                  onHeightMismatch={
                    currentItem.kind === "assistant"
                      ? ({ actualHeight, plannedHeight }) =>
                          handleAssistantHeightMismatch(currentItem, actualHeight, plannedHeight)
                      : currentItem.kind === "message" &&
                          Boolean(runtimeUiState.expandedMessageById[currentItem.id])
                        ? ({ actualHeight, plannedHeight }) =>
                            handleMessageHeightMismatch(currentItem, actualHeight, plannedHeight)
                        : undefined
                  }
                >
                  {itemContent(visibleItem.index, currentItem)}
                </MeasuredPretextRow>
              </div>
            </div>
          );})}
        </div>
      </div>
      <div
        className={`wb-scrollbar${scrollbarActive ? " is-active" : ""}${scrollbarDragging ? " is-dragging" : ""}${scrollbarNeeded ? "" : " is-hidden"}`}
        aria-hidden="true"
      >
        <div
          className="wb-scrollbar-track"
          ref={(node) => {
            scrollbarTrackRef.current = node;
            if (node) {
              scheduleScrollbarUpdate();
            }
          }}
          onPointerDown={handleScrollbarTrackPointerDown}
        >
          <div
            className="wb-scrollbar-thumb"
            ref={(node) => {
              scrollbarThumbRef.current = node;
              if (node) {
                scheduleScrollbarUpdate();
              }
            }}
            onPointerDown={handleScrollbarThumbPointerDown}
            onPointerMove={handleScrollbarThumbPointerMove}
            onPointerUp={handleScrollbarThumbPointerUp}
            onPointerCancel={handleScrollbarThumbPointerUp}
          />
        </div>
      </div>
      {showJumpToLatest && bottomOffsetPx > JUMP_TO_LATEST_THRESHOLD_PX ? (
        <div
          style={{
            position: "absolute",
            left: "50%",
            bottom: 12,
            transform: "translateX(-50%)",
            pointerEvents: "none",
          }}
        >
          <button
            type="button"
            className="new-activity-overlay"
            aria-label="Jump to latest"
            title="Jump to latest"
            onClick={() => restoreBottom("auto")}
            style={{ pointerEvents: "auto" }}
          >
            ↓
          </button>
        </div>
      ) : null}
    </div>
  );
});
