import { describe, expect, it } from "vitest";
import type { Message, Session, SessionTurn } from "../../api/client";
import type { WorkspaceActiveSnapshotState } from "../workspaceActiveSnapshotStore";
import {
  buildTurnOutcomeNotificationBodyPreview,
  resolveTurnOutcomeNotificationBody,
  resolveTurnOutcomeNotificationTitle,
} from "./turnOutcomeNotificationContent";

const baseIso = "2026-03-10T00:00:00.000Z";

const makeSession = (overrides: Partial<Session> = {}): Session => ({
  id: "session-1",
  task_id: "task-1",
  workspace_id: "workspace-1",
  worktree_id: "worktree-1",
  provider_id: "codex",
  model_id: "gpt-5",
  agent_role: "implementer",
  status: "active",
  title: "Fallback session title",
  created_at: baseIso,
  ...overrides,
});

const makeWorkspaceSnapshot = (taskTitle = "Implement desktop notifications"): WorkspaceActiveSnapshotState => ({
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
        title: taskTitle,
        status: "active",
        primary_session_id: "session-1",
        primary_worktree_id: "worktree-1",
        created_at: baseIso,
        updated_at: baseIso,
      },
      sessions: [],
      sortAtMs: Date.parse(baseIso),
    },
  },
  activeIds: ["task-1"],
  archivedIds: [],
  totalActive: 1,
  totalArchived: 0,
  archivedRev: 0,
  fetchState: {
    active: "idle",
    archived: "idle",
  },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: true,
});

const makeMessage = (overrides: Partial<Message> = {}): Message => ({
  id: "message-1",
  session_id: "session-1",
  task_id: "task-1",
  turn_id: "turn-1",
  role: "assistant",
  content: "Done",
  delivery: "immediate",
  created_at: baseIso,
  ...overrides,
});

const makeTurn = (overrides: Partial<SessionTurn> = {}): SessionTurn => ({
  turn_id: "turn-1",
  session_id: "session-1",
  status: "running",
  started_at: baseIso,
  updated_at: baseIso,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
  ...overrides,
});

describe("turnOutcomeNotificationContent", () => {
  it("prefers the task title from the workspace snapshot and falls back to a generic task label", () => {
    expect(
      resolveTurnOutcomeNotificationTitle({
        session: makeSession(),
        workspaceSnapshotState: makeWorkspaceSnapshot("Fix login race"),
      }),
    ).toBe("Fix login race");

    expect(
      resolveTurnOutcomeNotificationTitle({
        session: makeSession({ title: "Session fallback" }),
        workspaceSnapshotState: null,
      }),
    ).toBe("Task update");
  });

  it("builds a single-line preview from assistant markdown content", () => {
    expect(
      buildTurnOutcomeNotificationBodyPreview(
        "## Update\n\n- Added retry logic around token refresh.\n- Expanded Safari coverage.\n",
      ),
    ).toBe("Update Added retry logic around token refresh. Expanded Safari coverage.");
  });

  it("uses the latest non-queued assistant message for the finishing turn", () => {
    expect(
      resolveTurnOutcomeNotificationBody({
        turnId: "turn-2",
        messages: [
          makeMessage({
            id: "message-old",
            turn_id: "turn-1",
            content: "Old turn output",
          }),
          makeMessage({
            id: "message-queued",
            turn_id: "turn-2",
            content: "Queued preview",
            delivery: "queued",
          }),
          makeMessage({
            id: "message-final",
            turn_id: "turn-2",
            content: "Final answer with `inline code` and\nmultiple lines.",
          }),
        ],
        status: "completed",
      }),
    ).toBe("Final answer with inline code and multiple lines.");
  });

  it("uses turn failure message for failed turns without assistant output", () => {
    expect(
      resolveTurnOutcomeNotificationBody({
        turnId: "turn-2",
        messages: [],
        turn: makeTurn({
          turn_id: "turn-2",
          status: "failed",
          failure: { message: "OAuth token has expired.\nPlease reconnect." },
        }),
        status: "failed",
      }),
    ).toBe("OAuth token has expired. Please reconnect.");
  });

  it("truncates long previews to notification length", () => {
    const longText = "A".repeat(200);
    expect(
      resolveTurnOutcomeNotificationBody({
        turnId: "turn-1",
        messages: [makeMessage({ content: longText })],
        status: "completed",
      }),
    ).toBe(`${"A".repeat(139)}\u2026`);
  });
});
