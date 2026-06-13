import type { WorkspaceArchivedPage, WorkspaceIndexCursor } from "@ctx/types";
import { workerFetchJson, setWorkerClientConfig } from "../api/workerClient";
import type { SessionSubscriptionCursor } from "../state/sessionSubscription";
import { WorkspaceActiveSnapshotStoreImpl } from "../state/workspaceActiveSnapshotStoreCore";
import type {
  WorkspaceActiveSnapshotCommand,
  WorkspaceActiveSnapshotWorkerMessage,
} from "../state/workspaceActiveSnapshotProtocol";
import type { PersistedWorkspaceActiveSnapshotV1 } from "../state/uiStateStore";

const HEARTBEAT_INTERVAL_MS = 1000;
const HEARTBEAT_TIMEOUT_MS = 3000;

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

const setAuth = (baseUrl?: string | null, authToken?: string | null, runId?: string | null) => {
  setWorkerClientConfig({ baseUrl, authToken, runId });
};

const idToString = (id: string | null | undefined): string => {
  if (id === null || id === undefined) return "";
  if (typeof id !== "string") {
    throw new Error("Expected id to be a string");
  }
  return id;
};

let latestConnectionSeq = -1;

const listWorkspaceArchivedTaskSummaries = (
  workspaceId: string,
  params?: { limit?: number; cursor?: WorkspaceIndexCursor | null },
): Promise<WorkspaceArchivedPage> => {
  const search = new URLSearchParams();
  if (params?.limit) search.set("limit", String(params.limit));
  if (params?.cursor) {
    const cursorSortAt = String(params.cursor.sort_at ?? "").trim();
    const cursorTaskId = idToString(params.cursor.task_id);
    if (cursorSortAt) search.set("cursor_sort_at", cursorSortAt);
    if (cursorTaskId) search.set("cursor_task_id", cursorTaskId);
  }
  const qs = search.toString();
  const suffix = qs ? `?${qs}` : "";
  const path = `/api/workspaces/${workspaceId}/archived_task_summaries${suffix}`;
  return workerFetchJson<WorkspaceArchivedPage>(path);
};

let store: WorkspaceActiveSnapshotStoreImpl | null = null;
let pendingSeed: PersistedWorkspaceActiveSnapshotV1 | null = null;
let pendingSubscribedSessions: SessionSubscriptionCursor[] | null = null;
let pendingForegroundSessionId: string | null = null;
let heartbeatTimer: ReturnType<typeof globalThis.setInterval> | null = null;
let heartbeatDegraded = false;
const pendingHeartbeatSentAtByToken = new Map<string, number>();

const evaluateHeartbeat = () => {
  if (pendingHeartbeatSentAtByToken.size === 0) {
    heartbeatDegraded = false;
    return;
  }
  const currentMs = nowMs();
  let oldestOutstandingMs = currentMs;
  for (const sentAtMs of pendingHeartbeatSentAtByToken.values()) {
    oldestOutstandingMs = Math.min(oldestOutstandingMs, sentAtMs);
  }
  const missedForMs = Math.max(0, currentMs - oldestOutstandingMs);
  if (missedForMs < HEARTBEAT_TIMEOUT_MS || heartbeatDegraded) {
    if (missedForMs < HEARTBEAT_TIMEOUT_MS) {
      heartbeatDegraded = false;
    }
    return;
  }
  heartbeatDegraded = true;
  const message: WorkspaceActiveSnapshotWorkerMessage = {
    type: "heartbeat_missed",
    missedForMs,
    outstandingAcks: pendingHeartbeatSentAtByToken.size,
  };
  self.postMessage(message);
};

const emitHeartbeatPing = () => {
  const sentAtMs = nowMs();
  const token = `${Math.round(sentAtMs)}-${Math.random().toString(16).slice(2)}`;
  pendingHeartbeatSentAtByToken.set(token, sentAtMs);
  const message: WorkspaceActiveSnapshotWorkerMessage = {
    type: "heartbeat_ping",
    token,
    sentAtMs,
  };
  self.postMessage(message);
  evaluateHeartbeat();
};

