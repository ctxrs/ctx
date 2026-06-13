import {
  createBrowserDaemonTargetScope,
  type DaemonTargetScope,
  sameDaemonTargetScope,
} from "../state/scopeIdentity";
import {
  cloneNullableTargetScope,
  daemonTargetScopeFromDesktopConnectionLike,
  persistBaseIfRequested,
  persistMobileConnectionIfRequested,
  readRunId,
  readStoredDaemonConnection,
  readStoredMobileDaemonConnection,
  readStoredPersistedBase,
  writeCanonicalSession,
} from "./daemonConnectionStorage";
import {
  type DaemonConnection,
  type DaemonConnectionReadiness,
  type DaemonConnectionUpdate,
  type DesktopDaemonConnectionInfoLike,
  type MobileSecureConnection,
  type SetDaemonConnectionOptions,
} from "./daemonConnection.types";
import {
  deriveDaemonWsBaseUrl,
  isDesktopWindow,
  normalizeDaemonBaseUrl,
  normalizeDaemonWsBaseUrl,
  normalizeRunId,
  normalizeToken,
} from "./daemonConnectionUrl";
import { isMobileShellApp } from "../utils/runtime";

type DaemonConnectionListener = (connection: DaemonConnection) => void;

const listeners = new Set<DaemonConnectionListener>();

const sameNullableTargetScope = (
  lhs: DaemonTargetScope | null | undefined,
  rhs: DaemonTargetScope | null | undefined,
): boolean => {
  if (lhs === rhs) return true;
  if (!lhs || !rhs) return false;
  return sameDaemonTargetScope(lhs, rhs);
};

const cloneMobileSecureConnection = (
  connection: MobileSecureConnection | null | undefined,
): MobileSecureConnection | null => connection ? { ...connection } : null;

const sameMobileSecureConnection = (
  lhs: MobileSecureConnection | null | undefined,
  rhs: MobileSecureConnection | null | undefined,
): boolean => {
  if (lhs === rhs) return true;
  if (!lhs || !rhs) return false;
  return lhs.kind === rhs.kind
    && lhs.deviceId === rhs.deviceId
    && lhs.daemonPublicKey === rhs.daemonPublicKey
    && lhs.pairingRequestEncryption === rhs.pairingRequestEncryption
    && lhs.nextSeq === rhs.nextSeq;
};

export const isMobileSecureConnection = (
  connection: MobileSecureConnection | null | undefined,
): connection is MobileSecureConnection =>
  connection?.kind === "managed_tunnel"
  && Boolean(connection.deviceId)
  && Boolean(connection.daemonPublicKey)
  && Boolean(connection.pairingRequestEncryption)
  && Number.isInteger(connection.nextSeq)
  && connection.nextSeq >= 1;

