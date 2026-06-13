import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ArchiveTaskResponse,
  type Task,
  archiveTask,
  deleteTask,
  idToString,
  unarchiveTask,
  updateTaskTitle,
} from "../../api/client";
import type {
  SessionSupervisor,
  SessionSupervisorSnapshot,
} from "../../state/sessionSupervisor";
import { isReplicaAuthority } from "../../state/sessionSupervisor/config";
import type {
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "../../state/workspaceActiveSnapshotStore";
import { useEnsureArchivedLoaded } from "../../state/useEnsureArchivedLoaded";
import { findHarnessCatalogEntry, HARNESS_CATALOG } from "../../utils/harnessCatalog";
import { errorMessage } from "../../utils/errorMessage";
import type { WorkbenchStore } from "../../workbench/store";
import { TaskRow } from "./WorkbenchPage.taskRow";
import type {
  AnchorRect,
  ArchiveConfirmState,
  OptimisticTaskSummary,
} from "./WorkbenchPage.types";
import {
  ARCHIVE_CONFIRM_STORAGE_KEY,
  clampNum,
  isOptimisticTask,
  normalizeAnchorRect,
  parseMs,
} from "./WorkbenchPage.utils";
import { useWorkbenchTaskContextMenu } from "./useWorkbenchTaskContextMenu";
import { useWorkbenchTaskListModel } from "./workbenchTaskListModel";
import {
  deriveWorkbenchTaskStatusKind,
  isWorkbenchTaskUnread,
  type WorkbenchTaskLiveInfo,
} from "./workbenchTaskActivity";

type WorkspaceSnapshotStore = {
  ensureArchivedLoaded: () => void;
  loadMoreActive: () => void;
  loadMoreArchived: () => void;
  applyTaskUpdate: (task: Task) => void;
};

type TaskListControllerArgs = {
  workspaceId: string;
  activeTaskId: string | null;
  activeSessionId?: string | null;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  workspaceSnapshotStore: WorkspaceSnapshotStore;
  optimisticTasks: OptimisticTaskSummary[];
  setOptimisticTasks: React.Dispatch<React.SetStateAction<OptimisticTaskSummary[]>>;
  optimisticTasksById: Record<string, OptimisticTaskSummary>;
  taskLiveInfo: WorkbenchTaskLiveInfo;
  providerIdsByTaskFromSessions: Record<string, string[]>;
  sessionEntries: SessionSupervisorSnapshot["sessions"];
  isTaskUnread: (taskId: string) => boolean;
  focusTask: (taskId: string, sessionId?: string | null) => void;
  focusNewTask: () => void;
  markTaskRead: (taskId: string) => Promise<void>;
  markTaskUnread: (taskId: string) => Promise<void>;
  supervisor: Pick<SessionSupervisor, "dropSessionEntry">;
  workbenchStore: Pick<WorkbenchStore, "focusNewTask" | "getActiveTab" | "getNavToken">;
};

export function useWorkbenchTaskListController({
  workspaceId,
  activeTaskId,
  activeSessionId,
  tasksById,
  workspaceSnapshot,
  workspaceSnapshotStore,
  optimisticTasks,
  setOptimisticTasks,
  optimisticTasksById,
  taskLiveInfo,
  providerIdsByTaskFromSessions,
  sessionEntries,
  isTaskUnread,
  focusTask,
  focusNewTask,
  markTaskRead,
  markTaskUnread,
  supervisor,
  workbenchStore,
}: TaskListControllerArgs) {
  const [taskQuery, setTaskQuery] = useState("");
  const taskSearchRef = useRef<HTMLInputElement | null>(null);
  const [archivedCollapsed, setArchivedCollapsed] = useState(true);
  const [archiveConfirm, setArchiveConfirm] = useState<ArchiveConfirmState | null>(null);
  const [archiveConfirmDontRemind, setArchiveConfirmDontRemind] = useState(false);
  const [archiveConfirmDismissed, setArchiveConfirmDismissed] = useState(false);
  const [archivePendingById, setArchivePendingById] = useState<Record<string, "archive" | "unarchive">>({});
  const [archiveCleanupNotice, setArchiveCleanupNotice] = useState(false);
  const archiveConfirmRef = useRef<HTMLDivElement | null>(null);
  const [hoveredTaskId, setHoveredTaskId] = useState<string | null>(null);
  const [renamingTaskId, setRenamingTaskId] = useState<string | null>(null);
  const renameDraftsRef = useRef<Map<string, string>>(new Map());

  useEffect(() => {
    if (!workspaceId) return;
    const key = `wb.archivedCollapsed.${workspaceId}`;
    const value = localStorage.getItem(key);
    if (value === "0") setArchivedCollapsed(false);
    else setArchivedCollapsed(true);
  }, [workspaceId]);

  useEffect(() => {
    try {
      const value = localStorage.getItem(ARCHIVE_CONFIRM_STORAGE_KEY);
      setArchiveConfirmDismissed(value === "1");
    } catch {
      // ignore
    }
  }, []);

  useEffect(() => {
    if (!workspaceId) return;
    localStorage.setItem(`wb.archivedCollapsed.${workspaceId}`, archivedCollapsed ? "1" : "0");
  }, [archivedCollapsed, workspaceId]);

  useEffect(() => {
    if (!archiveConfirm) return;
    const onPointerDown = (event: PointerEvent) => {
      const element = event.target as HTMLElement | null;
      if (!element) return;
      if (element.closest(".wb-archive-confirm")) return;
      if (element.closest(".wb-archive-confirm-trigger")) return;
      setArchiveConfirm(null);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setArchiveConfirm(null);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [archiveConfirm]);

  const getRenameDraft = useCallback((taskId: string, fallback: string) => {
    return renameDraftsRef.current.get(taskId) ?? fallback;
  }, []);

  const setRenameDraft = useCallback((taskId: string, nextValue: string) => {
    renameDraftsRef.current.set(taskId, nextValue);
  }, []);

  const clearRenameDraft = useCallback((taskId: string) => {
    renameDraftsRef.current.delete(taskId);
  }, []);

  const normalizedTaskQuery = taskQuery.trim().toLowerCase();

  const optimisticActiveSummaries = useMemo(() => {
    const activeIdSet = new Set(workspaceSnapshot.activeIds);
    return optimisticTasks.filter((item) => {
      const serverHasItem = Boolean(tasksById[item.id]);
      const activeHasItem = activeIdSet.has(item.id);
      if (item.localStatus === "synced" && serverHasItem && activeHasItem) return false;
      if (!normalizedTaskQuery) return true;
      return (item.task.title ?? "").toLowerCase().includes(normalizedTaskQuery);
    });
  }, [normalizedTaskQuery, optimisticTasks, tasksById, workspaceSnapshot.activeIds]);

  const filteredActiveIds = useMemo(() => {
    return workspaceSnapshot.activeIds.filter((id) => {
      const summary = tasksById[id];
      if (!summary) return false;
      if (!normalizedTaskQuery) return true;
      return (summary.task.title ?? "").toLowerCase().includes(normalizedTaskQuery);
    });
  }, [normalizedTaskQuery, tasksById, workspaceSnapshot.activeIds]);

  const filteredArchivedIds = useMemo(() => {
    return workspaceSnapshot.archivedIds.filter((id) => {
      const summary = tasksById[id];
      if (!summary) return false;
      if (!normalizedTaskQuery) return true;
      return (summary.task.title ?? "").toLowerCase().includes(normalizedTaskQuery);
    });
  }, [normalizedTaskQuery, tasksById, workspaceSnapshot.archivedIds]);

  const activeTaskSummaries = useMemo(() => {
    const optimisticIds = new Set(optimisticActiveSummaries.map((item) => item.id));
    const serverSummaries = filteredActiveIds
      .filter((id) => !optimisticIds.has(id))
      .map((id) => tasksById[id])
      .filter((value): value is WorkspaceActiveSnapshotItem => Boolean(value));
    if (optimisticActiveSummaries.length === 0) return serverSummaries;
    return [...optimisticActiveSummaries, ...serverSummaries];
  }, [filteredActiveIds, optimisticActiveSummaries, tasksById]);

  const archivedTaskSummaries = useMemo(
    () => filteredArchivedIds.map((id) => tasksById[id]).filter((value): value is WorkspaceActiveSnapshotItem => Boolean(value)),
    [filteredArchivedIds, tasksById],
  );

  useEffect(() => {
    if (optimisticTasks.length === 0) return;
    const shouldTrim = optimisticTasks.some((item) => {
      if (item.localStatus !== "synced") return false;
      const serverItem = tasksById[item.id];
      if (!serverItem) return false;
      const hasSession =
        (serverItem.sessions?.length ?? 0) > 0 || Boolean(serverItem.task.primary_session_id);
      return hasSession;
    });
    if (!shouldTrim) return;
    setOptimisticTasks((prev) => {
      const next = prev.filter((item) => {
        if (item.localStatus === "failed") return true;
        const serverItem = tasksById[item.id];
        if (!serverItem) return true;
        const hasSession =
          (serverItem.sessions?.length ?? 0) > 0 || Boolean(serverItem.task.primary_session_id);
        if (!hasSession) return true;
        return item.localStatus !== "synced";
      });
      return next.length === prev.length ? prev : next;
    });
  }, [optimisticTasks, setOptimisticTasks, tasksById]);

  useEnsureArchivedLoaded({
    archivedCollapsed,
    archivedLoaded: workspaceSnapshot.archivedLoaded,
    fetchState: workspaceSnapshot.fetchState.archived,
    activeInitialized: workspaceSnapshot.initialized,
    activeFetchState: workspaceSnapshot.fetchState.active,
    prefetchAfterActive: true,
    ensureArchivedLoaded: workspaceSnapshotStore.ensureArchivedLoaded,
  });

  const applyArchiveToggle = useCallback(
    async (taskId: string, nextArchived: boolean) => {
      const startNavToken = workbenchStore.getNavToken();
      setArchivePendingById((prev) => ({
        ...prev,
        [taskId]: nextArchived ? "archive" : "unarchive",
      }));
      try {
        const updated = nextArchived ? await archiveTask(taskId) : await unarchiveTask(taskId);
        workspaceSnapshotStore.applyTaskUpdate(updated);
        if (nextArchived) {
          const cleanupFailed = (updated as ArchiveTaskResponse).cleanup_failed;
          if (cleanupFailed) {
            setArchiveCleanupNotice(true);
          }
          const activeTab = workbenchStore.getActiveTab();
          const activeTaskIdNow = activeTab?.kind === "task" ? activeTab.ref.taskId : null;
          if (activeTaskIdNow === taskId && workbenchStore.getNavToken() === startNavToken) {
            workbenchStore.focusNewTask({ navToken: startNavToken, source: "system" });
          }
        }
      } finally {
        setArchivePendingById((prev) => {
          if (!(taskId in prev)) return prev;
          const next = { ...prev };
          delete next[taskId];
          return next;
        });
      }
    },
    [workbenchStore, workspaceSnapshotStore],
  );

  const onToggleArchive = useCallback(
    async (taskId: string, nextArchived: boolean, anchor?: AnchorRect | null) => {
      if (taskId in archivePendingById) return;
      if (!nextArchived || archiveConfirmDismissed) {
        if (!nextArchived) {
          setArchiveConfirm(null);
        }
        await applyArchiveToggle(taskId, nextArchived);
        return;
      }
      const normalized = normalizeAnchorRect(anchor);
      setArchiveConfirm({ taskId, anchor: normalized });
      setArchiveConfirmDontRemind(false);
    },
    [applyArchiveToggle, archiveConfirmDismissed, archivePendingById],
  );

  const confirmArchive = useCallback(async () => {
    if (!archiveConfirm) return;
    const taskId = archiveConfirm.taskId;
    if (taskId in archivePendingById) {
      setArchiveConfirm(null);
      return;
    }
    setArchiveConfirm(null);
    if (archiveConfirmDontRemind) {
      try {
        localStorage.setItem(ARCHIVE_CONFIRM_STORAGE_KEY, "1");
      } catch {
        // ignore
      }
      setArchiveConfirmDismissed(true);
    }
    await applyArchiveToggle(taskId, true);
  }, [applyArchiveToggle, archiveConfirm, archiveConfirmDontRemind, archivePendingById]);

  const cancelArchiveConfirm = useCallback(() => {
    setArchiveConfirm(null);
  }, []);

  const dismissArchiveCleanupNotice = useCallback(() => {
    setArchiveCleanupNotice(false);
  }, []);

  const archiveConfirmStyle = useMemo(() => {
    if (!archiveConfirm) return null;
    const rect = archiveConfirm.anchor;
    const margin = 12;
    const viewportWidth = typeof window === "undefined" ? 1200 : window.innerWidth;
    const viewportHeight = typeof window === "undefined" ? 800 : window.innerHeight;
    const width = Math.min(360, viewportWidth - margin * 2);
    const left = clampNum(rect.left + rect.width / 2 - width / 2, margin, viewportWidth - width - margin);
    const top = clampNum(rect.bottom + 10, margin, viewportHeight - 180);
    return { left, top, width };
  }, [archiveConfirm]);

  const dismissOptimisticTask = useCallback(
    (taskId: string) => {
      const summary = optimisticTasksById[taskId];
      if (!summary) return;
      if (activeTaskId === taskId) {
        focusNewTask();
      }
      const sessionId = summary.primarySessionId ? String(summary.primarySessionId) : "";
      if (sessionId) {
        supervisor.dropSessionEntry(sessionId);
      }
      setOptimisticTasks((prev) => prev.filter((item) => item.id !== taskId));
    },
    [activeTaskId, focusNewTask, optimisticTasksById, setOptimisticTasks, supervisor],
  );

  const beginRenameTask = useCallback(
    (taskId: string) => {
      if (renamingTaskId && renamingTaskId !== taskId) {
        clearRenameDraft(renamingTaskId);
      }
      setRenamingTaskId(taskId);
    },
    [clearRenameDraft, renamingTaskId],
  );

  const cancelRenameTask = useCallback(() => {
    if (renamingTaskId) {
      clearRenameDraft(renamingTaskId);
    }
    setRenamingTaskId(null);
  }, [clearRenameDraft, renamingTaskId]);

  const commitRenameTask = useCallback(
    async (taskId: string, nextValue: string) => {
      const next = nextValue.trim();
      if (!next) {
        window.alert("Task title is required.");
        return;
      }
      const current = String(tasksById[taskId]?.task.title ?? "").trim();
      if (current && current === next) {
        clearRenameDraft(taskId);
        cancelRenameTask();
        return;
      }
      try {
        const updated = await updateTaskTitle(taskId, next);
        workspaceSnapshotStore.applyTaskUpdate(updated);
        clearRenameDraft(taskId);
        cancelRenameTask();
      } catch (error: unknown) {
        window.alert(errorMessage(error) || "Failed to rename.");
      }
    },
    [cancelRenameTask, clearRenameDraft, tasksById, workspaceSnapshotStore],
  );

  const onDeleteTask = useCallback(
    async (taskId: string) => {
      const summary = tasksById[taskId];
      const title = String(summary?.task.title ?? "this task");
      if (!window.confirm(`Delete “${title}”? This deletes all sessions and messages in the task.`)) return;
      try {
        await deleteTask(taskId);
        if (activeTaskId === taskId) {
          focusNewTask();
        }
      } catch (error: unknown) {
        window.alert(errorMessage(error) || "Failed to delete task.");
      }
    },
    [activeTaskId, focusNewTask, tasksById],
  );

  const isArchivePending = useCallback(
    (taskId: string | null | undefined) => (taskId ? Boolean(archivePendingById[taskId]) : false),
    [archivePendingById],
  );

  const {
    taskMenu,
    taskMenuRef,
    openTaskMenu,
    taskMenuArchiveDisabled,
    taskMenuArchiveLabel,
    taskMenuMarkReadDisabled,
    taskMenuMarkReadLabel,
    onTaskMenuRename,
    onTaskMenuToggleArchive,
    onTaskMenuToggleRead,
    onTaskMenuDelete,
  } = useWorkbenchTaskContextMenu({
    tasksById,
    beginRenameTask,
    isArchivePending,
    isTaskUnread,
    onToggleArchive,
    markTaskRead,
    markTaskUnread,
    onDeleteTask,
  });

  const renderTaskRow = useCallback(
    (summary: WorkspaceActiveSnapshotItem, opts?: { archived?: boolean }) => {
      const taskId = summary.id;
      const taskIndex = opts?.archived
        ? undefined
        : activeTaskSummaries.findIndex((candidate) => candidate.id === taskId);
      const task = summary.task;
      const optimistic = isOptimisticTask(summary) ? summary : null;
      const localStatus = optimistic?.localStatus ?? null;
      const selected = taskId === activeTaskId;
      const hovered = taskId === hoveredTaskId;
      const archived = Boolean(opts?.archived);
      const pendingAction = archivePendingById[taskId];
      const archivePending = typeof pendingAction !== "undefined";
      const title = task.title ?? "New Task";
      const focusSessionId =
        idToString(summary.primarySessionId) ||
        idToString(task.primary_session_id ?? "") ||
        idToString(summary.primarySessionHead?.session?.id ?? "");
      const focusEntry = focusSessionId ? sessionEntries[focusSessionId] : undefined;
      const working = taskLiveInfo.workingByTask.has(taskId);
      const hasError = taskLiveInfo.errorByTask.has(taskId);
      const unread = !working && isWorkbenchTaskUnread({ taskId, tasksById, taskLiveInfo });
      // Deliberately show recency in the nav ("last activity"), not run duration.
      // The thread view owns precise per-turn elapsed timing.
      const ageIso = task.last_activity_at ?? task.updated_at ?? task.created_at;
      let statusKind: "archive" | "error" | "working" | "unread" | "idle" = archivePending
        ? "archive"
        : deriveWorkbenchTaskStatusKind({
            hasError,
            working,
            unread,
            localStatus,
          });
      const summaryProviders =
        summary.providerIds && summary.providerIds.length
          ? summary.providerIds
          : summary.sessions.map((session) => String(session.session.provider_id ?? "").trim()).filter(Boolean);
      const providerIds =
        (providerIdsByTaskFromSessions[taskId] ?? []).length > 0
          ? providerIdsByTaskFromSessions[taskId]
          : summaryProviders;
      const providerCount = new Set(providerIds).size;
      const harnesses = providerIds
        .map((providerId) => findHarnessCatalogEntry(providerId))
        .filter(Boolean)
        .slice(0, 3) as Array<(typeof HARNESS_CATALOG)[number]>;
      const allowActions = !optimistic;
      const dismissHandler = localStatus === "failed" ? () => dismissOptimisticTask(taskId) : undefined;
      const dismissText = localStatus === "failed" ? "Dismiss failed start" : undefined;

      return (
        <TaskRow
          key={taskId}
          taskId={taskId}
          sessionId={focusSessionId || null}
          activeSessionId={activeSessionId ?? null}
          taskIndex={typeof taskIndex === "number" && taskIndex >= 0 ? taskIndex : undefined}
          subscribedAtClick={focusEntry ? focusEntry.subscribed : false}
          authoritativeAtClick={focusEntry ? isReplicaAuthority(focusEntry.freshness) : false}
          title={title}
          archived={archived}
          archivePending={archivePending}
          archivePendingAction={pendingAction ?? null}
          statusKind={statusKind}
          selected={selected}
          hovered={hovered}
          isRenaming={renamingTaskId === taskId}
          ageIso={ageIso}
          providerCount={providerCount}
          harnesses={harnesses}
          getRenameDraft={getRenameDraft}
          setRenameDraft={setRenameDraft}
          onFocusTask={focusTask}
          onOpenMenu={openTaskMenu}
          menuEnabled={allowActions}
          archiveEnabled={allowActions}
          onDismiss={dismissHandler}
          dismissLabel={dismissText}
          onToggleArchive={onToggleArchive}
          onHoverEnter={(id) => setHoveredTaskId(id)}
          onHoverLeave={(id) => setHoveredTaskId((current) => (current === id ? null : current))}
          onCancelRename={cancelRenameTask}
          onCommitRename={commitRenameTask}
        />
      );
    },
    [
      activeTaskId,
      activeSessionId,
      activeTaskSummaries,
      archivePendingById,
      cancelRenameTask,
      commitRenameTask,
      dismissOptimisticTask,
      focusTask,
      getRenameDraft,
      hoveredTaskId,
      onToggleArchive,
      openTaskMenu,
      providerIdsByTaskFromSessions,
      renamingTaskId,
      setRenameDraft,
      sessionEntries,
      taskLiveInfo.errorByTask,
      taskLiveInfo.workingByTask,
    ],
  );

  const {
    taskListVirtuosoKey,
    taskListItems,
    initialTaskListItemCount,
    computeTaskListItemKey,
    renderTaskListItem,
    taskListContext,
    onTaskListRangeChanged,
  } = useWorkbenchTaskListModel({
    activeTaskSummaries,
    archivedTaskSummaries,
    archivedCollapsed,
    setArchivedCollapsed,
    workspaceSnapshot,
    workspaceSnapshotStore,
    renderTaskRow,
  });

  return {
    taskSearchRef,
    taskQuery,
    setTaskQuery,
    onToggleArchive,
    beginRenameTask,
    onDeleteTask,
    markTaskRead,
    markTaskUnread,
    isArchivePending,
    archiveCleanupNotice,
    dismissArchiveCleanupNotice,
    archiveConfirm,
    archiveConfirmStyle,
    archiveConfirmRef,
    archiveConfirmDontRemind,
    setArchiveConfirmDontRemind,
    confirmArchive,
    cancelArchiveConfirm,
    taskMenu,
    taskMenuRef,
    taskMenuArchiveDisabled,
    taskMenuArchiveLabel,
    taskMenuMarkReadDisabled,
    taskMenuMarkReadLabel,
    onTaskMenuRename,
    onTaskMenuToggleArchive,
    onTaskMenuToggleRead,
    onTaskMenuDelete,
    taskListVirtuosoKey,
    taskListItems,
    initialTaskListItemCount,
    computeTaskListItemKey,
    renderTaskListItem,
    taskListContext,
    onTaskListRangeChanged,
  };
}
