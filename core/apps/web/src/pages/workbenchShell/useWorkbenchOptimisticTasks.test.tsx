import React, { act, useEffect } from "react";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Message, Session, SessionHeadSnapshot, SessionSnapshotSummary, SessionTurn, Task } from "../../api/client";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import type { OptimisticTaskSummary } from "./WorkbenchPage.types";
import { useWorkbenchOptimisticTasks } from "./useWorkbenchOptimisticTasks";

const now = "2026-03-10T00:00:00.000Z";

type HookValue = ReturnType<typeof useWorkbenchOptimisticTasks>;

function makeSession(sessionId: string, taskId: string): Session {
  return {
    id: sessionId,
    task_id: taskId,
    workspace_id: "workspace-1",
    worktree_id: "worktree-1",
    provider_id: "codex",
    model_id: "gpt-5",
    title: `Session ${sessionId}`,
    agent_role: "assistant",
    status: "starting",
    created_at: now,
    updated_at: now,
  };
}

function makeTask(taskId: string, sessionId: string): Task {
  return {
    id: taskId,
    workspace_id: "workspace-1",
    title: `Task ${taskId}`,
    status: "running",
    created_at: now,
    updated_at: now,
    last_activity_at: now,
    primary_session_id: sessionId,
    has_active_session: true,
  };
}

function makeSessionSummary(session: Session): SessionSnapshotSummary {
  return {
    session,
    last_message_at: now,
    last_message_preview: "preview",
    last_event_seq: 1,
    state_rev: 1,
    activity: { is_working: false },
    unread: false,
  };
}

function makeTaskSummary({
  taskId,
  primarySessionId,
  sessions,
  primarySessionHead = null,
}: {
  taskId: string;
  primarySessionId: string;
  sessions: SessionSnapshotSummary[];
  primarySessionHead?: SessionHeadSnapshot | null;
}): WorkspaceActiveSnapshotItem {
  return {
    id: taskId,
    task: {
      ...makeTask(taskId, primarySessionId),
      archived_at: null,
    },
    sessions,
    primarySessionId,
    primarySessionHead,
    sort_at: now,
    sortAtMs: Date.parse(now),
  };
}

function makeMessage(sessionId: string, taskId: string, content: string): Message {
  return {
    id: `message-${sessionId}`,
    session_id: sessionId,
    task_id: taskId,
    turn_id: `turn-${sessionId}`,
    turn_sequence: 1,
    role: "user",
    content,
    delivery: "immediate",
    created_at: now,
  };
}

function makeTurn(sessionId: string, turnId: string, userMessageId: string | null): SessionTurn {
  return {
    turn_id: turnId,
    session_id: sessionId,
    run_id: null,
    user_message_id: userMessageId,
    status: "running",
    start_seq: 1,
    end_seq: null,
    started_at: now,
    updated_at: now,
    assistant_partial: null,
    thought_partial: null,
    metrics_json: null,
    tool_total: 0,
    tool_pending: 0,
    tool_running: 0,
    tool_completed: 0,
    tool_failed: 0,
  };
}

function makeSessionHead(
  session: Session,
  messages: Message[] = [],
  turns: SessionTurn[] = [],
): SessionHeadSnapshot {
  return {
    session,
    turns,
    tool_summaries: [],
    events: [],
    messages,
    last_event_seq: messages.length,
    state_rev: messages.length,
    activity: { is_working: false },
    has_more_turns: false,
    history_cursor: null,
    has_more_history: false,
  };
}

function makeOptimisticTask(
  taskId = "task-1",
  sessionId = "session-1",
  localStatus: OptimisticTaskSummary["localStatus"] = "starting",
): OptimisticTaskSummary {
  const session = makeSession(sessionId, taskId);
  const base = makeTaskSummary({
    taskId,
    primarySessionId: sessionId,
    sessions: [makeSessionSummary(session)],
  });
  return {
    ...base,
    providerIds: ["codex"],
    localStatus,
    localPrompt: "Write docs",
    localMessageId: "message-1",
    localError: null,
  };
}

function requireValue(value: HookValue | null): HookValue {
  if (!value) {
    throw new Error("hook value not ready");
  }
  return value;
}

function Harness({
  activeTaskId,
  activeTaskIdFromTab,
  tasksById,
  onChange,
}: {
  activeTaskId: string | null;
  activeTaskIdFromTab: string | null;
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  onChange: (value: HookValue) => void;
}) {
  const value = useWorkbenchOptimisticTasks({ activeTaskId, activeTaskIdFromTab, tasksById });
  useEffect(() => {
    onChange(value);
  }, [onChange, value]);
  return null;
}

afterEach(() => {
  cleanup();
});

