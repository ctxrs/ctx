import type { SessionActivityState } from "@ctx/types";
import type { SessionTurn } from "../../api/client";
import { idToString } from "../../api/client";
import type { SessionSupervisorSnapshot } from "../../state/sessionSupervisor";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import { hasSessionActiveTurn } from "../../utils/sessionActivity";
import { pickPreferredSessionId } from "../../utils/workbenchSelection";
import { lastAssistantMessageMs, parseMs } from "./WorkbenchPage.utils";
import type { OptimisticTaskSummary } from "./WorkbenchPage.types";
import { SESSION_HEAD_PREFETCH_TARGET_LIMIT } from "./sessionHeadPrefetch";

export type WorkbenchTaskLiveInfo = {
  workingByTask: Set<string>;
  errorByTask: Set<string>;
  lastAssistantMsByTask: Record<string, number>;
};

export type WorkbenchTaskStatusKind = "error" | "working" | "unread" | "idle";

type SessionTaskProviderSample = { providerId: string; updatedAt: number };

const mergeUniqueIds = (...groups: Array<Array<string | null | undefined>>): string[] => {
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const group of groups) {
    for (const value of group) {
      const id = idToString(value ?? "");
      if (!id || seen.has(id)) continue;
      seen.add(id);
      ordered.push(id);
    }
  }
  return ordered;
};

export type WorkbenchTaskLiveState = {
  working: boolean;
  hasError: boolean;
  lastAssistantMs: number | null;
};

type TaskLiveSource = {
  activity: SessionActivityState | null | undefined;
  status: string | null | undefined;
  lastAssistantMs: number | null;
  lastEventSeq: number | null;
  projectionRev: number | null;
  stateRev: number | null;
  priority: number;
};

const readCursorNumber = (value: number | null | undefined): number | null =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

const compareTaskLiveSourceFreshness = (
  left: TaskLiveSource,
  right: TaskLiveSource,
): number => {
  const fields: Array<keyof Pick<TaskLiveSource, "lastEventSeq" | "projectionRev" | "stateRev">> = [
    "lastEventSeq",
    "projectionRev",
    "stateRev",
  ];
  for (const field of fields) {
    const leftValue = left[field];
    const rightValue = right[field];
    if (leftValue === rightValue) continue;
    if (leftValue === null) return -1;
    if (rightValue === null) return 1;
    return leftValue - rightValue;
  }
  return left.priority - right.priority;
};

const pickFreshestTaskLiveSource = (
  sources: TaskLiveSource[],
): TaskLiveSource | null => {
  let freshest: TaskLiveSource | null = null;
  for (const source of sources) {
    if (!freshest || compareTaskLiveSourceFreshness(source, freshest) > 0) {
      freshest = source;
    }
  }
  return freshest;
};

const compareSessionTurnOrder = (left: SessionTurn, right: SessionTurn): number => {
  const leftSeq = Number(left.start_seq ?? Number.NaN);
  const rightSeq = Number(right.start_seq ?? Number.NaN);
  if (Number.isFinite(leftSeq) && Number.isFinite(rightSeq) && leftSeq !== rightSeq) {
    return leftSeq - rightSeq;
  }
  if (Number.isFinite(leftSeq) && !Number.isFinite(rightSeq)) return -1;
  if (!Number.isFinite(leftSeq) && Number.isFinite(rightSeq)) return 1;
  const leftStartedAt = String(left.started_at ?? "");
  const rightStartedAt = String(right.started_at ?? "");
  if (leftStartedAt !== rightStartedAt) {
    return leftStartedAt.localeCompare(rightStartedAt);
  }
  return String(left.turn_id ?? "").localeCompare(String(right.turn_id ?? ""));
};

const getLatestTurnStatus = (turns: SessionTurn[] | null | undefined): SessionTurn["status"] | null => {
  let latestTurn: SessionTurn | null = null;
  for (const turn of turns ?? []) {
    if (!latestTurn || compareSessionTurnOrder(turn, latestTurn) > 0) {
      latestTurn = turn;
    }
  }
  return latestTurn?.status ?? null;
};

