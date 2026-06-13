import { describe, expect, it } from "vitest";
import type { WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import { deriveRetainedPrefetchSessionIds } from "./useWorkbenchSessionBridge";

const now = "2026-03-18T00:00:00.000Z";

const makeSnapshot = (sessionIds: readonly string[]): WorkspaceActiveSnapshotState => ({
  workspaceId: "workspace-1",
  initialized: true,
  liveSnapshotApplied: true,
  connection: "connected",
  tasksById: {
    "task-1": {
      id: "task-1",
      task: {
        id: "task-1",
        workspace_id: "workspace-1",
        title: "Task 1",
        status: "running",
        created_at: now,
        updated_at: now,
        last_activity_at: now,
        archived_at: null,
        assistant_seen_at: null,
        last_assistant_message_at: now,
        primary_session_id: sessionIds[0] ?? null,
      },
      sessions: sessionIds.map((sessionId) => ({
        session: {
          id: sessionId,
          task_id: "task-1",
          workspace_id: "workspace-1",
          worktree_id: "worktree-1",
          provider_id: "codex",
          model_id: "gpt-5",
          title: sessionId,
          agent_role: "assistant",
          status: "active",
          created_at: now,
          updated_at: now,
        },
        last_message_at: now,
        last_message_preview: "preview",
        last_event_seq: 1,
        projection_rev: 1,
        state_rev: 1,
        activity: { is_working: false, last_turn_status: null },
        unread: false,
      })),
      primarySessionId: sessionIds[0] ?? null,
      primarySessionHead: null,
      sort_at: now,
      sortAtMs: Date.parse(now),
    },
  },
  activeIds: ["task-1"],
  archivedIds: [],
  totalActive: 1,
  totalArchived: 0,
  archivedRev: 0,
  fetchState: { active: "idle", archived: "idle" },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: false,
});

describe("deriveRetainedPrefetchSessionIds", () => {
  it("retains a foreground session before the snapshot includes its summary", () => {
    const snapshot = makeSnapshot(["session-2"]);

    const retainedSessionIds = deriveRetainedPrefetchSessionIds({
      snapshot,
      foregroundSessionIds: ["session-1"],
      taskArchived: false,
    });

    expect(retainedSessionIds[0]).toBe("session-1");
    expect(retainedSessionIds).toContain("session-2");
  });

  it("keeps an archived foreground session before the snapshot includes its summary", () => {
    const snapshot = makeSnapshot(["session-2"]);

    const retainedSessionIds = deriveRetainedPrefetchSessionIds({
      snapshot,
      foregroundSessionIds: ["session-1"],
      taskArchived: true,
    });

    expect(retainedSessionIds[0]).toBe("session-1");
  });

  it("drops warm retained sessions while foreground work is active", () => {
    const snapshot = makeSnapshot(["session-2", "session-3"]);

    const retainedSessionIds = deriveRetainedPrefetchSessionIds({
      snapshot,
      foregroundSessionIds: ["session-1"],
      taskArchived: false,
      suppressWarmSessionIds: true,
    });

    expect(retainedSessionIds).toEqual(["session-1"]);
  });
});
