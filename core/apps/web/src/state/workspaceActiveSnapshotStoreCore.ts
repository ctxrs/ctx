import type {
  SessionHeadSnapshot,
  Task,
  WorkspaceActiveSnapshot,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import {
  getDaemonClientConfig,
  subscribeDaemonConfig,
  listWorkspaceArchivedTaskSummaries,
} from "../api/client";
import { isDesktopApp } from "../utils/desktop";
import { type PersistedWorkspaceActiveSnapshotV1 } from "./uiStateStore";
import { emitUiDiagnostic, normalizeDiagnosticErrorMessage } from "./diagnosticsChannel";
import type {
  WorkspaceActiveSnapshotCommand,
  WorkspaceActiveSnapshotPatch,
  WorkspaceActiveSnapshotStreamTelemetry,
} from "./workspaceActiveSnapshotProtocol";
import type { SessionSubscriptionCursor } from "./sessionSubscription";
import { WorkspaceActiveSnapshotStoreState } from "./workspaceActiveSnapshot/storeState";
import { noteWorkspaceStreamReset } from "./foregroundFreshnessTelemetry";
import type {
  WorkspaceActiveSnapshotEventSource,
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "./workspaceActiveSnapshot/storeTypes";
import {
  closeActiveSnapshotStream,
  flushSubscriptions as flushActiveSnapshotSubscriptions,
  getCanonicalStreamUrl,
  injectActiveSnapshotStreamMessage,
  notifyEventListeners,
  setDropActiveSnapshotMessages,
  setE2EEnabled,
  setForegroundSessionId,
  setSubscribedSessions,
  unwrapEvent,
  type WorkspaceActiveSnapshotControlHost,
} from "./workspaceActiveSnapshot/controls";
import {
  applySessionSummaryDelta as applyWorkspaceSessionSummaryDelta,
  applyWorkspaceSnapshot as applyWorkspaceStreamSnapshot,
  connectStream as connectWorkspaceStream,
  enqueueStreamMessage as enqueueWorkspaceStreamMessage,
  fetchArchivedPage as fetchArchivedWorkspacePage,
  handleStreamMessage as handleWorkspaceStreamMessage,
  openWebSocket as openWorkspaceStreamWebSocket,
  scheduleReconnect as scheduleWorkspaceStreamReconnect,
  type WorkspaceActiveSnapshotStreamHost,
} from "./workspaceActiveSnapshot/streamRuntime";
import {
  applyTaskUpdate as applyWorkerTaskUpdate,
  destroyWorkerRuntime,
  ensureWorkerAvailable,
  flushWorkerPatchNow,
  hydrateFromCache,
  isForegroundSessionEvent,
  postWorkerCommand,
  schedulePersistCache,
  scheduleWorkerPatchFlush,
  seedCachedSnapshot,
  startWorker,
  updateAuthConfig as updateWorkerAuthConfig,
  type WorkspaceActiveSnapshotWorkerHost,
} from "./workspaceActiveSnapshot/workerRuntime";
import { applyWorkerPatch as applyWorkspaceWorkerPatch } from "./workspaceActiveSnapshot/workerPatchQueue";
import type { WorkerAuthUpdateConfig } from "./workspaceActiveSnapshot/workerConnection";
export type {
  WorkspaceActiveSnapshotEventSource,
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "./workspaceActiveSnapshot/storeTypes";

type WorkspaceActiveSnapshotStoreOptions = {
  disableCache?: boolean;
  disableWorker?: boolean;
  e2eEnabled?: boolean;
  onPersistRequested?: () => void;
  onPatch?: (patch: WorkspaceActiveSnapshotPatch) => void;
  onStreamTelemetry?: (telemetry: WorkspaceActiveSnapshotStreamTelemetry) => void;
  patchFlushMs?: number;
  authToken?: string | null;
  wsBaseUrl?: string | null;
  listWorkspaceArchivedTaskSummaries?: typeof listWorkspaceArchivedTaskSummaries;
};

const SNAPSHOT_WAIT_MS = 1200;
const WORKSPACE_PATCH_FLUSH_MS = 50;
export class WorkspaceActiveSnapshotStoreImpl implements WorkspaceActiveSnapshotEventSource {
  private listeners = new Set<() => void>();
  eventListeners = new Set<(event: WorkspaceActiveSnapshotEvent) => void>();
  readonly state: WorkspaceActiveSnapshotStoreState;
  worker: Worker | null = null;
  private workerStarting = false;
  private workerAuthReconcileInFlight = false;
  private pendingWorkerAuthUpdate: WorkerAuthUpdateConfig | null = null;
  private workerConnectionSeq = 0;
  private useWorker = false;
  private disableCache = false;
  private disableWorker = false;
  e2eEnabled = false;
  e2eDropStreamMessages = false;
  private persistNotifier: (() => void) | null = null;
  workerPatchEmitter: ((patch: WorkspaceActiveSnapshotPatch) => void) | null = null;
  streamTelemetryEmitter: ((telemetry: WorkspaceActiveSnapshotStreamTelemetry) => void) | null = null;
  private workerPatchTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  workerPatchPendingEvents: WorkspaceActiveSnapshotEvent[] = [];
  private workerPatchPendingPersist = false;
  private workerPatchDirty = false;
  private workerPatchOldestEventReceivedAtMs: number | null = null;
  private workerPatchOldestForegroundEventReceivedAtMs: number | null = null;
  private workerPatchFlushMs = WORKSPACE_PATCH_FLUSH_MS;
  private workerPatchFlushSeq = 0;
  private lastWorkerPatchSnapshot: WorkspaceActiveSnapshotState | null = null;
  private lastWorkerPatchSessionHeads: Record<string, SessionHeadSnapshot> = {};
  private lastWorkerPatchWorktreeRoots: Record<string, string> = {};
  private lastWorkerPatchSnapshotRev = -1;
  authTokenOverride: string | null = null;
  wsBaseUrlOverride: string | null = null;
  canonicalStreamUrl: string | null = null;
  lastSubscriptionKey: string | null = null;
  private configUnsubscribe: (() => void) | null = null;
  private listWorkspaceArchivedTaskSummariesFn: typeof listWorkspaceArchivedTaskSummaries;
  subscribedSessions: SessionSubscriptionCursor[] = [];
  foregroundSessionId: string | null = null;
  ws: WebSocket | null = null;
  private connecting = false;
  private reconnectTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private reconnectDelayMs = 1000;
  private lastStreamSeq = 0;
  private allowSnapshotReset = false;
  private cacheHydrated = false;
  private pendingWorkerCache: PersistedWorkspaceActiveSnapshotV1 | null = null;
  private snapshotWaitTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private cachePersistTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private streamQueue: Promise<void> = Promise.resolve();
  destroyed = false;

  constructor(readonly workspaceId: string, opts?: WorkspaceActiveSnapshotStoreOptions) {
    this.state = new WorkspaceActiveSnapshotStoreState(workspaceId);
    this.disableCache = opts?.disableCache ?? false;
    this.disableWorker = Boolean(opts?.disableWorker) || isDesktopApp();
    this.e2eEnabled = opts?.e2eEnabled ?? false;
    this.persistNotifier = opts?.onPersistRequested ?? null;
    this.workerPatchEmitter = opts?.onPatch ?? null;
    this.streamTelemetryEmitter = opts?.onStreamTelemetry ?? null;
    this.workerPatchFlushMs = opts?.patchFlushMs ?? WORKSPACE_PATCH_FLUSH_MS;
    this.authTokenOverride = opts?.authToken ?? null;
    this.wsBaseUrlOverride = opts?.wsBaseUrl ?? null;
    this.listWorkspaceArchivedTaskSummariesFn =
      opts?.listWorkspaceArchivedTaskSummaries ?? listWorkspaceArchivedTaskSummaries;
  }

  private ensureWorkerAvailable() {
    ensureWorkerAvailable(this as unknown as WorkspaceActiveSnapshotWorkerHost);
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  subscribeEvents = (listener: (event: WorkspaceActiveSnapshotEvent) => void): (() => void) => {
    this.eventListeners.add(listener);
    return () => this.eventListeners.delete(listener);
  };

  getSnapshot = (): WorkspaceActiveSnapshotState => this.state.getSnapshot();

  getSessionHeadSnapshot = (sessionId: string): SessionHeadSnapshot | null => {
    return this.state.getSessionHeadSnapshot(sessionId);
  };

  getWorktreeRoot = (worktreeId: string): string | null => {
    return this.state.getWorktreeRoot(worktreeId);
  };

  getSessionHeadsSnapshot = (): Record<string, SessionHeadSnapshot> => {
    return this.state.getSessionHeadsSnapshot();
  };

  getWorktreeRootsSnapshot = (): Record<string, string> => {
    return this.state.getWorktreeRootsSnapshot();
  };

  getSnapshotRev = (): number => this.state.getSnapshotRev();

  setE2EEnabled = (enabled: boolean) => setE2EEnabled(this, enabled);

  e2eCloseActiveSnapshotStream = () => closeActiveSnapshotStream(this);

  e2eSetDropActiveSnapshotMessages = (drop: boolean) =>
    setDropActiveSnapshotMessages(this, drop);

  e2eInjectActiveSnapshotStreamMessage = (data: unknown): boolean =>
    injectActiveSnapshotStreamMessage(this, data);

  e2eGetCanonicalStreamUrl = (): string | null => getCanonicalStreamUrl(this);

  setSubscribedSessions = (sessions: SessionSubscriptionCursor[]) =>
    setSubscribedSessions(this, sessions);

  setForegroundSessionId = (sessionId: string | null) =>
    setForegroundSessionId(this, sessionId);

  init = () => {
    this.destroyed = false;
    const cachePromise = this.disableCache ? Promise.resolve() : this.hydrateFromCache();
    if (this.disableWorker) {
      this.connectStream().catch(() => {});
      return;
    }
    if (!this.configUnsubscribe && typeof window !== "undefined") {
      this.configUnsubscribe = subscribeDaemonConfig((config) => {
        this.updateAuthConfig({
          authToken: config.authToken ?? null,
          wsBaseUrl: config.wsBaseUrl ?? null,
          baseUrl: config.baseUrl ?? null,
          runId: config.runId ?? null,
        });
      });
    }
    this.ensureWorkerAvailable();
    cachePromise.finally(() => {
      if (!this.destroyed) {
        this.startWorker().catch(() => {});
      }
    });
  };

  private async startWorker() {
    await startWorker(this as unknown as WorkspaceActiveSnapshotWorkerHost);
  }

  updateAuthConfig = (opts: {
    authToken?: string | null;
    wsBaseUrl?: string | null;
    baseUrl?: string | null;
    runId?: string | null;
  }) => updateWorkerAuthConfig(this as unknown as WorkspaceActiveSnapshotWorkerHost, opts);

  postWorkerCommand(cmd: WorkspaceActiveSnapshotCommand) {
    postWorkerCommand(this as unknown as WorkspaceActiveSnapshotWorkerHost, cmd);
  }

  private getForegroundSessionId(): string {
    return String(this.foregroundSessionId ?? "").trim();
  }

  private isForegroundSessionEvent(evt: WorkspaceActiveSnapshotEvent): boolean {
    return isForegroundSessionEvent(this.getForegroundSessionId(), evt, this.subscribedSessions);
  }

  destroy = () => {
    this.destroyed = true;
    destroyWorkerRuntime(this as unknown as WorkspaceActiveSnapshotWorkerHost);
    if (this.configUnsubscribe) {
      this.configUnsubscribe();
      this.configUnsubscribe = null;
    }
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        // ignore
      }
      this.ws = null;
    }
    if (this.reconnectTimer) {
      globalThis.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.clearSnapshotWarning();
    this.listeners.clear();
    this.eventListeners.clear();
  };

  loadMoreActive = () => {
    return;
  };

  ensureArchivedLoaded = () => {
    if (this.getSnapshot().archivedLoaded || this.state.getFetchState("archived") === "loading") return;
    if (this.worker) {
      this.postWorkerCommand({ type: "ensure_archived_loaded" });
      return;
    }
    this.fetchArchivedPage(true).catch(() => {});
  };

  loadMoreArchived = () => {
    if (!this.getSnapshot().hasMoreArchived || this.state.getFetchState("archived") === "loading") return;
    if (this.worker) {
      this.postWorkerCommand({ type: "load_more_archived" });
      return;
    }
    this.fetchArchivedPage(false).catch(() => {});
  };

  applyTaskUpdate(task: Task) {
    applyWorkerTaskUpdate(this as unknown as WorkspaceActiveSnapshotWorkerHost, task);
  }

  seedCachedSnapshot(cached: PersistedWorkspaceActiveSnapshotV1) {
    seedCachedSnapshot(this as unknown as WorkspaceActiveSnapshotWorkerHost, cached);
  }

  private async hydrateFromCache() {
    await hydrateFromCache(this as unknown as WorkspaceActiveSnapshotWorkerHost);
  }

  scheduleSnapshotWarning(reason: string) {
    if (this.destroyed) return;
    const snapshot = this.getSnapshot();
    if (snapshot.initialized && (reason === "ws_open" || reason === "ready")) {
      return;
    }
    if (this.snapshotWaitTimer) {
      globalThis.clearTimeout(this.snapshotWaitTimer);
    }
    this.snapshotWaitTimer = globalThis.setTimeout(() => {
      this.snapshotWaitTimer = null;
      if (this.destroyed) return;
      emitUiDiagnostic({
        source: "workspace_stream",
        code: "workspace.snapshot_wait_timeout",
        severity: "warning",
        message: "Workspace active snapshot was not received from the stream in time.",
        context: {
          workspaceId: this.workspaceId,
          reason,
          snapshotRev: this.state.getSnapshotRev(),
          connection: this.getSnapshot().connection,
        },
      });
      console.error(
        `[ctx] Workspace active snapshot not received over WS (${reason}).`,
        {
          workspaceId: this.workspaceId,
          snapshotRev: this.state.getSnapshotRev(),
          connection: this.getSnapshot().connection,
        },
      );
    }, SNAPSHOT_WAIT_MS);
  }

  private clearSnapshotWarning() {
    if (!this.snapshotWaitTimer) return;
    globalThis.clearTimeout(this.snapshotWaitTimer);
    this.snapshotWaitTimer = null;
  }

  private schedulePersistCache() {
    schedulePersistCache(this as unknown as WorkspaceActiveSnapshotWorkerHost);
  }

  scheduleWorkerPatchFlush() {
    scheduleWorkerPatchFlush(this as unknown as WorkspaceActiveSnapshotWorkerHost);
  }

  flushWorkerPatchNow(prioritySessionIds?: readonly string[]) {
    flushWorkerPatchNow(this as unknown as WorkspaceActiveSnapshotWorkerHost, prioritySessionIds);
  }

  applyWorkerPatch(patch: WorkspaceActiveSnapshotPatch) {
    applyWorkspaceWorkerPatch(this as unknown as WorkspaceActiveSnapshotWorkerHost, patch);
  }

  private applyWorkspaceSnapshot(snapshot: WorkspaceActiveSnapshot, heads?: SessionHeadSnapshot[] | null) {
    applyWorkspaceStreamSnapshot(this as unknown as WorkspaceActiveSnapshotStreamHost, snapshot, heads);
  }

  private async fetchArchivedPage(firstLoad: boolean) {
    await fetchArchivedWorkspacePage(this as unknown as WorkspaceActiveSnapshotStreamHost, firstLoad);
  }

  private async connectStream() {
    await connectWorkspaceStream(this as unknown as WorkspaceActiveSnapshotStreamHost);
  }

  private openWebSocket(url: string) {
    return openWorkspaceStreamWebSocket(
      this as unknown as WorkspaceActiveSnapshotStreamHost,
      url,
    );
  }

  private scheduleReconnect() {
    scheduleWorkspaceStreamReconnect(this as unknown as WorkspaceActiveSnapshotStreamHost);
  }

  enqueueStreamMessage(data: unknown) {
    enqueueWorkspaceStreamMessage(this as unknown as WorkspaceActiveSnapshotStreamHost, data);
  }

  private async handleStreamMessage(
    input: unknown | { data: unknown; receivedAtMs: number },
  ): Promise<void> {
    await handleWorkspaceStreamMessage(
      this as unknown as WorkspaceActiveSnapshotStreamHost,
      input,
    );
  }

  private applySessionSummaryDelta(evt: WorkspaceActiveSnapshotEvent & { type: "session_summary_delta" }) {
    return applyWorkspaceSessionSummaryDelta(
      this as unknown as WorkspaceActiveSnapshotStreamHost,
      evt,
    );
  }

  private notifyEventListeners(evt: WorkspaceActiveSnapshotEvent) {
    notifyEventListeners(this, evt);
  }

  flushSubscriptions = (reason = "subscribe") =>
    flushActiveSnapshotSubscriptions(this, reason);

  publish() {
    if (this.workerPatchEmitter) {
      this.workerPatchDirty = true;
      this.scheduleWorkerPatchFlush();
    }
    for (const listener of this.listeners) {
      listener();
    }
  }
}
