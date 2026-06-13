import { useCallback, useLayoutEffect, useRef, type MutableRefObject } from "react";
import type {
  ListScrollLocation,
  VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import {
  computeHistoryPrefetchThresholdPx,
  pickAnchorIdsFromRange,
  pickAnchorIdsFromScroller,
} from "./sessionMessageListControllerUtils";

type Params = {
  sessionId: string;
  isActive: boolean;
  loaded: boolean;
  visibleListItems: WorkbenchListItem[];
  canLoadOlder: boolean;
  loadOlder: () => Promise<void>;
  loadingOlder: boolean;
  setLoadingOlder: (next: boolean) => void;
  deferTrailingAppends: boolean;
  setDeferTrailingAppends: (next: boolean) => void;
  methodsRef: MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  stickToBottomRef: MutableRefObject<boolean>;
  lastAtBottomRef: MutableRefObject<boolean | null>;
  renderedAnchorIdRef: MutableRefObject<string | null>;
  renderedTopIdRef: MutableRefObject<string | null>;
  firstListItemIdRef: MutableRefObject<string | null>;
  pendingHistoryRef: MutableRefObject<boolean>;
  historyExpectedRef: MutableRefObject<boolean>;
  historyRequestedAtTopRef: MutableRefObject<boolean>;
  historyRequestedAnchorIdRef: MutableRefObject<string | null>;
  showDebug: boolean;
  onAtBottomChange?: (atBottom: boolean) => void;
  recordDebugSnapshot: (label: string, detail?: Record<string, unknown>) => void;
  snapToBottom: (methods: VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>) => void;
};

type Result = {
  onScroll: (location: ListScrollLocation) => void;
  onRenderedDataChange: (range: WorkbenchListItem[]) => void;
};

export function useSessionMessageListViewportHistory(params: Params): Result {
  const {
    sessionId,
    isActive,
    loaded,
    visibleListItems,
    canLoadOlder,
    loadOlder,
    loadingOlder,
    setLoadingOlder,
    deferTrailingAppends,
    setDeferTrailingAppends,
    methodsRef,
    stickToBottomRef,
    lastAtBottomRef,
    renderedAnchorIdRef,
    renderedTopIdRef,
    firstListItemIdRef,
    pendingHistoryRef,
    historyExpectedRef,
    historyRequestedAtTopRef,
    historyRequestedAnchorIdRef,
    showDebug,
    onAtBottomChange,
    recordDebugSnapshot,
    snapToBottom,
  } = params;

  const lastIsActiveRef = useRef(isActive);
  const lastScrollLocationRef = useRef<ListScrollLocation | null>(null);
  const lastListOffsetRef = useRef<number | null>(null);
  const lastScrollDebugAtRef = useRef(0);

  firstListItemIdRef.current = visibleListItems[0]?.id ?? null;

  useLayoutEffect(() => {
    pendingHistoryRef.current = false;
    historyExpectedRef.current = false;
    historyRequestedAtTopRef.current = false;
    historyRequestedAnchorIdRef.current = null;
    setLoadingOlder(false);
    lastScrollLocationRef.current = null;
    lastListOffsetRef.current = null;
    stickToBottomRef.current = true;
    lastAtBottomRef.current = true;
    renderedAnchorIdRef.current = null;
    renderedTopIdRef.current = null;
    firstListItemIdRef.current = null;
  }, [
    firstListItemIdRef,
    historyExpectedRef,
    historyRequestedAnchorIdRef,
    historyRequestedAtTopRef,
    lastAtBottomRef,
    pendingHistoryRef,
    renderedAnchorIdRef,
    renderedTopIdRef,
    sessionId,
    setLoadingOlder,
    stickToBottomRef,
  ]);

  useLayoutEffect(() => {
    const becameActive = isActive && !lastIsActiveRef.current;
    lastIsActiveRef.current = isActive;
    if (!becameActive) return;

    const methods = methodsRef.current;
    if (!methods) return;
    stickToBottomRef.current = true;
    lastAtBottomRef.current = true;
    methods.cancelSmoothScroll();
    snapToBottom(methods);
    onAtBottomChange?.(true);
    recordDebugSnapshot("session:activated", { reason: "focusBottom" });
  }, [
    isActive,
    lastAtBottomRef,
    methodsRef,
    onAtBottomChange,
    recordDebugSnapshot,
    snapToBottom,
    stickToBottomRef,
  ]);

  const onScroll = useCallback(
    (location: ListScrollLocation) => {
      lastScrollLocationRef.current = location;
      if (!isActive) return;

      const scroller = methodsRef.current?.scrollerElement?.() ?? null;
      const atBottomFromLocation = location.bottomOffset <= 16;
      const atBottom =
        scroller
          ? scroller.scrollHeight - (scroller.scrollTop + scroller.clientHeight) <= 16
          : atBottomFromLocation;
      stickToBottomRef.current = atBottom;
      if (atBottom && deferTrailingAppends) {
        setDeferTrailingAppends(false);
      }
      if (onAtBottomChange && lastAtBottomRef.current !== atBottom) {
        lastAtBottomRef.current = atBottom;
        onAtBottomChange(atBottom);
      }

      const atTop = location.listOffset === 0;
      const prevOffset = lastListOffsetRef.current;
      lastListOffsetRef.current = location.listOffset;
      const scrollingUp = prevOffset == null ? false : location.listOffset > prevOffset;
      const prefetchThreshold = -computeHistoryPrefetchThresholdPx(location.visibleListHeight);
      const nearTop = location.listOffset > prefetchThreshold;
      const anchors = pickAnchorIdsFromScroller(scroller);
      if (anchors.topId) renderedTopIdRef.current = anchors.topId;
      if (anchors.anchorId) renderedAnchorIdRef.current = anchors.anchorId;

      if (import.meta.env.DEV && showDebug) {
        const now = Date.now();
        const shouldRecordScroll = now - lastScrollDebugAtRef.current >= 120 || nearTop || atBottom;
        if (shouldRecordScroll) {
          lastScrollDebugAtRef.current = now;
          recordDebugSnapshot("scroll", {
            listOffset: location.listOffset,
            visibleListHeight: location.visibleListHeight,
            bottomOffset: location.bottomOffset,
            atBottom,
          });
        }
      }

      if (import.meta.env.DEV && showDebug && nearTop) {
        // eslint-disable-next-line no-console
        console.debug("[MessageList][history:gate]", {
          sessionId,
          loaded,
          canLoadOlder,
          stickToBottom: stickToBottomRef.current,
          pendingHistory: pendingHistoryRef.current,
          loadingOlder,
          atTop,
          nearTop,
          scrollingUp,
          prefetchThreshold,
          firstRenderedId: renderedTopIdRef.current,
          firstListId: firstListItemIdRef.current,
          listOffset: location.listOffset,
          visibleListHeight: location.visibleListHeight,
          renderedTopId: renderedTopIdRef.current,
          renderedAnchorId: renderedAnchorIdRef.current,
        });
      }

      if (!canLoadOlder) return;
      if (stickToBottomRef.current) return;
      if (pendingHistoryRef.current || loadingOlder) return;
      if (!nearTop) return;
      if (!scrollingUp) return;

      pendingHistoryRef.current = true;
      historyExpectedRef.current = true;
      historyRequestedAtTopRef.current = atTop;
      historyRequestedAnchorIdRef.current = renderedAnchorIdRef.current;
      setLoadingOlder(true);
      if (import.meta.env.DEV && showDebug) {
        // eslint-disable-next-line no-console
        console.debug("[MessageList][history:request]", {
          sessionId,
          atTop,
          nearTop,
          scrollingUp,
          anchorId: renderedAnchorIdRef.current,
          firstRenderedId: renderedTopIdRef.current,
          firstListId: firstListItemIdRef.current,
          listOffset: location.listOffset,
          visibleListHeight: location.visibleListHeight,
        });
      }
      loadOlder()
        .catch(() => {})
        .finally(() => {
          pendingHistoryRef.current = false;
          setLoadingOlder(false);
        });
    },
    [
      canLoadOlder,
      deferTrailingAppends,
      firstListItemIdRef,
      historyExpectedRef,
      historyRequestedAnchorIdRef,
      historyRequestedAtTopRef,
      isActive,
      lastAtBottomRef,
      loaded,
      loadOlder,
      loadingOlder,
      methodsRef,
      onAtBottomChange,
      pendingHistoryRef,
      recordDebugSnapshot,
      renderedAnchorIdRef,
      renderedTopIdRef,
      sessionId,
      setDeferTrailingAppends,
      setLoadingOlder,
      showDebug,
      stickToBottomRef,
    ],
  );

  const onRenderedDataChange = useCallback(
    (range: WorkbenchListItem[]) => {
      const methods = methodsRef.current;
      const scroller = methods?.scrollerElement?.() ?? null;
      const domAnchors = pickAnchorIdsFromScroller(scroller);
      if (domAnchors.topId || domAnchors.anchorId) {
        renderedTopIdRef.current = domAnchors.topId;
        renderedAnchorIdRef.current = domAnchors.anchorId;
        return;
      }
      const rangeAnchors = pickAnchorIdsFromRange(range);
      renderedTopIdRef.current = rangeAnchors.topId;
      renderedAnchorIdRef.current = rangeAnchors.anchorId;
    },
    [methodsRef, renderedAnchorIdRef, renderedTopIdRef],
  );

  return { onScroll, onRenderedDataChange };
}
