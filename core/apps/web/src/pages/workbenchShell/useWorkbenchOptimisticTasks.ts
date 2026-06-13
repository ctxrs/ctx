import { useEffect, useMemo, useRef, useState } from "react";
import { idToString } from "../../api/client";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import type { OptimisticTaskSummary } from "./WorkbenchPage.types";

type UseWorkbenchOptimisticTasksArgs = {
  activeTaskId: string | null;
  activeTaskIdFromTab: string | null;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
};

const hasRenderablePrimaryHeadForOptimisticTask = (
  taskSummary: WorkspaceActiveSnapshotItem | null,
  optimisticTask: OptimisticTaskSummary,
): boolean => {
  if (!taskSummary) return false;
  const primarySessionId =
    idToString(taskSummary.primarySessionId) || idToString(taskSummary.task.primary_session_id);
  if (!primarySessionId) return false;
  const primaryHead = taskSummary.primarySessionHead;
  if (idToString(primaryHead?.session?.id) !== primarySessionId) return false;
  const localMessageId = String(optimisticTask.localMessageId ?? "").trim();
  const headMessages = Array.isArray(primaryHead?.messages) ? primaryHead.messages : [];
  if (!localMessageId) return headMessages.length > 0;
  const userMessage = headMessages.find((message) => idToString(message.id) === localMessageId);
  if (!userMessage || userMessage.role !== "user") return false;
  const headTurns = Array.isArray(primaryHead?.turns) ? primaryHead.turns : [];
  return headTurns.some((turn) => idToString(turn.user_message_id ?? "") === localMessageId);
};

export function useWorkbenchOptimisticTasks({
  activeTaskId,
  activeTaskIdFromTab,
  tasksById,
}: UseWorkbenchOptimisticTasksArgs) {
  const [optimisticTasks, setOptimisticTasks] = useState<OptimisticTaskSummary[]>([]);
  // Keep a synchronous fallback for first paint when an optimistic task tab races state commit.
  const optimisticStartingTaskRef = useRef<OptimisticTaskSummary | null>(null);

  const optimisticTasksById = useMemo(
    () => Object.fromEntries(optimisticTasks.map((item) => [item.id, item])),
    [optimisticTasks],
  );

  const optimisticSessionIdSet = useMemo(() => {
    const ids = new Set<string>();
    for (const item of optimisticTasks) {
      const server = tasksById[item.id] ?? null;
      if (item.localStatus === "synced" && hasRenderablePrimaryHeadForOptimisticTask(server, item)) continue;
      const sessionId = String(item.primarySessionId ?? "");
      if (sessionId) ids.add(sessionId);
    }
    return ids;
  }, [optimisticTasks, tasksById]);

  const optimisticFailureBySessionId = useMemo(() => {
    const out: Record<string, { prompt: string; error: string | null }> = {};
    for (const item of optimisticTasks) {
      if (item.localStatus !== "failed") continue;
      const sessionId = String(item.primarySessionId ?? "");
      if (!sessionId) continue;
      out[sessionId] = { prompt: item.localPrompt, error: item.localError ?? null };
    }
    return out;
  }, [optimisticTasks]);

  const activeTaskSummary = useMemo(() => {
    if (!activeTaskId) return null;
    const optimistic = optimisticTasksById[activeTaskId];
    const server = tasksById[activeTaskId] ?? null;
    if (optimistic) {
      if (optimistic.localStatus !== "synced") return optimistic;
      if (!hasRenderablePrimaryHeadForOptimisticTask(server, optimistic)) return optimistic;
    }
    if (server) return server;
    const fallback = optimisticStartingTaskRef.current;
    if (fallback && fallback.id === activeTaskId && fallback.localStatus !== "synced") {
      return fallback;
    }
    return optimistic ?? null;
  }, [activeTaskId, optimisticTasksById, tasksById]);

  useEffect(() => {
    const current = optimisticStartingTaskRef.current;
    if (!current) return;
    if (optimisticTasksById[current.id]) {
      optimisticStartingTaskRef.current = null;
      return;
    }
    if (activeTaskId && activeTaskId !== current.id && activeTaskIdFromTab) {
      optimisticStartingTaskRef.current = null;
    }
  }, [activeTaskId, activeTaskIdFromTab, optimisticTasksById]);

  return {
    optimisticTasks,
    setOptimisticTasks,
    optimisticStartingTaskRef,
    optimisticTasksById,
    optimisticSessionIdSet,
    optimisticFailureBySessionId,
    activeTaskSummary,
  };
}