const resolveCanonicalTaskWorkingTurnStatus = ({
  primaryEntry,
  primaryHead,
  primarySessionSummary,
}: {
  primaryEntry: SessionSupervisorSnapshot["sessions"][string] | undefined;
  primaryHead: WorkspaceActiveSnapshotItem["primarySessionHead"] | null;
  primarySessionSummary: WorkspaceActiveSnapshotItem["sessions"][number] | undefined;
}): SessionTurn["status"] | null => {
  if (primaryEntry?.freshness === "authoritative" || primaryEntry?.freshness === "replica") {
    const entryTurnStatus =
      getLatestTurnStatus(primaryEntry.turns) ?? primaryEntry.activity?.last_turn_status ?? null;
    if (entryTurnStatus) return entryTurnStatus;
    const headTurnStatus =
      getLatestTurnStatus(primaryHead?.turns ?? null) ?? primaryHead?.activity?.last_turn_status ?? null;
    if (headTurnStatus) return headTurnStatus;
  }

  const headTurnStatus =
    getLatestTurnStatus(primaryHead?.turns ?? null) ?? primaryHead?.activity?.last_turn_status ?? null;
  if (headTurnStatus) return headTurnStatus;

  return primarySessionSummary?.activity?.last_turn_status ?? null;
};

const readPrimarySessionFallbackId = (
  summary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null | undefined,
): string => {
  if (!summary || typeof summary !== "object") return "";
  const record = summary as Record<string, unknown>;
  const primarySession = record.primary_session;
  if (!primarySession || typeof primarySession !== "object") return "";
  const primaryRecord = primarySession as Record<string, unknown>;
  if (primaryRecord.session && typeof primaryRecord.session === "object") {
    const sessionRecord = primaryRecord.session as Record<string, unknown>;
    return typeof sessionRecord.id === "string" ? idToString(sessionRecord.id) : "";
  }
  return typeof primaryRecord.id === "string" ? idToString(primaryRecord.id) : "";
};

const resolvePrimarySessionId = (
  summary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null | undefined,
): string =>
  idToString(summary?.task.primary_session_id ?? "") ||
  readPrimarySessionFallbackId(summary) ||
  idToString(summary?.primarySessionId ?? "") ||
  idToString(summary?.primarySessionHead?.session?.id ?? "");

const resolvePrimaryServerLastAssistantMs = (
  summary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null | undefined,
): number | null => {
  const primarySessionId = resolvePrimarySessionId(summary);
  const primarySessionSummary = primarySessionId
    ? summary?.sessions.find((sessionSummary) => idToString(sessionSummary.session.id) === primarySessionId)
    : undefined;
  const taskMs = parseMs(summary?.task.last_assistant_message_at ?? null);
  const summaryMs = parseMs(primarySessionSummary?.last_message_at ?? null);
  const headMs = summary?.primarySessionHead ? lastAssistantMessageMs(summary.primarySessionHead.messages) : null;
  if (primarySessionSummary?.unread === false) {
    return headMs;
  }
  if (primarySessionSummary?.unread === true) {
    return headMs ?? taskMs ?? summaryMs;
  }
  return headMs ?? summaryMs ?? taskMs;
};

export const isPrimarySessionRunning = ({
  primarySessionSummary,
}: {
  primarySessionSummary?: WorkspaceActiveSnapshotItem["sessions"][number];
}): boolean => {
  return hasSessionActiveTurn(primarySessionSummary?.activity);
};

export const deriveWorkbenchTaskStatusKind = ({
  hasError,
  working,
  unread,
  localStatus,
}: {
  hasError: boolean;
  working: boolean;
  unread: boolean;
  localStatus: "starting" | "synced" | "failed" | null;
}): WorkbenchTaskStatusKind => {
  if (localStatus === "failed" || hasError) return "error";
  if (working) return "working";
  if (unread) return "unread";
  return "idle";
};

export const deriveActiveTaskSessionIds = (
  activeTaskSummary: WorkspaceActiveSnapshotItem | OptimisticTaskSummary | null,
  activeSessionIdFromTab?: string | null,
): {
  sessionSummaries: WorkspaceActiveSnapshotItem["sessions"];
  sessions: WorkspaceActiveSnapshotItem["sessions"][number]["session"][];
  sessionIds: string[];
  primarySessionId: string;
  activeTaskSessionIds: string[];
} => {
  const sessionSummaries = activeTaskSummary?.sessions ?? [];
  const sessions = sessionSummaries.map((summary) => summary.session);
  const sessionIds = sessionSummaries.map((summary) => idToString(summary.session.id)).filter(Boolean);
  const primarySessionId = resolvePrimarySessionId(activeTaskSummary);
  const activeTaskSessionIds = mergeUniqueIds(
    [activeSessionIdFromTab],
    [primarySessionId],
    sessionIds,
  );
  return {
    sessionSummaries,
    sessions,
    sessionIds,
    primarySessionId,
    activeTaskSessionIds,
  };
};

