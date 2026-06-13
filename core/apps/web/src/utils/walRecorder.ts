import type { ProfilerOnRenderCallback } from "react";
import { randomUuid } from "./randomUuid";
import { installWalRecorderHooks } from "./walRecorderHooks";
import { initWalRecorderPerformanceObservers } from "./walRecorderPerformance";
import {
  FLUSH_MS,
  getSessionId,
  globalAny,
  MAX_QUEUE,
  MAX_RING,
  nowMs,
  resolveEndpoint,
  resolveMode,
  shouldDisable,
  type WalEvent,
  type WalMode,
  type WalRecorder,
} from "./walRecorderShared";

export const initWalRecorder = (): WalRecorder | null => {
  if (shouldDisable()) return null;
  const mode = resolveMode();
  const existing = globalAny.__CTX_WAL__;
  if (existing) {
    existing.setMode(mode);
    return mode === "off" ? null : existing;
  }
  if (mode === "off") return null;

  const sessionId = getSessionId();
  const pageId = randomUuid();
  const endpoint = resolveEndpoint();

  let seq = 0;
  let dropped = 0;
  let lastFlushMs: number | null = null;
  const ring: WalEvent[] = [];
  const queue: WalEvent[] = [];
  let modeState: WalMode = mode;
  let flushTimer: number | null = null;

  const record = (kind: string, data?: Record<string, unknown>, opts?: { level?: "light" | "heavy" }) => {
    const level = opts?.level ?? "light";
    if (modeState === "off") return;
    if (level === "heavy" && modeState !== "heavy") return;
    const event: WalEvent = {
      seq: (seq += 1),
      ts_ms: nowMs(),
      kind,
      session_id: sessionId,
      page_id: pageId,
      data,
    };
    if (ring.length >= MAX_RING) ring.shift();
    ring.push(event);
    if (queue.length >= MAX_QUEUE) {
      queue.shift();
      dropped += 1;
    }
    queue.push(event);
    if (flushTimer === null && typeof window !== "undefined") {
      flushTimer = window.setTimeout(() => {
        flushTimer = null;
        flush("interval");
      }, FLUSH_MS);
    }
  };

  const buildEndpoint = () => {
    const separator = endpoint.includes("?") ? "&" : "?";
    return `${endpoint}${separator}session=${encodeURIComponent(sessionId)}`;
  };

  const flush = (reason: "interval" | "manual" | "unload" = "interval") => {
    if (queue.length === 0) return;
    const batch = queue.splice(0);
    const lines = batch
      .map((event) => {
        try {
          return JSON.stringify(event);
        } catch {
          return JSON.stringify({ kind: "wal:serialize_error", ts_ms: nowMs() });
        }
      })
      .join("\n");
    const payload = `${lines}\n`;
    const url = buildEndpoint();
    let sent = false;
    if (reason === "unload" && typeof navigator !== "undefined" && navigator.sendBeacon) {
      try {
        sent = navigator.sendBeacon(url, payload);
      } catch {
        sent = false;
      }
    }
    if (!sent && typeof fetch !== "undefined") {
      fetch(url, {
        method: "POST",
        headers: { "content-type": "text/plain", "x-ctx-wal": "1", "x-ctx-wal-session": sessionId },
        body: payload,
        keepalive: reason === "unload",
      }).catch(() => {
        for (const item of batch) {
          if (queue.length >= MAX_QUEUE) {
            queue.shift();
            dropped += 1;
          }
          queue.push(item);
        }
      });
    }
    lastFlushMs = nowMs();
  };

  const setMode = (next: WalMode) => {
    if (modeState === next) return;
    modeState = next;
    record("wal:mode", { mode: modeState });
  };

  const getStatus = () => ({
    mode: modeState,
    sessionId,
    pageId,
    ringSize: ring.length,
    queueSize: queue.length,
    dropped,
    endpoint,
    lastFlushMs,
  });

  const recorder: WalRecorder = {
    mode: modeState,
    sessionId,
    pageId,
    endpoint,
    record,
    dump: ({ limit } = {}) => {
      if (!limit || limit >= ring.length) return ring.slice();
      return ring.slice(Math.max(0, ring.length - limit));
    },
    flush,
    setMode: (next) => {
      setMode(next);
      recorder.mode = modeState;
    },
    getStatus,
  };

  recorder.onRender = ((id, phase, actualDuration, baseDuration, startTime, commitTime) => {
    const shouldSample = modeState !== "heavy" && actualDuration < 16;
    if (shouldSample) return;
    const origin = performance?.timeOrigin ?? Date.now();
    record(
      "react:render",
      {
        id,
        phase,
        actual_duration_ms: Math.round(actualDuration),
        base_duration_ms: Math.round(baseDuration),
        start_ms: Math.round(origin + startTime),
        commit_ms: Math.round(origin + commitTime),
      },
      { level: modeState === "heavy" ? "heavy" : "light" },
    );
  }) as ProfilerOnRenderCallback;

  globalAny.__CTX_WAL__ = recorder;
  installWalRecorderHooks(recorder, () => modeState);
  initWalRecorderPerformanceObservers(recorder, () => modeState);

  record("wal:init", {
    mode: modeState,
    href:
      typeof window !== "undefined" && window.location
        ? `${window.location.origin}${window.location.pathname}`
        : undefined,
  });

  return recorder;
};