const initialConnection = (): DaemonConnection => {
  const canonical = readStoredDaemonConnection();
  if (canonical) {
    const restored: DaemonConnection = {
      baseUrl: canonical.baseUrl,
      wsBaseUrl: canonical.wsBaseUrl,
      authToken: canonical.authToken,
      runId: readRunId(),
      source: canonical.source ?? null,
      targetScope: cloneNullableTargetScope(canonical.targetScope),
      mobileSecure: cloneMobileSecureConnection(canonical.mobileSecure),
    };
    writeCanonicalSession(restored);
    return restored;
  }

  if (isMobileShellApp()) {
    const mobilePersisted = readStoredMobileDaemonConnection();
    if (
      mobilePersisted?.baseUrl
      && (mobilePersisted.authToken || isMobileSecureConnection(mobilePersisted.mobileSecure))
    ) {
      const restored: DaemonConnection = {
        baseUrl: mobilePersisted.baseUrl,
        wsBaseUrl: mobilePersisted.wsBaseUrl,
        authToken: mobilePersisted.authToken,
        runId: readRunId(),
        source: mobilePersisted.source ?? "mobile_persisted",
        targetScope: cloneNullableTargetScope(mobilePersisted.targetScope),
        mobileSecure: cloneMobileSecureConnection(mobilePersisted.mobileSecure),
      };
      writeCanonicalSession(restored);
      return restored;
    }
  }

  const persisted = readStoredPersistedBase();
  const baseUrl = persisted?.baseUrl ?? null;
  const wsBaseUrl = normalizeDaemonWsBaseUrl(persisted?.wsBaseUrl ?? null, baseUrl);
  const restored: DaemonConnection = {
    baseUrl,
    wsBaseUrl,
    authToken: null,
    runId: readRunId(),
    source: baseUrl ? "persisted_base" : null,
    targetScope: cloneNullableTargetScope(persisted?.targetScope ?? null),
    mobileSecure: null,
  };
  if (baseUrl) {
    writeCanonicalSession(restored);
    return restored;
  }
  if (!isDesktopWindow() && typeof window !== "undefined") {
    const protocol = String(window.location.protocol || "").toLowerCase();
    if (protocol === "http:" || protocol === "https:") {
      const sameOrigin = normalizeDaemonBaseUrl(window.location.origin);
      if (sameOrigin) {
        const seeded: DaemonConnection = {
          baseUrl: sameOrigin,
          wsBaseUrl: deriveDaemonWsBaseUrl(sameOrigin),
          authToken: null,
          runId: readRunId(),
          source: "same_origin_bootstrap",
          targetScope: createBrowserDaemonTargetScope(sameOrigin),
          mobileSecure: null,
        };
        writeCanonicalSession(seeded);
        return seeded;
      }
    }
  }
  return restored;
};

let state: DaemonConnection = initialConnection();

export const resetDaemonConnectionStateForTests = (): DaemonConnection => {
  listeners.clear();
  state = initialConnection();
  return getDaemonConnection();
};

const areSameConnection = (a: DaemonConnection, b: DaemonConnection): boolean =>
  a.baseUrl === b.baseUrl
  && a.wsBaseUrl === b.wsBaseUrl
  && a.authToken === b.authToken
  && a.runId === b.runId
  && (a.source ?? null) === (b.source ?? null)
  && sameMobileSecureConnection(a.mobileSecure, b.mobileSecure)
  && sameNullableTargetScope(a.targetScope, b.targetScope);

const notifyListeners = () => {
  if (listeners.size === 0) return;
  const snapshot = getDaemonConnection();
  for (const listener of listeners) {
    listener(snapshot);
  }
};

export const getDaemonConnection = (): DaemonConnection => {
  const runId = readRunId();
  if (state.runId !== runId) {
    state = { ...state, runId };
  }
  return {
    ...state,
    targetScope: cloneNullableTargetScope(state.targetScope),
    mobileSecure: cloneMobileSecureConnection(state.mobileSecure),
  };
};

export const getDaemonConnectionReadiness = (
  connection: Pick<DaemonConnection, "baseUrl" | "authToken" | "mobileSecure"> = getDaemonConnection(),
): DaemonConnectionReadiness => {
  const hasBaseUrl = Boolean(connection.baseUrl);
  const hasAuthToken = Boolean(connection.authToken);
  const hasMobileSecure = isMobileSecureConnection(connection.mobileSecure);
  return {
    hasBaseUrl,
    hasAuthToken,
    hasMobileSecure,
    isReady: hasBaseUrl && (hasAuthToken || hasMobileSecure),
    missing: !hasBaseUrl ? "base" : !hasAuthToken && !hasMobileSecure ? "auth" : null,
  };
};

export const hasReadyDaemonConnection = (
  connection: Pick<DaemonConnection, "baseUrl" | "authToken" | "mobileSecure"> = getDaemonConnection(),
): boolean => getDaemonConnectionReadiness(connection).isReady;

