import type {
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  WorkspaceActiveTaskSummary,
  WorkspaceTaskSummary,
} from "@ctx/types";
import { idToString } from "../../api/client";
import { sanitizeSessionHeadSnapshot } from "../sessionHeadState";
import type { PersistedWorkspaceActiveTaskSummaryV1 } from "../uiStateStore";
import type { WorkspaceActiveSnapshotItem } from "./storeTypes";
import { hasOwnProperty, resolvePrimarySessionId, sortSessionSummaries } from "./projection";
import {
  isSessionHeadCompatibleWithSummary,
  normalizeSessionSummary,
  pickArchivedSessionId,
  pickArchivedSessionIdFromSummaries,
  readPrimarySessionHead,
  readPrimarySessionId,
  sessionToSummary,
  taskSortAt,
} from "./summaryHelpers";

type RememberSessionHead = (head: SessionHeadSnapshot | null) => void;

export function buildPersistedActiveSnapshotSummary(params: {
  item: WorkspaceActiveSnapshotItem;
  sessionHeadsById: Map<string, SessionHeadSnapshot>;
}): PersistedWorkspaceActiveTaskSummaryV1 | null {
  const { item, sessionHeadsById } = params;
  if (!item.task) return null;
  const sessions = Array.isArray(item.sessions) ? item.sessions : [];
  const primaryId = resolvePrimarySessionId(item);
  let primary = primaryId
    ? sessions.find((summary) => idToString(summary.session.id) === primaryId) ?? null
    : null;
  if (!primary && sessions.length > 0) {
    primary = sessions[0] ?? null;
  }
  if (!primary && item.primarySessionHead?.session) {
    primary = sessionToSummary(item.primarySessionHead.session);
  }
  const head =
    item.primarySessionHead ||
    (primaryId ? sessionHeadsById.get(primaryId) ?? null : null);
  const sortAt = taskSortAt(item.task, item.sort_at);
  return {
    task: item.task,
    primary_session: primary ?? null,
    primary_session_head: head ? sanitizeSessionHeadSnapshot(head) : null,
    sessions,
    sort_at: sortAt,
  };
}

export function buildArchivedSnapshotItem(params: {
  summary: WorkspaceTaskSummary;
  existing?: WorkspaceActiveSnapshotItem;
  sessionHeadsById: Map<string, SessionHeadSnapshot>;
  rememberSessionHead: RememberSessionHead;
}): WorkspaceActiveSnapshotItem | null {
  const { summary, existing, sessionHeadsById, rememberSessionHead } = params;
  const task = summary.task;
  const id = idToString(task.id);
  if (!id) return null;
  const providerIds = (summary.provider_ids ?? []).filter(Boolean);
  const summaries = existing?.sessions ?? [];
  const sessionList = summaries.map((item) => item.session).filter(Boolean);
  const summarySessions = Array.isArray(summary.sessions) ? summary.sessions : [];
  if (existing?.primarySessionHead) {
    rememberSessionHead(existing.primarySessionHead);
  }
  const summaryPrimaryId = readPrimarySessionId(summary);
  const primarySessionId =
    summaryPrimaryId ||
    pickArchivedSessionIdFromSummaries(task, summarySessions) ||
    pickArchivedSessionId(task, sessionList);
  let primarySessionHead = null;
  if (!primarySessionHead && primarySessionId) {
    primarySessionHead = sessionHeadsById.get(primarySessionId) ?? null;
  }
  if (!primarySessionHead && existing?.primarySessionHead) {
    primarySessionHead = existing.primarySessionHead;
  }
  const sortAt = taskSortAt(task);
  return {
    id,
    task: { ...task },
    sessions: sortSessionSummaries(summaries),
    providerIds: providerIds.length ? providerIds : existing?.providerIds,
    primarySessionId: primarySessionId || null,
    primarySessionHead: primarySessionHead ?? null,
    sortAtMs: Date.parse(sortAt) || Date.now(),
    sort_at: sortAt || null,
  };
}

export function normalizeActiveSnapshotSummary(params: {
  summary: WorkspaceActiveTaskSummary | PersistedWorkspaceActiveTaskSummaryV1;
  existing?: WorkspaceActiveSnapshotItem;
  sessionHeadsById: Map<string, SessionHeadSnapshot>;
}): WorkspaceActiveSnapshotItem {
  const { summary, existing, sessionHeadsById } = params;
  const id = idToString(summary.task.id);
  const summaryHasPrimary = hasOwnProperty(summary, "primary_session") || hasOwnProperty(summary, "primarySession");
  const summaryHasSessions = hasOwnProperty(summary, "sessions");
  const summaryHasHead =
    hasOwnProperty(summary, "primary_session_head") || hasOwnProperty(summary, "primarySessionHead");
  const summaryHasSortAt = hasOwnProperty(summary, "sort_at") || hasOwnProperty(summary, "sortAt");

  const fallbackSortAt = summaryHasSortAt
    ? (summary as PersistedWorkspaceActiveTaskSummaryV1).sort_at ?? null
    : existing?.sort_at ?? null;
  const sortAt = taskSortAt(summary.task, fallbackSortAt);
  const sortAtMs = Date.parse(sortAt) || existing?.sortAtMs || Date.now();
  const existingPrimarySessionId = resolvePrimarySessionId(existing);
  const primarySessionId =
    readPrimarySessionId(summary) ||
    idToString(summary.task.primary_session_id ?? "") ||
    existingPrimarySessionId ||
    idToString((summary as PersistedWorkspaceActiveTaskSummaryV1).primary_session?.session?.id ?? "");

  const existingSessions = existing?.sessions ?? [];
  const primaryFromSummary = summaryHasPrimary
    ? (summary as PersistedWorkspaceActiveTaskSummaryV1).primary_session
    : null;
  let primarySummary = primaryFromSummary ? normalizeSessionSummary(primaryFromSummary) : null;
  if (!primarySummary && primarySessionId) {
    primarySummary =
      existingSessions.find((item) => idToString(item.session.id) === primarySessionId) ?? null;
  }

  let primaryHead = summaryHasHead ? readPrimarySessionHead(summary) : existing?.primarySessionHead ?? null;
  if (!primaryHead && primarySessionId) {
    primaryHead = sessionHeadsById.get(primarySessionId) ?? null;
  }
  if (primaryHead && !isSessionHeadCompatibleWithSummary(primarySummary, primaryHead)) {
    primaryHead = null;
  }

  const sessionsRaw = summaryHasSessions
    ? Array.isArray((summary as PersistedWorkspaceActiveTaskSummaryV1).sessions)
      ? (summary as PersistedWorkspaceActiveTaskSummaryV1).sessions
      : []
    : existingSessions;
  const sessions = sessionsRaw.map((item) => normalizeSessionSummary(item));

  const merged: SessionSnapshotSummary[] = [];
  const seen = new Set<string>();
  const addSummary = (item: SessionSnapshotSummary) => {
    const sid = idToString(item.session.id);
    if (!sid || seen.has(sid)) return;
    seen.add(sid);
    merged.push(item);
  };
  if (primarySummary) {
    addSummary(primarySummary);
  }
  sessions.forEach(addSummary);

  return {
    id,
    task: { ...summary.task },
    sessions: sortSessionSummaries(merged),
    primarySessionHead: primaryHead ?? null,
    primarySessionId: primarySessionId || null,
    sortAtMs,
    sort_at: sortAt || null,
  };
}
