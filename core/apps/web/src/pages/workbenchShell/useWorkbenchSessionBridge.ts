import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { idToString, type SessionHeadSnapshot } from "../../api/client";
import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { SessionHeadBootstrapCache } from "../../state/sessionHeadBootstrapCache";
import {
  useSessionLifecycleCoordinator,
  type SessionSupervisor,
  type SessionSupervisorSnapshot,
} from "../../state/sessionSupervisor";
import type {
  WorkspaceActiveSnapshotEventSource,
  WorkspaceActiveSnapshotItem,
  WorkspaceActiveSnapshotState,
} from "../../state/workspaceActiveSnapshotStore";
import { WORKBENCH_TASK_IDLE_EVENT, type WorkbenchTaskIdleDetail } from "../../utils/updaterEvents";
import { getAppForegroundSnapshot, subscribeAppForeground } from "../../utils/windowFocus";
import { hasSessionActiveTurn } from "../../utils/sessionActivity";
import type { WorkbenchStore } from "../../workbench/store";
import {
  noteNavThreadActivityMismatch,
  noteSwitchStaleVisible,
} from "../../state/foregroundFreshnessTelemetry";
import type { OptimisticTaskSummary } from "./WorkbenchPage.types";
import {
  collectSessionHeadsForSupervisor,
  collectAuthoritativePrefetchReadySessionIds,
  buildWorkspaceSyncPrefetchVersionKey,
  planSessionHeadPrefetchTargets,
  primeAuthoritativeSessionHeads,
  type SessionHeadPrefetchReason,
  maybeCacheSessionHeadSeed,
  noteWorkspaceSyncPrefetchSuppressed,
  primePersistedSessionHeads,
} from "./sessionHeadPrefetch";
import {
  canRenderWorkbenchActiveSession,
  deriveActiveTaskSessionIds,
  deriveProviderIdsByTask,
  deriveTaskLiveInfo,
  deriveWarmSessionIds,
  isWorkbenchTaskUnread,
  resolveWorkbenchActiveSessionId,
} from "./workbenchTaskActivity";

type TaskBridgeArgs = {
  activeTaskId: string | null;
  activeSessionIdFromTab: string | null;
  activeTaskSummary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  sessionSnap: SessionSupervisorSnapshot;
  optimisticTasks: OptimisticTaskSummary[];
  optimisticTasksById: Record<string, OptimisticTaskSummary>;
  supervisor: Pick<
    SessionSupervisor,
    | "setActiveTaskSessionIds"
    | "setWarmSessionIds"
    | "setSubscribedSessionIdsSink"
    | "setWorkspaceSnapshotState"
    | "setWorkspaceSessionHeads"
    | "upsertWorkspaceSessionHead"
    | "handleWorkspaceEvent"
  >;
  workbenchStore: Pick<WorkbenchStore, "getActiveTab" | "setActiveSessionForActiveTask">;
  workspaceSnapshotStore: Pick<
    WorkspaceActiveSnapshotEventSource,
    | "subscribe"
    | "subscribeEvents"
    | "getSnapshot"
    | "getSessionHeadSnapshot"
    | "setForegroundSessionId"
    | "setSubscribedSessions"
  > & { getSessionHeadsSnapshot?: () => Record<string, SessionHeadSnapshot> };
  markTaskRead: (taskId: string) => Promise<void>;
};

const readWorkspaceSessionHeads = (
  snapshot: WorkspaceActiveSnapshotState,
  store: TaskBridgeArgs["workspaceSnapshotStore"],
  bootstrapHeads: SessionHeadBootstrapCache,
  sessionIds?: readonly string[],
): Record<string, SessionHeadSnapshot> => {
  return collectSessionHeadsForSupervisor(snapshot, store, bootstrapHeads, sessionIds);
};

