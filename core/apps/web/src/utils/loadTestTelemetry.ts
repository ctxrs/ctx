export type LoadTestTelemetrySnapshot = {
  session_switches: Array<{
    from_session_id: string | null;
    to_session_id: string | null;
    started_at_ms: number;
    completed_at_ms?: number;
    duration_ms?: number;
    status: "completed" | "abandoned";
  }>;
  visible_session_switches: VisibleSessionSwitchRecord[];
  long_tasks: Array<{
    start_ms: number;
    duration_ms: number;
  }>;
  event_loop_gaps: Array<{
    at_ms: number;
    gap_ms: number;
    document_hidden?: boolean;
    visibility_state?: string;
    time_since_last_raf_ms?: number;
    timer_throttled_suspected?: boolean;
  }>;
  raf_gaps: Array<{
    at_ms: number;
    gap_ms: number;
  }>;
  worker_apply: Array<{
    at_ms: number;
    duration_ms: number;
    batch_size?: number;
  }>;
  patch_latency: Array<{
    at_ms: number;
    duration_ms: number;
    patch_size?: number;
  }>;
  memory_samples: Array<{
    at_ms: number;
    used_js_heap_size?: number;
    total_js_heap_size?: number;
    js_heap_size_limit?: number;
  }>;
  meta: {
    time_origin_ms: number;
    user_agent: string;
  };
};

type PendingSessionSwitch = {
  from_session_id: string | null;
  to_session_id: string | null;
  started_at_ms: number;
};

export type VisibleSessionSwitchRecord = {
  from_session_id: string | null;
  to_session_id: string | null;
  task_id?: string;
  target_index?: number;
  source?: "pointer" | "keyboard" | "programmatic";
  started_at_ms: number;
  visible_at_ms?: number;
  stable_at_ms?: number;
  click_to_visible_ms?: number;
  click_to_stable_ms?: number;
  subscribed_at_click?: boolean;
  authoritative_at_click?: boolean;
  subscribed_when_active?: boolean;
  authoritative_when_active?: boolean;
  http_rehydrate_seen?: boolean;
  status: "pending" | "visible" | "stable" | "abandoned";
};

type PendingVisibleSessionSwitch = Omit<VisibleSessionSwitchRecord, "status"> & {
  status: "pending" | "visible";
};

type LoadTestTelemetry = {
  enabled: boolean;
  startSessionSwitch: (fromSessionId: string | null, toSessionId: string | null) => void;
  finishSessionSwitch: (toSessionId: string | null) => void;
  startVisibleSessionSwitch: (opts: {
    fromSessionId: string | null;
    toSessionId: string | null;
    taskId?: string;
    targetIndex?: number;
    source?: VisibleSessionSwitchRecord["source"];
    subscribedAtClick?: boolean;
    authoritativeAtClick?: boolean;
  }) => void;
  updateVisibleSessionSwitchState: (
    toSessionId: string,
    state: {
      subscribedWhenActive?: boolean;
      authoritativeWhenActive?: boolean;
      httpRehydrateSeen?: boolean;
    },
  ) => void;
  markVisibleSessionSwitchVisible: (toSessionId: string) => void;
  markVisibleSessionSwitchStable: (toSessionId: string) => void;
  recordWorkerApply: (durationMs: number, opts?: { batchSize?: number }) => void;
  recordPatchLatency: (durationMs: number, opts?: { patchSize?: number }) => void;
  getSnapshot: () => LoadTestTelemetrySnapshot;
  getSummary: () => LoadTestTelemetrySummary;
  reset: () => void;
  stop: () => void;
};

type LoadTestTelemetrySummary = {
  session_switch_ms: PercentileSummary;
  visible_switch_ms: PercentileSummary;
  stable_switch_ms: PercentileSummary;
  long_task_ms: PercentileSummary;
  event_loop_gap_ms: PercentileSummary;
  event_loop_gap_unthrottled_ms: PercentileSummary;
  raf_gap_ms: PercentileSummary;
  worker_apply_ms: PercentileSummary;
  patch_latency_ms: PercentileSummary;
};

type PercentileSummary = {
  count: number;
  p50?: number;
  p95?: number;
  p99?: number;
};

const MAX_ENTRIES = 2000;
const MEMORY_SAMPLE_MS = 2000;
const HEARTBEAT_INTERVAL_MS = 50;
const HEARTBEAT_GAP_RECORD_THRESHOLD_MS = 50;
const RAF_GAP_RECORD_THRESHOLD_MS = 50;
const TIMER_THROTTLE_GAP_THRESHOLD_MS = 500;
const TIMER_THROTTLE_RECENT_RAF_THRESHOLD_MS = 250;

let telemetry: LoadTestTelemetry | null = null;

