import React from "react";
import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionLifecycleCoordinator } from "./sessionLifecycleCoordinator";
import { SessionSupervisorProvider, useOpenSession } from "./sessionSupervisor";
import type { SessionMode } from "./sessionSupervisorCore";
import type { WorkspaceActiveSnapshotState } from "./workspaceActiveSnapshotStore";

const now = "2026-03-10T00:00:00.000Z";

const makeWorkspaceSnapshot = (
  overrides?: Partial<WorkspaceActiveSnapshotState>,
): WorkspaceActiveSnapshotState => {
  const liveSnapshotApplied = overrides?.liveSnapshotApplied ?? false;
  return {
    workspaceId: "workspace-1",
    initialized: false,
    connection: "connecting",
    tasksById: {},
    activeIds: [],
    archivedIds: [],
    totalActive: 0,
    totalArchived: 0,
    archivedRev: 0,
    fetchState: { active: "loading", archived: "idle" },
    hasMoreActive: false,
    hasMoreArchived: false,
    archivedLoaded: false,
    ...overrides,
    liveSnapshotApplied,
  };
};

const makeTaskSummary = ({
  taskId,
  sessionId,
  archived,
}: {
  taskId: string;
  sessionId: string;
  archived: boolean;
}) => ({
  id: taskId,
  task: {
    id: taskId,
    workspace_id: "workspace-1",
    title: `Task ${taskId}`,
    status: archived ? "completed" : "running",
    created_at: now,
    updated_at: now,
    last_activity_at: now,
    archived_at: archived ? now : null,
    primary_session_id: sessionId,
  },
  sessions: [
    {
      session: {
        id: sessionId,
        task_id: taskId,
        workspace_id: "workspace-1",
        worktree_id: "worktree-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: `Session ${sessionId}`,
        agent_role: "implementer",
        status: archived ? "completed" : "active",
        created_at: now,
        updated_at: now,
      },
      last_message_at: now,
      last_message_preview: null,
      last_event_seq: 1,
      state_rev: 1,
      activity: { is_working: false },
      unread: false,
    },
  ],
  primarySessionId: sessionId,
  primarySessionHead: null,
  sort_at: now,
  sortAtMs: Date.parse(now),
});

const makeSupervisor = () => ({
  openSession: vi.fn(() => vi.fn()),
  beginSessionOpen: vi.fn(),
  commitSessionOpenMode: vi.fn(),
  failPendingSessionOpen: vi.fn(),
  closeSession: vi.fn(),
});

describe("SessionLifecycleCoordinator", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("resolves an active route-open request from workspace membership without retry timers", () => {
    const supervisor = makeSupervisor();
    const coordinator = new SessionLifecycleCoordinator(supervisor);

    coordinator.registerRouteOpen("session-active");

    expect(supervisor.beginSessionOpen).toHaveBeenCalledWith("session-active", undefined);
    expect(supervisor.commitSessionOpenMode).not.toHaveBeenCalled();

    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
        activeIds: ["task-active"],
        totalActive: 1,
        tasksById: {
          "task-active": makeTaskSummary({ taskId: "task-active", sessionId: "session-active", archived: false }),
        },
      }),
    );

    expect(supervisor.commitSessionOpenMode).toHaveBeenCalledWith("session-active", "active", undefined);
    expect(supervisor.failPendingSessionOpen).not.toHaveBeenCalled();
  });

  it("resolves an archived route-open request from archived membership without remount timing", () => {
    const supervisor = makeSupervisor();
    const coordinator = new SessionLifecycleCoordinator(supervisor);

    coordinator.registerRouteOpen("session-archived");
    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
        archivedIds: ["task-archived"],
        totalArchived: 1,
        tasksById: {
          "task-archived": makeTaskSummary({ taskId: "task-archived", sessionId: "session-archived", archived: true }),
        },
      }),
    );

    expect(supervisor.commitSessionOpenMode).toHaveBeenCalledWith("session-archived", "archived", undefined);
    expect(supervisor.failPendingSessionOpen).not.toHaveBeenCalled();
  });

  it("keeps a route-open request pending until snapshot data arrives", () => {
    const supervisor = makeSupervisor();
    const coordinator = new SessionLifecycleCoordinator(supervisor);

    coordinator.registerRouteOpen("session-pending");
    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: false,
        connection: "connecting",
        fetchState: { active: "loading", archived: "idle" },
      }),
    );

    expect(supervisor.commitSessionOpenMode).not.toHaveBeenCalled();
    expect(supervisor.failPendingSessionOpen).not.toHaveBeenCalled();

    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
        activeIds: ["task-pending"],
        totalActive: 1,
        tasksById: {
          "task-pending": makeTaskSummary({ taskId: "task-pending", sessionId: "session-pending", archived: false }),
        },
      }),
    );

    expect(supervisor.commitSessionOpenMode).toHaveBeenCalledWith("session-pending", "active", undefined);
  });

  it("fails a pending route-open request once an initialized snapshot proves the session absent", () => {
    const supervisor = makeSupervisor();
    const coordinator = new SessionLifecycleCoordinator(supervisor);

    coordinator.registerRouteOpen("session-missing");
    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
      }),
    );

    expect(supervisor.commitSessionOpenMode).not.toHaveBeenCalled();
    expect(supervisor.failPendingSessionOpen).toHaveBeenCalledTimes(1);
    expect(supervisor.failPendingSessionOpen).toHaveBeenCalledWith("session-missing");

    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
      }),
    );

    expect(supervisor.failPendingSessionOpen).toHaveBeenCalledTimes(1);
  });

  it("does not reissue mode commits when the route request is unchanged across reconnects", () => {
    const supervisor = makeSupervisor();
    const coordinator = new SessionLifecycleCoordinator(supervisor);

    coordinator.registerRouteOpen("session-stable", { mode: "active" satisfies SessionMode });

    expect(supervisor.openSession).toHaveBeenCalledWith("session-stable", {
      watchDiff: false,
      force: false,
      silent: false,
      mode: "active",
    });
    expect(supervisor.beginSessionOpen).not.toHaveBeenCalled();
    expect(supervisor.commitSessionOpenMode).not.toHaveBeenCalled();

    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "disconnected",
        fetchState: { active: "idle", archived: "idle" },
      }),
    );
    coordinator.setWorkspaceSnapshotState(
      makeWorkspaceSnapshot({
        initialized: true,
        connection: "connected",
        fetchState: { active: "idle", archived: "idle" },
      }),
    );

    expect(supervisor.openSession).toHaveBeenCalledTimes(1);
    expect(supervisor.commitSessionOpenMode).not.toHaveBeenCalled();
  });
});

describe("useOpenSession", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("does not re-register route ownership when session and mode are unchanged across rerenders", () => {
    const registerSpy = vi.spyOn(SessionLifecycleCoordinator.prototype, "registerRouteOpen");

    function Harness({ sessionId, mode }: { sessionId: string; mode: SessionMode }) {
      useOpenSession(sessionId, { mode });
      return null;
    }

    const { rerender } = render(
      React.createElement(
        SessionSupervisorProvider,
        null,
        React.createElement(Harness, { sessionId: "session-rerender", mode: "active" }),
      ),
    );

    rerender(
      React.createElement(
        SessionSupervisorProvider,
        null,
        React.createElement(Harness, { sessionId: "session-rerender", mode: "active" }),
      ),
    );

    expect(registerSpy).toHaveBeenCalledTimes(1);
  });
});
