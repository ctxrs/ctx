import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import {
  recordClientCounterMetric,
  recordClientHistogramMetric,
} from "../../api/client";
import {
  trackWorkerPatchApply,
  trackWorkerPatchFlush,
} from "../../utils/analytics";
import { noteQueueAgeSample } from "../foregroundFreshnessTelemetry";
import type {
  WorkspaceActiveSnapshotPatch,
  WorkspaceActiveSnapshotStreamSource,
} from "../workspaceActiveSnapshotProtocol";
import { saveWorkspaceActiveSnapshotV1 } from "../uiStateStore";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import {
  markWorkspaceEventReceivedAt,
  markWorkspaceEventStreamSource,
  readWorkspaceEventReceivedAt,
  readWorkspaceEventStreamSource,
} from "../workspaceEventTelemetry";
import {
  isForegroundPrioritySessionEvent,
  workspaceEventSessionId,
} from "./foregroundPriority";
import type { WorkspaceActiveSnapshotState } from "./storeTypes";
import type { WorkspaceActiveSnapshotWorkerHost } from "./workerRuntime";

const patchKindFor = (patch: WorkspaceActiveSnapshotPatch): "replace" | "diff" =>
  patch.snapshot ? "replace" : "diff";

const WORKER_PATCH_LOG_SAMPLE_INTERVAL = 25;
const WORKER_PATCH_LOG_EVENT_THRESHOLD = 16;
const WORKER_PATCH_LOG_AGE_THRESHOLD_MS = 100;

const shouldRecordWorkerPatch = (
  seq: number,
  eventCount: number,
  publishSnapshot: boolean,
  persist: boolean,
  oldestAgeMs: number | null,
): boolean => {
  if (eventCount >= WORKER_PATCH_LOG_EVENT_THRESHOLD) return true;
  if (publishSnapshot || persist) return true;
  if (typeof oldestAgeMs === "number" && oldestAgeMs >= WORKER_PATCH_LOG_AGE_THRESHOLD_MS) {
    return true;
  }
  return seq % WORKER_PATCH_LOG_SAMPLE_INTERVAL === 0;
};

const estimatePatchBytes = (patch: WorkspaceActiveSnapshotPatch): number => {
  try {
    return new Blob([JSON.stringify(patch)]).size;
  } catch {
    return 0;
  }
};

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

const isWorkspaceActiveSnapshotStreamSource = (
  value: unknown,
): value is WorkspaceActiveSnapshotStreamSource => value === "live" || value === "replay";

const buildPatchEventMetadata = (
  events: readonly WorkspaceActiveSnapshotEvent[],
): Pick<WorkspaceActiveSnapshotPatch, "eventReceivedAtMs" | "eventStreamSources"> => {
  const eventReceivedAtMs = events.map(readWorkspaceEventReceivedAt);
  const eventStreamSources = events.map(readWorkspaceEventStreamSource);
  return {
    ...(eventReceivedAtMs.some((value) => typeof value === "number")
      ? { eventReceivedAtMs }
      : {}),
    ...(eventStreamSources.some((value) => value !== null)
      ? { eventStreamSources }
      : {}),
  };
};

const sameIdList = (left: readonly string[], right: readonly string[]): boolean => {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
};

const sameRecordRefs = <T,>(left: Record<string, T>, right: Record<string, T>): boolean => {
  if (left === right) return true;
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) return false;
  for (const key of leftKeys) {
    if (!(key in right) || left[key] !== right[key]) {
      return false;
    }
  }
  return true;
};

const diffRecordEntries = <T,>(
  previous: Record<string, T>,
  next: Record<string, T>,
): { upserts?: Record<string, T>; deletes?: string[] } => {
  const upserts: Record<string, T> = {};
  const deletes: string[] = [];
  for (const [key, value] of Object.entries(next)) {
    if (!(key in previous) || previous[key] !== value) {
      upserts[key] = value;
    }
  }
  for (const key of Object.keys(previous)) {
    if (!(key in next)) {
      deletes.push(key);
    }
  }
  return {
    ...(Object.keys(upserts).length > 0 ? { upserts } : {}),
    ...(deletes.length > 0 ? { deletes } : {}),
  };
};

