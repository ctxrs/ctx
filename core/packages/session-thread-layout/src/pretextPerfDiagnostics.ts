type PretextPerfBucketMap = Record<string, number>;

type PretextPerfSnapshot = {
  started_at_ms: number;
  counters: Record<string, number>;
  buckets: Record<string, PretextPerfBucketMap>;
  recent: Array<{
    at_ms: number;
    type: string;
    detail: Record<string, unknown> | null;
  }>;
};

type PretextPerfWindow = Window & {
  __CTX_PRETEXT_PERF__?: unknown;
  __ctxPretextPerfDiagnostics?: {
    enabled: boolean;
    getSnapshot: () => PretextPerfSnapshot;
    reset: () => void;
  };
};

const MAX_RECENT_EVENTS = 200;

const state: {
  startedAtMs: number;
  counters: Map<string, number>;
  buckets: Map<string, Map<string, number>>;
  recent: PretextPerfSnapshot["recent"];
} = {
  startedAtMs: Date.now(),
  counters: new Map(),
  buckets: new Map(),
  recent: [],
};

let enabledCache: boolean | null = null;
let exposed = false;

function readSearchParam(name: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    return new URLSearchParams(window.location.search).get(name);
  } catch {
    return null;
  }
}

function ensureBucket(name: string): Map<string, number> {
  let bucket = state.buckets.get(name);
  if (!bucket) {
    bucket = new Map();
    state.buckets.set(name, bucket);
  }
  return bucket;
}

function cloneSnapshot(): PretextPerfSnapshot {
  return {
    started_at_ms: state.startedAtMs,
    counters: Object.fromEntries(state.counters.entries()),
    buckets: Object.fromEntries(
      Array.from(state.buckets.entries(), ([bucketName, bucket]) => [
        bucketName,
        Object.fromEntries(bucket.entries()),
      ]),
    ),
    recent: state.recent.slice(),
  };
}

function exposeDiagnostics(enabled: boolean): void {
  if (!enabled || exposed || typeof window === "undefined") return;
  (window as PretextPerfWindow).__ctxPretextPerfDiagnostics = {
    enabled: true,
    getSnapshot: cloneSnapshot,
    reset: resetPretextPerfDiagnostics,
  };
  exposed = true;
}

export function readPretextPerfQueryFlag(name: string): string | null {
  return readSearchParam(name);
}

export function hashPretextPerfValue(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}

export function isPretextPerfDiagnosticsEnabled(): boolean {
  if (enabledCache != null) return enabledCache;
  if (typeof window === "undefined") {
    enabledCache = false;
    return enabledCache;
  }
  const perfWindow = window as PretextPerfWindow;
  enabledCache =
    perfWindow.__CTX_PRETEXT_PERF__ != null ||
    readSearchParam("perfdiag") === "1" ||
    readSearchParam("loadtest") === "1";
  exposeDiagnostics(enabledCache);
  return enabledCache;
}

export function initPretextPerfDiagnostics(): void {
  exposeDiagnostics(isPretextPerfDiagnosticsEnabled());
}

export function incrementPretextPerfCounter(name: string, delta = 1): void {
  if (!isPretextPerfDiagnosticsEnabled()) return;
  state.counters.set(name, (state.counters.get(name) ?? 0) + delta);
}

export function addPretextPerfBucket(name: string, key: string, delta = 1): void {
  if (!isPretextPerfDiagnosticsEnabled()) return;
  const bucket = ensureBucket(name);
  bucket.set(key, (bucket.get(key) ?? 0) + delta);
}

export function recordPretextPerfEvent(type: string, detail: Record<string, unknown> | null = null): void {
  if (!isPretextPerfDiagnosticsEnabled()) return;
  state.recent.push({
    at_ms: Date.now(),
    type,
    detail,
  });
  if (state.recent.length > MAX_RECENT_EVENTS) {
    state.recent.shift();
  }
}

export function resetPretextPerfDiagnostics(): void {
  state.startedAtMs = Date.now();
  state.counters.clear();
  state.buckets.clear();
  state.recent.length = 0;
}
