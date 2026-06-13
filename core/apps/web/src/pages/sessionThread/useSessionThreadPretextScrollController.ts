import { useCallback, useLayoutEffect, useRef, useState, type MutableRefObject } from "react";
import type { PretextVirtualizerSnapshot } from "@pretext-virtualizer/core";
import type { PretextVirtualizerItemLocation } from "@pretext-virtualizer/interface";
import {
  computeBottomOffsetPx,
  resolveFollowBottomAfterScroll,
} from "./pretextFollowBottom";
import {
  approximateIndexForLocation,
  resolveScrollTopForLocation,
} from "./pretextVirtualizerProjectionHelpers";
import { isSnapshotReadyForDisplay } from "./pretextVirtualizerDisplayState";
import type { SessionPretextRuntimeRecord } from "./pretextSessionRuntimeCache";
import type { WorkbenchListItem } from "../SessionPage.types";

export function useSessionThreadPretextScrollController(params: {
  core: SessionPretextRuntimeRecord["core"];
  createInitialSnapshot: () => PretextVirtualizerSnapshot<WorkbenchListItem>;
  initialListCount: number;
  requireBottomAlignmentForDisplay: boolean;
  bottomThresholdPx: number;
  jumpToLatestThresholdPx: number;
  containerRef: MutableRefObject<HTMLDivElement | null>;
  listItemsRef: MutableRefObject<readonly WorkbenchListItem[]>;
  followBottomRef: MutableRefObject<boolean>;
  lastScrollTopRef: MutableRefObject<number>;
  lastAtBottomRef: MutableRefObject<boolean | null>;
  snapshotRef: MutableRefObject<PretextVirtualizerSnapshot<WorkbenchListItem> | null>;
  pendingProgrammaticTopRef: MutableRefObject<number | null>;
  pendingProgrammaticBehaviorRef: MutableRefObject<ScrollBehavior>;
  commitRuntimeSnapshot: (
    nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
    nextItems: readonly WorkbenchListItem[],
  ) => void;
  emitRenderedData: (nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>) => void;
  emitAtBottomChange: (atBottom: boolean) => void;
  emitScroll: (payload: {
    listOffset: number;
    visibleListHeight: number;
    bottomOffset: number;
  }) => void;
  scheduleScrollbarUpdate: () => void;
  showScrollbarTemporarily: () => void;
}): {
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>;
  surfaceReady: boolean;
  showJumpToLatest: boolean;
  applySnapshotToDom: (
    nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
    options?: {
      behavior?: ScrollBehavior;
      followBottom?: boolean;
      nextItems?: readonly WorkbenchListItem[];
    },
  ) => void;
  syncFromDom: (scrollTopOverride?: number) => PretextVirtualizerSnapshot<WorkbenchListItem>;
  restoreBottom: (behavior?: ScrollBehavior) => void;
  scrollToOffset: (scrollTop: number, behavior?: ScrollBehavior) => void;
  scrollToItem: (location: PretextVirtualizerItemLocation) => void;
  handleScroll: () => void;
  handleWheel: (deltaY: number) => void;
} {
  const {
    core,
    initialListCount,
    requireBottomAlignmentForDisplay,
    bottomThresholdPx,
    jumpToLatestThresholdPx,
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
  } = params;
  const initialSnapshotRef = useRef<PretextVirtualizerSnapshot<WorkbenchListItem> | null>(null);
  if (initialSnapshotRef.current == null) {
    initialSnapshotRef.current = params.createInitialSnapshot();
  }
  const initialSnapshot = initialSnapshotRef.current;
  const [showJumpToLatest, setShowJumpToLatest] = useState(false);
  const [snapshot, setSnapshot] = useState(initialSnapshot);
  const [surfaceReady, setSurfaceReady] = useState(() =>
    isSnapshotReadyForDisplay(
      initialSnapshot,
      initialListCount,
      requireBottomAlignmentForDisplay,
      bottomThresholdPx,
    ),
  );
  snapshotRef.current = snapshot;

  const emitScrollState = useCallback(
    (nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>) => {
      const bottomOffsetPx = computeBottomOffsetPx({
        totalHeight: nextSnapshot.totalHeight,
        scrollTop: nextSnapshot.scrollTop,
        viewportHeight: nextSnapshot.viewportHeight,
      });
      const atBottom = bottomOffsetPx <= bottomThresholdPx;
      if (lastAtBottomRef.current !== atBottom) {
        lastAtBottomRef.current = atBottom;
        emitAtBottomChange(atBottom);
      }
      setShowJumpToLatest(bottomOffsetPx > jumpToLatestThresholdPx);
      emitScroll({
        listOffset: -nextSnapshot.scrollTop,
        visibleListHeight: nextSnapshot.viewportHeight,
        bottomOffset: bottomOffsetPx,
      });
      emitRenderedData(nextSnapshot);
      scheduleScrollbarUpdate();
    },
    [
      bottomThresholdPx,
      emitAtBottomChange,
      emitRenderedData,
      emitScroll,
      jumpToLatestThresholdPx,
      lastAtBottomRef,
      scheduleScrollbarUpdate,
    ],
  );

  const applySnapshotToDom = useCallback(
    (
      nextSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
      options?: {
        behavior?: ScrollBehavior;
        followBottom?: boolean;
        nextItems?: readonly WorkbenchListItem[];
      },
    ) => {
      const nextItems = options?.nextItems ?? listItemsRef.current;
      const scroller = containerRef.current;
      if (!scroller) {
        setSurfaceReady(
          isSnapshotReadyForDisplay(
            nextSnapshot,
            nextItems.length,
            options?.followBottom ?? followBottomRef.current,
            bottomThresholdPx,
          ),
        );
        setSnapshot(nextSnapshot);
        return;
      }
      const targetTop = Math.max(
        0,
        Math.min(nextSnapshot.scrollTop, Math.max(0, nextSnapshot.totalHeight - nextSnapshot.viewportHeight)),
      );
      pendingProgrammaticTopRef.current = targetTop;
      pendingProgrammaticBehaviorRef.current = options?.behavior ?? "auto";
      if (options?.behavior && typeof scroller.scrollTo === "function") {
        scroller.scrollTo({ top: targetTop, behavior: options.behavior });
      } else {
        scroller.scrollTop = targetTop;
      }
      if (options?.followBottom != null) {
        followBottomRef.current = options.followBottom;
      }
      lastScrollTopRef.current = targetTop;
      commitRuntimeSnapshot(nextSnapshot, nextItems);
      setSurfaceReady(
        isSnapshotReadyForDisplay(
          nextSnapshot,
          nextItems.length,
          options?.followBottom ?? followBottomRef.current,
          bottomThresholdPx,
        ),
      );
      setSnapshot(nextSnapshot);
      emitScrollState(nextSnapshot);
      if (Math.abs(scroller.scrollTop - targetTop) <= 1) {
        pendingProgrammaticTopRef.current = null;
      }
    },
    [
      bottomThresholdPx,
      commitRuntimeSnapshot,
      containerRef,
      emitScrollState,
      followBottomRef,
      listItemsRef,
      pendingProgrammaticBehaviorRef,
      pendingProgrammaticTopRef,
      lastScrollTopRef,
    ],
  );

  const syncFromDom = useCallback(
    (scrollTopOverride?: number): PretextVirtualizerSnapshot<WorkbenchListItem> => {
      const scroller = containerRef.current;
      if (!scroller) {
        const nextSnapshot = core.getSnapshot();
        commitRuntimeSnapshot(nextSnapshot, listItemsRef.current);
        setSurfaceReady(
          isSnapshotReadyForDisplay(
            nextSnapshot,
            listItemsRef.current.length,
            followBottomRef.current,
            bottomThresholdPx,
          ),
        );
        setSnapshot(nextSnapshot);
        return nextSnapshot;
      }
      const nextSnapshot = core.syncViewport({
        height: scroller.clientHeight,
        width: scroller.clientWidth,
        scrollTop: scrollTopOverride ?? scroller.scrollTop,
      });
      lastScrollTopRef.current = scroller.scrollTop;
      commitRuntimeSnapshot(nextSnapshot, listItemsRef.current);
      setSurfaceReady(
        isSnapshotReadyForDisplay(
          nextSnapshot,
          listItemsRef.current.length,
          followBottomRef.current,
          bottomThresholdPx,
        ),
      );
      setSnapshot(nextSnapshot);
      emitScrollState(nextSnapshot);
      return nextSnapshot;
    },
    [
      bottomThresholdPx,
      commitRuntimeSnapshot,
      containerRef,
      core,
      emitScrollState,
      followBottomRef,
      listItemsRef,
      lastScrollTopRef,
    ],
  );

  const restoreBottom = useCallback(
    (behavior: ScrollBehavior = "auto") => {
      const nextSnapshot = core.restoreAnchor({ kind: "bottom" });
      applySnapshotToDom(nextSnapshot, { behavior, followBottom: true });
    },
    [applySnapshotToDom, core],
  );

  const scrollToOffset = useCallback(
    (scrollTop: number, behavior: ScrollBehavior = "auto") => {
      const scroller = containerRef.current;
      if (!scroller) return;
      const nextTop = Math.max(0, scrollTop);
      pendingProgrammaticTopRef.current = nextTop;
      pendingProgrammaticBehaviorRef.current = behavior;
      if (typeof scroller.scrollTo === "function") {
        scroller.scrollTo({ top: nextTop, behavior });
      } else {
        scroller.scrollTop = nextTop;
      }
      if (behavior === "auto") {
        syncFromDom(nextTop);
        if (
          pendingProgrammaticTopRef.current != null &&
          Math.abs(scroller.scrollTop - pendingProgrammaticTopRef.current) <= 1
        ) {
          pendingProgrammaticTopRef.current = null;
        }
      }
    },
    [containerRef, pendingProgrammaticBehaviorRef, pendingProgrammaticTopRef, syncFromDom],
  );

  const scrollToItem = useCallback(
    (location: PretextVirtualizerItemLocation) => {
      const nextSnapshot = core.getSnapshot();
      const targetIndex = approximateIndexForLocation(listItemsRef.current, location);
      const resolvedLocation =
        location.index === "LAST" ? location : { ...location, index: targetIndex };
      const targetTop = resolveScrollTopForLocation(
        nextSnapshot,
        resolvedLocation,
        core,
        listItemsRef.current.length,
      );
      const followBottom = location.index === "LAST" && (location.align ?? "start") === "end";
      scrollToOffset(targetTop, location.behavior ?? "auto");
      if (followBottom) {
        followBottomRef.current = true;
      }
    },
    [core, followBottomRef, listItemsRef, scrollToOffset],
  );

  useLayoutEffect(() => {
    const scroller = containerRef.current;
    const pendingProgrammaticTop = pendingProgrammaticTopRef.current;
    if (!scroller || pendingProgrammaticTop == null) return;
    if (pendingProgrammaticBehaviorRef.current === "smooth") return;
    const maxScrollTop = Math.max(0, snapshot.totalHeight - snapshot.viewportHeight);
    const clampedTop = Math.max(0, Math.min(pendingProgrammaticTop, maxScrollTop));
    if (Math.abs(scroller.scrollTop - clampedTop) > 1) {
      scroller.scrollTop = clampedTop;
    }
    lastScrollTopRef.current = scroller.scrollTop;
    if (Math.abs(scroller.scrollTop - clampedTop) <= 1) {
      pendingProgrammaticTopRef.current = null;
      pendingProgrammaticBehaviorRef.current = "auto";
    }
  }, [
    containerRef,
    lastScrollTopRef,
    pendingProgrammaticBehaviorRef,
    pendingProgrammaticTopRef,
    snapshot.scrollTop,
    snapshot.totalHeight,
    snapshot.viewportHeight,
  ]);

  const handleScroll = useCallback(() => {
    const scroller = containerRef.current;
    if (!scroller) return;
    const currentScrollTop = scroller.scrollTop;
    const previousScrollTop = lastScrollTopRef.current;
    const pendingProgrammaticTop = pendingProgrammaticTopRef.current;
    let programmaticScroll = false;
    if (pendingProgrammaticTop != null) {
      const deltaToPendingTop = Math.abs(currentScrollTop - pendingProgrammaticTop);
      if (deltaToPendingTop <= 1) {
        pendingProgrammaticTopRef.current = null;
        pendingProgrammaticBehaviorRef.current = "auto";
        programmaticScroll = true;
      } else if (pendingProgrammaticBehaviorRef.current === "smooth") {
        programmaticScroll = true;
      } else {
        pendingProgrammaticTopRef.current = null;
        pendingProgrammaticBehaviorRef.current = "auto";
      }
    }
    if (Math.abs(currentScrollTop - previousScrollTop) > 0.5 && !programmaticScroll) {
      showScrollbarTemporarily();
    }
    lastScrollTopRef.current = currentScrollTop;
    const nextSnapshot = core.syncViewport({
      height: scroller.clientHeight,
      width: scroller.clientWidth,
      scrollTop: currentScrollTop,
    });
    const bottomOffsetPx = computeBottomOffsetPx({
      totalHeight: nextSnapshot.totalHeight,
      scrollTop: nextSnapshot.scrollTop,
      viewportHeight: nextSnapshot.viewportHeight,
    });
    followBottomRef.current = resolveFollowBottomAfterScroll({
      followBottom: followBottomRef.current,
      previousScrollTop,
      currentScrollTop,
      bottomOffsetPx,
      thresholdPx: bottomThresholdPx,
      programmaticScroll,
    });
    commitRuntimeSnapshot(nextSnapshot, listItemsRef.current);
    setSnapshot(nextSnapshot);
    emitScrollState(nextSnapshot);
  }, [
    bottomThresholdPx,
    commitRuntimeSnapshot,
    containerRef,
    core,
    emitScrollState,
    followBottomRef,
    listItemsRef,
    lastScrollTopRef,
    pendingProgrammaticBehaviorRef,
    pendingProgrammaticTopRef,
    showScrollbarTemporarily,
  ]);

  const handleWheel = useCallback(
    (deltaY: number) => {
      if (deltaY < 0) {
        followBottomRef.current = false;
        if (lastAtBottomRef.current !== false) {
          lastAtBottomRef.current = false;
          emitAtBottomChange(false);
        }
      }
      showScrollbarTemporarily();
    },
    [emitAtBottomChange, followBottomRef, lastAtBottomRef, showScrollbarTemporarily],
  );

  return {
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
  };
}
