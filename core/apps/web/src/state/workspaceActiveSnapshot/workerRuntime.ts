import type {
  SessionHeadSnapshot,
  Task,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import { getDaemonConnectionReadiness } from "../../api/client";
import { getAnalyticsSurface } from "../../utils/analytics/context";
import { getInstallId } from "../../utils/analytics/identity";
import { trackRendererHeartbeatMissed } from "../../utils/analytics";
import { isDesktopApp } from "../../utils/desktop";
import type {
  WorkspaceActiveSnapshotCommand,
  WorkspaceActiveSnapshotPatch,
  WorkspaceActiveSnapshotStreamTelemetry,
  WorkspaceActiveSnapshotStreamSource,
  WorkspaceActiveSnapshotWorkerMessage,
} from "../workspaceActiveSnapshotProtocol";
import {
  noteClientReceiveLag,
  noteWorkspaceStreamEventObserved,
} from "../foregroundFreshnessTelemetry";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import {
  loadWorkspaceActiveSnapshotV1,
  type PersistedWorkspaceActiveSnapshotV1,
} from "../uiStateStore";
import type { WorkspaceActiveSnapshotStoreState } from "./storeState";
import type { WorkspaceActiveSnapshotState } from "./storeTypes";
import {
  resolveWorkerConnectionState,
  type WorkerAuthUpdateConfig,
} from "./workerConnection";
import {
  applyWorkerPatch,
  flushWorkerPatchNow,
  isForegroundSessionEvent,
  resetWorkerPatchQueue,
  schedulePersistCache,
  scheduleWorkerPatchFlush,
} from "./workerPatchQueue";

type WorkspaceStreamTelemetrySample = {
  lane: "foreground" | "workspace";
  eventType: string;
  sessionId: string | null;
  emittedAtMs: number | null;
  receivedAtMs: number;
  streamSource: WorkspaceActiveSnapshotStreamSource;
};

type WindowWithWorkspaceStreamTelemetry = Window & {
  __ctxWorkspaceStreamTelemetrySamples?: WorkspaceStreamTelemetrySample[];
};

const recordWorkspaceStreamTelemetryForE2E = (
  host: WorkspaceActiveSnapshotWorkerHost,
  telemetry: WorkspaceActiveSnapshotStreamTelemetry,
): void => {
  if (!host.e2eEnabled || typeof window === "undefined") return;
  const win = window as WindowWithWorkspaceStreamTelemetry;
  const samples = win.__ctxWorkspaceStreamTelemetrySamples ?? [];
  if (samples.length >= 20_000) samples.shift();
  samples.push({
    lane: telemetry.lane,
    eventType: telemetry.eventType,
    sessionId: telemetry.sessionId,
    emittedAtMs: telemetry.emittedAtMs,
    receivedAtMs: telemetry.receivedAtMs,
    streamSource: telemetry.streamSource,
  });
  win.__ctxWorkspaceStreamTelemetrySamples = samples;
};

export type WorkspaceActiveSnapshotWorkerHost = {
  workspaceId: string;
  destroyed: boolean;
  disableWorker: boolean;
  e2eEnabled: boolean;
  authTokenOverride: string | null;
  wsBaseUrlOverride: string | null;
  state: WorkspaceActiveSnapshotStoreState;
  worker: Worker | null;
  workerStarting: boolean;
  workerAuthReconcileInFlight: boolean;
  pendingWorkerAuthUpdate: WorkerAuthUpdateConfig | null;
  workerConnectionSeq: number;
  useWorker: boolean;
  canonicalStreamUrl: string | null;
  workerPatchEmitter: ((patch: WorkspaceActiveSnapshotPatch) => void) | null;
  streamTelemetryEmitter: ((telemetry: WorkspaceActiveSnapshotStreamTelemetry) => void) | null;
  workerPatchTimer: ReturnType<typeof globalThis.setTimeout> | null;
  workerPatchPendingEvents: WorkspaceActiveSnapshotEvent[];
  workerPatchPendingPersist: boolean;
  workerPatchDirty: boolean;
  workerPatchOldestEventReceivedAtMs: number | null;
  workerPatchOldestForegroundEventReceivedAtMs: number | null;
  workerPatchFlushMs: number;
  workerPatchFlushSeq: number;
  lastWorkerPatchSnapshot: WorkspaceActiveSnapshotState | null;
  lastWorkerPatchSessionHeads: Record<string, SessionHeadSnapshot>;
  lastWorkerPatchWorktreeRoots: Record<string, string>;
  lastWorkerPatchSnapshotRev: number;
  cacheHydrated: boolean;
  pendingWorkerCache: PersistedWorkspaceActiveSnapshotV1 | null;
  cachePersistTimer: ReturnType<typeof globalThis.setTimeout> | null;
  subscribedSessions: SessionSubscriptionCursor[];
  foregroundSessionId: string | null;
  ws: WebSocket | null;
  publish(): void;
  notifyEventListeners(evt: WorkspaceActiveSnapshotEvent): void;
  connectStream(): Promise<void>;
};

export const ensureWorkerAvailable = (host: WorkspaceActiveSnapshotWorkerHost): void => {
  if (host.disableWorker) return;
  if (typeof Worker === "undefined") {
    throw new Error("Workspace active snapshot requires Worker support.");
  }
};

export const postWorkerCommand = (
  host: WorkspaceActiveSnapshotWorkerHost,
  cmd: WorkspaceActiveSnapshotCommand,
): void => {
  if (!host.worker) return;
  host.worker.postMessage(cmd);
};

export const startWorker = async (host: WorkspaceActiveSnapshotWorkerHost): Promise<void> => {
  if (host.worker || host.destroyed || host.workerStarting) return;
  host.workerStarting = true;
  try {
    const connection = await resolveWorkerConnectionState({
      workspaceId: host.workspaceId,
      phase: "worker_init",
      authTokenOverride: host.authTokenOverride,
      wsBaseUrlOverride: host.wsBaseUrlOverride,
    });
    if (host.worker || host.destroyed) return;
    if (isDesktopApp() && !getDaemonConnectionReadiness(connection).isReady) return;

    host.useWorker = true;
    host.worker = new Worker(
      new URL("../../workers/workspaceActiveSnapshot.worker.ts", import.meta.url),
      { type: "module" },
    );
    host.worker.onmessage = (event: MessageEvent<WorkspaceActiveSnapshotWorkerMessage>) => {
      const msg = event.data;
      if (!msg) return;
      if (msg.type === "patch") {
        applyWorkerPatch(host, msg.patch);
        return;
      }
      if (msg.type === "stream_event_telemetry") {
        recordWorkspaceStreamTelemetryForE2E(host, msg.telemetry);
        noteWorkspaceStreamEventObserved(msg.telemetry.lane, msg.telemetry.eventType);
        if (
          typeof msg.telemetry.emittedAtMs === "number" &&
          msg.telemetry.streamSource === "live"
        ) {
          noteClientReceiveLag(
            msg.telemetry.lane,
            msg.telemetry.receivedAtMs - msg.telemetry.emittedAtMs,
            {
              stream_source: msg.telemetry.streamSource,
              worker_source: "workspace_worker",
              event_type: msg.telemetry.eventType,
              workspace_id: host.workspaceId,
              session_id: msg.telemetry.sessionId,
            },
          );
        }
        return;
      }
      if (msg.type === "heartbeat_ping") {
        postWorkerCommand(host, { type: "heartbeat_ack", token: msg.token });
        return;
      }
      if (msg.type === "heartbeat_missed") {
        trackRendererHeartbeatMissed({
          source: "workspace_active_worker",
          missedForMs: msg.missedForMs,
          outstandingAcks: msg.outstandingAcks,
        });
      }
    };
    postWorkerCommand(host, {
      type: "init",
      workspaceId: host.workspaceId,
      connectionSeq: ++host.workerConnectionSeq,
      authToken: connection.authToken,
      baseUrl: connection.baseUrl,
      wsBaseUrl: connection.wsBaseUrl || null,
      runId: connection.runId,
      installId: getInstallId(),
      originRuntime: getAnalyticsSurface(),
      e2eEnabled: host.e2eEnabled,
    });
    if (host.subscribedSessions.length > 0) {
      postWorkerCommand(host, {
        type: "set_subscribed_sessions",
        sessions: host.subscribedSessions.slice(),
      });
    }
    if (host.foregroundSessionId) {
      postWorkerCommand(host, {
        type: "set_foreground_session_id",
        sessionId: host.foregroundSessionId,
      });
    }
    if (host.pendingWorkerCache) {
      postWorkerCommand(host, { type: "seed_cache", snapshot: host.pendingWorkerCache });
      host.pendingWorkerCache = null;
    }
  } finally {
    host.workerStarting = false;
  }
};

export const queueWorkerAuthUpdate = (
  host: WorkspaceActiveSnapshotWorkerHost,
  opts: WorkerAuthUpdateConfig,
): void => {
  host.pendingWorkerAuthUpdate = {
    authToken: opts.authToken ?? null,
    wsBaseUrl: opts.wsBaseUrl ?? null,
    baseUrl: opts.baseUrl,
    runId: opts.runId ?? null,
  };
  if (host.workerAuthReconcileInFlight || host.destroyed) return;
  host.workerAuthReconcileInFlight = true;
  void runWorkerAuthReconcileLoop(host);
};

const runWorkerAuthReconcileLoop = async (
  host: WorkspaceActiveSnapshotWorkerHost,
): Promise<void> => {
  try {
    while (!host.destroyed) {
      const pending = host.pendingWorkerAuthUpdate;
      if (!pending) return;
      host.pendingWorkerAuthUpdate = null;
      const connection = await resolveWorkerConnectionState({
        workspaceId: host.workspaceId,
        phase: "worker_update_auth",
        authTokenOverride: host.authTokenOverride,
        wsBaseUrlOverride: host.wsBaseUrlOverride,
        opts: pending,
      });
      if (!host.worker || host.destroyed) return;
      if (host.pendingWorkerAuthUpdate) continue;
      postWorkerCommand(host, {
        type: "update_auth",
        connectionSeq: ++host.workerConnectionSeq,
        authToken: connection.authToken,
        baseUrl: connection.baseUrl,
        wsBaseUrl: connection.wsBaseUrl,
        runId: connection.runId,
      });
    }
  } finally {
    host.workerAuthReconcileInFlight = false;
    if (!host.destroyed && host.pendingWorkerAuthUpdate) {
      host.workerAuthReconcileInFlight = true;
      void runWorkerAuthReconcileLoop(host);
    }
  }
};

export const updateAuthConfig = (
  host: WorkspaceActiveSnapshotWorkerHost,
  opts: {
    authToken?: string | null;
    wsBaseUrl?: string | null;
    baseUrl?: string | null;
    runId?: string | null;
  },
): void => {
  const nextAuth = opts.authToken ?? null;
  const nextWs = opts.wsBaseUrl ?? null;
  const authChanged = host.authTokenOverride !== nextAuth;
  const wsChanged = host.wsBaseUrlOverride !== nextWs;
  host.authTokenOverride = nextAuth;
  host.wsBaseUrlOverride = nextWs;
  if (authChanged || wsChanged) {
    host.canonicalStreamUrl = null;
  }

  if (host.worker) {
    queueWorkerAuthUpdate(host, {
      authToken: nextAuth,
      wsBaseUrl: nextWs,
      baseUrl: opts.baseUrl,
      runId: opts.runId ?? null,
    });
    return;
  }

  if (!host.disableWorker) {
    if (!host.destroyed) {
      void startWorker(host).catch(() => {});
    }
    return;
  }
  if (!authChanged && !wsChanged) return;
  if (host.ws) {
    try {
      host.ws.close();
    } catch {
      // ignore
    }
    host.ws = null;
    if (host.state.setConnection("disconnected")) {
      host.publish();
    }
  }
  if (!host.destroyed) {
    void host.connectStream().catch(() => {});
  }
};

export const destroyWorkerRuntime = (host: WorkspaceActiveSnapshotWorkerHost): void => {
  host.pendingWorkerAuthUpdate = null;
  host.workerAuthReconcileInFlight = false;
  host.workerConnectionSeq = 0;
  host.useWorker = false;
  host.workerStarting = false;
  host.pendingWorkerCache = null;
  if (host.worker) {
    host.worker.terminate();
    host.worker = null;
  }
  resetWorkerPatchQueue(host);
};

export const applyTaskUpdate = (
  host: WorkspaceActiveSnapshotWorkerHost,
  task: Task,
): void => {
  if (host.worker) {
    postWorkerCommand(host, { type: "apply_task_update", task });
    return;
  }
  if (!host.state.applyTaskUpdate(task)) return;
  host.publish();
  schedulePersistCache(host);
};

export const seedCachedSnapshot = (
  host: WorkspaceActiveSnapshotWorkerHost,
  cached: PersistedWorkspaceActiveSnapshotV1,
): void => {
  if (!cached || host.destroyed) return;
  try {
    host.state.applyCachedSnapshot(cached);
    host.publish();
  } catch {
    // ignore invalid cache payloads
  }
};

export const hydrateFromCache = async (
  host: WorkspaceActiveSnapshotWorkerHost,
): Promise<void> => {
  if (host.cacheHydrated) return;
  host.cacheHydrated = true;
  try {
    const cached = await loadWorkspaceActiveSnapshotV1(host.workspaceId);
    if (!cached || host.destroyed || host.state.hasLiveSnapshotApplied()) return;
    host.state.applyCachedSnapshot(cached);
    host.publish();
    if (host.worker) {
      postWorkerCommand(host, { type: "seed_cache", snapshot: cached });
    } else if (!host.disableWorker) {
      host.pendingWorkerCache = cached;
    }
  } catch {
    // ignore cache errors
  }
};

export {
  applyWorkerPatch,
  flushWorkerPatchNow,
  isForegroundSessionEvent,
  schedulePersistCache,
  scheduleWorkerPatchFlush,
};