const filterRecordEntries = <T,>(
  entries: Record<string, T> | undefined,
  allowedKeys: ReadonlySet<string>,
): Record<string, T> | undefined => {
  if (!entries) return undefined;
  const filtered: Record<string, T> = {};
  for (const [key, value] of Object.entries(entries)) {
    if (allowedKeys.has(key)) {
      filtered[key] = value;
    }
  }
  return Object.keys(filtered).length > 0 ? filtered : undefined;
};

const filterDeletedKeys = (
  keys: string[] | undefined,
  allowedKeys: ReadonlySet<string>,
): string[] | undefined => {
  if (!keys) return undefined;
  const filtered = keys.filter((key) => allowedKeys.has(key));
  return filtered.length > 0 ? filtered : undefined;
};

const applySessionHeadDiffToBaseline = <T,>(
  previous: Record<string, T>,
  upserts: Record<string, T> | undefined,
  deletes: string[] | undefined,
): Record<string, T> => {
  const next = { ...previous };
  for (const key of deletes ?? []) {
    delete next[key];
  }
  for (const [key, value] of Object.entries(upserts ?? {})) {
    next[key] = value;
  }
  return next;
};

export const applyWorkerPatch = (
  host: WorkspaceActiveSnapshotWorkerHost,
  patch: WorkspaceActiveSnapshotPatch,
): void => {
  if (host.destroyed) return;
  const appliedAtMs = nowMs();
  const oldestEventAgeMs =
    typeof patch.oldestEventReceivedAtMs === "number"
      ? Math.max(0, appliedAtMs - patch.oldestEventReceivedAtMs)
      : null;
  const oldestForegroundEventAgeMs =
    typeof patch.oldestForegroundEventReceivedAtMs === "number"
      ? Math.max(0, appliedAtMs - patch.oldestForegroundEventReceivedAtMs)
      : null;
  const patchKind = patchKindFor(patch);
  if (typeof patch.oldestEventReceivedAtMs === "number") {
    noteQueueAgeSample("workspace", oldestEventAgeMs ?? 0, { source: "worker_patch" });
  }
  if (typeof patch.oldestForegroundEventReceivedAtMs === "number") {
    noteQueueAgeSample("foreground", oldestForegroundEventAgeMs ?? 0, { source: "worker_patch" });
  }

  const applyStartedAtMs = nowMs();
  host.state.applyWorkerPatch(patch);
  recordClientHistogramMetric(
    "workspace.active_snapshot.worker_patch_apply_ms",
    "ms",
    Math.max(0, nowMs() - applyStartedAtMs),
    { patch_kind: patchKind },
  );
  recordClientHistogramMetric(
    "workspace.active_snapshot.worker_patch_event_count",
    "count",
    patch.events.length,
    { patch_kind: patchKind },
  );
  if (patch.publishSnapshot !== false) {
    host.publish();
  }
  if (patch.persist) {
    schedulePersistCache(host);
  }
  patch.events.forEach((event, index) => {
    const receivedAtMs = patch.eventReceivedAtMs?.[index];
    if (typeof receivedAtMs === "number" && Number.isFinite(receivedAtMs)) {
      markWorkspaceEventReceivedAt(event, receivedAtMs);
    }
    const streamSource = patch.eventStreamSources?.[index];
    if (isWorkspaceActiveSnapshotStreamSource(streamSource)) {
      markWorkspaceEventStreamSource(event, streamSource);
    }
    host.notifyEventListeners(event);
  });

  const applyDurationMs = Math.max(0, nowMs() - applyStartedAtMs);
  if (
    shouldRecordWorkerPatch(
      host.workerPatchFlushSeq,
      patch.events.length,
      patch.publishSnapshot !== false,
      patch.persist,
      oldestEventAgeMs,
    ) || applyDurationMs >= 250
  ) {
    trackWorkerPatchApply({
      source: "worker_patch",
      eventCount: patch.events.length,
      applyDurationMs,
      publishSnapshot: patch.publishSnapshot !== false,
      persist: patch.persist,
      oldestEventAgeStartMs: oldestEventAgeMs,
      oldestForegroundEventAgeStartMs: oldestForegroundEventAgeMs,
    });
  }
};

