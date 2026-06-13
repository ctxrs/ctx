import type {
  SessionHeadSnapshot,
  Task,
  WorkspaceActiveSnapshot,
  WorkspaceActiveSnapshotEvent,
  WorkspaceActiveSnapshotSessionSummaryDeltaEvent,
  WorkspaceActiveSnapshotTaskDeltaEvent,
} from "@ctx/types";
import {
  getDaemonClientConfig,
  idToString,
  listWorkspaceArchivedTaskSummaries,
} from "../../api/client";
import { setBrowserStreamQueryToken } from "../../api/browserStreamAuth";
import {
  decryptManagedMobileStreamEnvelope,
  deriveManagedMobileStreamQuery,
  managedMobileStreamPath,
} from "../../api/mobileSecureClient";
import {
  emitUiDiagnostic,
  normalizeDiagnosticErrorMessage,
} from "../diagnosticsChannel";
import {
  noteClientReceiveLag,
  noteQueueAgeSample,
  noteWorkspaceEventAge,
  noteWorkspaceStreamEventObserved,
  noteWorkspaceStreamReset,
} from "../foregroundFreshnessTelemetry";
import {
  markWorkspaceEventReceivedAt,
  markWorkspaceEventStreamSource,
} from "../workspaceEventTelemetry";
import type {
  WorkspaceActiveSnapshotPatch,
  WorkspaceActiveSnapshotStreamTelemetry,
  WorkspaceActiveSnapshotStreamSource,
} from "../workspaceActiveSnapshotProtocol";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import type { WorkspaceActiveSnapshotState } from "./storeTypes";
import { WorkspaceActiveSnapshotStoreState } from "./storeState";
import { parseWsJson } from "../../utils/wsJson";
import {
  readWorkspaceHeadsBatchPayload,
  readWorkspaceSnapshotPayload,
  readWorkspaceStreamRev,
  readWorkspaceStreamSource,
} from "./transport";

const ACTIVE_PAGE_SIZE = 50;
const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

const emittedAtMsForWorkspaceEvent = (
  evt: WorkspaceActiveSnapshotEvent,
): number | null => {
  switch (evt.type) {
    case "session_head_delta":
      return typeof evt.delta.emitted_at_ms === "number" && Number.isFinite(evt.delta.emitted_at_ms)
        ? evt.delta.emitted_at_ms
        : null;
    case "session_summary_delta":
      return typeof evt.delta.emitted_at_ms === "number" && Number.isFinite(evt.delta.emitted_at_ms)
        ? evt.delta.emitted_at_ms
        : null;
    default:
      return null;
  }
};

const noteWorkspaceEventClientReceiveLag = (
  host: WorkspaceActiveSnapshotStreamHost,
  evt: WorkspaceActiveSnapshotEvent,
  receivedAtMs: number,
  source: WorkspaceActiveSnapshotStreamSource,
): void => {
  const emittedAtMs = emittedAtMsForWorkspaceEvent(evt);
  const lane = host.isForegroundSessionEvent(evt) ? "foreground" : "workspace";
  const sessionId =
    evt.type === "session_head_delta"
      ? idToString(evt.delta.session_id)
      : evt.type === "session_summary_delta"
        ? idToString(evt.delta.session_id)
        : evt.type === "session_summary"
          ? idToString(evt.summary.session.id)
          : evt.type === "session_gap"
            ? idToString(evt.session_id)
            : null;
  host.streamTelemetryEmitter?.({
    lane,
    eventType: evt.type,
    sessionId,
    emittedAtMs,
    receivedAtMs,
    streamSource: source,
  });
  noteWorkspaceStreamEventObserved(lane, evt.type);
  if (typeof emittedAtMs !== "number") return;
  const ageContext = {
    stream_source: source,
    event_type: evt.type,
    workspace_id: host.workspaceId,
  };
  noteWorkspaceEventAge(lane, receivedAtMs - emittedAtMs, ageContext);
  if (source !== "live") return;
  noteClientReceiveLag(lane, receivedAtMs - emittedAtMs, ageContext);
};

