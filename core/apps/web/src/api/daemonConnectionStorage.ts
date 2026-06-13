import {
  cloneDaemonTargetScope,
  createBrowserDaemonTargetScope,
  createDesktopLocalDaemonTargetScope,
  daemonTargetScopeFromDesktopConnectionInfo,
  deserializeDaemonTargetScope,
  serializeDaemonTargetScope,
  type DaemonTargetScope,
} from "../state/scopeIdentity";
import { isMobileShellApp } from "../utils/runtime";
import {
  type DaemonConnection,
  type DesktopDaemonConnectionInfoLike,
  type MobileSecureConnection,
  type ParsedPersistedDaemonBase,
  type ParsedStoredDaemonConnection,
  type PersistedDaemonBaseV1,
  type SetDaemonConnectionOptions,
  type StoredDaemonConnectionV1,
} from "./daemonConnection.types";
import {
  isDesktopWindow,
  normalizeDaemonBaseUrl,
  normalizeDaemonWsBaseUrl,
  normalizeRunId,
  normalizeToken,
} from "./daemonConnectionUrl";

const SESSION_CONNECTION_KEY = "ctxDaemonConnectionV1";
const LOCAL_PERSISTED_BASE_KEY = "ctxDaemonConnectionBaseV1";
const MOBILE_PERSISTED_CONNECTION_KEY = "ctxMobileDaemonConnectionV1";
const RUN_ID_KEY = "ctxRunId";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object";

const inferLegacyDaemonTargetScope = (
  baseUrl: string | null,
  source: string | null | undefined,
): DaemonTargetScope | null => {
  if (!baseUrl) return null;
  if (source === "desktop" || isDesktopWindow()) {
    return createDesktopLocalDaemonTargetScope();
  }
  return createBrowserDaemonTargetScope(baseUrl);
};

const isDesktopTargetScope = (
  scope: DaemonTargetScope | null | undefined,
): boolean => scope?.kind === "desktop_local" || scope?.kind === "desktop_ssh";

const shouldPersistSessionAuthToken = (
  source: string | null | undefined,
  targetScope: DaemonTargetScope | null | undefined,
): boolean => source !== "desktop" && !isDesktopTargetScope(targetScope);

const parseStoredTargetScope = (
  value: unknown,
  fallbackBaseUrl: string | null,
  fallbackSource: string | null | undefined,
): DaemonTargetScope | null | undefined => {
  if (value === undefined) {
    return inferLegacyDaemonTargetScope(fallbackBaseUrl, fallbackSource);
  }
  if (value === null) {
    return fallbackBaseUrl ? undefined : null;
  }
  if (typeof value !== "string") return undefined;
  const targetScope = deserializeDaemonTargetScope(value);
  return targetScope ?? undefined;
};

export const daemonTargetScopeFromDesktopConnectionLike = (
  info: DesktopDaemonConnectionInfoLike | null | undefined,
): DaemonTargetScope | null => {
  if (!info) return null;
  const fromBridge = info.kind
    ? daemonTargetScopeFromDesktopConnectionInfo({
        kind: info.kind,
        host: info.host,
        user: info.user,
        remote_port: info.remote_port,
        remote_data_dir: info.remote_data_dir,
      })
    : null;
  if (fromBridge) return fromBridge;
  return info.base_url ? createDesktopLocalDaemonTargetScope() : null;
};

const readSession = (key: string): string | null => {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
};

const writeSession = (key: string, value: string | null) => {
  try {
    if (value === null) {
      sessionStorage.removeItem(key);
    } else {
      sessionStorage.setItem(key, value);
    }
  } catch {
    // ignore
  }
};

const readLocal = (key: string): string | null => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};

const writeLocal = (key: string, value: string | null) => {
  try {
    if (value === null) {
      localStorage.removeItem(key);
    } else {
      localStorage.setItem(key, value);
    }
  } catch {
    // ignore
  }
};

const parseStoredConnection = (value: string | null): ParsedStoredDaemonConnection | null => {
  if (!value) return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    if (!isRecord(parsed)) return null;
    if (parsed.v !== 1) return null;
    const baseUrl = normalizeDaemonBaseUrl(String(parsed.baseUrl ?? ""));
    const wsBaseUrl = normalizeDaemonWsBaseUrl(parsed.wsBaseUrl as string | null | undefined, baseUrl);
    const authToken = normalizeToken(parsed.authToken as string | null | undefined);
    const source = normalizeToken(parsed.source as string | null | undefined);
    const targetScope = parseStoredTargetScope(parsed.targetScope, baseUrl, source);
    const mobileSecure = parseStoredMobileSecureConnection(parsed.mobileSecure);
    if (targetScope === undefined) return null;
    if (mobileSecure === undefined) return null;
    return {
      baseUrl,
      wsBaseUrl,
      authToken: shouldPersistSessionAuthToken(source, targetScope) ? authToken : null,
      source,
      targetScope,
      mobileSecure,
    };
  } catch {
    return null;
  }
};