export const deriveWarmSessionIds = ({
  activeTaskSessionIds,
  tasksById,
  activeIds,
}: {
  activeTaskSessionIds: string[];
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  activeIds: string[];
}): string[] => {
  const activeSet = new Set(activeTaskSessionIds);
  const candidates: { id: string; updatedAt: number; running: boolean }[] = [];
  for (const taskId of activeIds) {
    const task = tasksById[taskId];
    if (!task) continue;
    for (const sessionSummary of task.sessions) {
      const sessionId = idToString(sessionSummary.session.id);
      if (!sessionId || activeSet.has(sessionId)) continue;
      const updatedAt = parseMs(sessionSummary.last_message_at) ?? parseMs(sessionSummary.session.updated_at) ?? 0;
      const running = hasSessionActiveTurn(sessionSummary.activity);
      candidates.push({ id: sessionId, updatedAt, running });
    }
  }
  candidates.sort((left, right) => {
    if (left.running !== right.running) return left.running ? -1 : 1;
    return right.updatedAt - left.updatedAt;
  });
  return candidates.map((candidate) => candidate.id).slice(0, SESSION_HEAD_PREFETCH_TARGET_LIMIT);
};

const buildTasksForLiveInfo = (
  tasksById: Record<string, WorkspaceActiveSnapshotItem>,
  optimisticTasks: OptimisticTaskSummary[],
): Record<string, WorkspaceActiveSnapshotItem | OptimisticTaskSummary> => {
  const merged: Record<string, WorkspaceActiveSnapshotItem | OptimisticTaskSummary> = { ...tasksById };
  for (const item of optimisticTasks) {
    if (item.localStatus === "failed" || !merged[item.id]) {
      merged[item.id] = item;
    }
  }
  return merged;
};

const buildSessionEntryIndex = (
  sessions: SessionSupervisorSnapshot["sessions"],
): Map<string, SessionSupervisorSnapshot["sessions"][string]> => {
  const entryBySessionId = new Map<string, SessionSupervisorSnapshot["sessions"][string]>();
  for (const entry of Object.values(sessions)) {
    const sessionId = entry.session ? idToString(entry.session.id) : "";
    if (sessionId) entryBySessionId.set(sessionId, entry);
  }
  return entryBySessionId;
};

export const selectWorkbenchTaskLiveState = ({
  task,
  entryBySessionId,
}: {
  task: WorkspaceActiveSnapshotItem | OptimisticTaskSummary;
  entryBySessionId: Map<string, SessionSupervisorSnapshot["sessions"][string]>;
}): WorkbenchTaskLiveState | null => {
  const primarySessionId = resolvePrimarySessionId(task);
  const primarySessionSummary = primarySessionId
    ? task.sessions.find((sessionSummary) => idToString(sessionSummary.session.id) === primarySessionId)
    : undefined;
  const primaryEntry = primarySessionId ? entryBySessionId.get(primarySessionId) : undefined;
  const primaryHead = task.primarySessionHead ?? null;
  const primaryEntryIsCanonical =
    primaryEntry?.freshness === "authoritative" || primaryEntry?.freshness === "replica";
  const sources: TaskLiveSource[] = [];
  const liveMs = primaryEntry ? lastAssistantMessageMs(primaryEntry.messages) : null;
  const headMs = primaryHead ? lastAssistantMessageMs(primaryHead.messages) : null;
  const summaryMs = parseMs(primarySessionSummary?.last_message_at ?? null);

  if (primaryEntry) {
    const canonicalEntry =
      primaryEntry.freshness === "authoritative" || primaryEntry.freshness === "replica";
    sources.push({
      activity: primaryEntry.activity ?? null,
      status: primaryEntry.session?.status,
      lastAssistantMs: liveMs,
      lastEventSeq: readCursorNumber(primaryEntry.lastEventSeq),
      projectionRev: readCursorNumber(primaryEntry.projectionRev),
      stateRev: readCursorNumber(primaryEntry.stateRev),
      priority: canonicalEntry ? 1 : 0,
    });
  }

  if (primarySessionSummary) {
    sources.push({
      activity: primarySessionSummary.activity ?? null,
      status: primarySessionSummary.session.status,
      lastAssistantMs: summaryMs,
      lastEventSeq: readCursorNumber(primarySessionSummary.last_event_seq ?? null),
      projectionRev: readCursorNumber(primarySessionSummary.projection_rev),
      stateRev: readCursorNumber(primarySessionSummary.state_rev),
      priority: 2,
    });
  }

  if (primaryHead) {
    sources.push({
      activity: primaryHead.activity ?? null,
      status: primaryHead.session.status,
      lastAssistantMs: headMs,
      lastEventSeq: readCursorNumber(primaryHead.last_event_seq),
      projectionRev: readCursorNumber(primaryHead.projection_rev),
      stateRev: readCursorNumber(primaryHead.state_rev),
      priority: 3,
    });
  }

  const freshestActivitySource = pickFreshestTaskLiveSource(
    sources.filter((source) => source.activity !== null && source.activity !== undefined),
  );
  const primaryStatus = primaryEntryIsCanonical
    ? primaryEntry?.session?.status ?? primaryHead?.session.status ?? primarySessionSummary?.session.status
    : primaryHead?.session.status ?? primarySessionSummary?.session.status ?? primaryEntry?.session?.status;
  if (!freshestActivitySource && !primaryStatus) return null;

  const assistantMs = primaryEntryIsCanonical
    ? liveMs ?? headMs ?? summaryMs
    : headMs ?? summaryMs ?? liveMs;
  const canonicalTurnStatus = resolveCanonicalTaskWorkingTurnStatus({
    primaryEntry,
    primaryHead,
    primarySessionSummary,
  });

  return {
    working:
      canonicalTurnStatus === "starting" ||
      canonicalTurnStatus === "running",
    hasError: primaryStatus === "failed" || primaryStatus === "cancelled",
    lastAssistantMs: assistantMs,
  };
};

