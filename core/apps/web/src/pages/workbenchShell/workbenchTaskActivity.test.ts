import { describe, expect, it } from "vitest";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import {
  deriveWorkbenchTaskAttentionKind,
  deriveWorkspaceAttentionState,
  type WorkbenchTaskLiveInfo,
} from "./workbenchTaskActivity";

describe("deriveWorkspaceAttentionState", () => {
  it("counts unread active tasks and flags unread errors", () => {
    const tasksById = {
      "task-1": {
        id: "task-1",
        task: {
          id: "task-1",
          assistant_seen_at: "2026-04-20T00:00:00.000Z",
          last_assistant_message_at: "2026-04-20T00:05:00.000Z",
          primary_session_id: "session-1",
        },
        sessions: [{ session: { id: "session-1" }, last_message_at: "2026-04-20T00:05:00.000Z" }],
      },
      "task-2": {
        id: "task-2",
        task: {
          id: "task-2",
          assistant_seen_at: "2026-04-20T00:10:00.000Z",
          last_assistant_message_at: "2026-04-20T00:05:00.000Z",
          primary_session_id: "session-2",
        },
        sessions: [{ session: { id: "session-2" }, last_message_at: "2026-04-20T00:05:00.000Z" }],
      },
      "task-3": {
        id: "task-3",
        task: {
          id: "task-3",
          assistant_seen_at: null,
          last_assistant_message_at: "2026-04-20T00:08:00.000Z",
          primary_session_id: "session-3",
        },
        sessions: [{ session: { id: "session-3" }, last_message_at: "2026-04-20T00:08:00.000Z" }],
      },
    } as unknown as Record<string, WorkspaceActiveSnapshotItem>;

    const taskLiveInfo: WorkbenchTaskLiveInfo = {
      workingByTask: new Set<string>(),
      errorByTask: new Set<string>(["task-3"]),
      lastAssistantMsByTask: {},
    };

    expect(
      deriveWorkspaceAttentionState({
        activeTaskIds: ["task-1", "task-2", "task-3"],
        tasksById,
        taskLiveInfo,
      }),
    ).toEqual({
      unreadPrimaryTaskCount: 2,
      hasUnreadError: true,
    });
  });

  it("ignores task-global assistant timestamps when the primary session has no unread output", () => {
    const tasksById = {
      "task-1": {
        id: "task-1",
        task: {
          id: "task-1",
          assistant_seen_at: "2026-04-20T00:10:00.000Z",
          last_assistant_message_at: "2026-04-20T00:20:00.000Z",
          primary_session_id: "session-1",
        },
        sessions: [
          {
            session: { id: "session-1" },
            last_message_at: "2026-04-20T00:05:00.000Z",
          },
        ],
      },
    } as unknown as Record<string, WorkspaceActiveSnapshotItem>;

    const taskLiveInfo: WorkbenchTaskLiveInfo = {
      workingByTask: new Set<string>(),
      errorByTask: new Set<string>(),
      lastAssistantMsByTask: {},
    };

    expect(
      deriveWorkspaceAttentionState({
        activeTaskIds: ["task-1"],
        tasksById,
        taskLiveInfo,
      }),
    ).toEqual({
      unreadPrimaryTaskCount: 0,
      hasUnreadError: false,
    });
  });

  it("suppresses workspace attention while the unread task is still working", () => {
    const tasksById = {
      "task-1": {
        id: "task-1",
        task: {
          id: "task-1",
          assistant_seen_at: "2026-04-20T00:00:00.000Z",
          last_assistant_message_at: "2026-04-20T00:05:00.000Z",
          primary_session_id: "session-1",
        },
        sessions: [{ session: { id: "session-1" }, last_message_at: "2026-04-20T00:05:00.000Z" }],
      },
    } as unknown as Record<string, WorkspaceActiveSnapshotItem>;

    const taskLiveInfo: WorkbenchTaskLiveInfo = {
      workingByTask: new Set<string>(["task-1"]),
      errorByTask: new Set<string>(["task-1"]),
      lastAssistantMsByTask: {},
    };

    expect(
      deriveWorkbenchTaskAttentionKind({
        taskId: "task-1",
        tasksById,
        taskLiveInfo,
      }),
    ).toBe("none");

    expect(
      deriveWorkspaceAttentionState({
        activeTaskIds: ["task-1"],
        tasksById,
        taskLiveInfo,
      }),
    ).toEqual({
      unreadPrimaryTaskCount: 0,
      hasUnreadError: false,
    });
  });
});