const parseStoredMobileSecureConnection = (
  value: unknown,
): MobileSecureConnection | null | undefined => {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return undefined;
  if (value.kind !== "managed_tunnel") return undefined;
  const deviceId = normalizeToken(typeof value.deviceId === "string" ? value.deviceId : null);
  const daemonPublicKey = normalizeToken(
    typeof value.daemonPublicKey === "string" ? value.daemonPublicKey : null,
  );
  const pairingRequestEncryption = normalizeToken(
    typeof value.pairingRequestEncryption === "string" ? value.pairingRequestEncryption : null,
  );
  const nextSeq = value.nextSeq;
  if (!deviceId || !daemonPublicKey || !pairingRequestEncryption) return undefined;
  if (typeof nextSeq !== "number" || !Number.isInteger(nextSeq) || nextSeq < 1) {
    return undefined;
  }
  return {
    kind: "managed_tunnel",
    deviceId,
    daemonPublicKey,
    pairingRequestEncryption,
    nextSeq,
  };
};

const parsePersistedBase = (value: string | null): ParsedPersistedDaemonBase | null => {
  if (!value) return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    if (!isRecord(parsed)) return null;
    if (parsed.v !== 1) return null;
    const baseUrl = normalizeDaemonBaseUrl(String(parsed.baseUrl ?? ""));
    const wsBaseUrl = normalizeDaemonWsBaseUrl(parsed.wsBaseUrl as string | null | undefined, baseUrl);
    const targetScope = parseStoredTargetScope(parsed.targetScope, baseUrl, "persisted_base");
    if (targetScope === undefined) return null;
    return {
      baseUrl,
      wsBaseUrl,
      targetScope,
    };
  } catch {
    return null;
  }
};

export const readStoredDaemonConnection = (): ParsedStoredDaemonConnection | null =>
  parseStoredConnection(readSession(SESSION_CONNECTION_KEY));

export const readStoredPersistedBase = (): ParsedPersistedDaemonBase | null =>
  parsePersistedBase(readLocal(LOCAL_PERSISTED_BASE_KEY));

export const readStoredMobileDaemonConnection = (): ParsedStoredDaemonConnection | null =>
  parseStoredConnection(readLocal(MOBILE_PERSISTED_CONNECTION_KEY));

export const readRunId = (): string | null => normalizeRunId(readSession(RUN_ID_KEY));

export const writeCanonicalSession = (connection: DaemonConnection) => {
  const serialized: StoredDaemonConnectionV1 = {
    v: 1,
    baseUrl: connection.baseUrl,
    wsBaseUrl: connection.wsBaseUrl,
    authToken: shouldPersistSessionAuthToken(
      connection.source ?? null,
      connection.targetScope ?? null,
    )
      ? connection.authToken
      : null,
    source: connection.source ?? null,
    targetScope: connection.targetScope ? serializeDaemonTargetScope(connection.targetScope) : null,
    mobileSecure: connection.mobileSecure ?? null,
  };
  writeSession(SESSION_CONNECTION_KEY, JSON.stringify(serialized));
};

export const persistBaseIfRequested = (
  connection: DaemonConnection,
  opts?: SetDaemonConnectionOptions,
) => {
  if (opts?.clearPersistedBaseUrl) {
    writeLocal(LOCAL_PERSISTED_BASE_KEY, null);
    return;
  }
  if (!opts?.persistBaseUrl) return;
  if (!connection.baseUrl) {
    writeLocal(LOCAL_PERSISTED_BASE_KEY, null);
    return;
  }
  const persisted: PersistedDaemonBaseV1 = {
    v: 1,
    baseUrl: connection.baseUrl,
    wsBaseUrl: connection.wsBaseUrl,
    targetScope: connection.targetScope ? serializeDaemonTargetScope(connection.targetScope) : null,
  };
  writeLocal(LOCAL_PERSISTED_BASE_KEY, JSON.stringify(persisted));
};

export const persistMobileConnectionIfRequested = (
  connection: DaemonConnection,
  opts?: SetDaemonConnectionOptions,
) => {
  if (opts?.clearPersistedAuthToken) {
    writeLocal(MOBILE_PERSISTED_CONNECTION_KEY, null);
    return;
  }
  if (!opts?.persistAuthToken || !isMobileShellApp()) return;
  if (!connection.baseUrl || (!connection.authToken && !connection.mobileSecure)) {
    writeLocal(MOBILE_PERSISTED_CONNECTION_KEY, null);
    return;
  }
  const persisted: StoredDaemonConnectionV1 = {
    v: 1,
    baseUrl: connection.baseUrl,
    wsBaseUrl: connection.wsBaseUrl,
    authToken: connection.authToken,
    source: connection.source ?? null,
    targetScope: connection.targetScope ? serializeDaemonTargetScope(connection.targetScope) : null,
    mobileSecure: connection.mobileSecure ?? null,
  };
  writeLocal(MOBILE_PERSISTED_CONNECTION_KEY, JSON.stringify(persisted));
};

export const cloneNullableTargetScope = (scope: DaemonTargetScope | null | undefined): DaemonTargetScope | null =>
  scope ? cloneDaemonTargetScope(scope) : null;
