import type { SessionHeadSnapshot, WorkspaceActiveSnapshot, WorkspaceIndexCursor } from "@ctx/types";
import { idToString } from "../../api/client";
import type { WorkspaceActiveSnapshotPatch } from "../workspaceActiveSnapshotProtocol";
import type {
  PersistedWorkspaceActiveSnapshotV1,
  PersistedWorkspaceActiveTaskSummaryV1,
} from "../uiStateStore";
import { buildPersistedActiveSnapshotSummary } from "./itemBuilders";
import type { WorkspaceActiveSnapshotItem, WorkspaceActiveSnapshotState } from "./storeTypes";
import {
  applyActiveHeadsForStore,
  collectArchivedHeadsForStore,
  mergeWorkerSessionHeadUpsertsForStore,
  normalizeActiveSummaryForStore,
  restoreRetainedLiveSessionHeadsForStore,
  type StoreSessionHeadState,
} from "./storeSessionHeads";

export type WorkspaceActiveSnapshotStoreSnapshotHost = StoreSessionHeadState & {
  snapshot: WorkspaceActiveSnapshotState;
  worktreeRootsById: Map<string, string>;
  activeSessionIds: string[];
  activeOrder: string[];
  archivedOrder: string[];
  totalActive: number;
  totalArchived: number;
  hasMoreActive: boolean;
  hasMoreArchived: boolean;
  archivedLoaded: boolean;
  archivedCursor: WorkspaceIndexCursor | null;
  snapshotRev: number;
  archivedRev: number;
  liveSnapshotApplied: boolean;
  placeInOrders(item: WorkspaceActiveSnapshotItem): void;
  syncSnapshot(): void;
};

export const applyWorkerPatchToStoreState = (
  host: WorkspaceActiveSnapshotStoreSnapshotHost,
  patch: WorkspaceActiveSnapshotPatch,
): void => {
  if (patch.snapshot) {
    host.snapshot = patch.snapshot;
    host.tasks = new Map(Object.entries(patch.snapshot.tasksById));
    host.activeOrder = patch.snapshot.activeIds.slice();
    host.archivedOrder = patch.snapshot.archivedIds.slice();
    host.totalActive = patch.snapshot.totalActive;
    host.totalArchived = patch.snapshot.totalArchived;
    host.archivedRev = patch.snapshot.archivedRev;
    host.hasMoreActive = patch.snapshot.hasMoreActive;
    host.hasMoreArchived = patch.snapshot.hasMoreArchived;
    host.archivedLoaded = patch.snapshot.archivedLoaded;
    host.activeSessionIds = patch.activeSessionIds.slice();
    host.sessionHeadsById = new Map();
    mergeWorkerSessionHeadUpsertsForStore(host, patch.sessionHeadUpserts ?? {});
    host.worktreeRootsById = new Map(Object.entries(patch.worktreeRootUpserts ?? {}));
    host.snapshotRev = patch.snapshotRev;
    host.archivedRev = patch.archivedRev;
    host.liveSnapshotApplied = Boolean(patch.snapshot.liveSnapshotApplied);
    return;
  }

  const shell = patch.shell;
  let nextTasksById = host.snapshot.tasksById;

  if ((patch.taskDeletes?.length ?? 0) > 0 || patch.taskUpserts) {
    nextTasksById = { ...nextTasksById };
  }
  for (const taskId of patch.taskDeletes ?? []) {
    host.tasks.delete(taskId);
    delete nextTasksById[taskId];
  }
  for (const [taskId, item] of Object.entries(patch.taskUpserts ?? {})) {
    host.tasks.set(taskId, item);
    nextTasksById[taskId] = item;
  }

  for (const sessionId of patch.sessionHeadDeletes ?? []) {
    host.sessionHeadsById.delete(sessionId);
  }
  mergeWorkerSessionHeadUpsertsForStore(host, patch.sessionHeadUpserts ?? {});

  for (const worktreeId of patch.worktreeRootDeletes ?? []) {
    host.worktreeRootsById.delete(worktreeId);
  }
  for (const [worktreeId, root] of Object.entries(patch.worktreeRootUpserts ?? {})) {
    host.worktreeRootsById.set(worktreeId, root);
  }

  if (shell?.activeIds) {
    host.activeOrder = shell.activeIds.slice();
  }
  if (shell?.archivedIds) {
    host.archivedOrder = shell.archivedIds.slice();
  }
  if (typeof shell?.totalActive === "number") {
    host.totalActive = shell.totalActive;
  }
  if (typeof shell?.totalArchived === "number") {
    host.totalArchived = shell.totalArchived;
  }
  if (typeof shell?.archivedRev === "number") {
    host.archivedRev = shell.archivedRev;
  }
  if (typeof shell?.hasMoreActive === "boolean") {
    host.hasMoreActive = shell.hasMoreActive;
  }
  if (typeof shell?.hasMoreArchived === "boolean") {
    host.hasMoreArchived = shell.hasMoreArchived;
  }
  if (typeof shell?.archivedLoaded === "boolean") {
    host.archivedLoaded = shell.archivedLoaded;
  }
  if (patch.activeSessionIds.length > 0 || host.activeSessionIds.length > 0) {
    host.activeSessionIds = patch.activeSessionIds.slice();
  }
  if (typeof patch.snapshotRev === "number") {
    host.snapshotRev = patch.snapshotRev;
  }
  if (typeof patch.archivedRev === "number") {
    host.archivedRev = patch.archivedRev;
  }

  if (
    shell ||
    nextTasksById !== host.snapshot.tasksById
  ) {
    host.snapshot = {
      ...host.snapshot,
      ...(shell ?? {}),
      ...(nextTasksById !== host.snapshot.tasksById ? { tasksById: nextTasksById } : {}),
    };
    host.liveSnapshotApplied = Boolean(host.snapshot.liveSnapshotApplied);
  }
};

