import React, { useEffect, useState } from "react";
import { act, render } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { Message, SessionEvent, SessionTurn } from "../api/client";
import { SessionView } from "./SessionPage";
import { buildSessionThreadProjectionFromSnapshot } from "../state/sessionThreadProjection/applySnapshot";

const sessionEntries = vi.hoisted(() => ({ map: {} as Record<string, unknown> }));
const focusTaskSpy = vi.hoisted(() => vi.fn());
const updateSettingsSpy = vi.hoisted(() => vi.fn());
const workbenchKeySpies = vi.hoisted(() => ({
  deriveTurnsKey: vi.fn(),
  deriveMessagesKey: vi.fn(),
}));
const controllerParamSpies = vi.hoisted(() => ({
  useWorkbenchThreadViewModelController: vi.fn(),
}));

vi.mock("./SessionPage.workbenchViewModel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./SessionPage.workbenchViewModel")>();
  return {
    ...actual,
    deriveTurnsKey: vi.fn((...args: Parameters<typeof actual.deriveTurnsKey>) => {
      workbenchKeySpies.deriveTurnsKey(...args);
      return actual.deriveTurnsKey(...args);
    }),
    deriveMessagesKey: vi.fn((...args: Parameters<typeof actual.deriveMessagesKey>) => {
      workbenchKeySpies.deriveMessagesKey(...args);
      return actual.deriveMessagesKey(...args);
    }),
  };
});

vi.mock("./useWorkbenchThreadViewModelController", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./useWorkbenchThreadViewModelController")>();
  return {
    ...actual,
    useWorkbenchThreadViewModelController: vi.fn(
      (...args: Parameters<typeof actual.useWorkbenchThreadViewModelController>) => {
        controllerParamSpies.useWorkbenchThreadViewModelController(...args);
        return actual.useWorkbenchThreadViewModelController(...args);
      },
    ),
  };
});

vi.mock("../api/client", () => ({
  deleteMessage: vi.fn(async () => ({})),
  postMessage: vi.fn(async () => ({})),
  setSessionModel: vi.fn(async () => ({})),
  authenticateSession: vi.fn(async () => ({})),
  getSettings: vi.fn(async () => ({ dictation: { enabled: false } })),
  idToString: (id: string | null | undefined) => {
    if (id === null || id === undefined) return "";
    if (typeof id !== "string") {
      throw new Error("Expected id to be a string");
    }
    return id;
  },
  interruptSession: vi.fn(async () => ({})),
  submitAskUserQuestion: vi.fn(async () => ({})),
  uploadBlob: vi.fn(async () => ({ blob_id: "blob-1" })),
}));

vi.mock("../state/sessionSupervisor", () => ({
  useSessionSupervisor: () => ({
    refreshQueue: vi.fn(async () => {}),
    refreshSession: vi.fn(async () => {}),
    loadMoreTurns: vi.fn(),
    loadTurnTools: vi.fn(),
    setSession: vi.fn(),
  }),
  useSessionEntry: (id: string) => sessionEntries.map[id] ?? null,
  useOpenSession: () => {},
}));

vi.mock("../state/settingsStore", () => ({
  useSettingsStore: () => ({ update: updateSettingsSpy }),
  useSettingsSnapshot: () => ({ settings: null }),
}));

vi.mock("../state/uiStateStore", () => ({
  loadSessionViewPrefsV1: vi.fn(async () => null),
  saveSessionViewPrefsV1: vi.fn(async () => {}),
}));

vi.mock("../components/AskUserQuestionCard", () => ({
  AskUserQuestionCard: () => null,
}));

vi.mock("../components/WorkbenchComposer", () => ({
  WorkbenchComposer: () => null,
}));

vi.mock("../utils/dragDropScopes", () => ({
  registerDropScope: () => () => {},
}));

vi.mock("../workbench/store", () => ({
  useWorkbenchStore: () => ({
    focusTask: focusTaskSpy,
  }),
}));

const workspaceId = "ws-1";
const taskIdA = "task-1";
const taskIdB = "task-2";
const sessionIdA = "session-1";
const sessionIdB = "session-2";

const baseIso = "2025-01-01T00:00:00.000Z";
const baseMs = Date.parse(baseIso);
type ThreadProjectionSource = Parameters<typeof buildSessionThreadProjectionFromSnapshot>[0];

