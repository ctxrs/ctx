export type CachedValue<T> = {
  fetched_at_ms: number;
  value: T;
};

export const ENTITLEMENTS_CACHE_KEY = "ctx_entitlements_snapshot_v1";

export const ENTITLEMENTS_CACHE_TTL_MS = 2 * 60 * 1000;

export function readCachedValue<T>(storage: Storage, key: string): CachedValue<T> | null {
  try {
    const raw = storage.getItem(key);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as CachedValue<T>;
    if (!parsed || typeof parsed !== "object") return null;
    if (typeof parsed.fetched_at_ms !== "number") return null;
    if (!("value" in parsed)) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function writeCachedValue<T>(storage: Storage, key: string, value: T, nowMs = Date.now()): void {
  try {
    const payload: CachedValue<T> = { fetched_at_ms: nowMs, value };
    storage.setItem(key, JSON.stringify(payload));
  } catch {
    // Ignore storage failures (private mode, quota, etc).
  }
}

export function shouldUseCachedValue(cache: CachedValue<unknown>, ttlMs: number, nowMs = Date.now()): boolean {
  if (!cache || typeof cache.fetched_at_ms !== "number") return false;
  return nowMs - cache.fetched_at_ms < ttlMs;
}
