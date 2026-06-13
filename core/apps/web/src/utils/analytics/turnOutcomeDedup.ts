import type { SessionTurnStatus } from "@ctx/types";

const STORAGE_KEY = "ctx.analytics.turn_outcomes.v1";
const MAX_ENTRIES = 2048;
const MAX_AGE_MS = 45 * 24 * 60 * 60 * 1000;

type TerminalTurnStatus = Extract<SessionTurnStatus, "completed" | "interrupted" | "failed">;

type StoredTurnOutcomes = Record<string, number>;

let memoryCache: StoredTurnOutcomes | null = null;

const loadStoredTurnOutcomes = (): StoredTurnOutcomes => {
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
    const next: StoredTurnOutcomes = {};
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

const persistStoredTurnOutcomes = (entries: StoredTurnOutcomes): void => {
  memoryCache = entries;
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Ignore storage failures; in-memory dedupe still protects the active session.
  }
};

const pruneStoredTurnOutcomes = (
  entries: StoredTurnOutcomes,
  nowMs: number,
): StoredTurnOutcomes => {
  const next: StoredTurnOutcomes = {};
  for (const [key, value] of Object.entries(entries)) {
    if (nowMs - value <= MAX_AGE_MS) {
      next[key] = value;
    }
  }
  const keyedEntries = Object.entries(next).sort((left, right) => right[1] - left[1]);
  if (keyedEntries.length <= MAX_ENTRIES) return next;
  return Object.fromEntries(keyedEntries.slice(0, MAX_ENTRIES));
};

const turnOutcomeKey = (
  sessionId: string,
  turnId: string,
  status: TerminalTurnStatus,
): string => `${sessionId}|${turnId}|${status}`;

export const markTurnOutcomeTracked = (
  sessionId: string,
  turnId: string,
  status: TerminalTurnStatus,
): boolean => {
  if (!sessionId.trim() || !turnId.trim()) return true;
  const nowMs = Date.now();
  const existing = pruneStoredTurnOutcomes(loadStoredTurnOutcomes(), nowMs);
  const key = turnOutcomeKey(sessionId, turnId, status);
  if (Object.prototype.hasOwnProperty.call(existing, key)) {
    if (memoryCache !== existing) {
      persistStoredTurnOutcomes(existing);
    } else {
      memoryCache = existing;
    }
    return false;
  }
  existing[key] = nowMs;
  persistStoredTurnOutcomes(pruneStoredTurnOutcomes(existing, nowMs));
  return true;
};

export const resetTurnOutcomeTrackingForTests = (): void => {
  memoryCache = null;
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore
  }
};
