import { getInstallId } from "./identity";

const STORAGE_KEY_PREFIX = "ctx.analytics.experiment_exposure.";
const MAX_TRACKED_EXPOSURES = 512;

let loaded = false;
let storageKey: string | null = null;
const trackedExperimentExposures = new Set<string>();

const trimToBound = () => {
  while (trackedExperimentExposures.size > MAX_TRACKED_EXPOSURES) {
    const oldest = trackedExperimentExposures.values().next().value;
    if (!oldest) break;
    trackedExperimentExposures.delete(oldest);
  }
};

const persist = () => {
  if (typeof window === "undefined" || !storageKey) return;
  try {
    window.localStorage.setItem(storageKey, JSON.stringify([...trackedExperimentExposures]));
  } catch {
    // Ignore localStorage write failures; in-memory dedupe still applies for this session.
  }
};

const ensureLoaded = () => {
  if (loaded) return;
  loaded = true;
  if (typeof window === "undefined") return;
  storageKey = `${STORAGE_KEY_PREFIX}${getInstallId()}`;
  try {
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return;
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return;
    for (const value of parsed) {
      if (typeof value === "string" && value) {
        trackedExperimentExposures.add(value);
      }
    }
    trimToBound();
  } catch {
    // Ignore parse/read failures; continue with empty in-memory set.
  }
};

const exposureKey = (gate: string, variant: string) => `${gate}:${variant}`;

export const hasTrackedExperimentExposure = (gate: string, variant: string): boolean => {
  ensureLoaded();
  const key = exposureKey(gate, variant);
  return trackedExperimentExposures.has(key);
};

export const markExperimentExposureTracked = (gate: string, variant: string): void => {
  ensureLoaded();
  const key = exposureKey(gate, variant);
  if (trackedExperimentExposures.has(key)) return;
  trackedExperimentExposures.add(key);
  trimToBound();
  persist();
};
