import type { SessionHeadSnapshot, SessionSnapshotSummary, WorkspaceActiveTaskSummary, WorkspaceTaskSummary } from "@ctx/types";
import { idToString } from "../../api/client";
import { compactActiveSessionHeadSnapshot } from "../sessionHeadState";
import type { PersistedWorkspaceActiveTaskSummaryV1 } from "../uiStateStore";
import { buildArchivedSnapshotItem, normalizeActiveSnapshotSummary } from "./itemBuilders";
import {
  projectPrimarySessionHeadOntoTasks,
  resolvePrimarySessionId,
} from "./projection";
import { isSessionHeadCompatibleWithSummary, shouldReplaceSessionHead } from "./summaryHelpers";
import type { WorkspaceActiveSnapshotItem } from "./storeTypes";

export type StoreSessionHeadState = {
  tasks: Map<string, WorkspaceActiveSnapshotItem>;
  sessionHeadsById: Map<string, SessionHeadSnapshot>;
  retainedLiveSessionIds: Set<string>;
};

export const resolveSessionSummaryForStore = (
  state: StoreSessionHeadState,
  sessionId: string,
): SessionSnapshotSummary | null => {
  const id = idToString(sessionId);
  if (!id) return null;
  for (const item of state.tasks.values()) {
    const summary = item.sessions.find((entry) => idToString(entry.session.id) === id) ?? null;
    if (summary) return summary;
  }
  return null;
};

export const resolvePrimarySessionSummaryForStore = (
  state: StoreSessionHeadState,
  sessionId: string,
  currentItem?: WorkspaceActiveSnapshotItem | null,
): SessionSnapshotSummary | null => {
  const id = idToString(sessionId);
  if (!id) return null;
  if (currentItem && resolvePrimarySessionId(currentItem) === id) {
    return currentItem.sessions.find((summary) => idToString(summary.session.id) === id) ?? null;
  }
  for (const item of state.tasks.values()) {
    if (resolvePrimarySessionId(item) !== id) continue;
    return item.sessions.find((summary) => idToString(summary.session.id) === id) ?? null;
  }
  return null;
};

export const shouldRetainSessionHeadForStore = (
  state: StoreSessionHeadState,
  sessionId: string,
  currentItem?: WorkspaceActiveSnapshotItem | null,
): boolean => {
  const id = idToString(sessionId);
  if (!id) return false;
  if (state.retainedLiveSessionIds.has(id)) return true;
  if (currentItem) {
    if (resolvePrimarySessionId(currentItem) === id) return true;
    if (idToString(currentItem.primarySessionHead?.session?.id ?? "") === id) return true;
  }
  for (const item of state.tasks.values()) {
    if (resolvePrimarySessionId(item) === id) return true;
    if (idToString(item.primarySessionHead?.session?.id ?? "") === id) return true;
  }
  return false;
};

export const rememberSessionHeadForStore = (
  state: StoreSessionHeadState,
  head: SessionHeadSnapshot | null,
  currentItem?: WorkspaceActiveSnapshotItem | null,
): void => {
  if (!head) return;
  const sessionId = idToString(head.session?.id ?? "");
  if (!sessionId) return;
  if (!shouldRetainSessionHeadForStore(state, sessionId, currentItem)) return;
  const compacted = compactActiveSessionHeadSnapshot(head);
  const primarySummary = resolvePrimarySessionSummaryForStore(state, sessionId, currentItem);
  if (!isSessionHeadCompatibleWithSummary(primarySummary, compacted)) return;
  const previous = state.sessionHeadsById.get(sessionId);
  if (!shouldReplaceSessionHead(previous, compacted)) return;
  state.sessionHeadsById.set(sessionId, compacted);
};

export const normalizeActiveSummaryForStore = (
  state: StoreSessionHeadState,
  summary: WorkspaceActiveTaskSummary | PersistedWorkspaceActiveTaskSummaryV1,
  existing?: WorkspaceActiveSnapshotItem,
): WorkspaceActiveSnapshotItem => {
  const normalized = normalizeActiveSnapshotSummary({
    summary,
    existing,
    sessionHeadsById: state.sessionHeadsById,
  });
  rememberSessionHeadForStore(state, normalized.primarySessionHead ?? null, normalized);
  const primarySessionId = resolvePrimarySessionId(normalized);
  if (primarySessionId) {
    normalized.primarySessionHead =
      state.sessionHeadsById.get(primarySessionId) ?? normalized.primarySessionHead;
  }
  return normalized;
};