type WindowWithLoadTest = Window & {
  __CTX_LOAD_TEST__?: unknown;
  __ctxLoadTestTelemetry?: {
    enabled: boolean;
    getSnapshot: () => LoadTestTelemetrySnapshot;
    getSummary: () => LoadTestTelemetrySummary;
    reset: () => void;
    stop: () => void;
  };
};

const summarizePercentiles = (values: number[]): PercentileSummary => {
  if (values.length === 0) return { count: 0 };
  const sorted = values.slice().sort((a, b) => a - b);
  const pick = (p: number) => {
    const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1));
    return sorted[idx];
  };
  const round = (v: number) => Math.round(v * 10) / 10;
  return {
    count: sorted.length,
    p50: round(pick(0.5)),
    p95: round(pick(0.95)),
    p99: round(pick(0.99)),
  };
};

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

export function shouldClassifyEventLoopGapAsTimerThrottle(params: {
  gapMs: number;
  documentHidden?: boolean;
  visibilityState?: string;
  timeSinceLastRafMs?: number;
}): boolean {
  if (params.documentHidden === true) {
    return true;
  }
  if (typeof params.visibilityState === "string" && params.visibilityState !== "visible") {
    return true;
  }
  return (
    params.gapMs >= TIMER_THROTTLE_GAP_THRESHOLD_MS &&
    typeof params.timeSinceLastRafMs === "number" &&
    params.timeSinceLastRafMs <= TIMER_THROTTLE_RECENT_RAF_THRESHOLD_MS
  );
}

const shouldEnable = (): boolean => {
  if (typeof window === "undefined") return false;
  const query = new URLSearchParams(window.location.search);
  const queryEnabled = query.get("loadtest") === "1";
  const flagEnabled = Boolean((window as WindowWithLoadTest).__CTX_LOAD_TEST__);
  return Boolean(import.meta.env.DEV || import.meta.env.MODE === "test" || queryEnabled || flagEnabled);
};

