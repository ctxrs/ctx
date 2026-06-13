import {
  type EventTimingEntryLike,
  type FirstInputEntryLike,
  type LargestContentfulPaintEntryLike,
  type LayoutShiftEntryLike,
  type PerformanceObserverWithSupportedEntryTypes,
  type WalMode,
  type WalRecorder,
} from "./walRecorderShared";

export const initWalRecorderPerformanceObservers = (recorder: WalRecorder, getMode: () => WalMode) => {
  if (typeof performance === "undefined" || typeof PerformanceObserver === "undefined") return;
  const supported = (PerformanceObserver as PerformanceObserverWithSupportedEntryTypes).supportedEntryTypes;
  const supports = new Set(supported ?? []);
  const timeOrigin = performance.timeOrigin ?? Date.now();

  if (supports.has("navigation")) {
    const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
    if (nav) {
      recorder.record("perf:navigation", {
        type: nav.type,
        dom_interactive: Math.round(nav.domInteractive),
        dom_content_loaded: Math.round(nav.domContentLoadedEventEnd),
        load_event_end: Math.round(nav.loadEventEnd),
        redirect_count: nav.redirectCount,
        transfer_size: nav.transferSize,
      });
    }
  }

  if (supports.has("paint")) {
    const paints = performance.getEntriesByType("paint") as PerformanceEntry[];
    for (const entry of paints) {
      recorder.record("perf:paint", {
        name: entry.name,
        start_ms: Math.round(timeOrigin + entry.startTime),
      });
    }
  }

  if (supports.has("longtask")) {
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        recorder.record("perf:longtask", {
          start_ms: Math.round(timeOrigin + entry.startTime),
          duration_ms: Math.round(entry.duration ?? 0),
        });
      }
    });
    try {
      observer.observe({ entryTypes: ["longtask"] });
    } catch {
      // ignore
    }
  }

  let clsTotal = 0;
  let lcpEntry: PerformanceEntry | null = null;

  if (supports.has("layout-shift")) {
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as unknown as LayoutShiftEntryLike[]) {
        if (!entry) continue;
        const value = typeof entry.value === "number" ? entry.value : 0;
        const hadRecentInput = Boolean(entry.hadRecentInput);
        if (!hadRecentInput) clsTotal += value;
        recorder.record("perf:layout-shift", {
          value,
          had_recent_input: hadRecentInput,
          cls_total: Number(clsTotal.toFixed(4)),
        });
      }
    });
    try {
      observer.observe({ entryTypes: ["layout-shift"] });
    } catch {
      // ignore
    }
  }

  if (supports.has("largest-contentful-paint")) {
    const observer = new PerformanceObserver((list) => {
      const entries = list.getEntries();
      if (!entries.length) return;
      lcpEntry = entries[entries.length - 1];
      const entry = lcpEntry as LargestContentfulPaintEntryLike;
      const mode = getMode();
      recorder.record(
        "perf:lcp",
        {
          start_ms: Math.round(timeOrigin + entry.startTime),
          size: entry.size,
          element: mode === "heavy" && entry.element ? entry.element.tagName : undefined,
          url: mode === "heavy" ? entry.url : undefined,
        },
        { level: mode === "heavy" ? "heavy" : "light" },
      );
    });
    try {
      observer.observe({ type: "largest-contentful-paint", buffered: true } as unknown as PerformanceObserverInit);
    } catch {
      // ignore
    }
  }

  if (supports.has("first-input")) {
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as unknown as FirstInputEntryLike[]) {
        recorder.record("perf:first-input", {
          name: entry.name,
          start_ms: Math.round(timeOrigin + entry.startTime),
          duration_ms: Math.round(entry.duration ?? 0),
          processing_start: entry.processingStart ?? undefined,
        });
      }
    });
    try {
      observer.observe({ entryTypes: ["first-input"] });
    } catch {
      // ignore
    }
  }

  if (supports.has("event")) {
    const observer = new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as unknown as EventTimingEntryLike[]) {
        if (!entry) continue;
        recorder.record(
          "perf:event",
          {
            name: entry.name,
            start_ms: Math.round(timeOrigin + entry.startTime),
            duration_ms: Math.round(entry.duration ?? 0),
            interaction_id: entry.interactionId ?? undefined,
          },
          { level: "heavy" },
        );
      }
    });
    try {
      observer.observe(
        { type: "event", buffered: true, durationThreshold: 40 } as unknown as PerformanceObserverInit,
      );
    } catch {
      // ignore
    }
  }

  if (typeof window !== "undefined") {
    window.addEventListener("visibilitychange", () => {
      if (document.visibilityState !== "hidden") return;
      if (lcpEntry) {
        recorder.record("perf:lcp:final", {
          start_ms: Math.round(timeOrigin + lcpEntry.startTime),
        });
      }
      recorder.record("perf:cls:final", { cls_total: Number(clsTotal.toFixed(4)) });
    });
  }
};