const buildSessionEntry = (sessionId: string, taskId: string, startedAtMs: number) => {
  const turnId = `${sessionId}-turn-1`;
  const userMessageId = `${sessionId}-user-1`;
  const turns: SessionTurn[] = [
    {
      turn_id: turnId,
      session_id: sessionId,
      run_id: null,
      user_message_id: userMessageId,
      status: "running",
      start_seq: null,
      end_seq: null,
      started_at: new Date(startedAtMs).toISOString(),
      updated_at: new Date(startedAtMs).toISOString(),
      assistant_partial: "",
      thought_partial: "",
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    },
  ];
  const messages: Message[] = [
    {
      id: userMessageId,
      session_id: sessionId,
      task_id: taskId,
      role: "user",
      content: "hello",
      attachments: [],
      delivery: "immediate",
      created_at: baseIso,
      turn_id: turnId,
      order_seq: 1,
    },
  ];
  const entry: ThreadProjectionSource & Record<string, unknown> = {
    sessionId,
    session: {
      id: sessionId,
      task_id: taskId,
      provider_id: "codex",
      status: "active",
      created_at: baseIso,
    },
    turns,
    turnToolsByTurnId: {},
    turnToolsLoading: [],
    toolSummariesReady: true,
    hasMoreTurns: false,
    events: [] as SessionEvent[],
    messages,
    artifacts: [],
    artifactsLoading: false,
    subagentInvocations: [],
    subagentInvocationsLoading: false,
    stateLoaded: true,
    stateLoading: false,
    turnsRev: 0,
    messagesRev: 0,
    eventsRev: 0,
    projectionRev: 0,
    queue: [],
    loading: false,
    subscribed: true,
    updatedAtMs: 0,
  };
  return {
    ...entry,
    threadProjection: buildSessionThreadProjectionFromSnapshot(entry),
  };
};

const withThreadProjection = <T extends ThreadProjectionSource>(entry: T): T & {
  threadProjection: ReturnType<typeof buildSessionThreadProjectionFromSnapshot>;
} => ({
  ...entry,
  threadProjection: buildSessionThreadProjectionFromSnapshot(entry),
});

beforeAll(() => {
  const globalWithMocks = globalThis as typeof globalThis & {
    localStorage?: Storage;
    ResizeObserver?: typeof ResizeObserver;
    IntersectionObserver?: typeof IntersectionObserver;
  };
  if (typeof globalWithMocks.localStorage?.getItem !== "function") {
    const store = new Map<string, string>();
    globalWithMocks.localStorage = {
      getItem: (key: string) => (store.has(key) ? store.get(key) ?? null : null),
      setItem: (key: string, value: string) => {
        store.set(key, String(value));
      },
      removeItem: (key: string) => {
        store.delete(key);
      },
      clear: () => {
        store.clear();
      },
      key: (index: number) => Array.from(store.keys())[index] ?? null,
      get length() {
        return store.size;
      },
    };
  }
  if (!("ResizeObserver" in globalThis)) {
    class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    globalWithMocks.ResizeObserver = ResizeObserver;
  }
  if (!("IntersectionObserver" in globalThis)) {
    class IntersectionObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    globalWithMocks.IntersectionObserver =
      IntersectionObserver as unknown as typeof globalThis.IntersectionObserver;
  }
});

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(baseIso));
  sessionEntries.map = {
    [sessionIdA]: buildSessionEntry(sessionIdA, taskIdA, baseMs - 12_000),
    [sessionIdB]: buildSessionEntry(sessionIdB, taskIdB, baseMs - 4_000),
  };
});

afterEach(() => {
  vi.useRealTimers();
  focusTaskSpy.mockClear();
  updateSettingsSpy.mockClear();
  workbenchKeySpies.deriveTurnsKey.mockClear();
  workbenchKeySpies.deriveMessagesKey.mockClear();
  controllerParamSpies.useWorkbenchThreadViewModelController.mockClear();
});

