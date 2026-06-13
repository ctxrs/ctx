import type {
  SessionHeadDelta,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  Task,
  WorkspaceActiveSnapshot,
  WorkspaceActiveSnapshotSessionSummaryDeltaEvent,
  WorkspaceActiveSnapshotTaskDeltaEvent,
  WorkspaceActiveTaskSummary,
  WorkspaceArchivedPage,
  WorkspaceIndexCursor,
  WorkspaceTaskSummary,
} from "@ctx/types";
import { idToString } from "../../api/client";
import type { WorkspaceActiveSnapshotPatch } from "../workspaceActiveSnapshotProtocol";
import type { PersistedWorkspaceActiveSnapshotV1 } from "../uiStateStore";
import { compactActiveSessionHeadSnapshot } from "../sessionHeadState";
import { findWorkspaceActiveSnapshotInsertIndex } from "./storeOrdering";
import { collectWorkspaceActivePrimarySessionIds } from "./projection";
import {
  isSessionHeadCompatibleWithSummary,
  shouldReplaceSessionHead,
  taskSortAt,
} from "./summaryHelpers";
import type {
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "./storeTypes";
import { applySessionHeadDeltaToSnapshot } from "./sessionHeadDeltaApply";
import {
  applySessionSummaryDeltaToTasks,
  applySessionSummaryToTasks,
} from "./sessionSummaryApply";
import {
  applyCachedSnapshotToStoreState,
  applyWorkerPatchToStoreState,
  applyWorkspaceSnapshotToStoreState,
  buildPersistedSnapshotForStoreState,
} from "./storeSnapshots";
import {
  buildArchivedSnapshotItemForStore,
  normalizeActiveSummaryForStore,
  pruneRetainedSessionHeadsForStore,
  resolveSessionSummaryForStore,
  shouldRetainSessionHeadForStore,
} from "./storeSessionHeads";

export class WorkspaceActiveSnapshotStoreState {
  snapshot: WorkspaceActiveSnapshotState;
  tasks = new Map<string, WorkspaceActiveSnapshotItem>();
  sessionHeadsById = new Map<string, SessionHeadSnapshot>();
  retainedLiveSessionIds = new Set<string>();
  worktreeRootsById = new Map<string, string>();
  activeSessionIds: string[] = [];
  activeOrder: string[] = [];
  archivedOrder: string[] = [];
  totalActive = 0;
  totalArchived = 0;
  hasMoreActive = true;
  hasMoreArchived = false;
  archivedLoaded = false;
  archivedCursor: WorkspaceIndexCursor | null = null;
  snapshotRev = 0;
  archivedRev = 0;
  liveSnapshotApplied = false;

  constructor(private readonly workspaceId: string) {
    this.snapshot = {
      workspaceId,
      initialized: false,
      liveSnapshotApplied: false,
      connection: "idle",
      tasksById: {},
      activeIds: [],
      archivedIds: [],
      totalActive: 0,
      totalArchived: 0,
      archivedRev: 0,
      fetchState: { active: "idle", archived: "idle" },
      hasMoreActive: true,
      hasMoreArchived: false,
      archivedLoaded: false,
    };
  }

  getSnapshot = (): WorkspaceActiveSnapshotState => this.snapshot;

  getSessionHeadSnapshot = (sessionId: string): SessionHeadSnapshot | null => {
    const id = idToString(sessionId);
    if (!id) return null;
    return this.sessionHeadsById.get(id) ?? null;
  };

  getSessionHeadsSnapshot = (): Record<string, SessionHeadSnapshot> =>
    Object.fromEntries(this.sessionHeadsById.entries()) as Record<string, SessionHeadSnapshot>;

  getWorktreeRoot = (worktreeId: string): string | null => {
    const id = idToString(worktreeId);
    if (!id) return null;
    return this.worktreeRootsById.get(id) ?? null;
  };

  getWorktreeRootsSnapshot = (): Record<string, string> =>
    Object.fromEntries(this.worktreeRootsById.entries()) as Record<string, string>;

  getSnapshotRev = (): number => this.snapshotRev;

  setRetainedLiveSessionIds(sessionIds: readonly string[]): boolean {
    const next = new Set<string>();
    for (const sessionId of sessionIds) {
      const id = idToString(sessionId);
      if (id) next.add(id);
    }
    if (
      next.size === this.retainedLiveSessionIds.size &&
      Array.from(next).every((sessionId) => this.retainedLiveSessionIds.has(sessionId))
    ) {
      return false;
    }
    this.retainedLiveSessionIds = next;
    this.syncSnapshot();
    return true;
  }

  getArchivedRev = (): number => this.archivedRev;

  getActiveSessionIds = (): string[] => this.activeSessionIds.slice();

  getFetchState = (target: "active" | "archived"): "idle" | "loading" | "error" =>
    this.snapshot.fetchState[target];

  getHasMoreArchived = (): boolean => this.hasMoreArchived;

  getArchivedCursor = (): WorkspaceIndexCursor | null => this.archivedCursor;

  hasLiveSnapshotApplied = (): boolean => this.liveSnapshotApplied;

  setConnection(connection: WorkspaceActiveSnapshotState["connection"]): boolean {
    if (this.snapshot.connection === connection) return false;
    this.snapshot = { ...this.snapshot, connection };
    return true;
  }

  setFetchState(target: "active" | "archived", state: "idle" | "loading" | "error"): boolean {
    if (this.snapshot.fetchState[target] === state) return false;
    this.snapshot = {
      ...this.snapshot,
      fetchState: { ...this.snapshot.fetchState, [target]: state },
    };
    return true;
  }

  updateSnapshotRev(nextRev: number, opts?: { allowReset?: boolean }): boolean {
    if (!Number.isFinite(nextRev)) return false;
    if (opts?.allowReset && nextRev < this.snapshotRev) {
      if (nextRev === this.snapshotRev) return false;
      this.snapshotRev = nextRev;
      return true;
    }
    if (nextRev <= this.snapshotRev) return false;
    this.snapshotRev = nextRev;
    return true;
  }

  updateArchivedRev(nextRev: number): boolean {
    if (!Number.isFinite(nextRev) || nextRev === this.archivedRev) return false;
    this.archivedRev = nextRev;
    this.archivedLoaded = false;
    this.archivedCursor = null;
    this.syncSnapshot();
    return true;
  }

  applyWorktreeRoot(worktreeId: string, root: string): boolean {
    const id = idToString(worktreeId);
    const nextRoot = String(root ?? "").trim();
    if (!id || !nextRoot || this.worktreeRootsById.get(id) === nextRoot) return false;
    this.worktreeRootsById.set(id, nextRoot);
    return true;
  }

  applyWorkerPatch(patch: WorkspaceActiveSnapshotPatch): void {
    applyWorkerPatchToStoreState(this, patch);
  }

  buildPersistedSnapshot(): Omit<
    PersistedWorkspaceActiveSnapshotV1,
    "v" | "workspaceId" | "updatedAtMs"
  > {
    return buildPersistedSnapshotForStoreState(this);
  }

  applyCachedSnapshot(cached: PersistedWorkspaceActiveSnapshotV1): void {
    applyCachedSnapshotToStoreState(this, cached);
  }

  applyWorkspaceSnapshot(
    snapshot: WorkspaceActiveSnapshot,
    heads?: SessionHeadSnapshot[] | null,
    opts?: { resetSnapshotRev?: boolean },
  ): void {
    applyWorkspaceSnapshotToStoreState(this, snapshot, heads, opts);
  }

  applyArchivedPage(page: WorkspaceArchivedPage, items: Array<WorkspaceActiveSnapshotItem | null>) {
    if (typeof page.archived_rev === "number" && page.archived_rev > this.archivedRev) {
      this.archivedRev = page.archived_rev;
    }
    this.totalArchived = page.total_archived ?? this.totalArchived;
    for (const item of items) {
      if (item) {
        this.upsertArchivedItem(item, { adjustCounts: false, sync: false });
      }
    }
    this.archivedCursor = page.next_cursor ?? null;
    this.hasMoreArchived = Boolean(page.next_cursor);
    this.archivedLoaded = true;
    this.syncSnapshot();
  }

  resetArchivedCursor(): void {
    this.archivedCursor = null;
  }

  markArchivedExhausted(): boolean {
    if (!this.hasMoreArchived) return false;
    this.hasMoreArchived = false;
    this.syncSnapshot();
    return true;
  }

  applyTaskUpdate(task: Task): boolean {
    const id = idToString(task.id);
    if (!id) return false;
    const existing = this.tasks.get(id);
    if (!existing) return false;
    const prevArchived = Boolean(existing.task.archived_at);
    const nextArchived = Boolean(task.archived_at);
    const stableSortAt = taskSortAt(task, existing.sort_at);
    const stableSortAtMs = Date.parse(stableSortAt ?? "") || existing.sortAtMs || Date.now();
    const updated: WorkspaceActiveSnapshotItem = {
      ...existing,
      task: { ...task },
      sortAtMs: stableSortAtMs,
      sort_at: stableSortAt ?? existing.sort_at,
    };
    this.tasks.set(id, updated);
    if (prevArchived !== nextArchived) {
      this.archivedLoaded = false;
      this.archivedCursor = null;
      this.hasMoreArchived = true;
    }
    this.updateCountsForMove(existing, updated);
    this.placeInOrders(updated);
    this.syncSnapshot();
    return true;
  }

  removeTask(taskId: string | undefined, opts?: { adjustCounts?: boolean; sync?: boolean }): boolean {
    const deleteId = idToString(taskId ?? "");
    if (!deleteId) return false;
    const existing = this.tasks.get(deleteId);
    if (!existing) return false;
    if (existing.primarySessionId) {
      this.sessionHeadsById.delete(existing.primarySessionId);
    }
    this.tasks.delete(deleteId);
    this.activeOrder = this.activeOrder.filter((id) => id !== deleteId);
    this.archivedOrder = this.archivedOrder.filter((id) => id !== deleteId);
    if (opts?.adjustCounts) {
      if (existing.task.archived_at) {
        this.totalArchived = Math.max(0, this.totalArchived - 1);
      } else {
        this.totalActive = Math.max(0, this.totalActive - 1);
      }
    }
    if (opts?.sync ?? true) {
      this.syncSnapshot();
    }
    return true;
  }

  removeArchivedTask(taskId: string | undefined, opts?: { adjustCounts?: boolean; sync?: boolean }): boolean {
    const deleteId = idToString(taskId ?? "");
    if (!deleteId) return false;
    const existing = this.tasks.get(deleteId);
    if (!existing?.task.archived_at) return false;
    return this.removeTask(deleteId, opts);
  }

  upsertActiveSummary(summary: WorkspaceActiveTaskSummary): boolean {
    const existing = this.tasks.get(idToString(summary.task.id));
    const normalized = normalizeActiveSummaryForStore(this, summary, existing);
    if (existing?.primarySessionId && existing.primarySessionId !== normalized.primarySessionId) {
      this.sessionHeadsById.delete(existing.primarySessionId);
    }
    this.tasks.set(normalized.id, normalized);
    if (existing) {
      this.updateCountsForMove(existing, normalized);
    } else {
      this.totalActive += 1;
    }
    this.placeInOrders(normalized);
    this.syncSnapshot();
    return true;
  }

  upsertArchivedItem(
    item: WorkspaceActiveSnapshotItem,
    opts?: { adjustCounts?: boolean; sync?: boolean },
  ): boolean {
    const existing = this.tasks.get(item.id);
    this.tasks.set(item.id, item);
    if (opts?.adjustCounts ?? true) {
      if (existing) {
        this.updateCountsForMove(existing, item);
      } else {
        this.totalArchived += 1;
      }
    }
    this.placeInOrders(item);
    if (opts?.sync ?? true) {
      this.syncSnapshot();
    }
    return true;
  }

  applyTaskDelta(evt: WorkspaceActiveSnapshotTaskDeltaEvent): boolean {
    const delta = evt.delta;
    const taskId = idToString(delta?.task?.id ?? "");
    if (!taskId) return false;
    const existing = this.tasks.get(taskId);
    if (!existing) return false;

    if (delta.kind === "archived") {
      return this.removeTask(taskId, { adjustCounts: true });
    }

    const deltaTask =
      delta.kind === "unarchived" ? { ...delta.task, archived_at: null } : delta.task;
    const nextTask = {
      ...existing.task,
      ...deltaTask,
      id: existing.task.id,
      workspace_id: existing.task.workspace_id,
    };
    const sortAt = taskSortAt(nextTask, existing.sort_at ?? null);
    const sortAtMs = Date.parse(sortAt) || existing.sortAtMs || Date.now();
    const nextPrimarySessionId = idToString(nextTask.primary_session_id ?? "") || null;
    if (existing.primarySessionId && existing.primarySessionId !== nextPrimarySessionId) {
      this.sessionHeadsById.delete(existing.primarySessionId);
    }
    const primarySessionHead = nextPrimarySessionId
      ? (this.sessionHeadsById.get(nextPrimarySessionId) ?? null)
      : null;
    const nextItem: WorkspaceActiveSnapshotItem = {
      ...existing,
      task: nextTask,
      primarySessionId: nextPrimarySessionId,
      primarySessionHead,
      sortAtMs,
      sort_at: sortAt || null,
    };
    if (Boolean(existing.task.archived_at) !== Boolean(nextItem.task.archived_at)) {
      this.archivedLoaded = false;
      this.archivedCursor = null;
      this.hasMoreArchived = true;
    }
    this.tasks.set(taskId, nextItem);
    this.updateCountsForMove(existing, nextItem);
    this.placeInOrders(nextItem);
    this.syncSnapshot();
    return true;
  }

  applySessionSummaryDelta(evt: WorkspaceActiveSnapshotSessionSummaryDeltaEvent): boolean {
    const changed = applySessionSummaryDeltaToTasks({ delta: evt.delta, tasks: this.tasks });
    if (changed) this.syncSnapshot();
    return changed;
  }

  applySessionSummary(summary: SessionSnapshotSummary): boolean {
    const changed = applySessionSummaryToTasks({ summary, tasks: this.tasks });
    if (!changed) return false;
    this.syncSnapshot();
    return true;
  }

  applySessionHeadDelta(delta: SessionHeadDelta): boolean {
    return applySessionHeadDeltaToSnapshot({
      delta,
      tasks: this.tasks,
      sessionHeadsById: this.sessionHeadsById,
      shouldRetainSessionHead: (sessionId) => shouldRetainSessionHeadForStore(this, sessionId),
    });
  }

  applySessionHeadSeed(head: SessionHeadSnapshot | null | undefined): boolean {
    if (!head) return false;
    const sessionId = idToString(head.session?.id ?? "");
    if (!sessionId) return false;
    const summary = resolveSessionSummaryForStore(this, sessionId);
    if (this.retainedLiveSessionIds.has(sessionId) && !summary) return false;
    if (!shouldRetainSessionHeadForStore(this, sessionId)) return false;
    const compacted = compactActiveSessionHeadSnapshot(head);
    if (summary && !isSessionHeadCompatibleWithSummary(summary, compacted)) return false;
    const previous = this.sessionHeadsById.get(sessionId);
    if (!shouldReplaceSessionHead(previous, compacted)) return false;
    this.sessionHeadsById.set(sessionId, compacted);
    return true;
  }

  buildArchivedItem(
    summary: WorkspaceTaskSummary,
    primaryHead?: SessionHeadSnapshot | null,
  ): WorkspaceActiveSnapshotItem | null {
    void primaryHead;
    return buildArchivedSnapshotItemForStore(this, summary);
  }

  syncSnapshot(): void {
    pruneRetainedSessionHeadsForStore(this);
    const tasksById: Record<string, WorkspaceActiveSnapshotItem> = {};
    for (const [id, item] of this.tasks.entries()) {
      tasksById[id] = item;
    }
    this.hasMoreActive = this.totalActive > this.activeOrder.length;
    this.activeSessionIds = collectWorkspaceActivePrimarySessionIds({
      activeIds: this.activeOrder,
      archivedIds: this.archivedOrder,
      tasksById,
    });
    this.snapshot = {
      ...this.snapshot,
      liveSnapshotApplied: this.liveSnapshotApplied,
      tasksById,
      activeIds: [...this.activeOrder],
      archivedIds: [...this.archivedOrder],
      totalActive: this.totalActive,
      totalArchived: this.totalArchived,
      archivedRev: this.archivedRev,
      hasMoreActive: this.hasMoreActive,
      hasMoreArchived: this.hasMoreArchived,
      archivedLoaded: this.archivedLoaded,
    };
  }

  private updateCountsForMove(
    prev: WorkspaceActiveSnapshotItem,
    next: WorkspaceActiveSnapshotItem,
  ): void {
    const prevArchived = Boolean(prev.task.archived_at);
    const nextArchived = Boolean(next.task.archived_at);
    if (prevArchived === nextArchived) return;
    if (prevArchived) {
      this.totalArchived = Math.max(0, this.totalArchived - 1);
      this.totalActive += 1;
    } else {
      this.totalActive = Math.max(0, this.totalActive - 1);
      this.totalArchived += 1;
    }
  }

  placeInOrders(item: WorkspaceActiveSnapshotItem): void {
    const { id } = item;
    this.activeOrder = this.activeOrder.filter((existing) => existing !== id);
    this.archivedOrder = this.archivedOrder.filter((existing) => existing !== id);
    if (item.task.archived_at) {
      this.archivedOrder.splice(
        findWorkspaceActiveSnapshotInsertIndex(this.tasks, this.archivedOrder, item.sortAtMs, id),
        0,
        id,
      );
      return;
    }
    this.activeOrder.splice(
      findWorkspaceActiveSnapshotInsertIndex(this.tasks, this.activeOrder, item.sortAtMs, id),
      0,
      id,
    );
  }
}