describe("useWorkbenchOptimisticTasks", () => {
  it("bridges the active task from optimisticStartingTaskRef before optimistic state commits", () => {
    let current: HookValue | null = null;
    const optimistic = makeOptimisticTask();
    const { rerender } = render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      requireValue(current).optimisticStartingTaskRef.current = optimistic;
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    expect(requireValue(current).activeTaskSummary).toMatchObject({
      id: "task-1",
      localStatus: "starting",
    });
  });

  it("clears optimisticStartingTaskRef once optimistic state contains the task", async () => {
    let current: HookValue | null = null;
    const optimistic = makeOptimisticTask();
    const { rerender } = render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      requireValue(current).optimisticStartingTaskRef.current = optimistic;
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await act(async () => {
      requireValue(current).setOptimisticTasks([optimistic]);
    });

    await waitFor(() => {
      expect(requireValue(current).optimisticStartingTaskRef.current).toBeNull();
    });
    expect(requireValue(current).activeTaskSummary).toMatchObject({
      id: "task-1",
      localStatus: "starting",
    });
  });

  it("clears optimisticStartingTaskRef when focus moves to another task", async () => {
    let current: HookValue | null = null;
    const optimistic = makeOptimisticTask();
    const { rerender } = render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      requireValue(current).optimisticStartingTaskRef.current = optimistic;
    });

    rerender(
      <Harness
        activeTaskId="task-2"
        activeTaskIdFromTab="task-2"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await waitFor(() => {
      expect(requireValue(current).optimisticStartingTaskRef.current).toBeNull();
    });
  });

  it("keeps a synced optimistic summary active until the server task publishes a session", async () => {
    let current: HookValue | null = null;
    const optimistic = {
      ...makeOptimisticTask("task-1", "session-1", "synced"),
      localPrompt: "hello",
    };
    const sessionlessServerTask = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: "",
      sessions: [],
    });

    render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": sessionlessServerTask }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      current?.setOptimisticTasks([optimistic]);
    });

    await waitFor(() => {
      expect(current?.activeTaskSummary).toEqual(optimistic);
    });
  });

  it("keeps a synced optimistic session id marked optimistic until the server publishes the first user header anchor", async () => {
    let current: HookValue | null = null;
    const optimisticSession = makeSession("session-1", "task-1");
    const optimistic = {
      ...makeOptimisticTask("task-1", optimisticSession.id, "synced"),
      localPrompt: "hello",
    };
    const sessionlessServerTask = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: "",
      sessions: [],
    });

    const { rerender } = render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": sessionlessServerTask }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      current?.setOptimisticTasks([optimistic]);
    });

    await waitFor(() => {
      expect(current?.optimisticSessionIdSet.has("session-1")).toBe(true);
    });

    const assistantOnlyServerTask = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: optimisticSession.id,
      sessions: [makeSessionSummary(optimisticSession)],
      primarySessionHead: makeSessionHead(optimisticSession, [
        {
          ...makeMessage(optimisticSession.id, "task-1", "done"),
          id: "assistant-1",
          role: "assistant",
        },
      ]),
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": assistantOnlyServerTask }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await waitFor(() => {
      expect(current?.optimisticSessionIdSet.has("session-1")).toBe(true);
    });

    const publishedServerTask = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: optimisticSession.id,
      sessions: [makeSessionSummary(optimisticSession)],
      primarySessionHead: makeSessionHead(optimisticSession, [
        {
          ...makeMessage(optimisticSession.id, "task-1", "hello"),
          id: "message-1",
        },
      ], [
        makeTurn(optimisticSession.id, `turn-${optimisticSession.id}`, "message-1"),
      ]),
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": publishedServerTask }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await waitFor(() => {
      expect(current?.optimisticSessionIdSet.has("session-1")).toBe(false);
    });
  });

  it("keeps failed optimistic session ids marked optimistic so snapshot loading stays suppressed", async () => {
    let current: HookValue | null = null;
    const failed = {
      ...makeOptimisticTask("task-1", "session-1", "failed"),
      localError: "model_id must be a concrete model id",
    };

    render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{}}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      current?.setOptimisticTasks([failed]);
    });

    await waitFor(() => {
      expect(current?.optimisticSessionIdSet.has("session-1")).toBe(true);
      expect(current?.optimisticFailureBySessionId["session-1"]).toEqual({
        prompt: "Write docs",
        error: "model_id must be a concrete model id",
      });
    });
  });

  it("keeps a synced optimistic task active until the server publishes a renderable first user header", async () => {
    let current: HookValue | null = null;
    const session = makeSession("session-1", "task-1");
    const optimistic = {
      ...makeOptimisticTask("task-1", session.id, "synced"),
      localPrompt: "hello",
    };
    const serverTaskWithEmptyHead = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: session.id,
      sessions: [makeSessionSummary(session)],
      primarySessionHead: makeSessionHead(session),
    });

    const { rerender } = render(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": serverTaskWithEmptyHead }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    act(() => {
      current?.setOptimisticTasks([optimistic]);
    });

    await waitFor(() => {
      expect(current?.activeTaskSummary).toEqual(optimistic);
      expect(current?.optimisticSessionIdSet.has(session.id)).toBe(true);
    });

    const serverTaskWithAssistantOnlyHead = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: session.id,
      sessions: [makeSessionSummary(session)],
      primarySessionHead: makeSessionHead(session, [
        {
          ...makeMessage(session.id, "task-1", "assistant"),
          id: "assistant-1",
          role: "assistant",
        },
      ]),
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": serverTaskWithAssistantOnlyHead }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await waitFor(() => {
      expect(current?.activeTaskSummary).toEqual(optimistic);
      expect(current?.optimisticSessionIdSet.has(session.id)).toBe(true);
    });

    const serverTaskWithMessageHead = makeTaskSummary({
      taskId: "task-1",
      primarySessionId: session.id,
      sessions: [makeSessionSummary(session)],
      primarySessionHead: makeSessionHead(session, [
        {
          ...makeMessage(session.id, "task-1", "hello"),
          id: "message-1",
        },
      ], [
        makeTurn(session.id, `turn-${session.id}`, "message-1"),
      ]),
    });

    rerender(
      <Harness
        activeTaskId="task-1"
        activeTaskIdFromTab="task-1"
        tasksById={{ "task-1": serverTaskWithMessageHead }}
        onChange={(value) => {
          current = value;
        }}
      />,
    );

    await waitFor(() => {
      expect(current?.activeTaskSummary).toEqual(serverTaskWithMessageHead);
      expect(current?.optimisticSessionIdSet.has(session.id)).toBe(false);
    });
  });
});