export const deriveRetainedPrefetchSessionIds = ({
  snapshot,
  foregroundSessionIds,
  suppressWarmSessionIds = false,
}: {
  snapshot: WorkspaceActiveSnapshotState;
  foregroundSessionIds: readonly string[];
  taskArchived: boolean;
  suppressWarmSessionIds?: boolean;
}): string[] => {
  const liveForegroundSessionIds = foregroundSessionIds
    .map((sessionId) => idToString(sessionId))
    .filter((sessionId) => sessionId.length > 0);
  const warmSessionIds = suppressWarmSessionIds
    ? []
    : deriveWarmSessionIds({
        activeTaskSessionIds: liveForegroundSessionIds,
        tasksById: snapshot.tasksById,
        activeIds: snapshot.activeIds,
      });
  return planSessionHeadPrefetchTargets({
    foregroundSessionIds: liveForegroundSessionIds,
    warmSessionIds,
  }).targetSessionIds;
};

export function useWorkbenchSessionBridge({
  activeTaskId,
  activeSessionIdFromTab,
  activeTaskSummary,
  tasksById,
  workspaceSnapshot,
  sessionSnap,
  optimisticTasks,
  optimisticTasksById,
  supervisor,
  workbenchStore,
  workspaceSnapshotStore,
  markTaskRead,
}: TaskBridgeArgs) {
  const lifecycleCoordinator = useSessionLifecycleCoordinator();
  const { sessionSummaries, sessions, sessionIds, primarySessionId } = useMemo(
    () => deriveActiveTaskSessionIds(activeTaskSummary, activeSessionIdFromTab),
    [activeSessionIdFromTab, activeTaskSummary],
  );

  const sessionHeadBootstrapCache = useMemo(() => new SessionHeadBootstrapCache(), []);
  const prefetchGenerationRef = useRef(0);
  const prefetchSessionIdsRef = useRef<Set<string>>(new Set());
  const foregroundSessionIdsRef = useRef<string[]>([]);
  const workspaceSyncPrefetchKeyRef = useRef<string | null>(null);
  const activeSessionId = useMemo(
    () =>
      resolveWorkbenchActiveSessionId({
        activeSessionIdFromTab,
        primarySessionId,
        sessions,
      }),
    [activeSessionIdFromTab, primarySessionId, sessions],
  );
  const taskArchived = Boolean(activeTaskSummary?.task.archived_at);
  const taskArchivedRef = useRef(taskArchived);
  const suppressWarmSessionIdsRef = useRef(false);
  const foregroundSessionIds = useMemo(
    () => (activeSessionId ? [activeSessionId] : primarySessionId ? [primarySessionId] : []),
    [activeSessionId, primarySessionId],
  );
  const taskLiveInfo = useMemo(
    () =>
      deriveTaskLiveInfo({
        tasksById,
        optimisticTasks,
        sessions: sessionSnap.sessions,
      }),
    [optimisticTasks, sessionSnap.sessions, tasksById],
  );
  const foregroundTaskWorking = Boolean(
    activeTaskId && taskLiveInfo.workingByTask.has(activeTaskId),
  );
  const warmSessionIds = useMemo(
    () =>
      deriveWarmSessionIds({
        activeTaskSessionIds: foregroundSessionIds,
        tasksById,
        activeIds: workspaceSnapshot.activeIds,
      }),
    [foregroundSessionIds, tasksById, workspaceSnapshot.activeIds],
  );
  const prefetchWarmSessionIds = useMemo(
    () => (foregroundTaskWorking ? [] : warmSessionIds),
    [foregroundTaskWorking, warmSessionIds],
  );
  const plannedPrefetchSessionIds = useMemo(
    () =>
      planSessionHeadPrefetchTargets({
        foregroundSessionIds,
        warmSessionIds: prefetchWarmSessionIds,
      }).targetSessionIds,
    [foregroundSessionIds, prefetchWarmSessionIds],
  );
  const prefetchSessionIdsKey = plannedPrefetchSessionIds.join("\u001f");
  const prefetchSessionIds = useMemo(
    () => plannedPrefetchSessionIds,
    [prefetchSessionIdsKey],
  );
  const computeRetainedPrefetchSessionIds = useCallback(
    (snapshot: WorkspaceActiveSnapshotState): string[] => {
      return deriveRetainedPrefetchSessionIds({
        snapshot,
        foregroundSessionIds: foregroundSessionIdsRef.current,
        taskArchived: taskArchivedRef.current,
        suppressWarmSessionIds: suppressWarmSessionIdsRef.current,
      });
    },
    [],
  );
  const isPrefetchSessionRetained = useCallback(
    (sessionId: string, snapshot?: WorkspaceActiveSnapshotState): boolean => {
      return computeRetainedPrefetchSessionIds(snapshot ?? workspaceSnapshotStore.getSnapshot()).includes(sessionId);
    },
    [computeRetainedPrefetchSessionIds, workspaceSnapshotStore],
  );
  const refreshRetainedPrefetchTargets = useCallback(
    (snapshot?: WorkspaceActiveSnapshotState) => {
      const nextSnapshot = snapshot ?? workspaceSnapshotStore.getSnapshot();
      const nextSessionIds = computeRetainedPrefetchSessionIds(nextSnapshot);
      prefetchSessionIdsRef.current = new Set(nextSessionIds);
      sessionHeadBootstrapCache.retain(nextSessionIds);
      return {
        snapshot: nextSnapshot,
        sessionIds: nextSessionIds,
        sessionIdSet: prefetchSessionIdsRef.current,
      };
    },
    [computeRetainedPrefetchSessionIds, sessionHeadBootstrapCache, workspaceSnapshotStore],
  );
  const primeAuthoritativeHeadsForSessions = useCallback(
    async (
      sessionIdsToPrime: readonly string[],
      generation?: number,
      opts?: { force?: boolean; reason?: SessionHeadPrefetchReason },
    ) => {
      const shouldContinue = () => generation === undefined || prefetchGenerationRef.current === generation;
      if (sessionIdsToPrime.length === 0 || !shouldContinue()) return;
      await primeAuthoritativeSessionHeads(
        workspaceSnapshotStore.getSnapshot(),
        workspaceSnapshotStore,
        sessionHeadBootstrapCache,
        sessionIdsToPrime,
        {
          shouldContinue,
          getSnapshot: () => workspaceSnapshotStore.getSnapshot(),
          shouldRetainSessionId: (sessionId) => isPrefetchSessionRetained(sessionId),
          force: opts?.force,
          reason: opts?.reason,
          onHead: (sessionId, head) => {
            const retain = isPrefetchSessionRetained(sessionId);
            const continueGeneration = shouldContinue();
            if (!continueGeneration || !retain) return;
            supervisor.upsertWorkspaceSessionHead(sessionId, head);
          },
        },
      );
    },
    [isPrefetchSessionRetained, sessionHeadBootstrapCache, supervisor, workspaceSnapshotStore],
  );

  useEffect(() => {
    supervisor.setSubscribedSessionIdsSink((sessionIdsForSubscription) => {
      workspaceSnapshotStore.setSubscribedSessions?.(sessionIdsForSubscription);
    });
    const syncWorkspace = () => {
      const { snapshot, sessionIds } = refreshRetainedPrefetchTargets();
      supervisor.setWorkspaceSessionHeads(
        readWorkspaceSessionHeads(
          snapshot,
          workspaceSnapshotStore,
          sessionHeadBootstrapCache,
          sessionIds,
        ),
      );
      supervisor.setWorkspaceSnapshotState(snapshot);
      lifecycleCoordinator.setWorkspaceSnapshotState(snapshot);
      if (snapshot.initialized && sessionIds.length > 0) {
        const prefetchSessionIdsForSync = collectAuthoritativePrefetchReadySessionIds(snapshot, sessionIds);
        if (prefetchSessionIdsForSync.length === 0) {
          noteWorkspaceSyncPrefetchSuppressed("working_sessions");
          return;
        }
        const prefetchKey = buildWorkspaceSyncPrefetchVersionKey(snapshot, prefetchSessionIdsForSync);
        if (prefetchKey !== workspaceSyncPrefetchKeyRef.current) {
          workspaceSyncPrefetchKeyRef.current = prefetchKey;
          void primeAuthoritativeHeadsForSessions(prefetchSessionIdsForSync, undefined, { reason: "workspace_sync" });
        } else {
          noteWorkspaceSyncPrefetchSuppressed("unchanged_session_versions");
        }
      }
    };
    const handleWorkspaceEvent = (evt: WorkspaceActiveSnapshotEvent) => {
      const { snapshot, sessionIds, sessionIdSet } = refreshRetainedPrefetchTargets();
      const didCacheSeed = maybeCacheSessionHeadSeed(
        sessionHeadBootstrapCache,
        evt,
        sessionIdSet,
      );
      const sessionId =
        evt.type === "session_head_delta"
          ? idToString(evt.delta.session_id)
          : evt.type === "session_head_seed"
            ? idToString(evt.head.session.id)
            : evt.type === "session_summary_delta"
              ? idToString(evt.delta.session_id)
              : evt.type === "session_summary"
                ? idToString(evt.summary.session.id)
            : "";
      if (sessionId) {
        const head = workspaceSnapshotStore.getSessionHeadSnapshot(sessionId);
        if (head && sessionIdSet.has(sessionId)) {
          supervisor.upsertWorkspaceSessionHead(sessionId, head);
        } else if (didCacheSeed && evt.type === "session_head_seed") {
          const cachedHead = readWorkspaceSessionHeads(
            snapshot,
            workspaceSnapshotStore,
            sessionHeadBootstrapCache,
            [sessionId],
          )[sessionId];
          if (cachedHead) {
            supervisor.upsertWorkspaceSessionHead(sessionId, cachedHead);
          }
        } else if (
          (evt.type === "session_summary_delta" || evt.type === "session_summary") &&
          sessionIdSet.has(sessionId)
        ) {
          const repairSessionIds = collectAuthoritativePrefetchReadySessionIds(snapshot, [sessionId]);
          if (repairSessionIds.length > 0) {
            void primeAuthoritativeHeadsForSessions(repairSessionIds, undefined, { reason: "summary_repair" });
          } else {
            noteWorkspaceSyncPrefetchSuppressed("working_sessions");
          }
        }
      } else if (didCacheSeed) {
        supervisor.setWorkspaceSessionHeads(
          readWorkspaceSessionHeads(
            snapshot,
            workspaceSnapshotStore,
            sessionHeadBootstrapCache,
            sessionIds,
          ),
        );
      }
      supervisor.handleWorkspaceEvent(evt);
    };
    syncWorkspace();
    const unsubState = workspaceSnapshotStore.subscribe(syncWorkspace);
    const unsubEvents = workspaceSnapshotStore.subscribeEvents(handleWorkspaceEvent);
    return () => {
      unsubEvents();
      unsubState();
      supervisor.setSubscribedSessionIdsSink(null);
      sessionHeadBootstrapCache.clear();
      supervisor.setWorkspaceSessionHeads({});
      supervisor.setWorkspaceSnapshotState(null);
      lifecycleCoordinator.setWorkspaceSnapshotState(null);
    };
  }, [
    refreshRetainedPrefetchTargets,
    lifecycleCoordinator,
    primeAuthoritativeHeadsForSessions,
    supervisor,
    sessionHeadBootstrapCache,
    workspaceSnapshotStore,
  ]);

  useEffect(() => {
    if (!activeTaskId) return;
    const snapshotReady = workspaceSnapshot.initialized && workspaceSnapshot.fetchState.active === "idle";
    const activeTab = workbenchStore.getActiveTab();
    const previousSessionId =
      activeTab?.kind === "task" && activeTab.ref.taskId === activeTaskId ? (activeTab.ref.sessionId ?? null) : null;
    const previousSessionEntry = previousSessionId ? sessionSnap.sessions[previousSessionId] ?? null : null;
    const optimisticActiveTask = optimisticTasksById[activeTaskId];
    const optimisticPrimarySessionId = optimisticActiveTask ? (optimisticActiveTask.primarySessionId ?? null) : null;
    if (!activeTaskSummary) {
      if (!snapshotReady) return;
      workbenchStore.setActiveSessionForActiveTask(null, { source: "system" });
      return;
    }
    if (sessions.length === 0 && !primarySessionId) {
      if (!snapshotReady) return;
      if (!taskArchived) {
        if (previousSessionId && optimisticPrimarySessionId && previousSessionId === optimisticPrimarySessionId) {
          return;
        }
        workbenchStore.setActiveSessionForActiveTask(null, { source: "system" });
        return;
      }
      if (previousSessionId && !previousSessionEntry) return;
      if (previousSessionEntry && previousSessionEntry.loadState !== "fatal") return;
      workbenchStore.setActiveSessionForActiveTask(null, { source: "system" });
      return;
    }
    const nextSessionId = resolveWorkbenchActiveSessionId({
      activeSessionIdFromTab: previousSessionId,
      primarySessionId,
      sessions,
    });
    if (!canRenderWorkbenchActiveSession(nextSessionId ? sessionSnap.sessions[nextSessionId] ?? null : null)) {
      return;
    }
    if (activeTab?.kind === "task" && activeTab.ref.taskId === activeTaskId && nextSessionId !== previousSessionId) {
      if (nextSessionId) {
        const snapshot = workspaceSnapshotStore.getSnapshot();
        const nextHead = readWorkspaceSessionHeads(
          snapshot,
          workspaceSnapshotStore,
          sessionHeadBootstrapCache,
          [nextSessionId],
        )[nextSessionId];
        if (nextHead) {
          supervisor.upsertWorkspaceSessionHead(nextSessionId, nextHead);
        }
      }
      if (previousSessionId && sessionSnap.sessions[previousSessionId]) {
        noteSwitchStaleVisible(activeTaskId, previousSessionId, nextSessionId ?? "");
      }
      workbenchStore.setActiveSessionForActiveTask(nextSessionId, { source: "system" });
    }
  }, [
    activeTaskId,
    activeTaskSummary,
    optimisticTasksById,
    primarySessionId,
    sessions,
    sessionSnap.sessions,
    sessionHeadBootstrapCache,
    supervisor,
    taskArchived,
    workbenchStore,
    workspaceSnapshot.fetchState.active,
    workspaceSnapshot.initialized,
    workspaceSnapshotStore,
  ]);

  useEffect(() => {
    const detail: WorkbenchTaskIdleDetail = {
      allTasksIdle: taskLiveInfo.workingByTask.size === 0,
    };
    window.dispatchEvent(
      new CustomEvent<WorkbenchTaskIdleDetail>(WORKBENCH_TASK_IDLE_EVENT, {
        detail,
      }),
    );
  }, [taskLiveInfo.workingByTask.size]);

  const isTaskUnread = useCallback(
    (taskId: string) => isWorkbenchTaskUnread({ taskId, tasksById, taskLiveInfo }),
    [taskLiveInfo, tasksById],
  );
  const appInForeground = useSyncExternalStore(
    subscribeAppForeground,
    getAppForegroundSnapshot,
    getAppForegroundSnapshot,
  );

  useEffect(() => {
    if (!activeTaskId) return;
    const task = tasksById[activeTaskId]?.task;
    if (!task) return;
    if (optimisticTasksById[activeTaskId]) return;
    if (taskLiveInfo.workingByTask.has(activeTaskId)) return;
    if (!isTaskUnread(activeTaskId)) return;
    if (!appInForeground) return;
    void markTaskRead(activeTaskId);
  }, [activeTaskId, appInForeground, isTaskUnread, markTaskRead, optimisticTasksById, taskLiveInfo.workingByTask, tasksById]);

  useEffect(() => {
    if (!activeTaskId || !activeSessionId) return;
    const activeEntry = sessionSnap.sessions[activeSessionId];
    if (!activeEntry) return;
    const navWorking = taskLiveInfo.workingByTask.has(activeTaskId);
    const latestTurnStatus = activeEntry.turns.at(-1)?.status ?? null;
    const threadWorking = hasSessionActiveTurn(activeEntry.activity, latestTurnStatus);
    if (navWorking === threadWorking) return;
    noteNavThreadActivityMismatch(activeTaskId, activeSessionId, navWorking, threadWorking);
  }, [activeSessionId, activeTaskId, sessionSnap.sessions, taskLiveInfo.workingByTask]);

  const providerIdsByTaskFromSessions = useMemo(
    () => deriveProviderIdsByTask(sessionSnap.sessions),
    [sessionSnap.sessions],
  );

  useEffect(() => {
    sessionHeadBootstrapCache.retain(prefetchSessionIds);
  }, [prefetchSessionIds, sessionHeadBootstrapCache]);

  useLayoutEffect(() => {
    foregroundSessionIdsRef.current = foregroundSessionIds;
    taskArchivedRef.current = taskArchived;
    suppressWarmSessionIdsRef.current = foregroundTaskWorking;
    prefetchSessionIdsRef.current = new Set(prefetchSessionIds);
  }, [foregroundSessionIds, foregroundTaskWorking, prefetchSessionIds, taskArchived]);

  useEffect(() => {
    const generation = prefetchGenerationRef.current + 1;
    prefetchGenerationRef.current = generation;
    const shouldContinue = () => prefetchGenerationRef.current === generation;
    const prefetchHeads = async () => {
      if (!workspaceSnapshot.initialized) return;
      const snapshot = workspaceSnapshotStore.getSnapshot();
      const persistedChanged = await primePersistedSessionHeads(
        snapshot,
        workspaceSnapshotStore,
        sessionHeadBootstrapCache,
        prefetchSessionIds,
        {
          shouldContinue,
          shouldRetainSessionId: (sessionId) => isPrefetchSessionRetained(sessionId),
        },
      );
      if (persistedChanged && shouldContinue()) {
        const { snapshot: nextSnapshot, sessionIds } = refreshRetainedPrefetchTargets();
        supervisor.setWorkspaceSessionHeads(
          readWorkspaceSessionHeads(
            nextSnapshot,
            workspaceSnapshotStore,
            sessionHeadBootstrapCache,
            sessionIds,
          ),
        );
      }
      await primeAuthoritativeHeadsForSessions(prefetchSessionIds, generation, { reason: "warm_prefetch" });
    };
    void prefetchHeads();
    return () => {
      if (prefetchGenerationRef.current === generation) {
        prefetchGenerationRef.current = generation + 1;
      }
    };
  }, [
    isPrefetchSessionRetained,
    prefetchSessionIds,
    primeAuthoritativeHeadsForSessions,
    refreshRetainedPrefetchTargets,
    sessionHeadBootstrapCache,
    supervisor,
    workspaceSnapshot.initialized,
    workspaceSnapshotStore,
  ]);

  useLayoutEffect(() => {
    const snapshot = workspaceSnapshotStore.getSnapshot();
    supervisor.setWorkspaceSessionHeads(
      readWorkspaceSessionHeads(
        snapshot,
        workspaceSnapshotStore,
        sessionHeadBootstrapCache,
        prefetchSessionIds,
      ),
    );
    supervisor.setActiveTaskSessionIds(foregroundSessionIds);
  }, [
    foregroundSessionIds,
    prefetchSessionIds,
    sessionHeadBootstrapCache,
    supervisor,
    workspaceSnapshotStore,
  ]);

  useEffect(() => {
    supervisor.setWarmSessionIds(prefetchWarmSessionIds);
  }, [prefetchWarmSessionIds, supervisor]);

  useEffect(() => {
    if (!workspaceSnapshot.initialized || foregroundSessionIds.length === 0) return;
    void primeAuthoritativeHeadsForSessions(foregroundSessionIds, undefined, {
      force: true,
      reason: "foreground_force",
    });
  }, [
    foregroundSessionIds,
    primeAuthoritativeHeadsForSessions,
    workspaceSnapshot.initialized,
  ]);

  useEffect(() => {
    workspaceSnapshotStore.setForegroundSessionId?.(activeSessionId ?? primarySessionId ?? null);
  }, [activeSessionId, primarySessionId, workspaceSnapshotStore]);

  return {
    sessionSummaries,
    sessions,
    sessionIds,
    activeSessionId,
    primarySessionId,
    activeTaskSessionIds: foregroundSessionIds,
    taskLiveInfo,
    providerIdsByTaskFromSessions,
    isTaskUnread,
  };
}
