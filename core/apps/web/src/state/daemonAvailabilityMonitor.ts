import { daemonFetchRaw } from "../api/client";
import { syncDesktopDaemonConnectionFromBridge } from "../api/desktopDaemonConnection";
import {
  desktopGetVersion,
  isDesktopApp,
  type DesktopConnectionKind,
  type DesktopRemoteDaemonUpdateState,
} from "../utils/desktop";

export type DaemonStatus = "unknown" | "ok" | "down" | "mismatch" | "update_required";
export type VersionMismatchKind = "daemon_older" | "desktop_older" | "unknown";

export type VersionMismatch = {
  desktop_version: string;
  daemon_version: string;
  expected_version: string;
  kind: VersionMismatchKind;
};

export type DaemonUpdateRequired = {
  reason: "local_data_newer";
};

export type DaemonAvailabilitySnapshot = {
  status: DaemonStatus;
  checking: boolean;
  error: string | null;
  desktopKind: DesktopConnectionKind | null;
  desktopVersion: string | null;
  mismatch: VersionMismatch | null;
  updateRequired: DaemonUpdateRequired | null;
  remoteUpdateMessage: string | null;
  remoteUpdateState: DesktopRemoteDaemonUpdateState | null;
};

type Listener = (snapshot: DaemonAvailabilitySnapshot) => void;

const DOWN_POLL_MS = 8000;
const HEALTHY_POLL_MS = 20000;

const listeners = new Set<Listener>();

let snapshot: DaemonAvailabilitySnapshot = {
  status: "unknown",
  checking: false,
  error: null,
  desktopKind: null,
  desktopVersion: null,
  mismatch: null,
  updateRequired: null,
  remoteUpdateMessage: null,
  remoteUpdateState: null,
};

let pollTimer: number | null = null;
let checkInFlight: Promise<DaemonAvailabilitySnapshot> | null = null;
let onlineListenerAttached = false;