export const buildPersistedSnapshotForStoreState = (
  host: WorkspaceActiveSnapshotStoreSnapshotHost,
): Omit<PersistedWorkspaceActiveSnapshotV1, "v" | "workspaceId" | "updatedAtMs"> => {
  const tasks: PersistedWorkspaceActiveTaskSummaryV1[] = [];
  for (const id of host.activeOrder) {
    const item = host.tasks.get(id);
    if (!item || item.task.archived_at) continue;
    const summary = buildPersistedActiveSnapshotSummary({
      item,
      sessionHeadsById: host.sessionHeadsById,
    });
    if (summary) tasks.push(summary);
  }
  return {
    snapshotRev: host.snapshotRev,
    archivedRev: host.archivedRev,
    active: {
      tasks,
      totalCount: Math.max(host.totalActive, tasks.length),
    },
  };
};

export const applyCachedSnapshotToStoreState = (
  host: WorkspaceActiveSnapshotStoreSnapshotHost,
  cached: PersistedWorkspaceActiveSnapshotV1,
): void => {
  host.snapshotRev = Math.max(host.snapshotRev, cached.snapshotRev ?? 0);
  host.archivedRev = Math.max(host.archivedRev, cached.archivedRev ?? 0);
  const activeTasks = Array.isArray(cached.active?.tasks) ? cached.active.tasks : [];
  const archivedHeads = collectArchivedHeadsForStore(host);
  const previousHeads = new Map(host.sessionHeadsById);

  for (const [id, item] of host.tasks.entries()) {
    if (!item.task.archived_at) {
      host.tasks.delete(id);
    }
  }
  host.activeOrder = [];
  host.sessionHeadsById.clear();
  for (const [sessionId, head] of archivedHeads) {
    host.sessionHeadsById.set(sessionId, head);
  }

  const nextActiveIds = new Set<string>();
  for (const summary of activeTasks) {
    const task = summary?.task;
    if (!task || typeof task !== "object" || task.archived_at) continue;
    const normalized = normalizeActiveSummaryForStore(
      host,
      summary,
      host.tasks.get(idToString(task.id)),
    );
    nextActiveIds.add(normalized.id);
    host.tasks.set(normalized.id, normalized);
    host.placeInOrders(normalized);
  }
  restoreRetainedLiveSessionHeadsForStore(host, previousHeads);

  const totalCountRaw = cached.active?.totalCount;
  const totalCount =
    typeof totalCountRaw === "number" && Number.isFinite(totalCountRaw)
      ? totalCountRaw
      : nextActiveIds.size;
  host.totalActive = Math.max(totalCount, nextActiveIds.size);
  host.snapshotRev = Math.max(host.snapshotRev, cached.snapshotRev ?? 0);
  host.snapshot = {
    ...host.snapshot,
    initialized: true,
  };
  host.syncSnapshot();
};

export const applyWorkspaceSnapshotToStoreState = (
  host: WorkspaceActiveSnapshotStoreSnapshotHost,
  snapshot: WorkspaceActiveSnapshot,
  heads?: SessionHeadSnapshot[] | null,
  opts?: { resetSnapshotRev?: boolean },
): void => {
  const incomingRev = typeof snapshot.snapshot_rev === "number" ? snapshot.snapshot_rev : 0;
  host.snapshotRev = opts?.resetSnapshotRev
    ? incomingRev
    : Math.max(host.snapshotRev, incomingRev);
  if (typeof snapshot.archived_rev === "number" && snapshot.archived_rev > host.archivedRev) {
    host.archivedRev = snapshot.archived_rev;
    host.archivedLoaded = false;
    host.archivedCursor = null;
  }

  const archivedHeads = collectArchivedHeadsForStore(host);
  const previousHeads = new Map(host.sessionHeadsById);
  for (const [id, item] of host.tasks.entries()) {
    if (!item.task.archived_at) {
      host.tasks.delete(id);
    }
  }
  host.activeOrder = [];
  host.sessionHeadsById.clear();
  for (const [sessionId, head] of archivedHeads) {
    host.sessionHeadsById.set(sessionId, head);
  }

  const nextActiveIds = new Set<string>();
  for (const summary of snapshot.active?.tasks ?? []) {
    const existing = host.tasks.get(idToString(summary.task.id));
    const normalized = normalizeActiveSummaryForStore(host, summary, existing);
    nextActiveIds.add(normalized.id);
    host.tasks.set(normalized.id, normalized);
    host.placeInOrders(normalized);
  }

  const nextTotalActive = Number.isFinite(snapshot.active?.total_count)
    ? snapshot.active.total_count
    : nextActiveIds.size;
  host.totalActive = Math.max(nextTotalActive, nextActiveIds.size);
  if (Array.isArray(heads) && heads.length > 0) {
    applyActiveHeadsForStore(host, heads);
  }
  restoreRetainedLiveSessionHeadsForStore(host, previousHeads);
  host.snapshot = {
    ...host.snapshot,
    initialized: true,
  };
  host.liveSnapshotApplied = true;
  host.syncSnapshot();
};