export type WorkspaceActiveSnapshotStreamHost = {
  workspaceId: string;
  destroyed: boolean;
  e2eDropStreamMessages: boolean;
  authTokenOverride: string | null;
  wsBaseUrlOverride: string | null;
  canonicalStreamUrl: string | null;
  state: WorkspaceActiveSnapshotStoreState;
  listWorkspaceArchivedTaskSummariesFn: typeof listWorkspaceArchivedTaskSummaries;
  ws: WebSocket | null;
  connecting: boolean;
  reconnectTimer: ReturnType<typeof globalThis.setTimeout> | null;
  reconnectDelayMs: number;
  lastStreamSeq: number;
  allowSnapshotReset: boolean;
  workerPatchEmitter: ((patch: WorkspaceActiveSnapshotPatch) => void) | null;
  streamTelemetryEmitter: ((telemetry: WorkspaceActiveSnapshotStreamTelemetry) => void) | null;
  workerPatchOldestEventReceivedAtMs: number | null;
  workerPatchOldestForegroundEventReceivedAtMs: number | null;
  streamQueue: Promise<void>;
  foregroundSessionId?: string | null;
  subscribedSessions?: SessionSubscriptionCursor[];
  openWebSocket?(url: string): Promise<void>;
  scheduleReconnect?(): void;
  handleStreamMessage?(
    input: unknown | { data: unknown; receivedAtMs: number },
  ): Promise<void>;
  getSnapshot(): WorkspaceActiveSnapshotState;
  publish(): void;
  schedulePersistCache(): void;
  clearSnapshotWarning(): void;
  flushSubscriptions(reason?: string): void;
  notifyEventListeners(evt: WorkspaceActiveSnapshotEvent): void;
  isForegroundSessionEvent(evt: WorkspaceActiveSnapshotEvent): boolean;
};

const notifyRecoverableSessionStreamGap = (
  host: WorkspaceActiveSnapshotStreamHost,
  reason: "stream_seq_gap" | "stream_seq_reset",
): void => {
  const sessionIds = new Set<string>();
  const foregroundSessionId = String(host.foregroundSessionId ?? "").trim();
  if (foregroundSessionId) {
    sessionIds.add(foregroundSessionId);
  }
  for (const subscription of host.subscribedSessions ?? []) {
    const sessionId = String(subscription.sessionId ?? "").trim();
    if (sessionId) {
      sessionIds.add(sessionId);
    }
  }
  for (const sessionId of sessionIds) {
    const head = host.state.getSessionHeadSnapshot(sessionId);
    const afterSeq =
      typeof head?.last_event_seq === "number" && Number.isFinite(head.last_event_seq)
        ? head.last_event_seq
        : 0;
    host.notifyEventListeners({
      type: "session_gap",
      workspace_id: host.workspaceId,
      snapshot_rev: host.state.getSnapshotRev(),
      session_id: sessionId,
      after_seq: afterSeq,
      reason,
    });
  }
};

const sessionGapSeedFollows = (evt: WorkspaceActiveSnapshotEvent): boolean =>
  evt.type === "session_gap" && (evt as { seed_follows?: unknown }).seed_follows === true;

const updateLiveWorkspaceSnapshotRev = (
  host: WorkspaceActiveSnapshotStreamHost,
  snapshotRev: number,
  streamSource: WorkspaceActiveSnapshotStreamSource,
): void => {
  if (streamSource === "replay") return;
  const currentRev = host.state.getSnapshotRev();
  if (snapshotRev < currentRev) {
    return;
  }
  host.state.updateSnapshotRev(snapshotRev);
};