const deriveTaskLiveInfoFromSources = (
  tasksForLiveInfo: Record<string, WorkspaceActiveSnapshotItem | OptimisticTaskSummary>,
  sessions: SessionSupervisorSnapshot["sessions"],
): WorkbenchTaskLiveInfo => {
  const workingByTask = new Set<string>();
  const errorByTask = new Set<string>();
  const lastAssistantMsByTask: Record<string, number> = {};
  const entryBySessionId = buildSessionEntryIndex(sessions);

  for (const task of Object.values(tasksForLiveInfo)) {
    const selectedState = selectWorkbenchTaskLiveState({
      task,
      entryBySessionId,
    });
    if (!selectedState) continue;

    if (selectedState.working) {
      workingByTask.add(task.id);
    }

    if (selectedState.hasError) {
      errorByTask.add(task.id);
    }

    if (selectedState.lastAssistantMs !== null) {
      lastAssistantMsByTask[task.id] = Math.max(
        lastAssistantMsByTask[task.id] ?? 0,
        selectedState.lastAssistantMs,
      );
    }
  }

  return { workingByTask, errorByTask, lastAssistantMsByTask };
};

export const deriveTaskLiveInfo = ({
  tasksById,
  optimisticTasks,
  sessions,
}: {
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  optimisticTasks: OptimisticTaskSummary[];
  sessions: SessionSupervisorSnapshot["sessions"];
}): WorkbenchTaskLiveInfo =>
  deriveTaskLiveInfoFromSources(buildTasksForLiveInfo(tasksById, optimisticTasks), sessions);

export const deriveProviderIdsByTask = (
  sessions: SessionSupervisorSnapshot["sessions"],
): Record<string, string[]> => {
  const providerSamplesByTask: Record<string, SessionTaskProviderSample[]> = {};
  for (const entry of Object.values(sessions)) {
    const session = entry.session;
    const taskId = session ? idToString(session.task_id) : "";
    const providerId = String(session?.provider_id ?? "").trim();
    if (!taskId || !providerId) continue;
    (providerSamplesByTask[taskId] ??= []).push({ providerId, updatedAt: entry.updatedAtMs ?? 0 });
  }

  const byTask: Record<string, string[]> = {};
  for (const [taskId, samples] of Object.entries(providerSamplesByTask)) {
    const seen = new Set<string>();
    byTask[taskId] = samples
      .slice()
      .sort((left, right) => (right.updatedAt ?? 0) - (left.updatedAt ?? 0))
      .map((sample) => sample.providerId)
      .filter((providerId) => {
        if (seen.has(providerId)) return false;
        seen.add(providerId);
        return true;
      });
  }
  return byTask;
};

export const deriveProviderIdsByTaskFromSessions = deriveProviderIdsByTask;

