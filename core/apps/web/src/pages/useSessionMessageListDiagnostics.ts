import { useCallback, useEffect, useRef, type MutableRefObject } from "react";
import {
  measureSessionMessageListState,
  recordSessionMessageListDebugSnapshot,
  recordSessionMessageListFlashTrace,
  type SessionMessageListFlashSample,
} from "./sessionMessageListDebug";

type ThreadScrollerMethods = {
  scrollerElement: () => HTMLElement | null;
};

type Params = {
  sessionId: string;
  isActive: boolean;
  loaded: boolean;
  listItemsLength: number;
  showDebug: boolean;
  methodsRef: MutableRefObject<ThreadScrollerMethods | null>;
  lastAtBottomRef: MutableRefObject<boolean | null>;
  renderedAnchorIdRef: MutableRefObject<string | null>;
  renderedTopIdRef: MutableRefObject<string | null>;
};

export function useSessionMessageListDiagnostics({
  sessionId,
  isActive,
  loaded,
  listItemsLength,
  showDebug,
  methodsRef,
  lastAtBottomRef,
  renderedAnchorIdRef,
  renderedTopIdRef,
}: Params) {
  const debugLoggedSessionRef = useRef<string | null>(null);
  const flashProbeCleanupRef = useRef<(() => void) | null>(null);

  const recordDebugSnapshot = useCallback(
    (cause: string, detail?: Record<string, unknown> | null) => {
      if (!showDebug) return;
      recordSessionMessageListDebugSnapshot({
        sessionId,
        cause,
        scroller: methodsRef.current?.scrollerElement?.() ?? null,
        isActive,
        loaded,
        listCount: listItemsLength,
        stickToBottom: lastAtBottomRef.current,
        renderedAnchorId: renderedAnchorIdRef.current,
        renderedTopId: renderedTopIdRef.current,
        detail: detail ?? null,
      });
    },
    [
      isActive,
      lastAtBottomRef,
      listItemsLength,
      loaded,
      methodsRef,
      renderedAnchorIdRef,
      renderedTopIdRef,
      sessionId,
      showDebug,
    ],
  );

  const startFlashProbe = useCallback(
    (cause: string, detail?: Record<string, unknown> | null) => {
      if (!showDebug || !isActive) return;
      if (typeof window === "undefined") return;
      const scroller = methodsRef.current?.scrollerElement?.() ?? null;
      if (!scroller) return;

      flashProbeCleanupRef.current?.();

      const startedAtMs = Date.now();
      const startedPerf = performance.now();
      const samples: SessionMessageListFlashSample[] = [];
      const maxFrames = 18;
      const minFrames = 6;
      const maxDurationMs = 320;
      let rafId: number | null = null;
      let finished = false;
      let stableFrames = 0;
      let previousSignature = "";
      let layoutShiftCount = 0;
      let layoutShiftValue = 0;
      let layoutShiftObserver: PerformanceObserver | null = null;

      const sample = () => {
        const metrics = measureSessionMessageListState(scroller);
        samples.push({
          atMs: Math.round((performance.now() - startedPerf) * 100) / 100,
          scrollTop: metrics.scrollTop,
          clientHeight: metrics.clientHeight,
          scrollHeight: metrics.scrollHeight,
          maxScrollTop: metrics.maxScrollTop,
          distanceFromMaxScrollPx: metrics.distanceFromMaxScrollPx,
          blankTailPx: metrics.blankTailPx,
          firstItemId: metrics.firstItemId,
          firstItemTopPx: metrics.firstItemTopPx,
          firstItemBottomPx: metrics.firstItemBottomPx,
          lastItemId: metrics.lastItemId,
          lastItemTopPx: metrics.lastItemTopPx,
          lastItemBottomPx: metrics.lastItemBottomPx,
          renderedTopId: renderedTopIdRef.current,
          renderedAnchorId: renderedAnchorIdRef.current,
        });

        const signature = [
          metrics.scrollTop ?? "null",
          metrics.scrollHeight ?? "null",
          metrics.clientHeight ?? "null",
          metrics.firstItemId ?? "null",
          metrics.firstItemTopPx ?? "null",
          renderedTopIdRef.current ?? "null",
          renderedAnchorIdRef.current ?? "null",
        ].join("|");
        if (signature === previousSignature) stableFrames += 1;
        else stableFrames = 0;
        previousSignature = signature;
      };

      const finish = () => {
        if (finished) return;
        finished = true;
        if (rafId != null) cancelAnimationFrame(rafId);
        layoutShiftObserver?.disconnect();
        flashProbeCleanupRef.current = null;
        const trace = recordSessionMessageListFlashTrace({
          sessionId,
          cause,
          startedAtMs,
          finishedAtMs: Date.now(),
          samples,
          layoutShiftCount,
          layoutShiftValue,
          detail: detail ?? null,
        });
        if (trace?.snapbackDetected) {
          // eslint-disable-next-line no-console
          console.warn("[MessageList][flash-detected]", {
            sessionId,
            cause,
            sampleCount: trace.sampleCount,
            maxAbsScrollTopDeltaPx: trace.maxAbsScrollTopDeltaPx,
            maxAbsFirstItemTopDeltaPx: trace.maxAbsFirstItemTopDeltaPx,
            maxAbsScrollHeightDeltaPx: trace.maxAbsScrollHeightDeltaPx,
            layoutShiftCount: trace.layoutShiftCount,
            layoutShiftValue: trace.layoutShiftValue,
          });
        }
      };

      if (typeof PerformanceObserver !== "undefined") {
        try {
          layoutShiftObserver = new PerformanceObserver((list) => {
            for (const entry of list.getEntries()) {
              if (entry.entryType !== "layout-shift") continue;
              if (entry.startTime < startedPerf) continue;
              const value = Number((entry as PerformanceEntry & { value?: number }).value ?? 0);
              layoutShiftCount += 1;
              layoutShiftValue += Number.isFinite(value) ? value : 0;
            }
          });
          layoutShiftObserver.observe({ type: "layout-shift", buffered: false } as PerformanceObserverInit);
        } catch {
          layoutShiftObserver = null;
        }
      }

      sample();
      const tick = () => {
        if (finished) return;
        sample();
        const elapsedMs = performance.now() - startedPerf;
        const settled = samples.length >= minFrames && stableFrames >= 3;
        const expired = samples.length >= maxFrames || elapsedMs >= maxDurationMs;
        if (settled || expired) {
          finish();
          return;
        }
        rafId = requestAnimationFrame(tick);
      };
      rafId = requestAnimationFrame(tick);
      flashProbeCleanupRef.current = finish;
    },
    [isActive, methodsRef, renderedAnchorIdRef, renderedTopIdRef, sessionId, showDebug],
  );

  useEffect(() => {
    if (!showDebug) return;
    if (debugLoggedSessionRef.current === sessionId) return;
    debugLoggedSessionRef.current = sessionId;
    // eslint-disable-next-line no-console
    console.debug("[MessageList][debug]", { sessionId, isActive, loaded });
  }, [isActive, loaded, sessionId, showDebug]);

  useEffect(() => {
    recordDebugSnapshot("session:init");
  }, [recordDebugSnapshot, sessionId]);

  useEffect(() => {
    if (!showDebug || !isActive) return;
    let cancelled = false;
    let rafId: number | null = null;
    let cleanup: (() => void) | null = null;

    const attach = () => {
      if (cancelled) return;
      const scroller = methodsRef.current?.scrollerElement?.() ?? null;
      if (!scroller) {
        rafId = requestAnimationFrame(attach);
        return;
      }
      const sessionView = scroller.closest(".wb-session-view") as HTMLElement | null;
      const sessionBottom = sessionView?.querySelector(".wb-session-bottom") as HTMLElement | null;
      const composer = sessionView?.querySelector(".wb-active-composer") as HTMLElement | null;
      const queuePanel = sessionView?.querySelector(".queue-panel") as HTMLElement | null;
      const observer = new ResizeObserver((entries) => {
        for (const entry of entries) {
          const target = entry.target;
          const targetLabel =
            target === scroller
              ? "scroller"
              : target === sessionBottom
                ? "session-bottom"
                : target === composer
                  ? "composer"
                  : target === queuePanel
                    ? "queue-panel"
                    : "unknown";
          recordDebugSnapshot(`resize:${targetLabel}`, {
            width: Math.round(entry.contentRect.width * 100) / 100,
            height: Math.round(entry.contentRect.height * 100) / 100,
          });
        }
      });
      observer.observe(scroller);
      if (sessionBottom) observer.observe(sessionBottom);
      if (composer) observer.observe(composer);
      if (queuePanel) observer.observe(queuePanel);
      recordDebugSnapshot("debug:attach");
      cleanup = () => observer.disconnect();
    };

    attach();
    return () => {
      cancelled = true;
      if (rafId != null) cancelAnimationFrame(rafId);
      recordDebugSnapshot("debug:detach");
      cleanup?.();
    };
  }, [isActive, methodsRef, recordDebugSnapshot, sessionId, showDebug]);

  useEffect(
    () => () => {
      flashProbeCleanupRef.current?.();
    },
    [],
  );

  return { recordDebugSnapshot, startFlashProbe };
}
