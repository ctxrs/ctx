import {
  trackApiErrorObserved,
  trackRuntimeErrorObserved,
  trackSessionLoadFatalObserved,
} from "../utils/analytics";

export type UiDiagnosticSeverity = "info" | "warning" | "error";

export type UiDiagnosticEvent = {
  id: number;
  ts: number;
  source: string;
  code: string;
  severity: UiDiagnosticSeverity;
  message: string;
  fatal?: boolean;
  context?: Record<string, unknown>;
};

export type UiDiagnosticInput = {
  source: string;
  code: string;
  message: string;
  severity?: UiDiagnosticSeverity;
  fatal?: boolean;
  context?: Record<string, unknown>;
};

export type UiDiagnosticPersistenceSink = (event: UiDiagnosticEvent) => void | Promise<void>;

const DEFAULT_MAX_EVENTS = 200;
const ANALYTICS_DIAGNOSTIC_THROTTLE_MS = 5 * 60 * 1000;
const MAX_ANALYTICS_THROTTLE_KEYS = 512;
const MAX_RUNTIME_STACK_CHARS = 2000;
let maxEvents = DEFAULT_MAX_EVENTS;
let nextId = 1;
let events: UiDiagnosticEvent[] = [];
const listeners = new Set<() => void>();
let runtimeHandlersInstalled = false;
let runtimeHandlersCleanup: (() => void) | null = null;
let persistenceSink: UiDiagnosticPersistenceSink | null = null;
let analyticsThrottleByKey = new Map<string, number>();

const UUID_PATTERN = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
const HEX_TOKEN_PATTERN = /\b[0-9a-f]{8,}\b/gi;
const NUMERIC_TOKEN_PATTERN = /\b\d+\b/g;
const UUID_SEGMENT_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const NUMERIC_PATH_SEGMENT_PATTERN = /^\d+$/;
const HEX_PATH_SEGMENT_PATTERN = /^[0-9a-f]{8,}$/i;
const ID_LIKE_SEGMENT_PATTERN = /^[A-Za-z0-9_-]{8,}$/;

