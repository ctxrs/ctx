import type {
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  Task,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import type { SessionSubscriptionCursor } from "../sessionSubscription";

export type WorkspaceActiveSnapshotItem = {
  id: string;
  task: Task;
  sessions: SessionSnapshotSummary[];
  providerIds?: string[];
  primarySessionHead?: SessionHeadSnapshot | null;
  primarySessionId?: string | null;
  sort_at?: string | null;
  sortAtMs: number;
};

export type WorkspaceActiveSnapshotState = {
  workspaceId: string;
  initialized: boolean;
  liveSnapshotApplied: boolean;
  connection: "idle" | "connecting" | "connected" | "disconnected";
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  activeIds: string[];
  archivedIds: string[];
  totalActive: number;
  totalArchived: number;
  archivedRev: number;
  fetchState: {
    active: "idle" | "loading" | "error";
    archived: "idle" | "loading" | "error";
  };
  hasMoreActive: boolean;
  hasMoreArchived: boolean;
  archivedLoaded: boolean;
};

export type WorkspaceActiveSnapshotEventSource = {
  subscribe: (listener: () => void) => () => void;
  subscribeEvents: (listener: (event: WorkspaceActiveSnapshotEvent) => void) => () => void;
  getSnapshot: () => WorkspaceActiveSnapshotState;
  getSessionHeadSnapshot: (sessionId: string) => SessionHeadSnapshot | null;
  getSessionHeadsSnapshot?: () => Record<string, SessionHeadSnapshot>;
  getWorktreeRoot: (worktreeId: string) => string | null;
  setSubscribedSessions?: (sessions: SessionSubscriptionCursor[]) => void;
  setForegroundSessionId?: (sessionId: string | null) => void;
};
