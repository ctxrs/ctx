import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MutableRefObject } from "react";
import type {
  AutoscrollToBottom,
  ItemLocation,
  ListScrollLocation,
  VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import { useRafCoalesced } from "../components/hooks/useRafCoalesced";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import { useSessionMessageListReconcileEffect } from "./useSessionMessageListReconcileEffect";
import { useSessionMessageListDiagnostics } from "./useSessionMessageListDiagnostics";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import {
  computeHistoryPrefetchThresholdPx,
  pickAnchorIdsFromRange,
  pickAnchorIdsFromScroller,
  shouldUseRawListItems,
} from "./sessionMessageListControllerUtils";

type Params = {
  sessionId: string;
  isActive: boolean;
  loaded: boolean;
  listItems: WorkbenchListItem[];
  canLoadOlder: boolean;
  loadOlder: () => Promise<void>;
  layoutRevision: string;
  itemSizeCacheKey: (item: WorkbenchListItem) => string | null;
  renderRevisionByItemId?: Readonly<Record<string, number>>;
  threadOp?: WorkbenchThreadProjectionOp | null;
  showDebug: boolean;
  onAtBottomChange?: (atBottom: boolean) => void;
};

type Result = {
  methodsRef: MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  context: WorkbenchMessageListContext;
  initialData: WorkbenchListItem[];
  initialLocation: ItemLocation;
  onScroll: (location: ListScrollLocation) => void;
  onRenderedDataChange: (range: WorkbenchListItem[]) => void;
};

const INITIAL_LOCATION_BOTTOM: ItemLocation = { index: "LAST", align: "end" };
const USER_SCROLL_INTENT_HOLD_MS = 600;

export function useSessionMessageListController(params: Params): Result {
  const {
    sessionId,
    isActive,
    loaded,
    listItems,
    canLoadOlder,
    loadOlder,
    layoutRevision,
    itemSizeCacheKey,
    renderRevisionByItemId,
    threadOp,
    showDebug,
    onAtBottomChange,
  } = params;

  const methodsRef = useRef<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>(null);
  const lastSessionIdRef = useRef(sessionId);
  const lastIsActiveRef = useRef(isActive);
  const contractViolationLoggedRef = useRef<{ sessionId: string; violationKey: string } | null>(null);
  const wheelIntentScrollerRef = useRef<HTMLElement | null>(null);
  const releaseBottomLockUntilRef = useRef(0);

  const lastScrollLocationRef = useRef<ListScrollLocation | null>(null);
  const stickToBottomRef = useRef(true);
  const lastAtBottomRef = useRef<boolean | null>(null);
  const lastListOffsetRef = useRef<number | null>(null);

  // Best-effort anchoring prefers the actually visible DOM rows and falls back to rendered data.
  // NOTE: `onRenderedDataChange` can include overscan. Anchoring to `range[0]` can anchor an offscreen
  // row and cause visible jumps, especially with large `increaseViewportBy`.
  const renderedAnchorIdRef = useRef<string | null>(null);
  const renderedTopIdRef = useRef<string | null>(null);
  const firstListItemIdRef = useRef<string | null>(null);

  const pendingHistoryRef = useRef(false);
  const historyExpectedRef = useRef(false);
  const historyRequestedAtTopRef = useRef(false);
  const historyRequestedAnchorIdRef = useRef<string | null>(null);
  const lastLayoutRevisionRef = useRef(layoutRevision);
  const reconcileEpochRef = useRef(0);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [deferTrailingAppends, setDeferTrailingAppends] = useState(false);
  const suppressIdDiffLogsRef = useRef<{ sessionId: string; remainingTicks: number } | null>(null);
  const lastScrollDebugAtRef = useRef(0);

  // Coalesce only for steady-state updates. Session boundaries must bypass coalescing,
  // otherwise the controller can reconcile against a stale coalesced list while the
  // upstream props have already moved to the new session transcript.
  const listItemsCoalesced = useRafCoalesced(listItems);
  const sessionBoundaryActive = lastSessionIdRef.current !== sessionId;

  const context = useMemo(
    () => ({
      loaded,
      loadingOlder,
      renderRevision: layoutRevision,
      renderRevisionByItemId,
    }),
    [layoutRevision, loaded, loadingOlder, renderRevisionByItemId],
  );
  const { recordDebugSnapshot, startFlashProbe } = useSessionMessageListDiagnostics({
    sessionId,
    isActive,
    loaded,
    listItemsLength: listItems.length,
    showDebug,
    methodsRef,
    lastAtBottomRef,
    renderedAnchorIdRef,
    renderedTopIdRef,
  });
  const logMessageListDebug = useCallback(
    (label: string, detail: Record<string, unknown>) => {
      if (!showDebug) return;
      // eslint-disable-next-line no-console
      console.log(`[MessageList][${label}] ${JSON.stringify({ sessionId, ...detail })}`);
    },
    [sessionId, showDebug],
  );
  const snapToBottom = useCallback(
    (methods: VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>) => {
      const applyBottom = () => {
        methods.scrollToItem({ index: "LAST", align: "end", behavior: "auto" });
        const scroller = methods.scrollerElement?.() ?? null;
        if (!scroller) return null;
        const maxScrollTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
        scroller.scrollTop = maxScrollTop;
        return {
          maxScrollTop,
          distanceFromMax: Math.max(0, maxScrollTop - scroller.scrollTop),
        };
      };
      const scroller = methods.scrollerElement?.() ?? null;
      let frame = 0;
      let stableFrames = 0;
      let lastMaxScrollTop = -1;
      let rafId = 0;

      const settle = () => {
        const result = applyBottom();
        if (!result) {
          return;
        }
        const { maxScrollTop, distanceFromMax } = result;
        if (distanceFromMax <= 1 && maxScrollTop === lastMaxScrollTop) {
          stableFrames += 1;
        } else {
          stableFrames = 0;
        }
        lastMaxScrollTop = maxScrollTop;
        frame += 1;
        if (stableFrames >= 2 || frame >= 8) {
          return;
        }
        rafId = requestAnimationFrame(settle);
      };

      settle();
    },
    [],
  );

  const releaseBottomLock = useCallback(() => {
    releaseBottomLockUntilRef.current = Date.now() + USER_SCROLL_INTENT_HOLD_MS;
    stickToBottomRef.current = false;
    if (onAtBottomChange && lastAtBottomRef.current !== false) {
      lastAtBottomRef.current = false;
      onAtBottomChange(false);
    }
  }, [onAtBottomChange]);

  const releaseBottomLockOnWheel = useCallback(
    (event: WheelEvent) => {
      if (event.deltaY >= 0) return;
      const scroller = methodsRef.current?.scrollerElement?.() ?? null;
      if (!scroller) return;
      const maxScrollTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
      if (maxScrollTop <= 0) return;
      const distanceFromBottom = Math.max(0, maxScrollTop - scroller.scrollTop);
      if (distanceFromBottom > 16) return;
      releaseBottomLock();
    },
    [releaseBottomLock],
  );

  const appendBehavior = useMemo<AutoscrollToBottom<WorkbenchListItem, WorkbenchMessageListContext>>(
    () => (params) => (params.atBottom ? "auto" : false),
    [],
  );
  const useRawListItems =
    sessionBoundaryActive ||
    shouldUseRawListItems({
      stickToBottom: stickToBottomRef.current,
      pendingHistory: pendingHistoryRef.current,
      loadingOlder,
    });
  const visibleListItems = useRawListItems ? listItems : listItemsCoalesced;

  const initialLocation: ItemLocation = INITIAL_LOCATION_BOTTOM;
  const initialData = useMemo<WorkbenchListItem[]>(() => visibleListItems, [visibleListItems]);

  // Keep an up-to-date reference without introducing additional hook ordering churn under HMR.
  firstListItemIdRef.current = visibleListItems?.[0]?.id ?? null;

  useEffect(() => {
    const previousScroller = wheelIntentScrollerRef.current;
    if (previousScroller) {
      previousScroller.removeEventListener("wheel", releaseBottomLockOnWheel);
      wheelIntentScrollerRef.current = null;
    }
    if (!isActive) return;
    const scroller = methodsRef.current?.scrollerElement?.() ?? null;
    if (!scroller) return;
    wheelIntentScrollerRef.current = scroller;
    scroller.addEventListener("wheel", releaseBottomLockOnWheel, { passive: true });
    return () => {
      scroller.removeEventListener("wheel", releaseBottomLockOnWheel);
      if (wheelIntentScrollerRef.current === scroller) {
        wheelIntentScrollerRef.current = null;
      }
    };
  });

  const onScroll = useCallback(
    (location: ListScrollLocation) => {
      lastScrollLocationRef.current = location;
      if (!isActive) return;

      const scroller = methodsRef.current?.scrollerElement?.() ?? null;
      const atBottomFromLocation = location.bottomOffset <= 16;
      // Prefer the live DOM scroller metrics when available; bottomOffset can be optimistic mid-transition.
      const atBottom =
        scroller
          ? scroller.scrollHeight - (scroller.scrollTop + scroller.clientHeight) <= 16
          : atBottomFromLocation;
      const holdBottomLockRelease = Date.now() < releaseBottomLockUntilRef.current;
      const effectiveAtBottom = holdBottomLockRelease ? false : atBottom;
      stickToBottomRef.current = effectiveAtBottom;
      if (effectiveAtBottom && deferTrailingAppends) {
        setDeferTrailingAppends(false);
      }
      if (isActive && onAtBottomChange && lastAtBottomRef.current !== effectiveAtBottom) {
        lastAtBottomRef.current = effectiveAtBottom;
        onAtBottomChange(effectiveAtBottom);
      }

      // History pagination trigger: use the library-provided scroll location only.
      // Prefetch when approaching top to avoid a hard stop + later prepend “resume”.
      const atTop = location.listOffset === 0;
      const prevOffset = lastListOffsetRef.current;
      lastListOffsetRef.current = location.listOffset;
      // Scrolling up means listOffset moves toward 0 (increases, since it's negative when scrolled down).
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
            atBottom: effectiveAtBottom,
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
      loaded,
      loadOlder,
      loadingOlder,
      onAtBottomChange,
      sessionId,
      showDebug,
      isActive,
      deferTrailingAppends,
    ],
  );

  const onRenderedDataChange = useCallback((range: WorkbenchListItem[]) => {
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
  }, []);

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
  }, [isActive, onAtBottomChange, recordDebugSnapshot, snapToBottom]);

  useSessionMessageListReconcileEffect({
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
  });

  return {
    methodsRef,
    context,
    initialData,
    initialLocation,
    onScroll,
    onRenderedDataChange,
  };
}