export const applyWorkspaceSnapshot = (
  host: WorkspaceActiveSnapshotStreamHost,
  snapshot: WorkspaceActiveSnapshot,
  heads?: SessionHeadSnapshot[] | null,
): void => {
  if (host.destroyed || !snapshot || typeof snapshot !== "object") return;
  const incomingRev = typeof snapshot.snapshot_rev === "number" ? snapshot.snapshot_rev : 0;
  const currentRev = host.state.getSnapshotRev();
  const allowLower = !host.state.hasLiveSnapshotApplied() || host.allowSnapshotReset;
  if (incomingRev < currentRev && !allowLower) {
    return;
  }
  const resetSnapshotRev = incomingRev < currentRev;
  host.allowSnapshotReset = false;
  host.state.applyWorkspaceSnapshot(snapshot, heads, { resetSnapshotRev });
  host.clearSnapshotWarning();
  host.publish();
  host.schedulePersistCache();
};

export const fetchArchivedPage = async (
  host: WorkspaceActiveSnapshotStreamHost,
  firstLoad: boolean,
): Promise<void> => {
  if (host.destroyed) return;
  if (firstLoad) {
    host.state.resetArchivedCursor();
  }
  const cursor = host.state.getArchivedCursor();
  if (!firstLoad && !cursor) {
    if (host.state.markArchivedExhausted()) {
      host.publish();
    }
    return;
  }
  setFetchState(host, "archived", "loading");
  try {
    const page = await host.listWorkspaceArchivedTaskSummariesFn(host.workspaceId, {
      limit: ACTIVE_PAGE_SIZE,
      cursor: cursor ?? undefined,
    });
    const summaries = await Promise.all(page.tasks.map((task) => host.state.buildArchivedItem(task)));
    host.state.applyArchivedPage(page, summaries);
    host.publish();
  } catch (err) {
    emitUiDiagnostic({
      source: "workspace_snapshot",
      code: "workspace.archived_load_failed",
      severity: "warning",
      message: "Archived task summaries failed to load.",
      context: {
        workspaceId: host.workspaceId,
        firstLoad,
        error: normalizeDiagnosticErrorMessage(err, "Archived task load failed."),
      },
    });
    setFetchState(host, "archived", "error");
    return;
  }
  setFetchState(host, "archived", "idle");
};

export const connectStream = async (host: WorkspaceActiveSnapshotStreamHost): Promise<void> => {
  if (host.destroyed || host.ws || host.connecting) return;
  host.connecting = true;
  if (host.state.setConnection("connecting")) {
    host.publish();
  }
  try {
    const daemonConfig = getDaemonClientConfig();
    const wsBaseUrl = host.wsBaseUrlOverride ?? daemonConfig.wsBaseUrl ?? null;
    const token = host.authTokenOverride ?? daemonConfig.authToken;
    if (!wsBaseUrl) {
      host.canonicalStreamUrl = null;
      emitUiDiagnostic({
        source: "workspace_stream",
        code: "workspace.stream_connection_missing",
        severity: "warning",
        message: "Workspace stream connection is not configured.",
        context: {
          workspaceId: host.workspaceId,
        },
      });
      if (host.state.setConnection("disconnected")) {
        host.publish();
      }
      return;
    }
    const managedMobileQuery = await deriveManagedMobileStreamQuery(host.workspaceId);
    const query = managedMobileQuery ?? new URLSearchParams();
    if (!managedMobileQuery) {
      await setBrowserStreamQueryToken(query, token, {
        kind: "workspace_active_snapshot",
        workspaceId: host.workspaceId,
      });
    }
    const serializedQuery = query.toString();
    const qs = serializedQuery ? `?${serializedQuery}` : "";
    const streamPath = managedMobileQuery
      ? managedMobileStreamPath(host.workspaceId)
      : `/api/workspaces/${host.workspaceId}/active_snapshot/stream`;
    const url = `${wsBaseUrl.replace(/\/+$/, "")}${streamPath}${qs}`;
    host.canonicalStreamUrl = url;
    if (host.destroyed) return;
    const openConnection =
      host.openWebSocket?.bind(host) ??
      ((streamUrl: string) => openWebSocket(host, streamUrl));
    const reconnect = host.scheduleReconnect?.bind(host) ?? (() => scheduleReconnect(host));
    try {
      await openConnection(url);
      return;
    } catch (err) {
      emitUiDiagnostic({
        source: "workspace_stream",
        code: "workspace.stream_connect_failed",
        severity: "warning",
        message: "Workspace stream connection failed; reconnect scheduled.",
        context: {
          workspaceId: host.workspaceId,
          url,
          error: err instanceof Error && err.message ? err.message : String(err),
        },
      });
      if (host.state.setConnection("disconnected")) {
        host.publish();
      }
      reconnect();
    }
  } finally {
    host.connecting = false;
  }
};

