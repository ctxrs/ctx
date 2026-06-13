import type { ProfilerOnRenderCallback } from "react";
import { randomUuid } from "./randomUuid";

export type WalMode = "off" | "light" | "heavy";

export type WalEvent = {
  seq: number;
  ts_ms: number;
  kind: string;
  session_id: string;
  page_id: string;
  data?: Record<string, unknown>;
};

export type WalRecorder = {
  mode: WalMode;
  sessionId: string;
  pageId: string;
  endpoint: string;
  record: (kind: string, data?: Record<string, unknown>, opts?: { level?: "light" | "heavy" }) => void;
  dump: (opts?: { limit?: number }) => WalEvent[];
  flush: (reason?: "interval" | "manual" | "unload") => void;
  setMode: (mode: WalMode) => void;
  getStatus: () => {
    mode: WalMode;
    sessionId: string;
    pageId: string;
    ringSize: number;
    queueSize: number;
    dropped: number;
    endpoint: string;
    lastFlushMs: number | null;
  };
  onRender?: ProfilerOnRenderCallback;
};

export const MAX_RING = 5000;
export const MAX_QUEUE = 2000;
export const FLUSH_MS = 2000;
export const MAX_STRING_LIGHT = 500;
export const MAX_STRING_HEAVY = 2000;
export const WS_IDLE_MS = 30000;
export const WS_SAMPLE_MS = 2000;
export const WAL_ENDPOINT_DEFAULT = "/__ctx_wal__";

export const REDACT_HEADERS = new Set([
  "authorization",
  "cookie",
  "set-cookie",
  "x-api-key",
  "x-supabase-key",
  "x-ctx-auth",
  "x-ctx-token",
]);

export const globalAny = globalThis as unknown as {
  __CTX_WAL__?: WalRecorder;
  __CTX_WAL_HOOKS__?: boolean;
};

export const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

export const clampString = (value: string, maxLen: number): string => {
  if (value.length <= maxLen) return value;
  return `${value.slice(0, maxLen)}...`;
};

export const normalizeUrl = (raw: string): string => {
  try {
    const base =
      typeof window !== "undefined" && window.location ? window.location.origin : "http://localhost";
    const url = new URL(raw, base);
    const cleanParams = new URLSearchParams();
    for (const key of url.searchParams.keys()) {
      cleanParams.append(key, "");
    }
    const search = cleanParams.toString();
    return `${url.origin}${url.pathname}${search ? `?${search}` : ""}`;
  } catch {
    return raw;
  }
};

export const sanitizeHeaders = (
  headers: Headers | Record<string, string> | Array<[string, string]> | string | null | undefined,
): Record<string, string> | undefined => {
  if (!headers) return undefined;
  let entries: Array<[string, string]> = [];
  if (typeof headers === "string") {
    entries = headers
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const idx = line.indexOf(":");
        if (idx === -1) return [line, ""];
        return [line.slice(0, idx), line.slice(idx + 1).trim()];
      });
  } else if (headers instanceof Headers) {
    entries = Array.from(headers.entries());
  } else if (Array.isArray(headers)) {
    entries = headers;
  } else {
    entries = Object.entries(headers);
  }
  const out: Record<string, string> = {};
  for (const [key, value] of entries) {
    const lower = key.toLowerCase();
    const safeValue = REDACT_HEADERS.has(lower) ? "<redacted>" : clampString(String(value ?? ""), MAX_STRING_LIGHT);
    out[key] = safeValue;
  }
  return out;
};

export const serializeConsoleArg = (value: unknown, mode: WalMode): unknown => {
  const maxLen = mode === "heavy" ? MAX_STRING_HEAVY : MAX_STRING_LIGHT;
  if (value instanceof Error) {
    return {
      type: "error",
      name: value.name,
      message: clampString(value.message ?? "", maxLen),
      stack: value.stack ? clampString(value.stack, maxLen) : undefined,
    };
  }
  if (typeof value === "string") return clampString(value, maxLen);
  if (typeof value === "number" || typeof value === "boolean" || value === null) return value;
  if (typeof value === "undefined") return "undefined";
  try {
    const json = JSON.stringify(value);
    if (json.length <= maxLen) return JSON.parse(json);
    return clampString(json, maxLen);
  } catch {
    return Object.prototype.toString.call(value);
  }
};

export const shouldDisable = (): boolean => {
  if (typeof window === "undefined") return true;
  if (import.meta.env.MODE === "test") return true;
  return false;
};

export const resolveMode = (): WalMode => {
  if (shouldDisable()) return "off";
  const query = typeof window !== "undefined" ? new URLSearchParams(window.location.search) : null;
  const queryMode = query?.get("wal")?.toLowerCase();
  if (queryMode === "0" || queryMode === "off" || queryMode === "false") return "off";
  if (queryMode === "heavy") return "heavy";
  if (queryMode === "light" || queryMode === "1" || queryMode === "true") return "light";

  const envMode = String(import.meta.env.VITE_CTX_WAL_MODE ?? "").toLowerCase();
  if (envMode === "off" || envMode === "0" || envMode === "false") return "off";
  if (envMode === "heavy") return "heavy";
  if (envMode === "light" || envMode === "1" || envMode === "true") return "light";

  return import.meta.env.DEV ? "light" : "off";
};

export const resolveEndpoint = (): string => {
  const raw = String(import.meta.env.VITE_CTX_WAL_ENDPOINT ?? "").trim();
  return raw.length > 0 ? raw : WAL_ENDPOINT_DEFAULT;
};

export const getSessionId = (): string => {
  try {
    const existing = sessionStorage.getItem("ctxWalSessionId");
    if (existing) return existing;
    const created = randomUuid();
    sessionStorage.setItem("ctxWalSessionId", created);
    return created;
  } catch {
    return randomUuid();
  }
};

export const parseContentLength = (value: string | null): number | undefined => {
  if (!value) return undefined;
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : undefined;
};

export type XhrWalMeta = {
  method: string;
  url: string;
  start: number;
  request_bytes?: number;
};

export type WalTrackedXmlHttpRequest = XMLHttpRequest & {
  __ctxWal?: XhrWalMeta;
};

export type PerformanceObserverWithSupportedEntryTypes = typeof PerformanceObserver & {
  supportedEntryTypes?: string[];
};

export type LayoutShiftEntryLike = PerformanceEntry & {
  value?: number;
  hadRecentInput?: boolean;
};

export type LargestContentfulPaintEntryLike = PerformanceEntry & {
  size?: number;
  element?: Element | null;
  url?: string;
};

export type FirstInputEntryLike = PerformanceEntry & {
  processingStart?: number;
};

export type EventTimingEntryLike = PerformanceEntry & {
  interactionId?: number;
};
