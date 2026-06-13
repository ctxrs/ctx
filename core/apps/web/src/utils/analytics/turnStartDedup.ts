const STORAGE_KEY = "ctx.analytics.turn_starts.v1";
const MAX_ENTRIES = 2048;
const MAX_AGE_MS = 45 * 24 * 60 * 60 * 1000;

type StoredTurnStarts = Record<string, number>;

let memoryCache: StoredTurnStarts | null = null;

const loadStoredTurnStarts = (): StoredTurnStarts => {
  if (memoryCache) return memoryCache;
  if (typeof window === "undefined") {
    memoryCache = {};
    return memoryCache;
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) {
      memoryCache = {};
      return memoryCache;
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const next: StoredTurnStarts = {};
    for (const [key, value] of Object.entries(parsed)) {
      if (typeof value === "number" && Number.isFinite(value)) {
        next[key] = value;
      }
    }
    memoryCache = next;
    return memoryCache;
  } catch {
    memoryCache = {};
    return memoryCache;
  }
};

const persistStoredTurnStarts = (entries: StoredTurnStarts): void => {
  memoryCache = entries;
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Ignore storage failures; in-memory dedupe still protects the active session.
  }
};

const pruneStoredTurnStarts = (
  entries: StoredTurnStarts,
  nowMs: number,
): StoredTurnStarts => {
  const next: StoredTurnStarts = {};
  for (const [key, value] of Object.entries(entries)) {
    if (nowMs - value <= MAX_AGE_MS) {
      next[key] = value;
    }
  }
  const keyedEntries = Object.entries(next).sort((left, right) => right[1] - left[1]);
  if (keyedEntries.length <= MAX_ENTRIES) return next;
  return Object.fromEntries(keyedEntries.slice(0, MAX_ENTRIES));
};

const turnStartKey = (sessionId: string, turnId: string): string => `${sessionId}|${turnId}`;

export const markTurnStartedTracked = (
  sessionId: string,
  turnId: string,
): boolean => {
  if (!sessionId.trim() || !turnId.trim()) return true;
  const nowMs = Date.now();
  const existing = pruneStoredTurnStarts(loadStoredTurnStarts(), nowMs);
  const key = turnStartKey(sessionId, turnId);
  if (Object.prototype.hasOwnProperty.call(existing, key)) {
    if (memoryCache !== existing) {
      persistStoredTurnStarts(existing);
    } else {
      memoryCache = existing;
    }
    return false;
  }
  existing[key] = nowMs;
  persistStoredTurnStarts(pruneStoredTurnStarts(existing, nowMs));
  return true;
};

export const resetTurnStartTrackingForTests = (): void => {
  memoryCache = null;
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore
  }
};