export const openWebSocket = (
  host: WorkspaceActiveSnapshotStreamHost,
  url: string,
): Promise<void> => {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    host.ws = ws;
    let opened = false;
    const timeoutId = globalThis.setTimeout(() => {
      if (opened) return;
      try {
        ws.close();
      } catch {
        // ignore
      }
      if (host.ws === ws) {
        host.ws = null;
      }
      reject(new Error("workspace active snapshot ws timeout"));
    }, 4000);

    ws.onopen = () => {
      opened = true;
      globalThis.clearTimeout(timeoutId);
      if (host.destroyed) {
        try {
          ws.close();
        } catch {
          // ignore
        }
        if (host.ws === ws) {
          host.ws = null;
        }
        resolve();
        return;
      }
      host.reconnectDelayMs = 1000;
      host.lastStreamSeq = 0;
      if (host.state.setConnection("connected")) {
        host.publish();
      }
      host.flushSubscriptions("ws_open");
      resolve();
    };

    ws.onmessage = (event) => {
      enqueueStreamMessage(host, event.data);
    };

    ws.onerror = () => {
      globalThis.clearTimeout(timeoutId);
      if (!opened) {
        if (host.ws === ws) {
          host.ws = null;
        }
        reject(new Error("workspace active snapshot ws error"));
      }
    };

    ws.onclose = () => {
      if (host.ws === ws) {
        host.ws = null;
      }
      if (host.state.setConnection("disconnected")) {
        host.publish();
      }
      scheduleReconnect(host);
    };
  });
};

export const scheduleReconnect = (host: WorkspaceActiveSnapshotStreamHost): void => {
  if (host.reconnectTimer || host.destroyed) return;
  const delay = host.reconnectDelayMs;
  host.reconnectDelayMs = Math.min(host.reconnectDelayMs * 2, 15000);
  host.reconnectTimer = globalThis.setTimeout(() => {
    host.reconnectTimer = null;
    void connectStream(host);
  }, delay);
};

export const enqueueStreamMessage = (
  host: WorkspaceActiveSnapshotStreamHost,
  data: unknown,
): void => {
  const handle =
    host.handleStreamMessage?.bind(host) ??
    ((input: unknown | { data: unknown; receivedAtMs: number }) =>
      handleStreamMessage(host, input));
  host.streamQueue = host.streamQueue
    .then(() => handle({ data, receivedAtMs: nowMs() }))
    .catch(() => {});
};

