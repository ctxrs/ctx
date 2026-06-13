// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import {
  interruptSession,
  postMessage,
  type Message,
  type Session,
} from "../api/client";
import {
  noteInterruptClicked,
  noteInterruptPendingVisible,
} from "../state/foregroundFreshnessTelemetry";
import { useSessionComposerQueueController } from "./useSessionComposerQueueController";

vi.mock("../state/foregroundFreshnessTelemetry", () => ({
  clearInterruptPendingMetric: vi.fn(),
  noteInterruptClicked: vi.fn(),
  noteInterruptPendingVisible: vi.fn(),
}));

vi.mock("../api/client", async () => {
  const actual = await vi.importActual<typeof import("../api/client")>("../api/client");
  return {
    ...actual,
    deleteMessage: vi.fn(),
    interruptSession: vi.fn(),
    postMessage: vi.fn(),
  };
});

const session: Session = {
  id: "session-1",
  task_id: "task-1",
  workspace_id: "workspace-1",
  worktree_id: "worktree-1",
  provider_id: "codex",
  model_id: "gpt-5.4",
  title: "Session 1",
  agent_role: "assistant",
  status: "active",
  execution_environment: "sandbox",
  created_at: "2026-04-01T00:00:00.000Z",
  updated_at: "2026-04-01T00:00:00.000Z",
};

const queuedMessage: Message = {
  id: "queued-1",
  session_id: "session-1",
  task_id: "task-1",
  turn_id: "turn-queued-1",
  turn_sequence: 1,
  order_seq: 1,
  role: "user",
  content: "queued",
  attachments: [],
  delivery: "queued",
  created_at: "2026-04-01T00:00:00.000Z",
};

const supervisor = {
  addOptimisticQueueRemovalId: vi.fn(),
  removeOptimisticQueueRemovalId: vi.fn(),
  removeOptimisticQueuedMessage: vi.fn(),
  removeOptimisticThreadMessage: vi.fn(),
  upsertOptimisticQueuedMessage: vi.fn(),
  upsertOptimisticThreadMessage: vi.fn(),
};

const postMessageMock = vi.mocked(postMessage);
const interruptSessionMock = vi.mocked(interruptSession);
const noteInterruptClickedMock = vi.mocked(noteInterruptClicked);
const noteInterruptPendingVisibleMock = vi.mocked(noteInterruptPendingVisible);

function createHookProps(overrides?: Partial<Parameters<typeof useSessionComposerQueueController>[0]>) {
  return {
    sessionId: "session-1",
    session,
    supervisor,
    input: "hello",
    setInput: vi.fn(),
    draftAttachments: [],
    setDraftAttachments: vi.fn(),
    optimisticThreadMessages: [],
    optimisticQueuedMessages: [queuedMessage],
    messageCount: 0,
    turnCount: 0,
    hasActiveTurn: false,
    queuedMessagesEnabled: true,
    currentModelId: "gpt-5.4/xhigh",
    interruptSessionId: "",
    resolveSendText: vi.fn(async () => "hello"),
    setAtBottom: vi.fn(),
    onDraftPersistNow: null,
    ...overrides,
  };
}

