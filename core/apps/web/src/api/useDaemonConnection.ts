import { useSyncExternalStore } from "react";
import { getDaemonConnection, subscribeDaemonConnection, type DaemonConnection } from "./daemonConnection";

const isSameConnection = (left: DaemonConnection | null, right: DaemonConnection): boolean => {
  if (!left) return false;
  return left.baseUrl === right.baseUrl
    && left.wsBaseUrl === right.wsBaseUrl
    && left.authToken === right.authToken
    && left.runId === right.runId
    && (left.source ?? null) === (right.source ?? null);
};

let cachedConnection: DaemonConnection | null = null;

const readConnectionSnapshot = (): DaemonConnection => {
  const next = getDaemonConnection();
  if (isSameConnection(cachedConnection, next)) {
    return cachedConnection as DaemonConnection;
  }
  cachedConnection = next;
  return next;
};

export const useDaemonConnection = (): DaemonConnection =>
  useSyncExternalStore(subscribeDaemonConnection, readConnectionSnapshot, readConnectionSnapshot);

const readBaseUrlSnapshot = (): string | null => readConnectionSnapshot().baseUrl;

export const useDaemonBaseUrl = (): string | null =>
  useSyncExternalStore(subscribeDaemonConnection, readBaseUrlSnapshot, readBaseUrlSnapshot);