export const subscribeDaemonConnection = (listener: DaemonConnectionListener): (() => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

export const setDaemonConnection = (
  update: DaemonConnectionUpdate,
  opts?: SetDaemonConnectionOptions,
): DaemonConnection => {
  const current = getDaemonConnection();
  const nextBase = update.baseUrl !== undefined
    ? normalizeDaemonBaseUrl(update.baseUrl)
    : current.baseUrl;
  const nextWs = update.wsBaseUrl !== undefined
    ? normalizeDaemonWsBaseUrl(update.wsBaseUrl, nextBase)
    : update.baseUrl !== undefined
      ? deriveDaemonWsBaseUrl(nextBase)
      : current.wsBaseUrl;
  const nextTargetScope = update.targetScope !== undefined
    ? cloneNullableTargetScope(update.targetScope)
    : update.baseUrl !== undefined
      ? nextBase
        ? isDesktopWindow() && current.targetScope && current.targetScope.kind !== "browser"
          ? cloneNullableTargetScope(current.targetScope)
          : createBrowserDaemonTargetScope(nextBase)
        : null
      : cloneNullableTargetScope(current.targetScope);
  const next: DaemonConnection = {
    baseUrl: nextBase,
    wsBaseUrl: nextWs,
    authToken: update.authToken !== undefined ? normalizeToken(update.authToken) : current.authToken,
    runId: update.runId !== undefined ? normalizeRunId(update.runId) : current.runId,
    source: update.source !== undefined ? normalizeToken(update.source) : current.source ?? null,
    targetScope: nextTargetScope,
    mobileSecure: update.mobileSecure !== undefined
      ? cloneMobileSecureConnection(update.mobileSecure)
      : nextBase
        ? cloneMobileSecureConnection(current.mobileSecure)
        : null,
  };

  writeCanonicalSession(next);
  persistMobileConnectionIfRequested(next, opts);
  persistBaseIfRequested(next, opts);

  if (!areSameConnection(current, next)) {
    state = next;
    notifyListeners();
  }
  return getDaemonConnection();
};

export const clearDaemonConnection = (opts?: SetDaemonConnectionOptions): DaemonConnection => {
  return setDaemonConnection(
    {
      baseUrl: null,
      wsBaseUrl: null,
      authToken: null,
      source: "cleared",
      targetScope: null,
      mobileSecure: null,
    },
    {
      persistBaseUrl: opts?.persistBaseUrl,
      clearPersistedBaseUrl: opts?.clearPersistedBaseUrl ?? true,
      persistAuthToken: opts?.persistAuthToken,
      clearPersistedAuthToken: opts?.clearPersistedAuthToken ?? true,
    },
  );
};

const isLoopbackHost = (host: string): boolean => {
  const normalized = host.replace(/^\[|\]$/g, "").toLowerCase();
  return normalized === "localhost" || normalized === "::1" || normalized.startsWith("127.");
};

const getBrowserSameOriginBaseUrl = (): string | null => {
  if (typeof window === "undefined" || isDesktopWindow()) return null;
  const protocol = String(window.location.protocol || "").toLowerCase();
  if (protocol !== "http:" && protocol !== "https:") return null;
  return normalizeDaemonBaseUrl(window.location.origin);
};

const stripQueryTokenFromLocation = (): void => {
  const params = new URLSearchParams(window.location.search);
  if (!params.has("token")) return;
  params.delete("token");
  const next =
    window.location.pathname
    + (params.toString() ? `?${params.toString()}` : "")
    + window.location.hash;
  window.history.replaceState({}, "", next);
};

const consumeFragmentTokenFromLocation = (): { token: string | null; hadTokenParam: boolean } => {
  const rawHash = String(window.location.hash || "").replace(/^#/, "");
  if (!rawHash) {
    return { token: null, hadTokenParam: false };
  }
  const normalizedHash = rawHash.startsWith("?") ? rawHash.slice(1) : rawHash;
  const params = new URLSearchParams(normalizedHash);
  if (!params.has("token")) {
    return { token: null, hadTokenParam: false };
  }
  const token = normalizeToken(params.get("token"));
  params.delete("token");
  const nextHash = params.toString();
  const next =
    window.location.pathname
    + window.location.search
    + (nextHash ? `#${nextHash}` : "");
  window.history.replaceState({}, "", next);
  return { token, hadTokenParam: Boolean(token) };
};

export const bootstrapDaemonConnectionFromRuntime = () => {
  if (typeof window === "undefined") return;
  stripQueryTokenFromLocation();
  const { token: tokenFromFragment, hadTokenParam } = consumeFragmentTokenFromLocation();
  const envToken = import.meta.env.DEV ? normalizeToken(import.meta.env.VITE_CTX_AUTH_TOKEN) : null;
  const envDaemonUrl = import.meta.env.DEV
    ? normalizeDaemonBaseUrl(import.meta.env.VITE_CTX_DAEMON_URL ?? null)
    : null;
  if (tokenFromFragment) {
    const sameOriginBaseUrl = getBrowserSameOriginBaseUrl();
    const current = getDaemonConnection();
    const shouldResetBrowserBaseFromToken =
      !envDaemonUrl
      && Boolean(sameOriginBaseUrl)
      && current.targetScope?.kind === "browser"
      && current.baseUrl !== sameOriginBaseUrl;
    setDaemonConnection(
      {
        baseUrl: shouldResetBrowserBaseFromToken ? sameOriginBaseUrl : undefined,
        authToken: tokenFromFragment,
        source: "url_token",
      },
      shouldResetBrowserBaseFromToken ? { clearPersistedBaseUrl: true } : undefined,
    );
  }

  if (import.meta.env.DEV) {
    if (envToken || envDaemonUrl) {
      const host = String(window.location.hostname ?? "").toLowerCase();
      if (isLoopbackHost(host)) {
        setDaemonConnection(
          {
            authToken: envToken && !hadTokenParam ? envToken : undefined,
            baseUrl: envDaemonUrl ?? undefined,
            source: "dev_env",
          },
          { persistBaseUrl: Boolean(envDaemonUrl) },
        );
      }
    }
  }

  const latest = getDaemonConnection();
  if (!latest.baseUrl && !isDesktopWindow()) {
    const sameOriginBaseUrl = getBrowserSameOriginBaseUrl();
    if (sameOriginBaseUrl) {
      setDaemonConnection({ baseUrl: sameOriginBaseUrl, source: "same_origin_bootstrap" });
    }
  }
};

export const applyDesktopDaemonConnection = (
  info: DesktopDaemonConnectionInfoLike | null | undefined,
): DaemonConnection => {
  return setDaemonConnection(
    {
      baseUrl: info?.base_url ?? null,
      authToken: info?.browser_query_secret ?? null,
      source: "desktop",
      targetScope: daemonTargetScopeFromDesktopConnectionLike(info),
    },
    { persistBaseUrl: true },
  );
};

export const getDaemonWsUrl = (path: string, query?: URLSearchParams): string => {
  const connection = getDaemonConnection();
  if (!connection.wsBaseUrl) {
    throw new Error("Daemon websocket base URL is not configured.");
  }
  const prefix = connection.wsBaseUrl.replace(/\/+$/, "");
  const pathname = path.startsWith("/") ? path : `/${path}`;
  const qs = query?.toString();
  return qs ? `${prefix}${pathname}?${qs}` : `${prefix}${pathname}`;
};

export const getDaemonHttpUrl = (path: string): string => {
  const connection = getDaemonConnection();
  if (!connection.baseUrl) return path;
  if (path.startsWith("http://") || path.startsWith("https://")) return path;
  const prefix = connection.baseUrl.replace(/\/+$/, "");
  const pathname = path.startsWith("/") ? path : `/${path}`;
  return `${prefix}${pathname}`;
};

export {
  deriveDaemonWsBaseUrl,
  normalizeDaemonBaseUrl,
  normalizeDaemonWsBaseUrl,
};

export type {
  DaemonConnection,
  DaemonConnectionReadiness,
  DaemonConnectionUpdate,
  DesktopDaemonConnectionInfoLike,
  SetDaemonConnectionOptions,
};
