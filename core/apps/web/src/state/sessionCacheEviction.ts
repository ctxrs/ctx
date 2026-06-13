export type SessionCacheEvictionCaps = {
  softLimit: number;
  hardLimit: number;
  warmTtlMs: number;
};

export const SESSION_CACHE_SOFT_LIMIT = 36;
export const SESSION_CACHE_HARD_LIMIT = 60;
export const SESSION_CACHE_WARM_TTL_MS = 10 * 60 * 1000;

export const SESSION_CACHE_CAPS: SessionCacheEvictionCaps = {
  softLimit: SESSION_CACHE_SOFT_LIMIT,
  hardLimit: SESSION_CACHE_HARD_LIMIT,
  warmTtlMs: SESSION_CACHE_WARM_TTL_MS,
};

export type SessionCacheEvictionEntry = {
  sessionId: string;
  refCount?: number;
  lastAccessMs?: number;
  updatedAtMs?: number;
  warmUntilMs?: number;
};

export type SessionCacheEvictionPlan = {
  evictIds: string[];
  remaining: number;
  pressure: "ok" | "soft" | "hard";
};

type NormalizedEntry = {
  sessionId: string;
  refCount: number;
  lastAccessMs: number;
  updatedAtMs: number;
  warmUntilMs: number;
  lruMs: number;
};

const normalizeCaps = (caps: SessionCacheEvictionCaps): SessionCacheEvictionCaps => {
  const softLimit = Math.max(1, Math.floor(caps.softLimit));
  const hardLimit = Math.max(softLimit, Math.floor(caps.hardLimit));
  const warmTtlMs = Math.max(0, Math.floor(caps.warmTtlMs));
  return { softLimit, hardLimit, warmTtlMs };
};

const normalizeEntry = (
  entry: SessionCacheEvictionEntry,
  caps: SessionCacheEvictionCaps,
  nowMs: number,
): NormalizedEntry => {
  const updatedAtMs = Number.isFinite(entry.updatedAtMs) ? entry.updatedAtMs! : nowMs;
  const lastAccessMs = Number.isFinite(entry.lastAccessMs) ? entry.lastAccessMs! : updatedAtMs;
  const activityMs = Math.max(lastAccessMs, updatedAtMs);
  const warmUntilMs = Number.isFinite(entry.warmUntilMs)
    ? entry.warmUntilMs!
    : activityMs + caps.warmTtlMs;
  return {
    sessionId: entry.sessionId,
    refCount: Math.max(0, entry.refCount ?? 0),
    lastAccessMs,
    updatedAtMs,
    warmUntilMs,
    lruMs: activityMs,
  };
};

const sortByLru = (a: NormalizedEntry, b: NormalizedEntry): number =>
  a.lruMs - b.lruMs || a.sessionId.localeCompare(b.sessionId);

export const planSessionCacheEvictions = (
  entries: SessionCacheEvictionEntry[],
  caps: SessionCacheEvictionCaps = SESSION_CACHE_CAPS,
  nowMs = Date.now(),
): SessionCacheEvictionPlan => {
  if (entries.length === 0) return { evictIds: [], remaining: 0, pressure: "ok" };
  const normalizedCaps = normalizeCaps(caps);
  const normalized = entries.map((entry) => normalizeEntry(entry, normalizedCaps, nowMs));
  const warm: NormalizedEntry[] = [];
  const cold: NormalizedEntry[] = [];
  for (const entry of normalized) {
    if (entry.refCount > 0) continue;
    if (nowMs <= entry.warmUntilMs) {
      warm.push(entry);
    } else {
      cold.push(entry);
    }
  }
  warm.sort(sortByLru);
  cold.sort(sortByLru);

  let remaining = normalized.length;
  const evictIds: string[] = [];
  const evictFrom = (candidates: NormalizedEntry[], target: number) => {
    for (const entry of candidates) {
      if (remaining <= target) return;
      evictIds.push(entry.sessionId);
      remaining -= 1;
    }
  };

  if (remaining > normalizedCaps.softLimit) {
    evictFrom(cold, normalizedCaps.softLimit);
  }
  if (remaining > normalizedCaps.hardLimit) {
    evictFrom(warm, normalizedCaps.hardLimit);
  }

  const pressure =
    remaining <= normalizedCaps.softLimit
      ? "ok"
      : remaining <= normalizedCaps.hardLimit
        ? "soft"
        : "hard";

  return { evictIds, remaining, pressure };
};

export const touchSessionCacheEntry = (
  entry: SessionCacheEvictionEntry,
  caps: SessionCacheEvictionCaps = SESSION_CACHE_CAPS,
  nowMs = Date.now(),
): SessionCacheEvictionEntry => {
  const normalizedCaps = normalizeCaps(caps);
  const updatedAtMs = Math.max(entry.updatedAtMs ?? nowMs, nowMs);
  return {
    ...entry,
    lastAccessMs: nowMs,
    updatedAtMs,
    warmUntilMs: nowMs + normalizedCaps.warmTtlMs,
  };
};
