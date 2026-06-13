import {
  IDLE_UPDATE_VERSION_STORAGE_KEY,
  PROMPT_SNOOZE_STORAGE_KEY,
  RESTART_READY_DISMISSED_VERSION_STORAGE_KEY,
  RESTART_REQUIRED_VERSION_STORAGE_KEY,
} from "./constants";

const readVersionSet = (key: string): Set<string> => {
  if (typeof window === "undefined") return new Set<string>();
  try {
    const raw = window.localStorage.getItem(key);
    if (!raw) return new Set<string>();
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set<string>();
    const versions = parsed.map((value) => String(value).trim()).filter(Boolean);
    return new Set<string>(versions);
  } catch {
    return new Set<string>();
  }
};

const writeVersionSet = (key: string, versions: Set<string>) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(key, JSON.stringify(Array.from(versions.values())));
  } catch {
    // ignore local storage failures
  }
};

export const readIdleUpdateVersions = (): Set<string> =>
  readVersionSet(IDLE_UPDATE_VERSION_STORAGE_KEY);

export const writeIdleUpdateVersions = (versions: Set<string>) =>
  writeVersionSet(IDLE_UPDATE_VERSION_STORAGE_KEY, versions);

export const readRestartRequiredVersion = (): string => {
  if (typeof window === "undefined") return "";
  try {
    return String(window.sessionStorage.getItem(RESTART_REQUIRED_VERSION_STORAGE_KEY) ?? "").trim();
  } catch {
    return "";
  }
};

export const writeRestartRequiredVersion = (version: string): void => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.setItem(RESTART_REQUIRED_VERSION_STORAGE_KEY, version);
  } catch {
    // ignore storage failures
  }
};

export const clearRestartRequiredVersion = (): void => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(RESTART_REQUIRED_VERSION_STORAGE_KEY);
  } catch {
    // ignore storage failures
  }
};

export const readRestartReadyDismissedVersion = (): string => {
  if (typeof window === "undefined") return "";
  try {
    return String(
      window.sessionStorage.getItem(RESTART_READY_DISMISSED_VERSION_STORAGE_KEY) ?? "",
    ).trim();
  } catch {
    return "";
  }
};

export const writeRestartReadyDismissedVersion = (version: string): void => {
  if (typeof window === "undefined") return;
  const normalized = String(version || "").trim();
  if (!normalized) return;
  try {
    window.sessionStorage.setItem(
      RESTART_READY_DISMISSED_VERSION_STORAGE_KEY,
      normalized,
    );
  } catch {
    // ignore storage failures
  }
};

export const clearRestartReadyDismissedVersion = (): void => {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(RESTART_READY_DISMISSED_VERSION_STORAGE_KEY);
  } catch {
    // ignore storage failures
  }
};

export const readPromptSnoozeByVersion = (): Record<string, number> => {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(PROMPT_SNOOZE_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const next: Record<string, number> = {};
    for (const [key, value] of Object.entries(parsed)) {
      const version = String(key || "").trim();
      if (!version) continue;
      const millis = Number(value);
      if (!Number.isFinite(millis) || millis <= 0) continue;
      next[version] = Math.floor(millis);
    }
    return next;
  } catch {
    return {};
  }
};

export const writePromptSnoozeByVersion = (value: Record<string, number>) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(PROMPT_SNOOZE_STORAGE_KEY, JSON.stringify(value));
  } catch {
    // ignore local storage failures
  }
};