describe("useSessionComposerQueueController", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    interruptSessionMock.mockResolvedValue(undefined);
    postMessageMock.mockImplementation(async (sessionId, text, delivery, attachments, optimistic) => ({
      id: optimistic?.id ?? "posted-1",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: optimistic?.turn_id ?? "turn-1",
      turn_sequence: 1,
      order_seq: 1,
      role: "user",
      content: text,
      attachments: attachments ?? [],
      delivery: delivery ?? "immediate",
      created_at: "2026-04-01T00:00:00.000Z",
    }));
  });

  it("tracks optimistic queued ids and exposes no interrupt action when the session is idle", () => {
    const { result } = renderHook(() =>
      useSessionComposerQueueController(
        createHookProps({
          input: "",
          resolveSendText: vi.fn(async () => ""),
        }),
      ),
    );

    expect(result.current.pendingQueueMessageIdSet).toEqual(new Set(["queued-1"]));
    expect(result.current.interruptPending).toBe(false);
    expect(result.current.onInterruptSession).toBeNull();
  });

  it("records interrupt pending telemetry when the stopping state is committed", async () => {
    const { result } = renderHook(() =>
      useSessionComposerQueueController(
        createHookProps({
          hasActiveTurn: true,
          interruptSessionId: "session-1",
        }),
      ),
    );

    await act(async () => {
      await result.current.onInterruptSession?.();
    });

    expect(noteInterruptClickedMock).toHaveBeenCalledWith("session-1", "thread_header");
    expect(noteInterruptPendingVisibleMock).toHaveBeenCalledWith("session-1");
    expect(result.current.interruptPending).toBe(true);
    expect(result.current.onInterruptSession).not.toBeNull();
  });

  it("records interrupt pending telemetry with the clicked session when the active target clears during dispatch", async () => {
    let hookProps = createHookProps({
      hasActiveTurn: true,
      interruptSessionId: "session-1",
    });
    const { result, rerender } = renderHook((props) => useSessionComposerQueueController(props), {
      initialProps: hookProps,
    });
    interruptSessionMock.mockImplementationOnce(async () => {
      hookProps = {
        ...hookProps,
        hasActiveTurn: false,
        interruptSessionId: "",
      };
      rerender(hookProps);
    });

    await act(async () => {
      await result.current.onInterruptSession?.();
    });

    expect(noteInterruptClickedMock).toHaveBeenCalledWith("session-1", "thread_header");
    expect(noteInterruptPendingVisibleMock).toHaveBeenCalledWith("session-1");
  });

  it("notifies when a valid composer send starts", async () => {
    const onSendStarted = vi.fn();
    const { result } = renderHook(() =>
      useSessionComposerQueueController(
        createHookProps({
          onSendStarted,
          optimisticQueuedMessages: [],
        }),
      ),
    );

    await act(async () => {
      await result.current.sendNow();
    });

    expect(onSendStarted).toHaveBeenCalledTimes(1);
  });

  it("carries the first optimistic thread message across an immediate session-id handoff", async () => {
    const initialProps = createHookProps({
      sessionId: "session-temp",
      optimisticQueuedMessages: [],
    });
    const { result, rerender } = renderHook((props) => useSessionComposerQueueController(props), {
      initialProps,
    });

    await act(async () => {
      await result.current.sendNow();
    });

    const initialOptimisticCall = supervisor.upsertOptimisticThreadMessage.mock.calls[0];
    expect(initialOptimisticCall?.[0]).toBe("session-temp");
    const optimisticMessage = initialOptimisticCall?.[1] as Message;
    expect(optimisticMessage).toBeTruthy();

    supervisor.removeOptimisticThreadMessage.mockClear();
    supervisor.upsertOptimisticThreadMessage.mockClear();

    rerender({
      ...initialProps,
      sessionId: "session-real",
      optimisticThreadMessages: [],
    });

    expect(supervisor.removeOptimisticThreadMessage).toHaveBeenCalledWith(
      "session-temp",
      String(optimisticMessage.id),
    );
    expect(supervisor.upsertOptimisticThreadMessage).toHaveBeenCalledWith(
      "session-real",
      expect.objectContaining({
        id: optimisticMessage.id,
        session_id: "session-real",
      }),
    );
  });

  it("does not carry the optimistic handoff once the next session has authoritative content", async () => {
    const initialProps = createHookProps({
      sessionId: "session-temp",
      optimisticQueuedMessages: [],
    });
    const { result, rerender } = renderHook((props) => useSessionComposerQueueController(props), {
      initialProps,
    });

    await act(async () => {
      await result.current.sendNow();
    });

    const optimisticMessage = supervisor.upsertOptimisticThreadMessage.mock.calls[0]?.[1] as Message;
    rerender({
      ...initialProps,
      optimisticThreadMessages: [optimisticMessage],
    });
    supervisor.removeOptimisticThreadMessage.mockClear();
    supervisor.upsertOptimisticThreadMessage.mockClear();

    rerender({
      ...initialProps,
      sessionId: "session-real",
      optimisticThreadMessages: [],
      messageCount: 1,
    });

    expect(supervisor.removeOptimisticThreadMessage).not.toHaveBeenCalled();
    expect(supervisor.upsertOptimisticThreadMessage).not.toHaveBeenCalled();
  });
});