const startHeartbeat = () => {
  if (heartbeatTimer !== null) return;
  heartbeatTimer = globalThis.setInterval(() => {
    emitHeartbeatPing();
  }, HEARTBEAT_INTERVAL_MS);
};

const ensureStore = (cmd: Extract<WorkspaceActiveSnapshotCommand, { type: "init" }>) => {
  if (cmd.connectionSeq < latestConnectionSeq) return;
  latestConnectionSeq = cmd.connectionSeq;
  setAuth(cmd.baseUrl, cmd.authToken, cmd.runId);
  if (store) return;
  store = new WorkspaceActiveSnapshotStoreImpl(cmd.workspaceId, {
    disableCache: true,
    disableWorker: true,
    authToken: cmd.authToken ?? null,
    wsBaseUrl: cmd.wsBaseUrl ?? null,
    e2eEnabled: cmd.e2eEnabled ?? false,
    listWorkspaceArchivedTaskSummaries,
    onPatch: (patch) => {
      const message: WorkspaceActiveSnapshotWorkerMessage = { type: "patch", patch };
      self.postMessage(message);
    },
    onStreamTelemetry: (telemetry) => {
      const message: WorkspaceActiveSnapshotWorkerMessage = {
        type: "stream_event_telemetry",
        telemetry,
      };
      self.postMessage(message);
    },
  });
  store.init();
  if (pendingSeed) {
    store.seedCachedSnapshot(pendingSeed);
    pendingSeed = null;
  }
  if (pendingSubscribedSessions) {
    store.setSubscribedSessions?.(pendingSubscribedSessions);
    pendingSubscribedSessions = null;
  }
  if (pendingForegroundSessionId !== null) {
    store.setForegroundSessionId?.(pendingForegroundSessionId);
    pendingForegroundSessionId = null;
  }
  startHeartbeat();
};

self.onmessage = (event: MessageEvent<WorkspaceActiveSnapshotCommand>) => {
  const cmd = event.data;
  if (!cmd) return;
  switch (cmd.type) {
    case "init":
      ensureStore(cmd);
      return;
    case "update_auth":
      if (cmd.connectionSeq < latestConnectionSeq) return;
      latestConnectionSeq = cmd.connectionSeq;
      setAuth(cmd.baseUrl, cmd.authToken, cmd.runId);
      store?.updateAuthConfig({
        authToken: cmd.authToken ?? null,
        wsBaseUrl: cmd.wsBaseUrl ?? null,
      });
      return;
    case "seed_cache":
      if (!store) {
        pendingSeed = cmd.snapshot;
        return;
      }
      store.seedCachedSnapshot(cmd.snapshot);
      return;
    case "set_subscribed_sessions":
      if (!store) {
        pendingSubscribedSessions = cmd.sessions;
        return;
      }
      store.setSubscribedSessions?.(cmd.sessions);
      return;
    case "set_foreground_session_id":
      if (!store) {
        pendingForegroundSessionId = cmd.sessionId;
        return;
      }
      store.setForegroundSessionId?.(cmd.sessionId);
      return;
    case "ensure_archived_loaded":
      store?.ensureArchivedLoaded();
      return;
    case "load_more_archived":
      store?.loadMoreArchived();
      return;
    case "apply_task_update":
      store?.applyTaskUpdate(cmd.task);
      return;
    case "e2e_set_enabled":
      store?.setE2EEnabled(cmd.enabled);
      return;
    case "e2e_close_stream":
      store?.e2eCloseActiveSnapshotStream();
      return;
    case "e2e_set_drop_messages":
      store?.e2eSetDropActiveSnapshotMessages(cmd.drop);
      return;
    case "e2e_inject_stream_message":
      store?.e2eInjectActiveSnapshotStreamMessage(cmd.data);
      return;
    case "heartbeat_ack":
      pendingHeartbeatSentAtByToken.delete(cmd.token);
      evaluateHeartbeat();
      return;
    default:
      return;
  }
};