export const collectArchivedHeadsForStore = (
  state: StoreSessionHeadState,
): Map<string, SessionHeadSnapshot> => {
  const archived = new Map<string, SessionHeadSnapshot>();
  for (const item of state.tasks.values()) {
    if (!item.task.archived_at) continue;
    const primarySessionId = resolvePrimarySessionId(item);
    if (!primarySessionId) continue;
    const head = state.sessionHeadsById.get(primarySessionId) ?? item.primarySessionHead ?? null;
    if (head) archived.set(primarySessionId, head);
  }
  return archived;
};

export const restoreRetainedLiveSessionHeadsForStore = (
  state: StoreSessionHeadState,
  previousHeads: ReadonlyMap<string, SessionHeadSnapshot>,
): void => {
  for (const sessionId of state.retainedLiveSessionIds) {
    if (state.sessionHeadsById.has(sessionId)) continue;
    const head = previousHeads.get(sessionId);
    const summary = resolveSessionSummaryForStore(state, sessionId);
    if (
      head &&
      summary &&
      shouldRetainSessionHeadForStore(state, sessionId) &&
      isSessionHeadCompatibleWithSummary(summary, head)
    ) {
      state.sessionHeadsById.set(sessionId, head);
      projectPrimarySessionHeadOntoTasks(state.tasks, head);
    }
  }
};

export const mergeWorkerSessionHeadUpsertsForStore = (
  state: StoreSessionHeadState,
  heads: Record<string, SessionHeadSnapshot>,
): void => {
  for (const [sessionId, head] of Object.entries(heads)) {
    const summary = resolveSessionSummaryForStore(state, sessionId);
    if (state.retainedLiveSessionIds.has(sessionId) && !summary) continue;
    if (!shouldRetainSessionHeadForStore(state, sessionId)) continue;
    const compacted = compactActiveSessionHeadSnapshot(head);
    if (summary && !isSessionHeadCompatibleWithSummary(summary, compacted)) continue;
    const previous = state.sessionHeadsById.get(sessionId);
    if (!shouldReplaceSessionHead(previous, compacted)) continue;
    state.sessionHeadsById.set(sessionId, compacted);
  }
};

export const applyActiveHeadsForStore = (
  state: StoreSessionHeadState,
  heads: SessionHeadSnapshot[],
): boolean => {
  if (!Array.isArray(heads) || heads.length === 0) return false;
  let changed = false;
  for (const head of heads) {
    if (!head || typeof head !== "object") continue;
    const sessionId = idToString(head.session?.id ?? "");
    if (!sessionId) continue;
    if (!shouldRetainSessionHeadForStore(state, sessionId)) continue;
    const compacted = compactActiveSessionHeadSnapshot(head);
    const primarySummary = resolvePrimarySessionSummaryForStore(state, sessionId);
    if (!isSessionHeadCompatibleWithSummary(primarySummary, compacted)) continue;
    const previous = state.sessionHeadsById.get(sessionId);
    if (shouldReplaceSessionHead(previous, compacted)) {
      state.sessionHeadsById.set(sessionId, compacted);
      changed = true;
    }
    if (projectPrimarySessionHeadOntoTasks(state.tasks, compacted)) {
      changed = true;
    }
  }
  return changed;
};

export const buildArchivedSnapshotItemForStore = (
  state: StoreSessionHeadState,
  summary: WorkspaceTaskSummary,
): WorkspaceActiveSnapshotItem | null =>
  buildArchivedSnapshotItem({
    summary,
    existing: state.tasks.get(idToString(summary.task.id)),
    sessionHeadsById: state.sessionHeadsById,
    rememberSessionHead: (head) => rememberSessionHeadForStore(state, head),
  });

export const pruneRetainedSessionHeadsForStore = (
  state: StoreSessionHeadState,
): void => {
  const retainedSessionIds = new Set<string>();
  for (const item of state.tasks.values()) {
    const primarySessionId = resolvePrimarySessionId(item);
    if (primarySessionId) retainedSessionIds.add(primarySessionId);
    const primaryHeadSessionId = idToString(item.primarySessionHead?.session?.id ?? "");
    if (primaryHeadSessionId) retainedSessionIds.add(primaryHeadSessionId);
  }
  for (const sessionId of state.retainedLiveSessionIds) {
    if (resolveSessionSummaryForStore(state, sessionId)) {
      retainedSessionIds.add(sessionId);
    }
  }
  for (const sessionId of state.sessionHeadsById.keys()) {
    if (!retainedSessionIds.has(sessionId)) {
      state.sessionHeadsById.delete(sessionId);
    }
  }
};