export const resolveWorkbenchActiveSessionId = ({
  activeSessionIdFromTab,
  primarySessionId,
  sessions,
}: {
  activeSessionIdFromTab: string | null;
  primarySessionId: string;
  sessions: WorkspaceActiveSnapshotItem["sessions"][number]["session"][];
}): string | null => {
  const activeSessionBelongsToTask =
    Boolean(activeSessionIdFromTab)
    && (
      activeSessionIdFromTab === primarySessionId
      || sessions.some((session) => idToString(session.id) === activeSessionIdFromTab)
    );
  if (activeSessionBelongsToTask) return activeSessionIdFromTab;
  if (primarySessionId) return primarySessionId;
  if (sessions.length === 0) return activeSessionIdFromTab ?? null;
  return pickPreferredSessionId(sessions, activeSessionIdFromTab);
};

export const canRenderWorkbenchActiveSession = (
  entry: SessionSupervisorSnapshot["sessions"][string] | null | undefined,
): boolean => {
  if (!entry) return false;
  if (entry.stateLoaded) return true;
  return (
    entry.turns.length > 0 ||
    entry.messages.length > 0 ||
    entry.events.length > 0 ||
    entry.queue.length > 0
  );
};

export const resolveRenderableWorkbenchActiveSessionId = ({
  activeSessionIdFromTab,
  primarySessionId,
  sessions,
  sessionEntries,
}: {
  activeSessionIdFromTab: string | null;
  primarySessionId: string;
  sessions: WorkspaceActiveSnapshotItem["sessions"][number]["session"][];
  sessionEntries: SessionSupervisorSnapshot["sessions"];
}): string | null => {
  const candidateSessionId = resolveWorkbenchActiveSessionId({
    activeSessionIdFromTab,
    primarySessionId,
    sessions,
  });
  if (!candidateSessionId) return null;
  return canRenderWorkbenchActiveSession(sessionEntries[candidateSessionId]) ? candidateSessionId : null;
};

export const isWorkbenchTaskUnread = ({
  taskId,
  tasksById,
  taskLiveInfo,
}: {
  taskId: string;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  taskLiveInfo: WorkbenchTaskLiveInfo;
}): boolean => {
  const summary = tasksById[taskId];
  const task = summary?.task;
  if (!task) return false;
  const serverLastAssistantMs = resolvePrimaryServerLastAssistantMs(summary);
  const liveLastAssistantMs = taskLiveInfo.lastAssistantMsByTask[taskId] ?? null;
  const lastAssistantMs =
    liveLastAssistantMs !== null && serverLastAssistantMs !== null
      ? Math.max(liveLastAssistantMs, serverLastAssistantMs)
      : liveLastAssistantMs ?? serverLastAssistantMs;
  if (lastAssistantMs === null) return false;
  const seenMs = parseMs(task.assistant_seen_at ?? null);
  return seenMs === null || lastAssistantMs > seenMs;
};

export type WorkbenchWorkspaceAttentionState = {
  hasUnreadError: boolean;
  unreadPrimaryTaskCount: number;
};

export type WorkbenchTaskAttentionKind = "none" | "unread_completed" | "unread_error";

export const deriveWorkbenchTaskAttentionKind = ({
  taskId,
  tasksById,
  taskLiveInfo,
}: {
  taskId: string;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  taskLiveInfo: WorkbenchTaskLiveInfo;
}): WorkbenchTaskAttentionKind => {
  if (taskLiveInfo.workingByTask.has(taskId)) return "none";
  if (!isWorkbenchTaskUnread({ taskId, tasksById, taskLiveInfo })) return "none";
  return taskLiveInfo.errorByTask.has(taskId) ? "unread_error" : "unread_completed";
};

export const deriveWorkspaceAttentionState = ({
  activeTaskIds,
  tasksById,
  taskLiveInfo,
}: {
  activeTaskIds: string[];
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  taskLiveInfo: WorkbenchTaskLiveInfo;
}): WorkbenchWorkspaceAttentionState => {
  let unreadPrimaryTaskCount = 0;
  let hasUnreadError = false;

  for (const taskId of activeTaskIds) {
    const attentionKind = deriveWorkbenchTaskAttentionKind({ taskId, tasksById, taskLiveInfo });
    if (attentionKind === "none") continue;
    unreadPrimaryTaskCount += 1;
    if (attentionKind === "unread_error") hasUnreadError = true;
  }

  return {
    unreadPrimaryTaskCount,
    hasUnreadError,
  };
};
