import { desktopStorageBatch, desktopStorageGet, isDesktopApp } from "../desktop";

const PENDING_DOWNLOAD_ID_KEY = "ctx.analytics.pending_download_id.v1";
const DOWNLOAD_ID_PARAM = "ctx_download_id";
const DOWNLOAD_ID_PATTERN = /^[A-Za-z0-9._:-]{1,64}$/;

const hasWindow = (): boolean => typeof window !== "undefined";

const hasUriScheme = (value: string): boolean => /^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(value);

export const normalizeDownloadAttributionId = (raw: unknown): string | null => {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (!DOWNLOAD_ID_PATTERN.test(trimmed)) return null;
  return trimmed;
};

export const createDownloadAttributionId = (): string => {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
};

export const appendDownloadAttributionIdToUrl = (
  href: string,
  downloadId: string,
): string => {
  const normalized = normalizeDownloadAttributionId(downloadId);
  if (!normalized || !href.trim()) return href;
  try {
    const parsed = new URL(href, "https://ctx.invalid");
    parsed.searchParams.set(DOWNLOAD_ID_PARAM, normalized);
    if (hasUriScheme(href)) return parsed.toString();
    return `${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    return href;
  }
};

const removeDownloadIdQueryParam = (): void => {
  if (!hasWindow()) return;
  try {
    const current = new URL(window.location.href);
    if (!current.searchParams.has(DOWNLOAD_ID_PARAM)) return;
    current.searchParams.delete(DOWNLOAD_ID_PARAM);
    const next = `${current.pathname}${current.search}${current.hash}`;
    window.history.replaceState(window.history.state, "", next);
  } catch {
    // ignore
  }
};

const readDownloadIdFromQuery = (): string | null => {
  if (!hasWindow()) return null;
  try {
    const current = new URL(window.location.href);
    return normalizeDownloadAttributionId(current.searchParams.get(DOWNLOAD_ID_PARAM));
  } catch {
    return null;
  }
};

const readLocalPendingDownloadId = (): string | null => {
  if (!hasWindow()) return null;
  try {
    const raw = window.localStorage.getItem(PENDING_DOWNLOAD_ID_KEY);
    return normalizeDownloadAttributionId(raw);
  } catch {
    return null;
  }
};

const setLocalPendingDownloadId = (downloadId: string): boolean => {
  if (!hasWindow()) return false;
  const normalized = normalizeDownloadAttributionId(downloadId);
  if (!normalized) return false;
  try {
    window.localStorage.setItem(PENDING_DOWNLOAD_ID_KEY, normalized);
    return true;
  } catch {
    return false;
  }
};

const clearLocalPendingDownloadId = (): void => {
  if (!hasWindow()) return;
  try {
    window.localStorage.removeItem(PENDING_DOWNLOAD_ID_KEY);
  } catch {
    // ignore
  }
};

export const setPendingDownloadAttributionId = async (downloadId: string): Promise<boolean> => {
  const normalized = normalizeDownloadAttributionId(downloadId);
  if (!normalized) return false;
  if (!isDesktopApp()) {
    return setLocalPendingDownloadId(normalized);
  }
  try {
    await desktopStorageBatch([{ kind: "set", key: PENDING_DOWNLOAD_ID_KEY, value: normalized }]);
    return true;
  } catch {
    return setLocalPendingDownloadId(normalized);
  }
};

export const clearPendingDownloadAttributionId = async (): Promise<void> => {
  if (isDesktopApp()) {
    try {
      await desktopStorageBatch([{ kind: "delete", key: PENDING_DOWNLOAD_ID_KEY }]);
    } catch {
      // ignore and fall through to local cleanup
    }
  }
  clearLocalPendingDownloadId();
};

export const getPendingDownloadAttributionId = async (): Promise<string | null> => {
  const fromQuery = readDownloadIdFromQuery();
  if (fromQuery) return fromQuery;

  let pending: string | null = null;
  if (isDesktopApp()) {
    try {
      const raw = await desktopStorageGet(PENDING_DOWNLOAD_ID_KEY);
      pending = normalizeDownloadAttributionId(raw);
    } catch {
      pending = null;
    }
    if (!pending) {
      pending = readLocalPendingDownloadId();
    }
  } else {
    pending = readLocalPendingDownloadId();
  }

  return pending;
};

export const consumePendingDownloadAttributionId = async (): Promise<string | null> => {
  const pending = await getPendingDownloadAttributionId();
  if (!pending) return null;
  if (readDownloadIdFromQuery()) {
    removeDownloadIdQueryParam();
  }
  await clearPendingDownloadAttributionId();
  return pending;
};