export const schedulePersistCache = (host: WorkspaceActiveSnapshotWorkerHost): void => {
  if (host.workerPatchEmitter) {
    host.workerPatchPendingPersist = true;
    scheduleWorkerPatchFlush(host);
    return;
  }
  if (host.destroyed || host.cachePersistTimer) return;
  host.cachePersistTimer = globalThis.setTimeout(() => {
    host.cachePersistTimer = null;
    void persistCache(host);
  }, 300);
};

export const scheduleWorkerPatchFlush = (host: WorkspaceActiveSnapshotWorkerHost): void => {
  if (!host.workerPatchEmitter || host.workerPatchTimer) return;
  host.workerPatchTimer = globalThis.setTimeout(() => {
    host.workerPatchTimer = null;
    flushWorkerPatch(host);
  }, host.workerPatchFlushMs);
};

export const flushWorkerPatchNow = (
  host: WorkspaceActiveSnapshotWorkerHost,
  prioritySessionIds?: readonly string[],
): void => {
  if (host.workerPatchTimer) {
    globalThis.clearTimeout(host.workerPatchTimer);
    host.workerPatchTimer = null;
  }
  flushWorkerPatch(host, prioritySessionIds);
};

export const resetWorkerPatchQueue = (host: WorkspaceActiveSnapshotWorkerHost): void => {
  if (host.cachePersistTimer) {
    globalThis.clearTimeout(host.cachePersistTimer);
    host.cachePersistTimer = null;
  }
  if (host.workerPatchTimer) {
    globalThis.clearTimeout(host.workerPatchTimer);
    host.workerPatchTimer = null;
  }
  host.workerPatchPendingEvents = [];
  host.workerPatchPendingPersist = false;
  host.workerPatchDirty = false;
  host.workerPatchOldestEventReceivedAtMs = null;
  host.workerPatchOldestForegroundEventReceivedAtMs = null;
  host.lastWorkerPatchSnapshot = null;
  host.lastWorkerPatchSessionHeads = {};
  host.lastWorkerPatchWorktreeRoots = {};
  host.lastWorkerPatchSnapshotRev = -1;
};