const trimError = (value: string): string => {
  const text = String(value || "").trim();
  if (!text) return "";
  return text.length > 220 ? `${text.slice(0, 220)}...` : text;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const extractErrorMessage = (resp: { status: number; body: string }): string => {
  const raw = String(resp.body ?? "").trim();
  if (!raw) return `Daemon responded with ${resp.status}.`;
  try {
    const parsed = JSON.parse(raw);
    const msg = parsed?.error ?? parsed?.message;
    if (typeof msg === "string" && msg.trim()) return trimError(msg);
  } catch {
    // Ignore parse errors and fall back to raw text.
  }
  return trimError(raw);
};

const classifyUpdateRequired = (
  message: string | null | undefined,
): DaemonUpdateRequired | null => {
  const text = String(message ?? "").trim();
  if (!text) return null;
  if (
    /migration\s+\d+\s+was previously applied but is missing in the resolved migrations/i.test(text)
  ) {
    return { reason: "local_data_newer" };
  }
  if (/data (?:on this machine|directory|dir).*newer version of ctx/i.test(text)) {
    return { reason: "local_data_newer" };
  }
  if (/schema .*newer than.*(?:app|client|version)/i.test(text)) {
    return { reason: "local_data_newer" };
  }
  return null;
};

const forcedUpdateRequiredFromDevFlag = (): DaemonUpdateRequired | null => {
  if (!import.meta.env.DEV || typeof window === "undefined") return null;
  const params = new URLSearchParams(window.location.search);
  if (params.get("ctx_force_update_required") !== "1") return null;
  return { reason: "local_data_newer" };
};

const normalizeVersionParts = (value: string): number[] | null => {
  const trimmed = String(value || "").trim();
  if (!trimmed) return null;
  const cleaned = trimmed.replace(/^v/i, "");
  const parts = cleaned.split(".");
  const nums = parts.map((part) => {
    const match = part.match(/^(\d+)/);
    return match ? Number(match[1]) : Number.NaN;
  });
  if (nums.some((n) => Number.isNaN(n))) return null;
  return nums;
};

const normalizeVersionString = (value: string): string => {
  const trimmed = String(value || "").trim();
  if (!trimmed) return "";
  return trimmed.replace(/^v/i, "");
};

const compareVersions = (left: string, right: string): number | null => {
  const leftParts = normalizeVersionParts(left);
  const rightParts = normalizeVersionParts(right);
  if (!leftParts || !rightParts) return null;
  const len = Math.max(leftParts.length, rightParts.length);
  for (let i = 0; i < len; i += 1) {
    const a = leftParts[i] ?? 0;
    const b = rightParts[i] ?? 0;
    if (a < b) return -1;
    if (a > b) return 1;
  }
  return 0;
};

const sameMismatch = (left: VersionMismatch | null, right: VersionMismatch | null): boolean => {
  if (left === right) return true;
  if (!left || !right) return false;
  return left.desktop_version === right.desktop_version
    && left.daemon_version === right.daemon_version
    && left.expected_version === right.expected_version
    && left.kind === right.kind;
};

const sameSnapshot = (
  left: DaemonAvailabilitySnapshot,
  right: DaemonAvailabilitySnapshot,
): boolean =>
  left.status === right.status
  && left.checking === right.checking
  && left.error === right.error
  && left.desktopKind === right.desktopKind
  && left.desktopVersion === right.desktopVersion
  && left.remoteUpdateMessage === right.remoteUpdateMessage
  && left.remoteUpdateState === right.remoteUpdateState
  && left.updateRequired?.reason === right.updateRequired?.reason
  && sameMismatch(left.mismatch, right.mismatch);

const emitChange = (): void => {
  for (const listener of listeners) {
    listener(snapshot);
  }
};

const setSnapshot = (next: DaemonAvailabilitySnapshot): void => {
  if (sameSnapshot(snapshot, next)) return;
  snapshot = next;
  emitChange();
};

const clearPollTimer = (): void => {
  if (pollTimer !== null) {
    window.clearTimeout(pollTimer);
    pollTimer = null;
  }
};

const schedulePoll = (): void => {
  clearPollTimer();
  if (listeners.size === 0 || typeof window === "undefined") return;
  const intervalMs = snapshot.status === "down" || snapshot.status === "mismatch"
    ? DOWN_POLL_MS
    : HEALTHY_POLL_MS;
  pollTimer = window.setTimeout(() => {
    pollTimer = null;
    if (typeof document !== "undefined" && document.visibilityState !== "visible") {
      schedulePoll();
      return;
    }
    void checkDaemonAvailabilityNow();
  }, intervalMs);
};

const syncDesktopMetadata = async (): Promise<{
  desktopKind: DesktopConnectionKind | null;
  desktopVersion: string | null;
  desktopConnectionError: string | null;
  remoteUpdateMessage: string | null;
  remoteUpdateState: DesktopRemoteDaemonUpdateState | null;
}> => {
  if (!isDesktopApp()) {
    return {
      desktopKind: null,
      desktopVersion: null,
      desktopConnectionError: null,
      remoteUpdateMessage: null,
      remoteUpdateState: null,
    };
  }
  let desktopKind: DesktopConnectionKind | null = null;
  let desktopVersion: string | null = null;
  let desktopConnectionError: string | null = null;
  let remoteUpdateMessage: string | null = null;
  let remoteUpdateState: DesktopRemoteDaemonUpdateState | null = null;
  try {
    const sync = await syncDesktopDaemonConnectionFromBridge({
      connectLocalWhenMissing: false,
      reason: "daemon_availability_poll",
    });
    const info = sync.info;
    desktopConnectionError = sync.error;
    desktopKind = info?.kind ?? snapshot.desktopKind ?? null;
    remoteUpdateMessage = typeof info?.remote_update_message === "string"
      ? info.remote_update_message
      : snapshot.remoteUpdateMessage ?? null;
    remoteUpdateState = info?.remote_update_state ?? snapshot.remoteUpdateState ?? null;
  } catch {
    desktopKind = null;
    remoteUpdateMessage = null;
    remoteUpdateState = null;
  }
  try {
    desktopVersion = await desktopGetVersion();
  } catch {
    desktopVersion = null;
  }
  return {
    desktopKind,
    desktopVersion,
    desktopConnectionError,
    remoteUpdateMessage,
    remoteUpdateState,
  };
};

export const getDaemonAvailabilitySnapshot = (): DaemonAvailabilitySnapshot => snapshot;

export const checkDaemonAvailabilityNow = async (): Promise<DaemonAvailabilitySnapshot> => {
  if (checkInFlight) {
    return checkInFlight;
  }

  setSnapshot({
    ...snapshot,
    checking: true,
  });

  const run = (async (): Promise<DaemonAvailabilitySnapshot> => {
    const {
      desktopKind,
      desktopVersion,
      desktopConnectionError,
      remoteUpdateMessage,
      remoteUpdateState,
    } = await syncDesktopMetadata();

    let nextStatus: DaemonStatus = "down";
    let nextError: string | null = null;
    let nextMismatch: VersionMismatch | null = null;
    let nextUpdateRequired: DaemonUpdateRequired | null =
      forcedUpdateRequiredFromDevFlag() ?? classifyUpdateRequired(desktopConnectionError);

    try {
      if (nextUpdateRequired) {
        nextStatus = "update_required";
      } else {
        const resp = await daemonFetchRaw("/api/health", undefined, {
          connectLocalWhenMissing: false,
        });
        if (resp.status >= 200 && resp.status < 300) {
          let parsed: unknown = null;
          if (resp.body) {
            try {
              parsed = JSON.parse(resp.body);
            } catch {
              parsed = null;
            }
          }
          const parsedRecord = asRecord(parsed);
          const compat = asRecord(parsedRecord.compatibility);
          const daemonVersion = String(
            parsedRecord.daemon_version ?? parsedRecord.version ?? "",
          ).trim();
          const expectedVersion = String(
            compat.desktop_exact_version ?? daemonVersion ?? "",
          ).trim();
          const normalizedDesktopVersion = normalizeVersionString(desktopVersion ?? "");
          const normalizedExpectedVersion = normalizeVersionString(expectedVersion);
          const cmp = compareVersions(normalizedDesktopVersion, normalizedExpectedVersion);
          const versionsMatch =
            cmp === 0 || (cmp === null && normalizedDesktopVersion === normalizedExpectedVersion);
          if (
            isDesktopApp()
            && normalizedDesktopVersion
            && normalizedExpectedVersion
            && !versionsMatch
          ) {
            const kind: VersionMismatchKind =
              cmp === 1 ? "daemon_older" : cmp === -1 ? "desktop_older" : "unknown";
            nextStatus = "mismatch";
            nextMismatch = {
              desktop_version: desktopVersion ?? "",
              daemon_version: daemonVersion || expectedVersion,
              expected_version: expectedVersion,
              kind,
            };
          } else {
            nextStatus = "ok";
          }
        } else {
          nextStatus = "down";
          nextError = extractErrorMessage(resp);
          nextUpdateRequired = classifyUpdateRequired(nextError);
          if (nextUpdateRequired) {
            nextStatus = "update_required";
            nextError = null;
          }
        }
      }
    } catch (err) {
      nextStatus = "down";
      const message = err instanceof Error ? err.message : String(err);
      nextError = trimError(message || "Unable to reach the ctx daemon.");
      nextUpdateRequired = classifyUpdateRequired(nextError);
      if (nextUpdateRequired) {
        nextStatus = "update_required";
        nextError = null;
      }
    }

    const nextSnapshot: DaemonAvailabilitySnapshot = {
      status: nextStatus,
      checking: false,
      error: nextError,
      desktopKind,
      desktopVersion,
      mismatch: nextMismatch,
      updateRequired: nextUpdateRequired,
      remoteUpdateMessage,
      remoteUpdateState,
    };
    setSnapshot(nextSnapshot);
    schedulePoll();
    return nextSnapshot;
  })();

  checkInFlight = run;
  try {
    return await run;
  } finally {
    if (checkInFlight === run) {
      checkInFlight = null;
    }
  }
};

const handleOnline = (): void => {
  void checkDaemonAvailabilityNow();
};

const ensureOnlineListener = (): void => {
  if (onlineListenerAttached || typeof window === "undefined") return;
  window.addEventListener("online", handleOnline);
  onlineListenerAttached = true;
};

const removeOnlineListener = (): void => {
  if (!onlineListenerAttached || typeof window === "undefined") return;
  window.removeEventListener("online", handleOnline);
  onlineListenerAttached = false;
};

export const subscribeDaemonAvailability = (
  listener: Listener,
): (() => void) => {
  listeners.add(listener);
  listener(snapshot);
  ensureOnlineListener();
  if (listeners.size === 1) {
    void checkDaemonAvailabilityNow();
  } else {
    schedulePoll();
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      clearPollTimer();
      removeOnlineListener();
    }
  };
};
