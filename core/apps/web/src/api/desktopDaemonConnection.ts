import {
  desktopConnectLocal,
  desktopGetConnection,
  isDesktopApp,
  type DesktopConnectionInfo,
} from "../utils/desktop";
import { emitUiDiagnostic, normalizeDiagnosticErrorMessage } from "../state/diagnosticsChannel";
import {
  applyDesktopDaemonConnection,
  getDaemonConnection,
  getDaemonHttpUrl,
  hasReadyDaemonConnection,
  type DaemonConnection,
} from "./daemonConnection";
import { buildDaemonRequestHeaders } from "./daemonRequestHeaders";

export type DesktopDaemonConnectionSyncResult = {
  connection: DaemonConnection;
  info: DesktopConnectionInfo | null;
  synced: boolean;
  error: string | null;
};

export type DesktopDaemonConnectionSyncOptions = {
  force?: boolean;
  connectLocalWhenMissing?: boolean;
  reason?: string;
};

const DESKTOP_DAEMON_SYNC_THROTTLE_MS = 1000;
const DESKTOP_LOCAL_AUTH_PROBE_TIMEOUT_MS = 5000;

let desktopSyncInFlight: Promise<DesktopDaemonConnectionSyncResult> | null = null;
let desktopLastSyncAtMs = 0;

export const resetDesktopDaemonConnectionSyncForTests = (): void => {
  desktopSyncInFlight = null;
  desktopLastSyncAtMs = 0;
};

const makeDesktopSyncResult = (
  info: DesktopConnectionInfo | null,
  error: string | null,
): DesktopDaemonConnectionSyncResult => ({
  connection: getDaemonConnection(),
  info,
  synced: Boolean(info),
  error,
});

const shouldConnectLocalWhenMissing = (
  info: DesktopConnectionInfo | null,
  connectLocalWhenMissing: boolean,
): boolean => {
  if (!connectLocalWhenMissing) return false;
  if (!info?.local_auto_bootstrap_allowed) return false;
  if (info.kind === "ssh") return false;
  return !info.base_url;
};

const shouldRepairExistingLocalDesktopTarget = (
  current: DaemonConnection,
  info: DesktopConnectionInfo | null,
): info is DesktopConnectionInfo & { kind: "local"; base_url: string } => {
  return Boolean(
    info
    && info.kind === "local"
    && info.base_url
    && current.targetScope?.kind === "desktop_local"
    && current.baseUrl === info.base_url,
  );
};

const shouldProbeExistingLocalDesktopAuth = (
  current: DaemonConnection,
  info: DesktopConnectionInfo | null,
): info is DesktopConnectionInfo & { kind: "local"; base_url: string } =>
  shouldRepairExistingLocalDesktopTarget(current, info);

const probeDesktopLocalDaemonAuth = async (): Promise<boolean> => {
  const current = getDaemonConnection();
  if (!current.baseUrl || typeof fetch === "undefined") return false;
  let timeoutId: ReturnType<typeof globalThis.setTimeout> | null = null;
  try {
    const request = fetch(getDaemonHttpUrl("/api/workspaces"), {
      method: "GET",
      headers: buildDaemonRequestHeaders({
        token: current.authToken,
      }),
    });
    const response = await Promise.race([
      request,
      new Promise<never>((_, reject) => {
        timeoutId = globalThis.setTimeout(() => {
          reject(new Error("desktop local auth probe timed out"));
        }, DESKTOP_LOCAL_AUTH_PROBE_TIMEOUT_MS);
      }),
    ]);
    return response.status >= 200 && response.status < 300;
  } catch {
    return false;
  } finally {
    if (timeoutId !== null) {
      globalThis.clearTimeout(timeoutId);
    }
  }
};

const hasBrowserQuerySecret = (
  info: DesktopConnectionInfo | null,
): info is DesktopConnectionInfo & { browser_query_secret: string } =>
  Boolean(info?.browser_query_secret);

export const syncDesktopDaemonConnectionFromBridge = async (
  opts?: DesktopDaemonConnectionSyncOptions,
): Promise<DesktopDaemonConnectionSyncResult> => {
  if (!isDesktopApp()) return makeDesktopSyncResult(null, null);
  const now = Date.now();
  const current = getDaemonConnection();
  if (
    !opts?.force
    && hasReadyDaemonConnection(current)
    && now - desktopLastSyncAtMs < DESKTOP_DAEMON_SYNC_THROTTLE_MS
  ) {
    return makeDesktopSyncResult(null, null);
  }
  if (desktopSyncInFlight) return desktopSyncInFlight;
  const run = (async (): Promise<DesktopDaemonConnectionSyncResult> => {
    let info: DesktopConnectionInfo | null = null;
    let error: string | null = null;
    try {
      info = await desktopGetConnection();
      if (shouldConnectLocalWhenMissing(info, opts?.connectLocalWhenMissing ?? false)) {
        info = await desktopConnectLocal();
      } else if (
        shouldRepairExistingLocalDesktopTarget(current, info)
        && !hasBrowserQuerySecret(info)
      ) {
        info = await desktopConnectLocal();
      } else if (shouldProbeExistingLocalDesktopAuth(current, info)) {
        const authOk = await probeDesktopLocalDaemonAuth();
        if (!authOk) {
          info = await desktopConnectLocal();
        }
      }
      applyDesktopDaemonConnection(info);
    } catch (err) {
      error = normalizeDiagnosticErrorMessage(err, "Desktop daemon connection sync failed.");
      if (opts?.reason) {
        emitUiDiagnostic({
          source: "api",
          code: "api.desktop_connection_sync_failed",
          severity: "warning",
          message: `Desktop daemon connection sync failed during ${opts.reason}.`,
          context: { reason: opts.reason, error },
        });
      }
    } finally {
      desktopLastSyncAtMs = Date.now();
    }
    return makeDesktopSyncResult(info, error);
  })();
  desktopSyncInFlight = run;
  try {
    return await run;
  } finally {
    if (desktopSyncInFlight === run) {
      desktopSyncInFlight = null;
    }
  }
};

export const ensureDesktopDaemonConnection = async (
  opts?: DesktopDaemonConnectionSyncOptions,
): Promise<DaemonConnection> => {
  const current = getDaemonConnection();
  if (!isDesktopApp()) return current;
  const synced = await syncDesktopDaemonConnectionFromBridge({
    force: opts?.force ?? !hasReadyDaemonConnection(current),
    connectLocalWhenMissing: opts?.connectLocalWhenMissing ?? true,
    reason: opts?.reason ?? "desktop_transport_bootstrap",
  });
  if (hasReadyDaemonConnection(synced.connection)) {
    return synced.connection;
  }
  throw new Error(synced.error ?? "Desktop daemon connection is not configured.");
};
