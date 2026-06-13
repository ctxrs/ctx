import { checkUpdates, type UpdateCheck } from "../api/client";
import { readCachedValue, shouldUseCachedValue, writeCachedValue } from "./entitlementsCache";

const UPDATE_CHECK_CACHE_KEY = "ctx_update_check_v1";
const UPDATE_CHECK_TTL_MS = 60 * 60 * 1000;

let inFlight: Promise<UpdateCheck | null> | null = null;

const getStorage = (): Storage | null => {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
};

export const readCachedUpdateCheck = (): UpdateCheck | null => {
  const storage = getStorage();
  if (!storage) return null;
  const cached = readCachedValue<UpdateCheck>(storage, UPDATE_CHECK_CACHE_KEY);
  return cached?.value ?? null;
};

export const writeCachedUpdateCheck = (value: UpdateCheck): void => {
  const storage = getStorage();
  if (!storage) return;
  writeCachedValue(storage, UPDATE_CHECK_CACHE_KEY, value);
};

export const refreshUpdateCheck = async (opts?: { force?: boolean }): Promise<UpdateCheck | null> => {
  const storage = getStorage();
  const cached = storage ? readCachedValue<UpdateCheck>(storage, UPDATE_CHECK_CACHE_KEY) : null;
  if (!opts?.force && cached && shouldUseCachedValue(cached, UPDATE_CHECK_TTL_MS)) {
    return cached.value;
  }
  if (inFlight) return inFlight;

  inFlight = checkUpdates()
    .then((info) => {
      if (storage) writeCachedValue(storage, UPDATE_CHECK_CACHE_KEY, info);
      return info;
    })
    .catch(() => null)
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
};