const flushWorkerPatch = (
  host: WorkspaceActiveSnapshotWorkerHost,
  prioritySessionIds?: readonly string[],
): void => {
  if (!host.workerPatchEmitter) return;
  if (
    !host.workerPatchDirty &&
    !host.workerPatchPendingPersist &&
    host.workerPatchPendingEvents.length === 0
  ) {
    return;
  }

  host.workerPatchFlushSeq += 1;
  const flushStartedAtMs = nowMs();
  const pendingEvents = host.workerPatchPendingEvents.slice();
  const prioritySessionIdSet = new Set(
    (prioritySessionIds ?? []).map((sessionId) => sessionId.trim()).filter(Boolean),
  );
  const snapshot = host.state.getSnapshot();
  const sessionHeads = host.state.getSessionHeadsSnapshot();
  const worktreeRoots = host.state.getWorktreeRootsSnapshot();
  const activeSessionIds = host.state.getActiveSessionIds();
  const snapshotRev = host.state.getSnapshotRev();
  const forceSnapshotReplace =
    host.lastWorkerPatchSnapshot == null || snapshotRev < host.lastWorkerPatchSnapshotRev;
  let events = pendingEvents;
  let deferredEvents: WorkspaceActiveSnapshotEvent[] = [];
  const canFlushPrioritySubset = !forceSnapshotReplace && prioritySessionIdSet.size > 0;
  if (canFlushPrioritySubset) {
    const priorityEvents: WorkspaceActiveSnapshotEvent[] = [];
    const otherEvents: WorkspaceActiveSnapshotEvent[] = [];
    for (const event of pendingEvents) {
      if (prioritySessionIdSet.has(workspaceEventSessionId(event))) {
        priorityEvents.push(event);
      } else {
        otherEvents.push(event);
      }
    }
    if (priorityEvents.length > 0 && otherEvents.length > 0) {
      events = priorityEvents;
      deferredEvents = otherEvents;
    }
  }
  host.workerPatchPendingEvents = deferredEvents;
  const hasDeferredEvents = deferredEvents.length > 0;

  let patch: WorkspaceActiveSnapshotPatch;
  let sentSessionHeadUpserts: Record<string, (typeof sessionHeads)[string]> | undefined;
  let sentSessionHeadDeletes: string[] | undefined;
  if (forceSnapshotReplace) {
    patch = {
      snapshot,
      sessionHeadUpserts: sessionHeads,
      worktreeRootUpserts: worktreeRoots,
      events,
      ...buildPatchEventMetadata(events),
      snapshotRev,
      archivedRev: host.state.getArchivedRev(),
      activeSessionIds,
      publishSnapshot: true,
      persist: host.workerPatchPendingPersist,
      oldestEventReceivedAtMs: host.workerPatchOldestEventReceivedAtMs,
      oldestForegroundEventReceivedAtMs: host.workerPatchOldestForegroundEventReceivedAtMs,
    };
  } else {
    const previousSnapshot = host.lastWorkerPatchSnapshot as WorkspaceActiveSnapshotState;
    const taskDiff = diffRecordEntries(previousSnapshot.tasksById, snapshot.tasksById);
    const rawSessionHeadDiff = diffRecordEntries(host.lastWorkerPatchSessionHeads, sessionHeads);
    const sessionHeadDiff =
      hasDeferredEvents && prioritySessionIdSet.size > 0
        ? {
            upserts: filterRecordEntries(rawSessionHeadDiff.upserts, prioritySessionIdSet),
            deletes: filterDeletedKeys(rawSessionHeadDiff.deletes, prioritySessionIdSet),
          }
        : rawSessionHeadDiff;
    const worktreeRootDiff = diffRecordEntries(host.lastWorkerPatchWorktreeRoots, worktreeRoots);
    const shell: NonNullable<WorkspaceActiveSnapshotPatch["shell"]> = {};

    if (snapshot.initialized !== previousSnapshot.initialized) {
      shell.initialized = snapshot.initialized;
    }
    if (snapshot.liveSnapshotApplied !== previousSnapshot.liveSnapshotApplied) {
      shell.liveSnapshotApplied = snapshot.liveSnapshotApplied;
    }
    if (snapshot.connection !== previousSnapshot.connection) {
      shell.connection = snapshot.connection;
    }
    if (!sameIdList(snapshot.activeIds, previousSnapshot.activeIds)) {
      shell.activeIds = snapshot.activeIds.slice();
    }
    if (!sameIdList(snapshot.archivedIds, previousSnapshot.archivedIds)) {
      shell.archivedIds = snapshot.archivedIds.slice();
    }
    if (snapshot.totalActive !== previousSnapshot.totalActive) {
      shell.totalActive = snapshot.totalActive;
    }
    if (snapshot.totalArchived !== previousSnapshot.totalArchived) {
      shell.totalArchived = snapshot.totalArchived;
    }
    if (snapshot.archivedRev !== previousSnapshot.archivedRev) {
      shell.archivedRev = snapshot.archivedRev;
    }
    if (
      snapshot.fetchState.active !== previousSnapshot.fetchState.active ||
      snapshot.fetchState.archived !== previousSnapshot.fetchState.archived
    ) {
      shell.fetchState = { ...snapshot.fetchState };
    }
    if (snapshot.hasMoreActive !== previousSnapshot.hasMoreActive) {
      shell.hasMoreActive = snapshot.hasMoreActive;
    }
    if (snapshot.hasMoreArchived !== previousSnapshot.hasMoreArchived) {
      shell.hasMoreArchived = snapshot.hasMoreArchived;
    }
    if (snapshot.archivedLoaded !== previousSnapshot.archivedLoaded) {
      shell.archivedLoaded = snapshot.archivedLoaded;
    }
    const shellChanged = Object.keys(shell).length > 0;
    const taskChanged = Boolean(taskDiff.upserts) || Boolean(taskDiff.deletes);
    sentSessionHeadUpserts = sessionHeadDiff.upserts;
    sentSessionHeadDeletes = sessionHeadDiff.deletes;
    patch = {
      ...(shellChanged ? { shell } : {}),
      ...(taskDiff.upserts ? { taskUpserts: taskDiff.upserts } : {}),
      ...(taskDiff.deletes ? { taskDeletes: taskDiff.deletes } : {}),
      ...(sessionHeadDiff.upserts ? { sessionHeadUpserts: sessionHeadDiff.upserts } : {}),
      ...(sessionHeadDiff.deletes ? { sessionHeadDeletes: sessionHeadDiff.deletes } : {}),
      ...(worktreeRootDiff.upserts ? { worktreeRootUpserts: worktreeRootDiff.upserts } : {}),
      ...(worktreeRootDiff.deletes ? { worktreeRootDeletes: worktreeRootDiff.deletes } : {}),
      events,
      ...buildPatchEventMetadata(events),
      snapshotRev,
      archivedRev: host.state.getArchivedRev(),
      activeSessionIds,
      publishSnapshot: host.workerPatchDirty || shellChanged || taskChanged,
      persist: host.workerPatchPendingPersist,
      oldestEventReceivedAtMs: host.workerPatchOldestEventReceivedAtMs,
      oldestForegroundEventReceivedAtMs: host.workerPatchOldestForegroundEventReceivedAtMs,
    };
  }

  const patchKind = patchKindFor(patch);
  host.workerPatchDirty = hasDeferredEvents ? host.workerPatchDirty : false;
  host.workerPatchPendingPersist = hasDeferredEvents ? host.workerPatchPendingPersist : false;
  host.workerPatchOldestEventReceivedAtMs = hasDeferredEvents
    ? host.workerPatchOldestEventReceivedAtMs
    : null;
  host.workerPatchOldestForegroundEventReceivedAtMs = null;
  host.lastWorkerPatchSnapshot = snapshot;
  host.lastWorkerPatchSessionHeads =
    hasDeferredEvents && !forceSnapshotReplace
      ? applySessionHeadDiffToBaseline(
          host.lastWorkerPatchSessionHeads,
          sentSessionHeadUpserts,
          sentSessionHeadDeletes,
        )
      : sessionHeads;
  host.lastWorkerPatchWorktreeRoots = worktreeRoots;
  host.lastWorkerPatchSnapshotRev = snapshotRev;
  if (hasDeferredEvents) {
    scheduleWorkerPatchFlush(host);
  }
  const oldestEventAgeMs =
    typeof patch.oldestEventReceivedAtMs === "number"
      ? Math.max(0, nowMs() - patch.oldestEventReceivedAtMs)
      : null;
  const oldestForegroundEventAgeMs =
    typeof patch.oldestForegroundEventReceivedAtMs === "number"
      ? Math.max(0, nowMs() - patch.oldestForegroundEventReceivedAtMs)
      : null;
  if (
    shouldRecordWorkerPatch(
      host.workerPatchFlushSeq,
      patch.events.length,
      patch.publishSnapshot !== false,
      patch.persist,
      oldestEventAgeMs,
    )
  ) {
    trackWorkerPatchFlush({
      source: "worker_patch",
      eventCount: patch.events.length,
      activeSessionCount: patch.activeSessionIds.length,
      publishSnapshot: patch.publishSnapshot !== false,
      persist: patch.persist,
      patchBytesEstimate: estimatePatchBytes(patch),
      oldestEventAgeMs,
      oldestForegroundEventAgeMs,
    });
  }
  recordClientCounterMetric("workspace.active_snapshot.worker_patch_flush_count", {
    patch_kind: patchKind,
    persist: patch.persist ? "true" : "false",
  });
  recordClientHistogramMetric(
    "workspace.active_snapshot.worker_patch_flush_ms",
    "ms",
    Math.max(0, nowMs() - flushStartedAtMs),
    {
      patch_kind: patchKind,
      persist: patch.persist ? "true" : "false",
    },
  );
  host.workerPatchEmitter(patch);
};

const persistCache = async (host: WorkspaceActiveSnapshotWorkerHost): Promise<void> => {
  if (host.destroyed) return;
  try {
    await saveWorkspaceActiveSnapshotV1(host.workspaceId, host.state.buildPersistedSnapshot());
  } catch {
    // ignore cache errors
  }
};

export const isForegroundSessionEvent = (
  foregroundSessionId: string | null,
  evt: WorkspaceActiveSnapshotEvent,
  subscribedSessions?: readonly SessionSubscriptionCursor[],
): boolean => {
  return isForegroundPrioritySessionEvent(foregroundSessionId, subscribedSessions, evt);
};