export const initLoadTestTelemetry = (): LoadTestTelemetry | null => {
  if (telemetry) return telemetry;
  if (!shouldEnable()) return null;

  const session_switches: LoadTestTelemetrySnapshot["session_switches"] = [];
  const visible_session_switches: LoadTestTelemetrySnapshot["visible_session_switches"] = [];
  const long_tasks: LoadTestTelemetrySnapshot["long_tasks"] = [];
  const event_loop_gaps: LoadTestTelemetrySnapshot["event_loop_gaps"] = [];
  const raf_gaps: LoadTestTelemetrySnapshot["raf_gaps"] = [];
  const worker_apply: LoadTestTelemetrySnapshot["worker_apply"] = [];
  const patch_latency: LoadTestTelemetrySnapshot["patch_latency"] = [];
  const memory_samples: LoadTestTelemetrySnapshot["memory_samples"] = [];
  const meta: LoadTestTelemetrySnapshot["meta"] = {
    time_origin_ms:
      typeof performance !== "undefined" && typeof performance.timeOrigin === "number"
        ? performance.timeOrigin
        : Date.now(),
    user_agent: typeof navigator !== "undefined" ? navigator.userAgent : "unknown",
  };

  let pending: PendingSessionSwitch | null = null;
  let pendingVisible: PendingVisibleSessionSwitch | null = null;
  let observer: PerformanceObserver | null = null;
  let memoryTimer: number | null = null;
  let heartbeatTimer: number | null = null;
  let rafHandle: number | null = null;
  let lastHeartbeatAtMs = nowMs();
  let lastRafAtMs = typeof performance !== "undefined" ? performance.now() : 0;

  const pushWithLimit = <T>(list: T[], entry: T) => {
    if (list.length >= MAX_ENTRIES) list.shift();
    list.push(entry);
  };

  const recordLongTask = (entry: PerformanceEntry) => {
    const start_ms = meta.time_origin_ms + (entry.startTime ?? 0);
    const duration_ms = entry.duration ?? 0;
    pushWithLimit(long_tasks, { start_ms, duration_ms });
  };

  const pushPendingVisible = (status: VisibleSessionSwitchRecord["status"]) => {
    if (!pendingVisible) return;
    pushWithLimit(visible_session_switches, { ...pendingVisible, status });
    pendingVisible = null;
  };

  const sampleMemory = () => {
    if (typeof performance === "undefined") return;
    const mem = (performance as Performance & {
      memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number };
    }).memory;
    if (!mem || typeof mem.usedJSHeapSize !== "number") return;
    pushWithLimit(memory_samples, {
      at_ms: nowMs(),
      used_js_heap_size: mem.usedJSHeapSize,
      total_js_heap_size: mem.totalJSHeapSize,
      js_heap_size_limit: mem.jsHeapSizeLimit,
    });
  };

  const sampleEventLoopHeartbeat = () => {
    const currentMs = nowMs();
    const expectedMs = lastHeartbeatAtMs + HEARTBEAT_INTERVAL_MS;
    const gapMs = currentMs - expectedMs;
    lastHeartbeatAtMs = currentMs;
    if (gapMs >= HEARTBEAT_GAP_RECORD_THRESHOLD_MS) {
      const currentPerfNow =
        typeof performance !== "undefined" && typeof performance.now === "function"
          ? performance.now()
          : undefined;
      const timeSinceLastRafMs =
        typeof currentPerfNow === "number" && Number.isFinite(lastRafAtMs)
          ? Math.max(0, currentPerfNow - lastRafAtMs)
          : undefined;
      const visibilityState =
        typeof document !== "undefined" && typeof document.visibilityState === "string"
          ? document.visibilityState
          : undefined;
      const documentHidden =
        typeof document !== "undefined" ? Boolean(document.hidden) : undefined;
      pushWithLimit(event_loop_gaps, {
        at_ms: currentMs,
        gap_ms: gapMs,
        document_hidden: documentHidden,
        visibility_state: visibilityState,
        time_since_last_raf_ms: timeSinceLastRafMs,
        timer_throttled_suspected: shouldClassifyEventLoopGapAsTimerThrottle({
          gapMs,
          documentHidden,
          visibilityState,
          timeSinceLastRafMs,
        }),
      });
    }
  };

  const sampleRafHeartbeat = (timestamp: number) => {
    const gapMs = timestamp - lastRafAtMs;
    lastRafAtMs = timestamp;
    if (gapMs >= RAF_GAP_RECORD_THRESHOLD_MS) {
      pushWithLimit(raf_gaps, {
        at_ms: meta.time_origin_ms + timestamp,
        gap_ms: gapMs,
      });
    }
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      rafHandle = window.requestAnimationFrame(sampleRafHeartbeat);
    }
  };

  if (typeof PerformanceObserver !== "undefined") {
    const types = (PerformanceObserver as typeof PerformanceObserver & { supportedEntryTypes?: string[] })
      .supportedEntryTypes;
    if (types?.includes("longtask")) {
      observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          recordLongTask(entry);
        }
      });
      try {
        observer.observe({ entryTypes: ["longtask"] });
      } catch {
        observer = null;
      }
    }
  }

  sampleMemory();
  if (typeof window !== "undefined" && typeof window.setInterval === "function") {
    memoryTimer = window.setInterval(sampleMemory, MEMORY_SAMPLE_MS);
    heartbeatTimer = window.setInterval(sampleEventLoopHeartbeat, HEARTBEAT_INTERVAL_MS);
  }
  if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
    rafHandle = window.requestAnimationFrame(sampleRafHeartbeat);
  }

  telemetry = {
    enabled: true,
    startSessionSwitch: (fromSessionId, toSessionId) => {
      if (pending) {
        pushWithLimit(session_switches, { ...pending, status: "abandoned" });
      }
      pending = {
        from_session_id: fromSessionId,
        to_session_id: toSessionId,
        started_at_ms: nowMs(),
      };
    },
    finishSessionSwitch: (toSessionId) => {
      if (!pending) return;
      if (pending.to_session_id && toSessionId && pending.to_session_id !== toSessionId) return;
      const completed_at_ms = nowMs();
      const duration_ms = completed_at_ms - pending.started_at_ms;
      pushWithLimit(session_switches, {
        ...pending,
        completed_at_ms,
        duration_ms,
        status: "completed",
      });
      pending = null;
    },
    startVisibleSessionSwitch: (opts) => {
      pushPendingVisible("abandoned");
      pendingVisible = {
        from_session_id: opts.fromSessionId,
        to_session_id: opts.toSessionId,
        task_id: opts.taskId,
        target_index: opts.targetIndex,
        source: opts.source,
        started_at_ms: nowMs(),
        subscribed_at_click: opts.subscribedAtClick,
        authoritative_at_click: opts.authoritativeAtClick,
        status: "pending",
      };
    },
    updateVisibleSessionSwitchState: (toSessionId, state) => {
      if (!pendingVisible) return;
      if (pendingVisible.to_session_id && pendingVisible.to_session_id !== toSessionId) return;
      if (typeof state.subscribedWhenActive === "boolean") {
        pendingVisible.subscribed_when_active = state.subscribedWhenActive;
      }
      if (typeof state.authoritativeWhenActive === "boolean") {
        pendingVisible.authoritative_when_active = state.authoritativeWhenActive;
      }
      if (typeof state.httpRehydrateSeen === "boolean") {
        pendingVisible.http_rehydrate_seen =
          Boolean(pendingVisible.http_rehydrate_seen) || state.httpRehydrateSeen;
      }
    },
    markVisibleSessionSwitchVisible: (toSessionId) => {
      if (!pendingVisible) return;
      if (pendingVisible.to_session_id && pendingVisible.to_session_id !== toSessionId) return;
      if (pendingVisible.visible_at_ms !== undefined) return;
      const visibleAtMs = nowMs();
      pendingVisible.visible_at_ms = visibleAtMs;
      pendingVisible.click_to_visible_ms = visibleAtMs - pendingVisible.started_at_ms;
      pendingVisible.status = "visible";
    },
    markVisibleSessionSwitchStable: (toSessionId) => {
      if (!pendingVisible) return;
      if (pendingVisible.to_session_id && pendingVisible.to_session_id !== toSessionId) return;
      const stableAtMs = nowMs();
      if (pendingVisible.visible_at_ms === undefined) {
        pendingVisible.visible_at_ms = stableAtMs;
        pendingVisible.click_to_visible_ms = stableAtMs - pendingVisible.started_at_ms;
      }
      pendingVisible.stable_at_ms = stableAtMs;
      pendingVisible.click_to_stable_ms = stableAtMs - pendingVisible.started_at_ms;
      pushPendingVisible("stable");
    },
    recordWorkerApply: (durationMs, opts) => {
      pushWithLimit(worker_apply, {
        at_ms: nowMs(),
        duration_ms: durationMs,
        batch_size: opts?.batchSize,
      });
    },
    recordPatchLatency: (durationMs, opts) => {
      pushWithLimit(patch_latency, {
        at_ms: nowMs(),
        duration_ms: durationMs,
        patch_size: opts?.patchSize,
      });
    },
    getSnapshot: () => ({
      session_switches: session_switches.slice(),
      visible_session_switches: visible_session_switches.slice(),
      long_tasks: long_tasks.slice(),
      event_loop_gaps: event_loop_gaps.slice(),
      raf_gaps: raf_gaps.slice(),
      worker_apply: worker_apply.slice(),
      patch_latency: patch_latency.slice(),
      memory_samples: memory_samples.slice(),
      meta,
    }),
    getSummary: () => ({
      session_switch_ms: summarizePercentiles(
        session_switches
          .map((entry) => entry.duration_ms ?? null)
          .filter((entry): entry is number => typeof entry === "number"),
      ),
      visible_switch_ms: summarizePercentiles(
        visible_session_switches
          .map((entry) => entry.click_to_visible_ms ?? null)
          .filter((entry): entry is number => typeof entry === "number"),
      ),
      stable_switch_ms: summarizePercentiles(
        visible_session_switches
          .map((entry) => entry.click_to_stable_ms ?? null)
          .filter((entry): entry is number => typeof entry === "number"),
      ),
      long_task_ms: summarizePercentiles(long_tasks.map((entry) => entry.duration_ms)),
      event_loop_gap_ms: summarizePercentiles(event_loop_gaps.map((entry) => entry.gap_ms)),
      event_loop_gap_unthrottled_ms: summarizePercentiles(
        event_loop_gaps
          .filter((entry) => entry.timer_throttled_suspected !== true)
          .map((entry) => entry.gap_ms),
      ),
      raf_gap_ms: summarizePercentiles(raf_gaps.map((entry) => entry.gap_ms)),
      worker_apply_ms: summarizePercentiles(worker_apply.map((entry) => entry.duration_ms)),
      patch_latency_ms: summarizePercentiles(patch_latency.map((entry) => entry.duration_ms)),
    }),
    reset: () => {
      session_switches.length = 0;
      visible_session_switches.length = 0;
      long_tasks.length = 0;
      event_loop_gaps.length = 0;
      raf_gaps.length = 0;
      worker_apply.length = 0;
      patch_latency.length = 0;
      memory_samples.length = 0;
      pending = null;
      pendingVisible = null;
      lastHeartbeatAtMs = nowMs();
      lastRafAtMs = typeof performance !== "undefined" ? performance.now() : 0;
      sampleMemory();
    },
    stop: () => {
      observer?.disconnect();
      observer = null;
      if (memoryTimer !== null && typeof window !== "undefined") {
        window.clearInterval(memoryTimer);
        memoryTimer = null;
      }
      if (heartbeatTimer !== null && typeof window !== "undefined") {
        window.clearInterval(heartbeatTimer);
        heartbeatTimer = null;
      }
      if (rafHandle !== null && typeof window !== "undefined") {
        window.cancelAnimationFrame(rafHandle);
        rafHandle = null;
      }
    },
  };

  if (typeof window !== "undefined") {
    (window as WindowWithLoadTest).__ctxLoadTestTelemetry = {
      enabled: true,
      getSnapshot: telemetry.getSnapshot,
      getSummary: telemetry.getSummary,
      reset: telemetry.reset,
      stop: telemetry.stop,
    };
  }

  return telemetry;
};

export const getLoadTestTelemetry = (): LoadTestTelemetry | null => telemetry;
