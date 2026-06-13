import type {
  SessionHeadSnapshot,
  SessionSnapshotSummary,
} from "@ctx/types";
import { idToString } from "../../api/client";

export const sortSessionSummaries = (summaries: SessionSnapshotSummary[]): SessionSnapshotSummary[] => {
  return summaries
    .slice()
    .sort((a, b) => String(a.session.created_at ?? "").localeCompare(String(b.session.created_at ?? "")));
};

export const hasOwnProperty = (value: unknown, key: string): boolean => {
  if (!value || typeof value !== "object") return false;
  return Object.prototype.hasOwnProperty.call(value, key);
};

export const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export const readString = (value: unknown): string | null => {
  if (typeof value !== "string") return null;
  return value;
};

type WorkspaceActiveTaskLike = {
  task: {
    id?: unknown;
    primary_session_id?: unknown;
  };
  primarySessionId?: string | null;
  primarySessionHead?: SessionHeadSnapshot | null;
  sessions?: Array<{
    session: {
      id?: unknown;
    };
  }>;
};

type WorkspaceActiveSnapshotStateLike = {
  activeIds: string[];
  archivedIds: string[];
  tasksById: Record<string, WorkspaceActiveTaskLike>;
};

export const resolvePrimarySessionId = (item: WorkspaceActiveTaskLike | null | undefined): string | null => {
  if (!item) return null;
  const direct =
    item.primarySessionId ||
    idToString(readString(item.task?.primary_session_id) ?? "");
  if (direct) return direct;
  const headId = idToString(readString(item.primarySessionHead?.session?.id) ?? "");
  if (headId) return headId;
  const summary = item.sessions?.[0];
  const sessionId = idToString(readString(summary?.session?.id) ?? "");
  return sessionId || null;
};

export const workspaceTaskIncludesSession = (
  item: WorkspaceActiveTaskLike | null | undefined,
  sessionId: string,
): boolean => {
  const id = String(sessionId ?? "").trim();
  if (!item || !id) return false;
  if (resolvePrimarySessionId(item) === id) return true;
  for (const summary of item.sessions ?? []) {
    if (idToString(readString(summary.session?.id) ?? "") === id) return true;
  }
  return false;
};

export const resolveSessionModeFromWorkspaceState = (
  state: WorkspaceActiveSnapshotStateLike | null | undefined,
  sessionId: string,
): "active" | "archived" | null => {
  const id = String(sessionId ?? "").trim();
  if (!state || !id) return null;
  for (const taskId of state.activeIds) {
    if (workspaceTaskIncludesSession(state.tasksById[taskId], id)) return "active";
  }
  for (const taskId of state.archivedIds) {
    if (workspaceTaskIncludesSession(state.tasksById[taskId], id)) return "archived";
  }
  return null;
};

export const collectWorkspaceActivePrimarySessionIds = (
  state: WorkspaceActiveSnapshotStateLike | null | undefined,
): string[] => {
  if (!state) return [];
  const ids = new Set<string>();
  for (const taskId of state.activeIds) {
    const primaryId = resolvePrimarySessionId(state.tasksById[taskId]);
    if (primaryId) ids.add(primaryId);
  }
  return Array.from(ids);
};

export const findWorkspaceSessionHead = (
  state: WorkspaceActiveSnapshotStateLike | null | undefined,
  sessionHeadsById: ReadonlyMap<string, SessionHeadSnapshot>,
  sessionId: string,
): SessionHeadSnapshot | null => {
  const id = String(sessionId ?? "").trim();
  if (!id) return null;
  const direct = sessionHeadsById.get(id) ?? null;
  if (direct) return direct;
  if (!state) return null;
  for (const taskId of state.activeIds) {
    const item = state.tasksById[taskId];
    if (!workspaceTaskIncludesSession(item, id)) continue;
    const head = item?.primarySessionHead ?? null;
    if (idToString(head?.session?.id ?? "") === id) {
      return head;
    }
  }
  return null;
};

export const projectPrimarySessionHeadOntoTasks = <T extends WorkspaceActiveTaskLike>(
  tasks: Map<string, T>,
  head: SessionHeadSnapshot,
): boolean => {
  const headTaskId = idToString(head.session?.task_id ?? "");
  const headSessionId = idToString(head.session?.id ?? "");
  if (!headTaskId && !headSessionId) return false;
  let changed = false;
  for (const [taskId, item] of tasks.entries()) {
    const primaryId = resolvePrimarySessionId(item);
    const matchesSession = primaryId ? primaryId === headSessionId : false;
    const matchesTask = headTaskId ? headTaskId === taskId : false;
    if (!matchesSession && !matchesTask) continue;
    if (primaryId && !matchesSession) continue;
    tasks.set(taskId, {
      ...item,
      primarySessionId: primaryId || headSessionId || null,
      primarySessionHead: head,
    });
    changed = true;
  }
  return changed;
};
