import {
  getDaemonClientConfig,
  getDaemonConnection,
  getDaemonConnectionReadiness,
  syncDesktopDaemonConnectionFromBridge,
} from "../../api/client";
import { isDesktopApp } from "../../utils/desktop";
import { emitUiDiagnostic } from "../diagnosticsChannel";
import { toWorkspaceHttpBaseUrl } from "./transport";

export type WorkerAuthUpdateConfig = {
  authToken?: string | null;
  wsBaseUrl?: string | null;
  baseUrl?: string | null;
  runId?: string | null;
};

export type ResolvedWorkerConnectionState = {
  authToken: string | null;
  wsBaseUrl: string | null;
  baseUrl: string | null;
  runId: string | null;
};

const emitWorkerConnectionMissingDiagnostic = (
  workspaceId: string,
  phase: "worker_init" | "worker_update_auth",
  bridgeKind: "none" | "local" | "ssh" | null,
  syncError: string | null,
  missing: "base" | "auth" = "base",
) => {
  const connection = getDaemonConnection();
  const bridgeConnected = bridgeKind === "local" || bridgeKind === "ssh";
  const missingAuth = missing === "auth";
  const code = missingAuth
    ? bridgeConnected
      ? "workspace.worker_desktop_bridge_missing_auth"
      : "workspace.worker_connection_missing"
    : bridgeConnected
      ? "workspace.worker_desktop_bridge_missing_base"
      : "workspace.worker_connection_missing";
  const message = missingAuth
    ? bridgeConnected
      ? "Desktop bridge is connected, but worker daemon auth token is missing."
      : "Worker daemon auth token is missing."
    : bridgeConnected
      ? "Desktop bridge is connected, but worker daemon HTTP base URL is missing."
      : "Worker daemon HTTP base URL is missing.";
  emitUiDiagnostic({
    source: "workspace_snapshot",
    code,
    severity: "warning",
    message,
    context: {
      workspaceId,
      phase,
      missing,
      bridgeKind,
      connectionSource: connection.source ?? null,
      syncError: syncError ?? undefined,
    },
  });
};

export const resolveWorkerConnectionState = async ({
  workspaceId,
  phase,
  authTokenOverride,
  wsBaseUrlOverride,
  opts,
}: {
  workspaceId: string;
  phase: "worker_init" | "worker_update_auth";
  authTokenOverride: string | null;
  wsBaseUrlOverride: string | null;
  opts?: WorkerAuthUpdateConfig;
}): Promise<ResolvedWorkerConnectionState> => {
  let daemonConfig = getDaemonClientConfig();
  let bridgeKind: "none" | "local" | "ssh" | null = null;
  let syncError: string | null = null;

  const readState = (): ResolvedWorkerConnectionState => {
    const authToken = opts?.authToken ?? authTokenOverride ?? daemonConfig.authToken ?? null;
    const wsBaseUrl = opts?.wsBaseUrl ?? wsBaseUrlOverride ?? daemonConfig.wsBaseUrl ?? null;
    const baseUrl =
      opts?.baseUrl ?? daemonConfig.baseUrl ?? (wsBaseUrl ? toWorkspaceHttpBaseUrl(wsBaseUrl) : null);
    const runId = opts?.runId ?? daemonConfig.runId ?? null;
    return { authToken, wsBaseUrl, baseUrl, runId };
  };

  let state = readState();
  let readiness = getDaemonConnectionReadiness(state);
  if (isDesktopApp() && !readiness.isReady) {
    const synced = await syncDesktopDaemonConnectionFromBridge({
      force: true,
      probeHealth: true,
      reason: phase,
    });
    daemonConfig = getDaemonClientConfig();
    bridgeKind = synced.info?.kind ?? null;
    syncError = synced.error;
    state = readState();
    readiness = getDaemonConnectionReadiness(state);
  }

  if (isDesktopApp() && !readiness.isReady) {
    emitWorkerConnectionMissingDiagnostic(
      workspaceId,
      phase,
      bridgeKind,
      syncError,
      readiness.missing ?? "base",
    );
  }
  return state;
};