describe("SessionPage timer stability", () => {
  it("keeps elapsed time aligned when mounting sessions at different times", async () => {
    const sharedStartMs = baseMs - 12_000;
    sessionEntries.map = {
      [sessionIdA]: buildSessionEntry(sessionIdA, taskIdA, sharedStartMs),
      [sessionIdB]: buildSessionEntry(sessionIdB, taskIdB, sharedStartMs),
    };

    const DualSessionHarness = ({
      firstId,
      secondId,
      delayMs,
    }: {
      firstId: string;
      secondId: string;
      delayMs: number;
    }) => {
      const [showSecond, setShowSecond] = useState(false);
      useEffect(() => {
        const timer = window.setTimeout(() => setShowSecond(true), delayMs);
        return () => window.clearTimeout(timer);
      }, [delayMs]);
      return (
        <>
          <div data-testid="session-a">
            <SessionView sessionId={firstId} isActive autoOpenSession={false} />
          </div>
          {showSecond && (
            <div data-testid="session-b">
              <SessionView sessionId={secondId} isActive autoOpenSession={false} />
            </div>
          )}
        </>
      );
    };

    const { queryByTestId } = render(
      <DualSessionHarness firstId={sessionIdA} secondId={sessionIdB} delayMs={500} />,
    );

    await act(async () => {
      vi.advanceTimersByTime(16);
    });
    let times = Array.from(document.querySelectorAll(".wb-turn-status-time"))
      .map((node) => node.textContent)
      .filter((value): value is string => typeof value === "string" && value.trim().length > 0);
    expect(times).toEqual(["12s"]);

    await act(async () => {
      vi.advanceTimersByTime(500);
    });
    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    expect(queryByTestId("session-b")).toBeTruthy();
    times = Array.from(document.querySelectorAll(".wb-turn-status-time"))
      .map((node) => node.textContent)
      .filter((value): value is string => typeof value === "string" && value.trim().length > 0);
    expect(times).toHaveLength(2);

    await act(async () => {
      vi.advanceTimersByTime(750);
    });
    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    times = Array.from(document.querySelectorAll(".wb-turn-status-time"))
      .map((node) => node.textContent)
      .filter((value): value is string => typeof value === "string" && value.trim().length > 0);
    expect(new Set(times)).toEqual(new Set(["13s"]));
  });

  it("does not rehash turns or messages when only events append", async () => {
    const { rerender } = render(
      <SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />,
    );

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const initialControllerArgs = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0];
    expect(initialControllerArgs).toBeTruthy();
    const initialEntry = sessionEntries.map[sessionIdA] as ReturnType<typeof buildSessionEntry>;
    const nextEntry = withThreadProjection({
      ...initialEntry,
      lastEventSeq: 1,
      eventsRev: 1,
      events: [
        {
          seq: 1,
          id: "event-1",
          session_id: sessionIdA,
          run_id: "run-1",
          turn_id: `${sessionIdA}-turn-1`,
          event_type: "notice",
          payload_json: { kind: "context.compacted", message: "Compacted." },
          created_at: new Date(baseMs + 1_000).toISOString(),
        },
      ] as SessionEvent[],
      updatedAtMs: 1,
    });
    sessionEntries.map[sessionIdA] = nextEntry;

    rerender(<SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />);

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const nextControllerArgs = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0];
    expect(nextControllerArgs?.turns).toBe(initialControllerArgs?.turns);
    expect(nextControllerArgs?.messages).toBe(initialControllerArgs?.messages);
    expect(nextEntry.threadProjection.turnsStamp).toBe(initialEntry.threadProjection.turnsStamp);
    expect(nextEntry.threadProjection.messagesStamp).toBe(initialEntry.threadProjection.messagesStamp);
  });

  it("keeps ask-user answers referentially stable across unrelated event appends", async () => {
    const { rerender } = render(
      <SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />,
    );

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const initialAskAnswers = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0]
      ?.askUserQuestionAnswers;
    expect(initialAskAnswers).toBeInstanceOf(Map);

    const initialEntry = sessionEntries.map[sessionIdA] as ReturnType<typeof buildSessionEntry>;
    sessionEntries.map[sessionIdA] = withThreadProjection({
      ...initialEntry,
      lastEventSeq: 1,
      eventsRev: 1,
      events: [
        {
          seq: 1,
          id: "event-ask-stable",
          session_id: sessionIdA,
          run_id: "run-1",
          turn_id: `${sessionIdA}-turn-1`,
          event_type: "notice",
          payload_json: { kind: "context.compacted", message: "Compacted." },
          created_at: new Date(baseMs + 1_000).toISOString(),
        },
      ] as SessionEvent[],
      updatedAtMs: 1,
    });

    rerender(<SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />);

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const nextAskAnswers = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0]
      ?.askUserQuestionAnswers;
    expect(nextAskAnswers).toBe(initialAskAnswers);
  });

  it("updates ask-user answers when an answer event appends", async () => {
    const { rerender } = render(
      <SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />,
    );

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const initialAskAnswers = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0]
      ?.askUserQuestionAnswers as Map<string, { outcome: string; answers: Record<string, string> }>;
    expect(initialAskAnswers.size).toBe(0);

    const initialEntry = sessionEntries.map[sessionIdA] as ReturnType<typeof buildSessionEntry>;
    sessionEntries.map[sessionIdA] = withThreadProjection({
      ...initialEntry,
      lastEventSeq: 1,
      eventsRev: 1,
      events: [
        {
          seq: 1,
          id: "event-ask-answer",
          session_id: sessionIdA,
          run_id: "run-1",
          turn_id: `${sessionIdA}-turn-1`,
          event_type: "notice",
          payload_json: {
            kind: "ask_user_question_answered",
            tool_call_id: "tool-call-1",
            outcome: "submitted",
            answers: {
              summary: "Ship it",
            },
          },
          created_at: new Date(baseMs + 1_000).toISOString(),
        },
      ] as SessionEvent[],
      updatedAtMs: 1,
    });

    rerender(<SessionView sessionId={sessionIdA} isActive autoOpenSession={false} />);

    await act(async () => {
      vi.advanceTimersByTime(16);
    });

    const nextAskAnswers = controllerParamSpies.useWorkbenchThreadViewModelController.mock.calls.at(-1)?.[0]
      ?.askUserQuestionAnswers as Map<string, { outcome: string; answers: Record<string, string> }>;
    expect(nextAskAnswers).not.toBe(initialAskAnswers);
    expect(nextAskAnswers.get("tool-call-1")).toEqual({
      outcome: "submitted",
      answers: { summary: "Ship it" },
    });
  });
});
