import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyTurnOutcomeEffects, replayTurnOutcomeEffectsFromTurns } from "./turnOutcomeEffects";
import { resetTurnOutcomeTrackingForTests } from "../../utils/analytics/turnOutcomeDedup";

const sendDesktopNotification = vi.hoisted(() => vi.fn());
const isAppInForeground = vi.hoisted(() => vi.fn());
const getClientSettingsState = vi.hoisted(() => vi.fn());
const trackTurnCompleted = vi.hoisted(() => vi.fn());
const trackProviderRunCompleted = vi.hoisted(() => vi.fn());
const trackFirstTurnCompleted = vi.hoisted(() => vi.fn());

vi.mock("../../utils/desktopNotifications", () => ({
  sendDesktopNotification,
}));

vi.mock("../../utils/windowFocus", () => ({
  isAppInForeground,
}));

vi.mock("../clientSettings", () => ({
  getClientSettingsState,
}));

vi.mock("../../utils/analytics", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../utils/analytics")>();
  return {
    ...actual,
    trackTurnCompleted,
    trackProviderRunCompleted,
    trackFirstTurnCompleted,
  };
});

const baseIso = "2024-01-01T00:00:00.000Z";

const makeWorkspaceSnapshot = (taskTitle: string) => ({
  workspaceId: "workspace-1",
  initialized: true,
  liveSnapshotApplied: true,
  connection: "connected" as const,
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
    active: "idle" as const,
    archived: "idle" as const,
  },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: true,
});

describe("turnOutcomeEffects", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetTurnOutcomeTrackingForTests();
    isAppInForeground.mockReturnValue(false);
    getClientSettingsState.mockReturnValue({
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    });
  });

  it("tracks analytics during replay but skips notifications", () => {
    replayTurnOutcomeEffectsFromTurns({
      notify: false,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
      session: {
        id: "session-1",
        task_id: "task-1",
        workspace_id: "workspace-1",
        worktree_id: "worktree-1",
        provider_id: "codex",
        model_id: "gpt-5",
        agent_role: "implementer",
        status: "active",
        title: "Demo session",
        created_at: baseIso,
      },
      workspaceSnapshotState: makeWorkspaceSnapshot("Fix login race"),
      events: [],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "Added retry logic around token refresh.",
        delivery: "immediate",
        created_at: baseIso,
      }],
      previousTurns: [{
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: null,
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: baseIso,
        updated_at: baseIso,
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      }],
      nextTurns: [{
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: null,
        status: "completed",
        start_seq: 1,
        end_seq: 2,
        started_at: baseIso,
        updated_at: "2024-01-01T00:00:10.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      }],
    });

    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("tracks terminal outcomes and notifies only for completed primary turns", () => {
    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-1",
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
      startedAt: "2026-03-10T00:00:00.000Z",
      completedAt: "2026-03-10T00:00:42.000Z",
      notificationTitle: "Fix login race",
      notificationBody: "Added retry logic around token refresh.",
      previousStatus: "running",
      nextStatus: "completed",
    });

    expect(trackTurnCompleted).toHaveBeenCalledWith({
      providerId: "codex",
      modelId: "gpt-5",
      reasoningEffort: undefined,
      executionEnvironment: undefined,
      status: "completed",
      durationMs: 42000,
      sessionKind: "primary",
      metrics: undefined,
      failureKind: undefined,
    });
    expect(trackProviderRunCompleted).toHaveBeenCalledWith({
      providerId: "codex",
      modelId: "gpt-5",
      status: "completed",
      durationMs: 42000,
      sessionKind: "primary",
      failureKind: undefined,
    });
    expect(trackFirstTurnCompleted).toHaveBeenCalledWith({
      sessionId: "session-1",
      providerId: "codex",
      status: "completed",
      sessionKind: "primary",
    });
    expect(sendDesktopNotification).toHaveBeenCalledWith({
      kind: "turn_completed",
      title: "Fix login race",
      body: "Added retry logic around token refresh.",
      workspaceId: "workspace-1",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("sends error notifications for failed primary turns", () => {
    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-2",
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
      notificationTitle: "Fix login race",
      notificationBody: "The browser cookie still disappears after refresh.",
      previousStatus: "running",
      nextStatus: "failed",
    });

    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackTurnCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "failed",
      failureKind: "unknown",
    }));
    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "failed",
      failureKind: "unknown",
    }));
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
    expect(sendDesktopNotification).toHaveBeenCalledWith({
      kind: "turn_failed",
      title: "Fix login race",
      body: "The browser cookie still disappears after refresh.",
      workspaceId: "workspace-1",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("does not notify until client settings finish loading", () => {
    getClientSettingsState.mockReturnValue({
      loaded: false,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    });

    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-4",
      sessionKind: "primary",
      notificationTitle: "Fix login race",
      notificationBody: "Added retry logic around token refresh.",
      previousStatus: "running",
      nextStatus: "completed",
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("falls back to a generic status title when no notification title is provided", () => {
    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-5",
      sessionKind: "primary",
      previousStatus: "running",
      nextStatus: "completed",
    });

    expect(sendDesktopNotification).toHaveBeenCalledWith({
      kind: "turn_completed",
      title: "Turn completed",
      body: undefined,
      workspaceId: "workspace-1",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("does not notify for subagent turns", () => {
    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-1",
      sessionKind: "subagent",
      previousStatus: "running",
      nextStatus: "completed",
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("deduplicates analytics for the same terminal turn across replay and live updates", () => {
    applyTurnOutcomeEffects({
      notify: false,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-3",
      providerId: "codex",
      modelId: "gpt-5",
      previousStatus: "running",
      nextStatus: "completed",
    });
    applyTurnOutcomeEffects({
      notify: true,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      turnId: "turn-3",
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
      previousStatus: "running",
      nextStatus: "completed",
    });

    expect(trackTurnCompleted).toHaveBeenCalledTimes(1);
    expect(trackProviderRunCompleted).toHaveBeenCalledTimes(1);
    expect(trackFirstTurnCompleted).toHaveBeenCalledTimes(1);
    expect(sendDesktopNotification).toHaveBeenCalledTimes(1);
  });

  it("tracks bounded first-turn failure kinds from terminal failure metadata", () => {
    replayTurnOutcomeEffectsFromTurns({
      notify: false,
      sessionId: "session-1",
      taskId: "task-1",
      workspaceId: "workspace-1",
      providerId: "codex",
      modelId: "gpt-5",
      sessionKind: "primary",
      session: null,
      workspaceSnapshotState: makeWorkspaceSnapshot("Fix login race"),
      events: [],
      messages: [],
      previousTurns: [{
        turn_id: "turn-auth",
        session_id: "session-1",
        run_id: null,
        user_message_id: null,
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: baseIso,
        updated_at: baseIso,
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      }],
      nextTurns: [{
        turn_id: "turn-auth",
        session_id: "session-1",
        run_id: null,
        user_message_id: null,
        status: "failed",
        start_seq: 1,
        end_seq: 2,
        started_at: baseIso,
        updated_at: "2024-01-01T00:00:10.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        failure: {
          kind: "provider_auth_required",
        },
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      }],
    });

    expect(trackTurnCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "failed",
      failureKind: "auth_missing",
    }));
    expect(trackProviderRunCompleted).toHaveBeenCalledWith(expect.objectContaining({
      status: "failed",
      failureKind: "auth_missing",
    }));
  });
});