const fnv1a32 = (input: string): string => {
  let hash = 0x811c9dc5;
  for (let idx = 0; idx < input.length; idx += 1) {
    hash ^= input.charCodeAt(idx);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
};

const buildDiagnosticSignature = (event: UiDiagnosticEvent): string => {
  const normalizedMessage = event.message
    .toLowerCase()
    .replace(UUID_PATTERN, ":uuid")
    .replace(HEX_TOKEN_PATTERN, ":hex")
    .replace(NUMERIC_TOKEN_PATTERN, ":n")
    .slice(0, 240);
  return fnv1a32(`${event.source}|${event.code}|${normalizedMessage}`);
};

const normalizeEndpoint = (value: unknown): string => {
  const raw = typeof value === "string" ? value.trim() : "";
  if (!raw) return "unknown";

  let path = raw;
  if (raw.startsWith("http://") || raw.startsWith("https://")) {
    try {
      path = new URL(raw).pathname;
    } catch {
      return "unknown";
    }
  } else {
    path = raw.split("?")[0]?.split("#")[0] ?? "";
  }

  if (!(path === "/api" || path.startsWith("/api/"))) return "unknown";
  const normalizedSegments = path
    .split("/")
    .filter(Boolean)
    .map((segment, index) => {
      if (index === 0 && segment === "api") return segment;
      if (segment.startsWith(":")) return segment;
      if (NUMERIC_PATH_SEGMENT_PATTERN.test(segment)) return ":id";
      if (UUID_SEGMENT_PATTERN.test(segment)) return ":id";
      if (HEX_PATH_SEGMENT_PATTERN.test(segment)) return ":id";
      if (ID_LIKE_SEGMENT_PATTERN.test(segment) && /\d/.test(segment)) return ":id";
      return segment;
    });
  if (normalizedSegments.length === 0) return "unknown";
  return `/${normalizedSegments.join("/")}`;
};

const normalizeMethod = (value: unknown): string => {
  const raw = typeof value === "string" ? value.trim().toUpperCase() : "";
  return raw || "UNKNOWN";
};

const normalizeStatusFamily = (
  value: unknown,
): "2xx" | "3xx" | "4xx" | "5xx" | "none" => {
  if (typeof value !== "number" || !Number.isFinite(value)) return "none";
  const normalized = Math.trunc(value);
  if (normalized >= 200 && normalized < 300) return "2xx";
  if (normalized >= 300 && normalized < 400) return "3xx";
  if (normalized >= 400 && normalized < 500) return "4xx";
  if (normalized >= 500 && normalized < 600) return "5xx";
  return "none";
};

const diagnosticStack = (value: unknown): string | undefined => {
  if (typeof value !== "object" || value === null || !("stack" in value)) return undefined;
  const stack = (value as { stack?: unknown }).stack;
  if (typeof stack !== "string" || !stack.trim()) return undefined;
  return stack.length > MAX_RUNTIME_STACK_CHARS ? `${stack.slice(0, MAX_RUNTIME_STACK_CHARS)}...` : stack;
};

const buildAnalyticsThrottleKey = (event: UiDiagnosticEvent, signature: string): string => {
  if (event.source === "api") {
    return [
      event.source,
      event.code,
      normalizeEndpoint(event.context?.path),
      normalizeMethod(event.context?.method),
      normalizeStatusFamily(event.context?.status),
      signature,
    ].join("|");
  }
  if (event.source === "session_supervisor") {
    const mode = event.context && typeof event.context.mode === "string" ? event.context.mode.trim() : "unknown";
    return [event.source, event.code, mode || "unknown", signature].join("|");
  }
  return [event.source, event.code, signature].join("|");
};

const pruneAnalyticsThrottle = (nowMs: number): void => {
  for (const [key, ts] of analyticsThrottleByKey.entries()) {
    if (nowMs - ts > ANALYTICS_DIAGNOSTIC_THROTTLE_MS) {
      analyticsThrottleByKey.delete(key);
    }
  }
  while (analyticsThrottleByKey.size > MAX_ANALYTICS_THROTTLE_KEYS) {
    const oldestKey = analyticsThrottleByKey.keys().next().value;
    if (!oldestKey) break;
    analyticsThrottleByKey.delete(oldestKey);
  }
};

const shouldEmitAnalyticsDiagnostic = (event: UiDiagnosticEvent, signature: string): boolean => {
  const nowMs = Date.now();
  const key = buildAnalyticsThrottleKey(event, signature);
  pruneAnalyticsThrottle(nowMs);
  const previousTs = analyticsThrottleByKey.get(key);
  if (typeof previousTs === "number" && nowMs - previousTs <= ANALYTICS_DIAGNOSTIC_THROTTLE_MS) {
    return false;
  }
  analyticsThrottleByKey.delete(key);
  analyticsThrottleByKey.set(key, nowMs);
  return true;
};

const emitAnalyticsDiagnostic = (event: UiDiagnosticEvent) => {
  const signature = buildDiagnosticSignature(event);
  if (!shouldEmitAnalyticsDiagnostic(event, signature)) {
    return;
  }
  if (event.source === "runtime" && event.severity !== "info") {
    trackRuntimeErrorObserved({
      errorKey: event.code,
      severity: event.severity,
      signature,
    });
    return;
  }

  if (event.source === "session_supervisor" && event.code === "session.load_fatal") {
    const modeRaw = event.context && typeof event.context.mode === "string" ? event.context.mode.trim() : "";
    trackSessionLoadFatalObserved({
      mode: modeRaw || "unknown",
      signature,
    });
    return;
  }

  if (event.source === "api" && (event.code === "api.transport_error" || event.code === "api.http_error")) {
    trackApiErrorObserved({
      errorKey: event.code,
      endpoint: normalizeEndpoint(event.context?.path),
      method: normalizeMethod(event.context?.method),
      statusFamily: normalizeStatusFamily(event.context?.status),
      signature,
    });
  }
};

const notifyListeners = () => {
  for (const listener of listeners) {
    listener();
  }
};

export const normalizeDiagnosticErrorMessage = (value: unknown, fallback = "Unknown error"): string => {
  if (value instanceof Error) {
    return value.message || fallback;
  }
  if (typeof value === "string" && value.trim().length > 0) {
    return value.trim();
  }
  if (value && typeof value === "object") {
    const maybeMessage = (value as Record<string, unknown>).message;
    if (typeof maybeMessage === "string" && maybeMessage.trim().length > 0) {
      return maybeMessage.trim();
    }
    try {
      return JSON.stringify(value);
    } catch {
      return fallback;
    }
  }
  return fallback;
};

export const emitUiDiagnostic = (input: UiDiagnosticInput): UiDiagnosticEvent => {
  const event: UiDiagnosticEvent = {
    id: nextId++,
    ts: Date.now(),
    source: String(input.source || "unknown"),
    code: String(input.code || "unknown"),
    severity: input.severity ?? "error",
    message: String(input.message || "Unknown error"),
    fatal: input.fatal === true ? true : undefined,
    context: input.context,
  };
  events = [...events, event];
  if (events.length > maxEvents) {
    events = events.slice(events.length - maxEvents);
  }
  try {
    emitAnalyticsDiagnostic(event);
  } catch {
    // Ignore analytics failures; diagnostics channel must remain local-first and robust.
  }
  try {
    const forwarded = persistenceSink?.(event);
    if (forwarded && typeof (forwarded as Promise<void>).then === "function") {
      void (forwarded as Promise<void>).catch(() => {});
    }
  } catch {
    // Ignore persistence sink failures; diagnostics channel must remain local-first and robust.
  }
  notifyListeners();
  return event;
};

export const setUiDiagnosticPersistenceSink = (sink: UiDiagnosticPersistenceSink | null) => {
  persistenceSink = sink;
};

export const getUiDiagnostics = (): UiDiagnosticEvent[] => [...events];

export const clearUiDiagnostics = () => {
  if (events.length === 0) return;
  events = [];
  notifyListeners();
};

export const subscribeUiDiagnostics = (listener: () => void): (() => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

export const installGlobalRuntimeDiagnosticHandlers = () => {
  if (runtimeHandlersInstalled) return;
  if (typeof window === "undefined") return;
  const onError = (event: ErrorEvent) => {
    const message = normalizeDiagnosticErrorMessage(event.error ?? event.message ?? "Unhandled runtime error");
    const isResizeObserverLoop =
      message.includes("ResizeObserver loop completed with undelivered notifications") ||
      message.includes("ResizeObserver loop limit exceeded");
    const stack = diagnosticStack(event.error);
    emitUiDiagnostic({
      source: "runtime",
      code: isResizeObserverLoop ? "runtime.resize_observer_loop" : "runtime.error",
      severity: isResizeObserverLoop ? "warning" : "error",
      message,
      context: {
        filename: event.filename,
        lineno: event.lineno,
        colno: event.colno,
        ...(stack ? { stack } : {}),
      },
    });
  };
  const onUnhandledRejection = (event: PromiseRejectionEvent) => {
    emitUiDiagnostic({
      source: "runtime",
      code: "runtime.unhandled_rejection",
      severity: "error",
      message: normalizeDiagnosticErrorMessage(event.reason, "Unhandled promise rejection"),
    });
  };
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onUnhandledRejection);
  runtimeHandlersCleanup = () => {
    window.removeEventListener("error", onError);
    window.removeEventListener("unhandledrejection", onUnhandledRejection);
    runtimeHandlersCleanup = null;
    runtimeHandlersInstalled = false;
  };
  runtimeHandlersInstalled = true;
};

export const resetUiDiagnosticsForTests = () => {
  clearUiDiagnostics();
  maxEvents = DEFAULT_MAX_EVENTS;
  nextId = 1;
  persistenceSink = null;
  analyticsThrottleByKey.clear();
  if (runtimeHandlersCleanup) {
    runtimeHandlersCleanup();
  }
};

export const setUiDiagnosticsMaxEventsForTests = (next: number) => {
  maxEvents = Math.max(1, Math.trunc(next));
  if (events.length > maxEvents) {
    events = events.slice(events.length - maxEvents);
  }
};
