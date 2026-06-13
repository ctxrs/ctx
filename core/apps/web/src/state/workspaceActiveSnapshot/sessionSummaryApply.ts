import type {
  SessionSnapshotSummary,
  WorkspaceActiveSnapshotSessionSummaryDeltaEvent,
} from "@ctx/types";
import { idToString } from "../../api/client";
import type { WorkspaceActiveSnapshotItem } from "./storeTypes";
import { sortSessionSummaries } from "./projection";
import {
  mergeSessionSummaryDelta,
  normalizeSessionSummary,
} from "./summaryHelpers";

export function applySessionSummaryDeltaToTasks(params: {
  delta: WorkspaceActiveSnapshotSessionSummaryDeltaEvent["delta"];
  tasks: Map<string, WorkspaceActiveSnapshotItem>;
}): boolean {
  const { delta, tasks } = params;
  const sessionId = idToString(delta.session_id ?? "");
  if (!sessionId) return false;
  const taskIdHint = idToString(delta.task_id ?? "");

  const tryUpdate = (taskId: string): boolean => {
    const task = tasks.get(taskId);
    if (!task) return false;
    const nextSessions = task.sessions.slice();
    const sessionIdx = nextSessions.findIndex((s) => idToString(s.session.id) === sessionId);
    if (sessionIdx < 0) return false;
    const current = nextSessions[sessionIdx];
    const nextSummary = mergeSessionSummaryDelta(current, delta);
    if (!nextSummary) return false;
    nextSessions[sessionIdx] = nextSummary;
    tasks.set(taskId, { ...task, sessions: sortSessionSummaries(nextSessions) });
    return true;
  };

  if (taskIdHint && tryUpdate(taskIdHint)) {
    return true;
  }
  for (const taskId of tasks.keys()) {
    if (taskIdHint && taskId === taskIdHint) continue;
    if (tryUpdate(taskId)) return true;
  }
  return false;
}

export function applySessionSummaryToTasks(params: {
  summary: SessionSnapshotSummary;
  tasks: Map<string, WorkspaceActiveSnapshotItem>;
}): boolean {
  const { summary, tasks } = params;
  const taskId = idToString(summary.session.task_id);
  if (!taskId) return false;
  const task = tasks.get(taskId);
  if (!task) return false;
  const nextSessions = task.sessions.slice();
  const sessionId = idToString(summary.session.id);
  const sessionIdx = nextSessions.findIndex((s) => idToString(s.session.id) === sessionId);
  const normalized = normalizeSessionSummary(summary);
  if (sessionIdx >= 0) {
    nextSessions[sessionIdx] = normalized;
  } else {
    nextSessions.push(normalized);
  }
  tasks.set(taskId, { ...task, sessions: sortSessionSummaries(nextSessions) });
  return true;
}