export const handleStreamMessage = async (
  host: WorkspaceActiveSnapshotStreamHost,
  input: unknown | { data: unknown; receivedAtMs: number },
): Promise<void> => {
  if (host.e2eDropStreamMessages) return;
  const payload =
    input &&
    typeof input === "object" &&
    "data" in input &&
    "receivedAtMs" in input &&
    typeof (input as { receivedAtMs?: unknown }).receivedAtMs === "number"
      ? (input as { data: unknown; receivedAtMs: number })
      : { data: input, receivedAtMs: nowMs() };
  const parsedEnvelope = await parseWsJson(payload.data);
  const parsed = await decryptManagedMobileStreamEnvelope(parsedEnvelope);
  if (!parsed || typeof parsed !== "object") return;
  const streamRev = readWorkspaceStreamRev(parsed);
  if (typeof streamRev === "number") {
    if (host.lastStreamSeq > 0 && streamRev < host.lastStreamSeq) {
      host.lastStreamSeq = streamRev;
      host.allowSnapshotReset = true;
      notifyRecoverableSessionStreamGap(host, "stream_seq_reset");
      host.flushSubscriptions("stream_seq_reset");
    } else if (host.lastStreamSeq > 0 && streamRev > host.lastStreamSeq + 1) {
      host.allowSnapshotReset = true;
      notifyRecoverableSessionStreamGap(host, "stream_seq_gap");
      host.flushSubscriptions("stream_seq_gap");
    }
    host.lastStreamSeq = Math.max(host.lastStreamSeq, streamRev);
  }
  const normalized =
    "event" in parsed
      ? ((parsed as { event?: unknown }).event ?? parsed)
      : parsed;
  if (!normalized || typeof normalized !== "object") return;
  const parsedType = (normalized as { type?: string }).type;
  if (parsedType === "reset_required") {
    noteWorkspaceStreamReset();
    const latestRev =
      (normalized as { latest_rev?: number }).latest_rev ??
      (normalized as { latestRev?: number }).latestRev ??
      0;
    if (typeof latestRev === "number") {
      host.state.updateSnapshotRev(latestRev);
    }
    host.lastStreamSeq = 0;
    host.allowSnapshotReset = true;
    host.flushSubscriptions("reset_required");
    return;
  }
  const wsSnapshot = readWorkspaceSnapshotPayload(parsed);
  if (wsSnapshot) {
    applyWorkspaceSnapshot(host, wsSnapshot.snapshot, wsSnapshot.heads);
    return;
  }
  const headsBatch = readWorkspaceHeadsBatchPayload(parsed);
  if (headsBatch) {
    const streamSource = readWorkspaceStreamSource(parsed);
    const batchRev = headsBatch.snapshotRev;
    if (typeof batchRev === "number") {
      updateLiveWorkspaceSnapshotRev(host, batchRev, streamSource);
    }
    let changed = false;
    for (const delta of headsBatch.deltas) {
      if (host.state.applySessionHeadDelta(delta)) {
        changed = true;
      }
      const evt: WorkspaceActiveSnapshotEvent = {
        type: "session_head_delta",
        workspace_id: host.workspaceId,
        snapshot_rev: batchRev,
        delta,
      };
      markWorkspaceEventReceivedAt(evt, payload.receivedAtMs);
      markWorkspaceEventStreamSource(evt, streamSource);
      noteWorkspaceEventClientReceiveLag(host, evt, payload.receivedAtMs, streamSource);
      host.notifyEventListeners(evt);
    }
    if (changed) {
      host.schedulePersistCache();
    }
    return;
  }
  const evt = normalized as WorkspaceActiveSnapshotEvent;
  const streamSource = readWorkspaceStreamSource(parsed);
  markWorkspaceEventReceivedAt(evt, payload.receivedAtMs);
  markWorkspaceEventStreamSource(evt, streamSource);
  noteWorkspaceEventClientReceiveLag(host, evt, payload.receivedAtMs, streamSource);
  const queueAgeMs = Math.max(0, nowMs() - payload.receivedAtMs);
  const foregroundEvent = host.isForegroundSessionEvent(evt);
  if (host.workerPatchEmitter) {
    host.workerPatchOldestEventReceivedAtMs = Math.min(
      host.workerPatchOldestEventReceivedAtMs ?? payload.receivedAtMs,
      payload.receivedAtMs,
    );
    if (foregroundEvent) {
      host.workerPatchOldestForegroundEventReceivedAtMs = Math.min(
        host.workerPatchOldestForegroundEventReceivedAtMs ?? payload.receivedAtMs,
        payload.receivedAtMs,
      );
    }
  } else {
    noteQueueAgeSample("workspace", queueAgeMs, { source: "stream_handler" });
    if (foregroundEvent) {
      noteQueueAgeSample("foreground", queueAgeMs, { source: "stream_handler" });
    }
  }
  if (typeof evt.snapshot_rev === "number") {
    updateLiveWorkspaceSnapshotRev(host, evt.snapshot_rev, streamSource);
  }
  const archivedStateChanged =
    "archived_rev" in evt && typeof evt.archived_rev === "number"
      ? host.state.updateArchivedRev(evt.archived_rev)
      : false;

  let published = false;
  const publish = () => {
    if (published) return;
    published = true;
    host.publish();
  };
  let flushAfterNotifyReason: string | null = null;
  switch (evt.type) {
    case "ready":
      if (host.state.setConnection("connected")) {
        publish();
      }
      break;
    case "task_delta":
      if (applyTaskDelta(host, evt)) {
        publish();
      }
      break;
    case "active_task_upsert":
      if (host.state.upsertActiveSummary(evt.task)) {
        publish();
        host.schedulePersistCache();
      }
      flushAfterNotifyReason = "active_task_upsert";
      break;
    case "active_task_delete":
      if (host.state.removeTask(idToString(evt.task_id), { adjustCounts: true })) {
        publish();
        host.schedulePersistCache();
      }
      break;
    case "archived_task_upsert": {
      const item = host.state.buildArchivedItem(evt.task, null);
      if (item && host.state.upsertArchivedItem(item)) {
        publish();
      }
      break;
    }
    case "archived_task_delete":
      if (host.state.removeArchivedTask(idToString(evt.task_id), { adjustCounts: true })) {
        publish();
      }
      break;
    case "session_summary":
      if (host.state.applySessionSummary(evt.summary)) {
        publish();
        host.schedulePersistCache();
      }
      break;
    case "session_summary_delta":
      if (applySessionSummaryDelta(host, evt)) {
        publish();
      }
      break;
    case "session_head_delta":
      if (host.state.applySessionHeadDelta(evt.delta)) {
        host.schedulePersistCache();
      }
      break;
    case "session_head_seed":
      if (host.state.applySessionHeadSeed(evt.head)) {
        publish();
        host.schedulePersistCache();
      }
      break;
    case "session_gap":
      if (!sessionGapSeedFollows(evt)) {
        flushAfterNotifyReason = "session_gap";
      }
      break;
    case "worktree_bootstrap":
      if (
        host.state.applyWorktreeRoot(
          idToString(evt.notice.worktree_id),
          String(evt.notice.worktree_root ?? ""),
        )
      ) {
        publish();
      }
      break;
    default:
      break;
  }

  if (archivedStateChanged) {
    publish();
  }

  host.notifyEventListeners(evt);
  if (flushAfterNotifyReason) {
    host.flushSubscriptions(flushAfterNotifyReason);
  }
};

const applyTaskDelta = (
  host: WorkspaceActiveSnapshotStreamHost,
  evt: WorkspaceActiveSnapshotTaskDeltaEvent,
): boolean => {
  const changed = host.state.applyTaskDelta(evt);
  if (changed) {
    host.schedulePersistCache();
  }
  return changed;
};

export const applySessionSummaryDelta = (
  host: WorkspaceActiveSnapshotStreamHost,
  evt: WorkspaceActiveSnapshotSessionSummaryDeltaEvent,
): boolean => {
  const changed = host.state.applySessionSummaryDelta(evt);
  if (changed) {
    host.schedulePersistCache();
  }
  return changed;
};

const setFetchState = (
  host: WorkspaceActiveSnapshotStreamHost,
  target: "active" | "archived",
  state: "idle" | "loading" | "error",
): void => {
  if (!host.state.setFetchState(target, state)) return;
  host.publish();
};
