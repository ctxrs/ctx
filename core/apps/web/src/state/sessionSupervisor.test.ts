import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { waitForCondition } from "../testUtils/waitForCondition";

import type {
  Message,
  Session,
  SessionHead,
  SessionEvent,
  SessionHeadSnapshot,
  SessionTurn,
  WorkspaceActiveSnapshotEvent,
} from "../api/client";
import type { SessionReplicaPatch } from "./sessionReplicaProtocol";
import type { SessionSubscriptionCursor } from "./sessionSubscription";
import type { WorkspaceActiveSnapshotEventSource, WorkspaceActiveSnapshotState } from "./workspaceActiveSnapshotStore";
import {
  loadSessionHeadV1,
  loadSessionHistoryPageV1,
  loadTaskThoughtsV1,
  saveSessionHistoryPageV1,
  saveTaskThoughtsV1,
} from "./uiStateStore";

vi.mock("../api/client", () => {
  const idToString = (id: string | null | undefined): string => {
    if (id === null || id === undefined) return "";
    if (typeof id !== "string") {
      throw new Error("Expected id to be a string");
    }
    return id;
  };
  return {
    authToken: vi.fn(() => null),
    idToString,
    getDaemonConnection: vi.fn(() => ({
      baseUrl: "http://daemon.test",
      wsBaseUrl: "ws://daemon.test",
      authToken: null,
      runId: null,
      source: "test",
      targetScope: { kind: "browser", baseUrl: "http://daemon.test" },
    })),
    getDaemonClientConfig: vi.fn(() => ({
      baseUrl: "",
      wsBaseUrl: "",
      authToken: null,
      runId: null,
    })),
    subscribeDaemonConfig: vi.fn(() => () => {}),
    getProviderOptions: vi.fn(async () => undefined),
    getSessionHead: vi.fn(),
    getSessionSnapshot: vi.fn(),
    getSessionState: vi.fn(async () => ({ artifacts: [], git_status: null })),
    getSessionHistory: vi.fn(),
    listSessionSubagentInvocations: vi.fn(async () => []),
    listTurnTools: vi.fn(async () => []),
    recordClientCounterMetric: vi.fn(),
    recordClientGaugeMetric: vi.fn(),
    recordClientHistogramMetric: vi.fn(),
  };
});

vi.mock("./uiStateStore", () => ({
  clearSessionHeadV1: vi.fn(async () => {}),
  clearSessionHistoryPagesV1: vi.fn(async () => {}),
  loadSessionAcpMetaV1: vi.fn(async () => null),
  loadSessionHeadV1: vi.fn(async () => null),
  loadSessionHistoryPageV1: vi.fn(async () => null),
  loadTaskThoughtsV1: vi.fn(async () => null),
  saveTaskThoughtsV1: vi.fn(async () => {}),
  clearTaskThoughtsV1: vi.fn(async () => {}),
  saveSessionAcpMetaV1: vi.fn(async () => {}),
  saveSessionHeadV1: vi.fn(async () => {}),
  saveSessionHistoryPageV1: vi.fn(async () => {}),
}));

import {
  getSessionHistory,
  getSessionHead,
  getSessionSnapshot,
  getSessionState,
  listSessionSubagentInvocations,
} from "../api/client";

const mkSession = (sessionId: string): Session => ({
  id: sessionId,
  task_id: "task-1",
  workspace_id: "ws-1",
  worktree_id: "wt-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "New Task",
  agent_role: "assistant",
  status: "active",
});

const mkTurn = ({
  sessionId,
  turnId,
  status,
  startSeq,
  startedAt,
}: {
  sessionId: string;
  turnId: string;
  status: SessionTurn["status"];
  startSeq: number;
  startedAt?: string;
}): SessionTurn => {
  const resolvedStartedAt =
    startedAt ?? new Date(Date.UTC(2026, 2, 9, 0, 0, startSeq)).toISOString();
  return {
    turn_id: turnId,
    session_id: sessionId,
    run_id: null,
    user_message_id: `user-${turnId}`,
    status,
    start_seq: startSeq,
    end_seq: status === "completed" ? startSeq + 1 : null,
    started_at: resolvedStartedAt,
    updated_at: resolvedStartedAt,
    assistant_partial: "",
    thought_partial: "",
    metrics_json: null,
    tool_total: 0,
    tool_pending: 0,
    tool_running: 0,
    tool_completed: 0,
    tool_failed: 0,
  };
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

type TestInternalEntry = {
  session?: Session;
  activity?: { is_working?: boolean | null; last_turn_status?: string | null } | null;
  turnsHydrated: boolean;
  turns: SessionTurn[];
  turnsRev: number;
  freshness?: "bootstrap" | "authoritative" | "recovering" | "replica";
  messages: Message[];
  messagesRev: number;
  events: SessionEvent[];
  eventsRev: number;
  assistantStreamingByTurnId: Record<
    string,
    { content: string; providerMessageId: string | null; orderSeq: number | null }
  >;
  assistantStreamingRev: number;
  queue: Message[];
  hasMoreTurns: boolean;
  historyExtended: boolean;
  lastEventSeq?: number;
  projectionRev?: number;
  oldestTurnSeq?: number;
  stateRev?: number;
  loadState: "pending_hydration" | "live" | "recovering" | "fatal";
  support: {
    stateLoaded?: boolean;
    stateLoading?: boolean;
    stateRev?: number;
    stateAppliedRev?: number;
    artifacts?: Array<{ absolute_path?: string }>;
    subagentInvocationsLoaded?: boolean;
    subagentInvocationsLoading?: boolean;
    subagentInvocationsAppliedRev?: number;
  };
};

type SessionSupervisorInternals = {
  entries: Map<string, TestInternalEntry>;
  stateCacheBySessionId: Map<string, { state: { git_status: unknown }; stateRev?: number }>;
  ensureEntry: (sessionId: string) => TestInternalEntry;
  mergeEvents: (entry: TestInternalEntry, events: SessionEvent[]) => void;
  handleReplicaPatches: (patches: SessionReplicaPatch[]) => void;
};

const asSupervisorInternals = (value: unknown): SessionSupervisorInternals => value as SessionSupervisorInternals;

const getSessionHistoryMock = vi.mocked(getSessionHistory);
const getSessionHeadMock = vi.mocked(getSessionHead);
const getSessionSnapshotMock = vi.mocked(getSessionSnapshot);
const getSessionStateMock = vi.mocked(getSessionState);
const listSessionSubagentInvocationsMock = vi.mocked(listSessionSubagentInvocations);
const loadSessionHeadV1Mock = vi.mocked(loadSessionHeadV1);
const loadSessionHistoryPageV1Mock = vi.mocked(loadSessionHistoryPageV1);
const saveSessionHistoryPageV1Mock = vi.mocked(saveSessionHistoryPageV1);
const loadTaskThoughtsV1Mock = vi.mocked(loadTaskThoughtsV1);
const saveTaskThoughtsV1Mock = vi.mocked(saveTaskThoughtsV1);
const SUPPORT_RACE_WAIT_TIMEOUT_MS = 5_000;
const SUPPORT_RACE_TEST_TIMEOUT_MS = 30_000;
const SUPERVISOR_ASYNC_WAIT_TIMEOUT_MS = 5_000;

const flushSupportRace = async () => {
  await Promise.resolve();
  await new Promise((resolve) => setTimeout(resolve, 0));
};

const waitForSessionHeadCalls = async (count: number): Promise<void> => {
  await waitForCondition(
    () => getSessionHeadMock.mock.calls.length >= count,
    SUPERVISOR_ASYNC_WAIT_TIMEOUT_MS,
  );
};

const getSessionHeadCallsFor = (sessionId: string) =>
  getSessionHeadMock.mock.calls.filter(([calledSessionId]) => calledSessionId === sessionId);

beforeEach(() => {
  getSessionHeadMock.mockReset();
  getSessionHeadMock.mockImplementation(async () => {
    throw new Error("getSessionHead must be mocked per test");
  });
  getSessionSnapshotMock.mockReset();
  getSessionSnapshotMock.mockImplementation(async () => {
    throw new Error("getSessionSnapshot must be mocked per test");
  });
  getSessionStateMock.mockReset();
  getSessionStateMock.mockResolvedValue({ artifacts: [], git_status: null });
  getSessionHistoryMock.mockReset();
  listSessionSubagentInvocationsMock.mockReset();
  listSessionSubagentInvocationsMock.mockResolvedValue([]);
  loadSessionHeadV1Mock.mockReset();
  loadSessionHeadV1Mock.mockResolvedValue(null);
  loadSessionHistoryPageV1Mock.mockReset();
  loadSessionHistoryPageV1Mock.mockResolvedValue(null);
  saveSessionHistoryPageV1Mock.mockReset();
  saveSessionHistoryPageV1Mock.mockResolvedValue();
  loadTaskThoughtsV1Mock.mockReset();
  loadTaskThoughtsV1Mock.mockResolvedValue(null);
  saveTaskThoughtsV1Mock.mockReset();
  saveTaskThoughtsV1Mock.mockResolvedValue();
});

const mkWorkspaceSnapshotState = (): WorkspaceActiveSnapshotState => ({
  workspaceId: "ws-1",
  initialized: true,
  liveSnapshotApplied: true,
  connection: "connected" as const,
  tasksById: {},
  activeIds: [],
  archivedIds: [],
  totalActive: 0,
  totalArchived: 0,
  archivedRev: 0,
  fetchState: { active: "idle" as const, archived: "idle" as const },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: false,
});

const mkWorkspaceTaskSummary = ({
  taskId,
  primarySessionId,
  sessionIds,
}: {
  taskId: string;
  primarySessionId: string;
  sessionIds: string[];
}) => ({
  id: taskId,
  task: {
    id: taskId,
    workspace_id: "ws-1",
    title: `Task ${taskId}`,
    status: "running",
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    last_activity_at: new Date().toISOString(),
    archived_at: null,
    primary_session_id: primarySessionId,
  },
  sessions: sessionIds.map((sessionId) => ({
    session: mkSession(sessionId),
    last_message_at: null,
    last_message_preview: null,
    last_event_seq: null,
    state_rev: undefined,
    activity: { is_working: false, last_turn_status: null },
    unread: false,
  })),
  primarySessionId,
  primarySessionHead: null,
  sort_at: new Date().toISOString(),
  sortAtMs: Date.now(),
});

const attachWorkspaceStore = (
  sup: {
    setSubscribedSessionIdsSink: (sink: ((sessions: SessionSubscriptionCursor[]) => void) | null) => void;
    setWorkspaceSnapshotState: (state: WorkspaceActiveSnapshotState | null) => void;
    setWorkspaceSessionHeads: (heads: Record<string, SessionHeadSnapshot>) => void;
    handleWorkspaceEvent: (evt: WorkspaceActiveSnapshotEvent) => void;
  },
  store: WorkspaceActiveSnapshotEventSource & { getSessionHeadsSnapshot?: () => Record<string, SessionHeadSnapshot> },
) => {
  const sync = () => {
    sup.setWorkspaceSnapshotState(store.getSnapshot());
    sup.setWorkspaceSessionHeads(store.getSessionHeadsSnapshot?.() ?? {});
  };
  sup.setSubscribedSessionIdsSink((sessions) => store.setSubscribedSessions?.(sessions));
  sync();
  const unsubState = store.subscribe(sync);
  const unsubEvents = store.subscribeEvents((evt) => sup.handleWorkspaceEvent(evt));
  return () => {
    unsubEvents();
    unsubState();
    sup.setSubscribedSessionIdsSink(null);
    sup.setWorkspaceSessionHeads({});
    sup.setWorkspaceSnapshotState(null);
  };
};

describe("SessionSupervisor", () => {
  beforeEach(() => {
    vi.useRealTimers();
  });

  afterEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it("hydrates session head and derives queue from queued messages", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-1";
    const headMessages: Message[] = [
      {
        id: "m1",
        session_id: sessionId,
        task_id: "task-1",
        role: "user",
        content: "queued",
        delivery: "queued",
        created_at: new Date().toISOString(),
      },
    ];

    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: headMessages,
      last_event_seq: 1,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    const messagesHydrated = new Promise<void>((resolve, reject) => {
      let unsubscribe: (() => void) | null = null;
      const timeout = setTimeout(() => {
        unsubscribe?.();
        reject(new Error("Timed out waiting for queued message hydration"));
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);
      const maybeResolve = () => {
        if (sup.getSnapshot().sessions[sessionId]?.messages.length !== 1) return;
        clearTimeout(timeout);
        unsubscribe?.();
        resolve();
      };
      unsubscribe = sup.subscribe(maybeResolve);
      maybeResolve();
    });
    sup.openSession(sessionId, { mode: "archived" });

    await messagesHydrated;

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.length).toBe(1);
    expect(entry?.queue.length).toBe(1);
  }, SUPPORT_RACE_TEST_TIMEOUT_MS);

  it("bumps messagesRev when streamed queue events flip delivery in place", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-queue-rev";
    const createdAt = new Date().toISOString();
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.messages = [
      {
        id: "message-1",
        session_id: sessionId,
        task_id: "task-1",
        role: "user",
        content: "queue me",
        attachments: [],
        delivery: "immediate",
        created_at: createdAt,
        turn_id: "turn-1",
        order_seq: 1,
      } as Message,
    ];
    entry.queue = [];
    const beforeMessagesRev = entry.messagesRev;

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          messages: [
            {
              ...entry.messages[0],
              delivery: "queued",
            } as Message,
          ],
          messagesRev: beforeMessagesRev + 1,
          events: [
            {
              seq: 1,
              id: "event-queue-added",
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "message_queue_added",
              payload_json: { message_id: "message-1" },
              created_at: createdAt,
            },
          ],
        },
      },
    ]);

    expect(entry.messages).toHaveLength(1);
    expect(entry.messages[0]?.delivery).toBe("queued");
    expect(entry.queue.map((message) => String(message.id))).toEqual(["message-1"]);
    expect(entry.messagesRev).toBeGreaterThan(beforeMessagesRev);
  });

  it("bumps turnsRev when streamed turn events mutate an existing turn in place", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-turn-rev";
    const createdAt = new Date().toISOString();
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.turns = [
      {
        turn_id: "turn-1",
        session_id: sessionId,
        run_id: "run-1",
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: createdAt,
        updated_at: createdAt,
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      } as SessionTurn,
    ];
    const beforeTurnsRev = entry.turnsRev;

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          turns: [
            {
              ...entry.turns[0],
              status: "completed",
              end_seq: 2,
              updated_at: createdAt,
            } as SessionTurn,
          ],
          turnsRev: beforeTurnsRev + 1,
          events: [
            {
              seq: 2,
              id: "event-turn-done",
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "done",
              payload_json: {},
              created_at: createdAt,
            },
          ],
        },
      },
    ]);

    expect(entry.turns).toHaveLength(1);
    expect(entry.turns[0]?.status).toBe("completed");
    expect(entry.turnsRev).toBeGreaterThan(beforeTurnsRev);
  });

  it("preserves assistant chunk order_seq in supervisor streaming state", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-assistant-chunk-order";
    const turnId = "turn-1";
    const createdAt = new Date().toISOString();
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId, status: "running", startSeq: 1, startedAt: createdAt })];

    internals.mergeEvents(entry, [
      {
        seq: 2,
        id: "event-assistant-chunk",
        session_id: sessionId,
        run_id: "run-1",
        turn_id: turnId,
        event_type: "assistant_chunk",
        payload_json: {
          content_fragment: "done: hello",
          message_id: "provider-message-1",
          order_seq: 2,
        },
        created_at: createdAt,
      },
    ]);

    expect(entry.assistantStreamingByTurnId[turnId]).toMatchObject({
      content: "done: hello",
      providerMessageId: "provider-message-1",
      orderSeq: 2,
    });
  });

  it("preserves assistant complete order_seq in supervisor streaming state", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-assistant-complete-order";
    const turnId = "turn-1";
    const createdAt = new Date().toISOString();
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId, status: "running", startSeq: 1, startedAt: createdAt })];

    internals.mergeEvents(entry, [
      {
        seq: 2,
        id: "event-assistant-complete",
        session_id: sessionId,
        run_id: "run-1",
        turn_id: turnId,
        event_type: "assistant_complete",
        payload_json: {
          full_content: "done: hello",
          message_id: "provider-message-1",
          orderSeq: 2,
        },
        created_at: createdAt,
      },
    ]);

    expect(entry.assistantStreamingByTurnId[turnId]).toMatchObject({
      content: "done: hello",
      providerMessageId: "provider-message-1",
      orderSeq: 2,
    });
  });

  it("re-emits subscribed session ids when active-task membership changes under an open session", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-reemit-active-membership";
    const sink = vi.fn();
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 1,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sink.mockClear();

    sup.openSession(sessionId, { mode: "active" });

    expect(sink).toHaveBeenCalledWith([{ sessionId, intent: "replay", replay: { kind: "auto" } }]);
    sink.mockClear();

    sup.setActiveTaskSessionIds([sessionId]);

    expect(sink).toHaveBeenCalledWith([{ sessionId, intent: "replay", replay: { kind: "auto" } }]);
  });

  it("auto-subscribes workspace active-primary sessions even when they are not open or warm", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sink = vi.fn();
    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sink.mockClear();

    sup.setWorkspaceSnapshotState({
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-primary"],
      tasksById: {
        "task-primary": mkWorkspaceTaskSummary({
          taskId: "task-primary",
          primarySessionId: "session-primary",
          sessionIds: ["session-primary"],
        }),
      },
      totalActive: 1,
    });

    expect(sink).toHaveBeenCalledWith([
      { sessionId: "session-primary", intent: "head", replay: { kind: "auto" } },
    ]);
  });

  it("re-emits subscribed session ids when workspace active-primary membership reprioritizes an existing plan", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sink = vi.fn();
    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sink.mockClear();

    sup.setWarmSessionIds(["session-1", "session-2"]);
    expect(sink).toHaveBeenCalledWith([
      { sessionId: "session-1", intent: "head", replay: { kind: "auto" } },
      { sessionId: "session-2", intent: "head", replay: { kind: "auto" } },
    ]);
    sink.mockClear();

    const stateWithPrimaryOne: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-1"],
      tasksById: {
        "task-1": mkWorkspaceTaskSummary({
          taskId: "task-1",
          primarySessionId: "session-1",
          sessionIds: ["session-1", "session-2"],
        }),
      },
      totalActive: 1,
    };
    sup.setWorkspaceSnapshotState(stateWithPrimaryOne);
    expect(sink).toHaveBeenCalledWith([
      { sessionId: "session-1", intent: "head", replay: { kind: "auto" } },
      { sessionId: "session-2", intent: "head", replay: { kind: "auto" } },
    ]);
    sink.mockClear();

    const stateWithPrimaryTwo: WorkspaceActiveSnapshotState = {
      ...stateWithPrimaryOne,
      tasksById: {
        "task-1": mkWorkspaceTaskSummary({
          taskId: "task-1",
          primarySessionId: "session-2",
          sessionIds: ["session-1", "session-2"],
        }),
      },
    };
    sup.setWorkspaceSnapshotState(stateWithPrimaryTwo);

    expect(sink).toHaveBeenCalledWith([
      { sessionId: "session-2", intent: "head", replay: { kind: "auto" } },
      { sessionId: "session-1", intent: "head", replay: { kind: "auto" } },
    ]);
  });

  it("drops replay cursors for subscribed sessions that enter recovering on session_gap", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-gap-cursor";
    const sink = vi.fn();
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-gap-cursor"],
      tasksById: {
        "task-gap-cursor": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-gap-cursor",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: head,
        },
      },
      totalActive: 1,
    };

    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() =>
      sink.mock.calls.some(
        (call) =>
          call[0]?.[0]?.sessionId === sessionId &&
          call[0]?.[0]?.replay?.kind === "resume" &&
          call[0]?.[0]?.replay?.afterSeq === 7,
      ),
    );
    sink.mockClear();

    sup.handleWorkspaceEvent({
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 8,
      session_id: sessionId,
      after_seq: 7,
    });

    expect(sink).toHaveBeenCalledWith([{ sessionId, intent: "replay", replay: { kind: "reset" } }]);
    expect(sup.getSnapshot().sessions[sessionId]?.freshness).toBe("recovering");
  });

  it("keeps subscription cursors stable when session_gap declares a paired seed", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-gap-seed-cursor";
    const sink = vi.fn();
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-gap-seed-cursor"],
      tasksById: {
        "task-gap-seed-cursor": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-gap-seed-cursor",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: head,
        },
      },
      totalActive: 1,
    };

    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() =>
      sink.mock.calls.some(
        (call) =>
          call[0]?.[0]?.sessionId === sessionId &&
          call[0]?.[0]?.replay?.kind === "resume" &&
          call[0]?.[0]?.replay?.afterSeq === 7,
      ),
    );
    sink.mockClear();

    sup.handleWorkspaceEvent({
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 8,
      session_id: sessionId,
      after_seq: 7,
      reason: "replay_limit_exceeded",
      seed_follows: true,
    });

    expect(sink).not.toHaveBeenCalled();
    expect(sup.getSnapshot().sessions[sessionId]?.freshness).toBe("recovering");

    const seedHead: SessionHeadSnapshot = {
      ...head,
      last_event_seq: 8,
      state_rev: 8,
    };
    sup.setWorkspaceSessionHeads({ [sessionId]: seedHead });

    const replayCursors = sink.mock.calls
      .map((call) => call[0]?.[0])
      .filter((cursor) => cursor?.sessionId === sessionId);
    expect(replayCursors.length).toBeGreaterThan(0);
    expect(replayCursors.some((cursor) => cursor?.replay?.kind === "reset")).toBe(false);
    expect(replayCursors.at(-1)?.replay).toEqual({ kind: "auto" });
  });

  it("resets subscription cursors for unseeded session_gap recovery", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-gap-reset-cursor";
    const sink = vi.fn();
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-gap-reset-cursor"],
      tasksById: {
        "task-gap-reset-cursor": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-gap-reset-cursor",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: head,
        },
      },
      totalActive: 1,
    };

    const sup = new SessionSupervisor();
    sup.setSubscribedSessionIdsSink(sink);
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() =>
      sink.mock.calls.some(
        (call) =>
          call[0]?.[0]?.sessionId === sessionId &&
          call[0]?.[0]?.replay?.kind === "resume" &&
          call[0]?.[0]?.replay?.afterSeq === 7,
      ),
    );
    sink.mockClear();

    sup.handleWorkspaceEvent({
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 8,
      session_id: sessionId,
      after_seq: 7,
      reason: "stream_seq_gap",
    });

    expect(sink).toHaveBeenCalledTimes(1);
    expect(sink.mock.calls[0]?.[0]?.[0]).toMatchObject({
      sessionId,
      replay: { kind: "reset" },
    });
  });

  it("hydrates protocol-derived slash command metadata from archived init events", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-slash-meta";
    const now = new Date().toISOString();
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [
        {
          seq: 1,
          id: "init-1",
          session_id: sessionId,
          event_type: "init",
          payload_json: {
            commands: [
              {
                name: "compact",
                description: "Summarize conversation to save context",
                argument_hint: "<focus>",
              },
            ],
            slash_commands: ["compact", "review"],
          },
          created_at: now,
        },
      ] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 1,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "archived" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return Array.isArray(entry?.acpCommands) && Array.isArray(entry?.acpSlashCommands);
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.acpCommands).toEqual([
      {
        name: "compact",
        description: "Summarize conversation to save context",
        argument_hint: "<focus>",
      },
    ]);
    expect(entry?.acpSlashCommands).toEqual(["compact", "review"]);
  });

  it("pushes ACP model catalogs back into the shared provider bootstrap store", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");
    const providersBootstrapStore = await import("./providersBootstrapStore");

    const sessionId = "session-shared-provider-models";
    const workspaceId = "ws-shared-provider-models";
    const now = new Date().toISOString();

    providersBootstrapStore.updateProvidersBootstrap(workspaceId, (current) => ({
      ...current,
      provider_options: {
        ...current.provider_options,
        codex: {
          provider_id: "codex",
          workspace_id: workspaceId,
          supports_load: false,
          auth_required: false,
          probed_at: now,
          models: {
            models: [{ id: "gpt-5.4" }, { id: "gpt-5.3-codex" }],
            current_model_id: "gpt-5.4",
            meta: {
              source_kind: "subscription",
              catalog_source: "runtime_probe_live",
              refresh_pending: false,
            },
          },
        },
      },
    }));

    const session = {
      ...mkSession(sessionId),
      workspace_id: workspaceId,
      provider_id: "codex",
      model_id: "gpt-5.3-codex",
    };
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session,
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session,
      turns: [] as SessionTurn[],
      events: [
        {
          seq: 1,
          id: "init-models-1",
          session_id: sessionId,
          event_type: "init",
          payload_json: {
            current_model_id: "gpt-5.3-codex",
            models: {
              models: [{ id: "gpt-5.4" }, { id: "gpt-5.3-codex" }, { id: "gpt-5.3-codex-spark" }],
              current_model_id: "gpt-5.3-codex",
            },
          },
          created_at: now,
        },
      ] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 1,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "archived" });

    await waitForCondition(() => {
      const models = providersBootstrapStore
        .getProvidersBootstrapSnapshot(workspaceId)
        .provider_options
        .codex
        ?.models as Record<string, unknown> | undefined;
      const list = Array.isArray(models?.models) ? models.models : [];
      return list.some((entry) => asRecord(entry).id === "gpt-5.3-codex-spark");
    });

    const models = providersBootstrapStore
      .getProvidersBootstrapSnapshot(workspaceId)
      .provider_options
      .codex
      ?.models as Record<string, unknown> | undefined;
    const list = Array.isArray(models?.models) ? models.models : [];
    const meta = asRecord(models?.meta);

    expect(list.map((entry) => String(asRecord(entry).id))).toContain("gpt-5.3-codex-spark");
    expect(models?.current_model_id).toBe("gpt-5.4");
    expect(meta.catalog_source).toBe("session_acp_live");
    expect(meta.refresh_pending).toBe(false);
  });

  it("updates the live session current model from init events without rewriting shared provider defaults", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");
    const providersBootstrapStore = await import("./providersBootstrapStore");

    const sessionId = "session-shared-provider-current-model";
    const workspaceId = "ws-shared-provider-current-model";
    const now = new Date().toISOString();

    providersBootstrapStore.updateProvidersBootstrap(workspaceId, (current) => ({
      ...current,
      provider_options: {
        ...current.provider_options,
        codex: {
          provider_id: "codex",
          workspace_id: workspaceId,
          supports_load: false,
          auth_required: false,
          probed_at: now,
          models: {
            models: [{ id: "gpt-5.4" }, { id: "gpt-5.3-codex" }, { id: "gpt-5.3-codex-spark" }],
            current_model_id: "gpt-5.4",
            meta: {
              source_kind: "subscription",
              catalog_source: "runtime_probe_live",
              refresh_pending: false,
            },
          },
        },
      },
    }));

    const session = {
      ...mkSession(sessionId),
      workspace_id: workspaceId,
      provider_id: "codex",
      model_id: "gpt-5.3-codex",
    };
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session,
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session,
      turns: [] as SessionTurn[],
      events: [
        {
          seq: 1,
          id: "init-models-1",
          session_id: sessionId,
          event_type: "init",
          payload_json: {
            current_model_id: "gpt-5.3-codex",
            models: {
              models: [{ id: "gpt-5.4" }, { id: "gpt-5.3-codex" }, { id: "gpt-5.3-codex-spark" }],
              current_model_id: "gpt-5.3-codex",
            },
          },
          created_at: now,
        },
        {
          seq: 2,
          id: "init-models-2",
          session_id: sessionId,
          event_type: "init",
          payload_json: {
            current_model_id: "gpt-5.3-codex-spark",
          },
          created_at: now,
        },
      ] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 2,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "archived" });

    await waitForCondition(
      () => sup.getSnapshot().sessions[sessionId]?.acpCurrentModelId === "gpt-5.3-codex-spark",
    );

    expect(sup.getSnapshot().sessions[sessionId]?.acpCurrentModelId).toBe("gpt-5.3-codex-spark");
    const models = providersBootstrapStore
      .getProvidersBootstrapSnapshot(workspaceId)
      .provider_options
      .codex
      ?.models as Record<string, unknown> | undefined;
    expect(models?.current_model_id).toBe("gpt-5.4");
  });

  it("applies session head deltas from workspace stream", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-2";
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);
    const seedEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_seed",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      head: {
        session: mkSession(sessionId),
        turns: [] as SessionTurn[],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        last_event_seq: 2,
        state_rev: 2,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
    };
    listeners.forEach((listener) => listener(seedEvent));

    const now = new Date().toISOString();
    const event: SessionEvent = {
      seq: 2,
      id: "e1",
      session_id: sessionId,
      turn_id: "turn-1",
      event_type: "turn_started",
      payload_json: {},
      created_at: now,
    };
    const message: Message = {
      id: "m2",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: "assistant",
      content: "hello",
      delivery: "immediate",
      created_at: now,
    };
    const turn: SessionTurn = {
      turn_id: "turn-1",
      session_id: sessionId,
      run_id: "run-1",
      user_message_id: "m1",
      status: "running",
      start_seq: 2,
      end_seq: null,
      started_at: now,
      updated_at: now,
      assistant_partial: null,
      thought_partial: "",
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    };

    const deltaEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_delta",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      delta: {
        session_id: sessionId,
        last_event_seq: 2,
        state_rev: 2,
        event,
        turn,
        message,
      },
    };

    listeners.forEach((listener) => listener(deltaEvent));

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.events.length).toBe(1);
    expect(entry?.messages.length).toBe(1);
    expect(entry?.turns.length).toBe(1);
    expect(entry?.lastEventSeq).toBe(2);
  });

  it("uses recovering->live transitions for active session gap recovery", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-recovery";
    const now = new Date().toISOString();
    const activeState = mkWorkspaceSnapshotState();
    activeState.activeIds = ["task-recovery"];
    activeState.tasksById = {
      "task-recovery": {
        id: "task-recovery",
        task: {
          id: "task-recovery",
          workspace_id: "ws-1",
          title: "Recovery",
          status: "running",
          primary_session_id: sessionId,
          created_at: now,
          updated_at: now,
          archived_at: null,
        },
        sessions: [{ session: mkSession(sessionId) }],
        primarySessionHead: null,
        sortAtMs: Date.parse(now),
      },
    };

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => activeState,
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => Boolean(sup.getSnapshot().sessions[sessionId]));
    expect(sup.getSnapshot().sessions[sessionId]?.loadState).toBe("pending_hydration");
    await waitForSessionHeadCalls(1);
    getSessionHeadMock.mockClear();
    getSessionHeadMock.mockImplementation(() => new Promise(() => {}));

    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => {});
    const gapEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      session_id: sessionId,
      after_seq: 100,
    };
    listeners.forEach((listener) => listener(gapEvent));
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "recovering" && entry.loadState === "pending_hydration";
    });
    expect(sup.getSnapshot().sessions[sessionId]?.error).toBeUndefined();
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    const seedEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_seed",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      head: {
        session: mkSession(sessionId),
        turns: [] as SessionTurn[],
        events: [] as SessionEvent[],
        messages: [
          {
            id: "m-recovery",
            session_id: sessionId,
            task_id: "task-recovery",
            role: "assistant",
            content: "Recovered",
            delivery: "immediate",
            created_at: now,
          } as Message,
        ],
        last_event_seq: 101,
        state_rev: 101,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
    };
    listeners.forEach((listener) => listener(seedEvent));
    await waitForCondition(() => (sup.getSnapshot().sessions[sessionId]?.messages.length ?? 0) > 0);
    expect(sup.getSnapshot().sessions[sessionId]?.loadState).toBe("live");
    expect(sup.getSnapshot().sessions[sessionId]?.error).toBeUndefined();
    alertSpy.mockRestore();
  });

  it("preserves arrival order for transient assistant overlay chunks (seq=null)", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-transient-order";
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [
        mkTurn({
          sessionId,
          turnId: "turn-1",
          status: "running",
          startSeq: 1,
          startedAt: new Date(1).toISOString(),
        }),
      ],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 1,
      has_more_turns: false,
    } as SessionHeadSnapshot);

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => (sup.getSnapshot().sessions[sessionId]?.turns.length ?? 0) === 1);

    const now = Date.now();
    const sendChunk = (id: string, fragment: string, t: number) => {
      const ev = {
        seq: null,
        transient: true,
        id,
        session_id: sessionId,
        turn_id: "turn-1",
        event_type: "assistant_chunk",
        payload_json: { content_fragment: fragment },
        created_at: new Date(t).toISOString(),
      } as unknown as SessionEvent;
      const deltaEvent: WorkspaceActiveSnapshotEvent = {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        delta: {
          session_id: sessionId,
          last_event_seq: 1,
          state_rev: 1,
          event: ev,
        },
      };
      listeners.forEach((listener) => listener(deltaEvent));
    };

    sendChunk("e1", "a", now);
    sendChunk("e2", "b", now + 1);
    sendChunk("e3", "c", now + 2);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.events).toEqual([]);
    expect(entry?.assistantStreamingByTurnId?.["turn-1"]?.content).toBe("abc");
  });

  it("does not mark turn completed on assistant_complete before done", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-2b";
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);

    const now = new Date().toISOString();
    const turnId = "turn-1";
    const sendDelta = (seq: number, event_type: SessionEvent["event_type"], payload_json: unknown = {}) => {
      const event: SessionEvent = {
        seq,
        id: "e" + seq,
        session_id: sessionId,
        turn_id: turnId,
        event_type,
        payload_json: asRecord(payload_json),
        created_at: now,
      };
      const deltaEvent: WorkspaceActiveSnapshotEvent = {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        delta: {
          session_id: sessionId,
          last_event_seq: seq,
          state_rev: seq,
          event,
        },
      };
      listeners.forEach((listener) => listener(deltaEvent));
    };

    sendDelta(1, "turn_started");
    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("running");

    sendDelta(2, "assistant_complete", { full_content: "hello" });
    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("running");

    sendDelta(3, "done");
    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("completed");
  });

  it("synthesizes assistant messages from assistant_message_inserted deltas", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-inserted-message";
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);

    const now = new Date().toISOString();
    const turnId = "turn-1";
    const sendDelta = (seq: number, event_type: SessionEvent["event_type"], payload_json: unknown = {}) => {
      const event: SessionEvent = {
        seq,
        id: `e${seq}`,
        session_id: sessionId,
        turn_id: turnId,
        event_type,
        payload_json: asRecord(payload_json),
        created_at: now,
      };
      const deltaEvent: WorkspaceActiveSnapshotEvent = {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        delta: {
          session_id: sessionId,
          last_event_seq: seq,
          state_rev: seq,
          event,
        },
      };
      listeners.forEach((listener) => listener(deltaEvent));
    };

    sendDelta(1, "turn_started", { message_id: "user-msg-1" });
    sendDelta(2, "assistant_complete", {
      full_content: "Hello. What do you want to work on?",
      message_id: "provider-msg-1",
      order_seq: 2,
    });
    sendDelta(3, "assistant_message_inserted", {
      message_id: "assistant-msg-1",
      content: "Hello. What do you want to work on?",
      delivery: "immediate",
      order_seq: 2,
      turn_sequence: 1,
      provider_message_id: "provider-msg-1",
    });

    await waitForCondition(
      () => (sup.getSnapshot().sessions[sessionId]?.messages.length ?? 0) === 1,
    );

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages[0]).toMatchObject({
      id: "assistant-msg-1",
      session_id: sessionId,
      turn_id: turnId,
      turn_sequence: 1,
      order_seq: 2,
      role: "assistant",
      content: "Hello. What do you want to work on?",
      delivery: "immediate",
    });
    expect(entry?.turns[0]?.assistant_partial ?? "").toBe("");
  });

  it("applies seeded+deltas without requiring /head hydration", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-3";
    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
      head: {
        session: mkSession(sessionId),
        turns: [] as SessionTurn[],
        events: [] as SessionEvent[],
        messages: [] as Message[],
        last_event_seq: 0,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
    });

    const sup = new SessionSupervisor();

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);
    await waitForSessionHeadCalls(1);
    getSessionHeadMock.mockClear();

    const now = new Date().toISOString();
    const event: SessionEvent = {
      seq: 3,
      id: "e2",
      session_id: sessionId,
      turn_id: "turn-2",
      event_type: "turn_finished",
      payload_json: {},
      created_at: now,
    };
    const message: Message = {
      id: "m3",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-2",
      role: "assistant",
      content: "final",
      delivery: "immediate",
      created_at: now,
    };

    const deltaEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_delta",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      delta: {
        session_id: sessionId,
        last_event_seq: 3,
        state_rev: 3,
        event,
        message,
      },
    };

    listeners.forEach((listener) => listener(deltaEvent));

    await waitForCondition(() => (sup.getSnapshot().sessions[sessionId]?.events.length ?? 0) > 0);
    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.length).toBe(1);
    expect(entry?.events.length).toBe(1);
    expect(entry?.loadState).toBe("live");
    expect(getSessionHead).not.toHaveBeenCalled();
    expect(getSessionSnapshot).not.toHaveBeenCalled();
  });

  it("marks entry stale on session gap while forcing a new /head hydrate", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-gap";
    const seededMessage: Message = {
      id: "msg-gap",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-gap",
      role: "assistant",
      content: "hello",
      delivery: "immediate",
      created_at: new Date().toISOString(),
    };

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });
    const seedEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_head_seed",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      head: {
        session: mkSession(sessionId),
        turns: [] as SessionTurn[],
        events: [] as SessionEvent[],
        messages: [seededMessage],
        last_event_seq: 2,
        state_rev: 2,
        has_more_turns: false,
        has_more_history: false,
        history_cursor: null,
      },
    };
    listeners.forEach((listener) => listener(seedEvent));

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 1);
    getSessionHeadMock.mockClear();
    getSessionHeadMock.mockImplementation(() => new Promise(() => {}));

    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => {});
    const priorSeq = sup.getSnapshot().sessions[sessionId]?.lastEventSeq;
    const gapEvent: WorkspaceActiveSnapshotEvent = {
      type: "session_gap",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      session_id: sessionId,
      after_seq: 5,
    };
    listeners.forEach((listener) => listener(gapEvent));

    const internalEntry = asSupervisorInternals(sup).entries.get(sessionId);
    expect(internalEntry).toBeDefined();
    if (!internalEntry) throw new Error("Expected internal entry to exist");
    expect(internalEntry.turnsHydrated).toBe(false);
    expect(internalEntry.messages.length).toBe(1);
    expect(internalEntry.lastEventSeq).toBe(priorSeq);
    expect(internalEntry.loadState).toBe("pending_hydration");
    expect(internalEntry.freshness).toBe("recovering");
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    alertSpy.mockRestore();
  });

  it("recovers active session gap from stream seed while a forced /head hydrate is pending and preserves local queued drafts", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-gap-queue-recovery";
    const now = new Date().toISOString();
    const activeState = mkWorkspaceSnapshotState();
    activeState.activeIds = ["task-gap-queue"];
    activeState.tasksById = {
      "task-gap-queue": {
        id: "task-gap-queue",
        task: {
          id: "task-gap-queue",
          workspace_id: "ws-1",
          title: "Gap queue recovery",
          status: "running",
          primary_session_id: sessionId,
          created_at: now,
          updated_at: now,
          archived_at: null,
        },
        sessions: [{ session: mkSession(sessionId) }],
        primarySessionHead: null,
        sortAtMs: Date.parse(now),
      },
    };

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => activeState,
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    const serverMessage: Message = {
      id: "m-server",
      session_id: sessionId,
      task_id: "task-gap-queue",
      turn_id: "turn-server",
      role: "assistant",
      content: "baseline server copy",
      delivery: "immediate",
      created_at: now,
    };
    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        head: {
          session: mkSession(sessionId),
          turns: [] as SessionTurn[],
          events: [] as SessionEvent[],
          messages: [serverMessage],
          last_event_seq: 1,
          state_rev: 1,
          has_more_turns: false,
          has_more_history: false,
          history_cursor: null,
        },
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.loadState === "live");
    getSessionHeadMock.mockClear();
    getSessionHeadMock.mockImplementation(() => new Promise(() => {}));

    const internals = asSupervisorInternals(sup);
    const internalEntry = internals.entries.get(sessionId);
    expect(internalEntry).toBeDefined();
    if (!internalEntry) throw new Error("Expected internal entry to exist");

    const queuedLocalMessage: Message = {
      id: "m-local",
      session_id: sessionId,
      task_id: "task-gap-queue",
      turn_id: "turn-local",
      role: "user",
      content: "queued local draft",
      delivery: "queued",
      created_at: new Date(Date.parse(now) + 1).toISOString(),
    };
    internalEntry.messages = [serverMessage, queuedLocalMessage];
    internalEntry.queue = [queuedLocalMessage];

    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => {});
    listeners.forEach((listener) =>
      listener({
        type: "session_gap",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        session_id: sessionId,
        after_seq: 50,
      }),
    );
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "recovering" && entry.loadState === "pending_hydration";
    });
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 3,
        head: {
          session: mkSession(sessionId),
          turns: [] as SessionTurn[],
          events: [] as SessionEvent[],
          messages: [
            {
              ...serverMessage,
              content: "fresh server copy",
            },
          ],
          last_event_seq: 51,
          state_rev: 51,
          has_more_turns: false,
          has_more_history: false,
          history_cursor: null,
        },
      }),
    );

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.loadState === "live" &&
        entry.messages.some((message) => message.id === "m-server" && message.content === "fresh server copy") &&
        entry.messages.some((message) => message.id === "m-local" && message.content === "queued local draft") &&
        entry.queue.some((message) => message.id === "m-local")
      );
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.map((message) => message.id)).toEqual(["m-server", "m-local"]);
    expect(entry?.queue.map((message) => message.id)).toEqual(["m-local"]);
    expect(entry?.loadState).toBe("live");
    expect(entry?.error).toBeUndefined();
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    alertSpy.mockRestore();
  });

  it("preserves local-only queued messages across replica replace patches", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-replace-local-only";
    const sup = new SessionSupervisor();
    const internalEntry = asSupervisorInternals(sup).ensureEntry(sessionId);
    const now = Date.now();

    const serverMessage: Message = {
      id: "m-server",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: "assistant",
      content: "old server copy",
      delivery: "immediate",
      created_at: new Date(now).toISOString(),
    };
    const queuedLocalMessage: Message = {
      id: "m-local",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-2",
      role: "user",
      content: "queued local draft",
      delivery: "queued",
      created_at: new Date(now + 1).toISOString(),
    };

    internalEntry.messages = [serverMessage, queuedLocalMessage];
    internalEntry.queue = [queuedLocalMessage];

    asSupervisorInternals(sup).handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          messages: [
            {
              ...serverMessage,
              content: "fresh server copy",
            },
          ],
        },
      },
    ]);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.map((message) => message.id)).toEqual(["m-server", "m-local"]);
    expect(entry?.messages.find((message) => message.id === "m-server")?.content).toBe("fresh server copy");
    expect(entry?.messages.find((message) => message.id === "m-local")?.content).toBe("queued local draft");
    expect(entry?.queue.map((message) => message.id)).toEqual(["m-local"]);
  });

  it("does not overwrite transcript turns/messages/events when a replica session receives replace patches", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-replica-replace-no-overwrite";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    const initialTurn = mkTurn({
      sessionId,
      turnId: "turn-initial",
      status: "completed",
      startSeq: 1,
    });
    const initialMessage: Message = {
      id: "m-initial",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-initial",
      role: "assistant",
      content: "initial server copy",
      delivery: "immediate",
      created_at: new Date(0).toISOString(),
    };
    const initialEvent: SessionEvent = {
      seq: 1,
      id: "e-initial",
      session_id: sessionId,
      turn_id: "turn-initial",
      event_type: "assistant_chunk",
      payload_json: { content_fragment: "initial" },
      created_at: new Date(1).toISOString(),
    };

    entry.freshness = "replica";
    entry.lastEventSeq = 100;
    entry.projectionRev = 100;
    entry.turns = [initialTurn];
    entry.messages = [initialMessage];
    entry.events = [initialEvent];
    entry.turnsHydrated = true;

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          replaceMode: "authoritative_replace",
          freshness: "authoritative",
          turns: [
            mkTurn({
              sessionId,
              turnId: "turn-replacement",
              status: "completed",
              startSeq: 99,
            }),
          ],
          messages: [
            {
              ...initialMessage,
              content: "replacement message",
            },
          ],
          events: [
            {
              seq: 99,
              id: "e-replacement",
              session_id: sessionId,
              turn_id: "turn-replacement",
              event_type: "assistant_chunk",
              payload_json: { content_fragment: "replacement" },
              created_at: new Date(2).toISOString(),
            } as SessionEvent,
          ],
          projectionRev: 99,
          lastEventSeq: 99,
          hasMoreTurns: false,
        },
      },
    ]);

    const replaced = internals.entries.get(sessionId);
    expect(replaced?.freshness).toBe("replica");
    expect(replaced?.turns).toHaveLength(1);
    expect(replaced?.turns[0]?.turn_id).toBe("turn-initial");
    expect(replaced?.messages).toHaveLength(1);
    expect(replaced?.messages[0]).toEqual(initialMessage);
    expect(replaced?.events).toHaveLength(1);
    expect(replaced?.events[0]).toEqual(initialEvent);
    expect(replaced?.lastEventSeq).toBe(100);
    expect(replaced?.projectionRev).toBe(100);
  });

  it("keeps an interrupted turn interrupted across bootstrap replace replay", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-replace-interrupted-monotonic";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [
      mkTurn({
        sessionId,
        turnId: "turn-1",
        status: "interrupted",
        startSeq: 1,
      }),
    ];
    entry.turnsHydrated = true;
    entry.lastEventSeq = 2;
    entry.freshness = "bootstrap";

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [
            mkTurn({
              sessionId,
              turnId: "turn-1",
              status: "running",
              startSeq: 1,
            }),
          ],
          events: [] as SessionEvent[],
          messages: [] as Message[],
          lastEventSeq: 1,
          hasMoreTurns: false,
        },
      },
    ]);

    const current = sup.getSnapshot().sessions[sessionId];
    expect(current?.turns).toHaveLength(1);
    expect(current?.turns[0]?.status).toBe("interrupted");
  });

  it("lets authoritative replacement patches clear live tool counters", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-replace-tool-counters";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [
      {
        ...mkTurn({
          sessionId,
          turnId: "turn-1",
          status: "running",
          startSeq: 1,
        }),
        tool_total: 2,
        tool_pending: 1,
        tool_running: 1,
      },
    ];
    entry.turnsHydrated = true;
    entry.lastEventSeq = 2;
    entry.freshness = "bootstrap";

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          replaceMode: "authoritative_replace",
          turns: [
            {
              ...mkTurn({
                sessionId,
                turnId: "turn-1",
                status: "completed",
                startSeq: 1,
              }),
              tool_total: 2,
              tool_pending: 0,
              tool_running: 0,
              tool_completed: 2,
            },
          ],
          events: [] as SessionEvent[],
          messages: [] as Message[],
          lastEventSeq: 3,
          hasMoreTurns: false,
        },
      },
    ]);

    const current = sup.getSnapshot().sessions[sessionId];
    expect(current?.turns).toHaveLength(1);
    expect(current?.turns[0]?.status).toBe("completed");
    expect(current?.turns[0]?.tool_pending).toBe(0);
    expect(current?.turns[0]?.tool_running).toBe(0);
    expect(current?.turns[0]?.tool_completed).toBe(2);
  });

  it("ignores active task upserts without head data", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-4";
    const now = new Date().toISOString();
    const task = {
      id: "task-4",
      workspace_id: "ws-1",
      title: "Active task",
      status: "running",
      created_at: now,
      updated_at: now,
    };
    const session = mkSession(sessionId);
    const message = {
      id: "m4",
      session_id: sessionId,
      task_id: "task-4",
      role: "assistant",
      content: "hello",
      delivery: "immediate",
      created_at: now,
    };

    const summary = {
      task,
      primary_session: {
        session,
        last_message_at: now,
        last_message_preview: "hello",
        last_event_seq: 1,
        state_rev: 1,
        activity: { is_working: false },
        unread: false,
      },
      primary_session_head: null,
      sessions: [],
      sort_at: now,
    };

    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);

    const upsertEvent: WorkspaceActiveSnapshotEvent = {
      type: "active_task_upsert",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      task: summary,
    };
    listeners.forEach((listener) => listener(upsertEvent));

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry).toBeUndefined();
  });

  it("hydrates tool summaries from head and still loads full tools on demand", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");
    const { listTurnTools } = await import("../api/client");

    const sessionId = "session-3";
    const turnId = "turn-1";

    getSessionSnapshotMock.mockResolvedValue({
      summary: {
        session: mkSession(sessionId),
      },
    });
    getSessionHeadMock.mockResolvedValue({
      session: mkSession(sessionId),
      turns: [
        {
          turn_id: turnId,
          session_id: sessionId,
          run_id: null,
          user_message_id: null,
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 1,
          tool_failed: 0,
        } as SessionTurn,
      ],
      tool_summaries: [
        {
          session_id: sessionId,
          tool_call_id: "tool-1",
          turn_id: turnId,
          tool_kind: "execute",
          provider_tool_name: "Bash",
          title: "Run",
          subtitle: "pwd",
          status: "completed",
          input_preview: { command: "pwd" },
          order_seq: 2,
          first_event_seq: 2,
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        },
      ],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "archived" });

    await waitForCondition(() => getSessionHeadMock.mock.calls.length > 0);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.turnToolsByTurnId[turnId]?.length === 1);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.turnToolsByTurnId[turnId]?.length).toBe(1);
    expect(entry?.turnToolsByTurnId[turnId]?.[0]?.order_seq).toBe(2);
    expect(entry?.turnToolsByTurnId[turnId]?.[0]?.provider_tool_name).toBe("Bash");
    expect(entry?.turnToolsByTurnId[turnId]?.[0]?.subtitle).toBe("pwd");

    await sup.loadTurnTools(sessionId, turnId);
    expect(listTurnTools).toHaveBeenCalledWith(sessionId, turnId);
  });

  it("rehydrates foreground active sessions from /head even when a live active snapshot head exists", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-5";
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      state_rev: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);
    const store: WorkspaceActiveSnapshotEventSource & {
      getSessionHeadsSnapshot: () => Record<string, SessionHeadSnapshot>;
    } = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getSessionHeadsSnapshot: () => ({ [sessionId]: head }),
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "bootstrap" && entry.lastEventSeq === 0;
    });
    expect(getSessionHead).toHaveBeenCalledTimes(1);
    expect(getSessionSnapshot).not.toHaveBeenCalled();

    resolveHead(head);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
  });

  it("does not publish bounded cached bootstrap heads during active open", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-active-bounded-cache";
    const cachedHead = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-cached", status: "completed", startSeq: 1 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-cached",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-cached",
          role: "assistant",
          content: "cached bounded bootstrap",
          delivery: "immediate",
          created_at: new Date().toISOString(),
        },
      ],
      last_event_seq: 1,
      has_more_turns: true,
      head_window: {
        turn_limit: 40,
        message_limit: 120,
        event_limit: 120,
        byte_limit: 1024 * 1024,
        turn_count: 40,
        message_count: 40,
        event_count: 40,
        bytes: 4096,
      },
    } satisfies SessionHead;
    loadSessionHeadV1Mock.mockResolvedValueOnce({
      v: 1,
      sessionId,
      updatedAtMs: Date.now(),
      head: cachedHead,
    });

    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "active" });

    await waitForSessionHeadCalls(1);

    const pendingEntry = sup.getSnapshot().sessions[sessionId];
    expect(pendingEntry?.freshness).toBe("bootstrap");
    expect(pendingEntry?.messages).toHaveLength(0);
    expect(pendingEntry?.turns).toHaveLength(0);
    expect(pendingEntry?.loadState).toBe("pending_hydration");

    resolveHead({
      ...cachedHead,
      messages: [
        {
          id: "m-authoritative",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-cached",
          role: "assistant",
          content: "authoritative head",
          delivery: "immediate",
          created_at: new Date().toISOString(),
        },
      ],
      last_event_seq: 2,
      state_rev: 2,
      has_more_history: true,
      history_cursor: null,
    });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    expect(
      sup.getSnapshot().sessions[sessionId]?.messages.some(
        (message) => message.content === "authoritative head",
      ),
    ).toBe(true);
  });

  it("keeps visible bootstrap cached active sessions pending until authoritative catch-up", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-active-visible-bootstrap";
    const cachedHead = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-cached", status: "completed", startSeq: 1 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-cached",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-cached",
          role: "assistant",
          content: "cached visible bootstrap",
          delivery: "immediate",
          created_at: new Date().toISOString(),
        },
      ],
      last_event_seq: 1,
      has_more_turns: false,
    } satisfies SessionHead;
    loadSessionHeadV1Mock.mockResolvedValueOnce({
      v: 1,
      sessionId,
      updatedAtMs: Date.now(),
      head: cachedHead,
    });

    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "bootstrap" &&
        entry.messages.some((message) => message.content === "cached visible bootstrap")
      );
    });

    const bootstrapEntry = sup.getSnapshot().sessions[sessionId];
    expect(bootstrapEntry?.loadState).toBe("pending_hydration");

    resolveHead({
      ...cachedHead,
      messages: [
        {
          id: "m-authoritative",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-cached",
          role: "assistant",
          content: "authoritative catch-up",
          delivery: "immediate",
          created_at: new Date().toISOString(),
        },
      ],
      last_event_seq: 2,
      state_rev: 2,
      has_more_history: false,
      history_cursor: null,
    });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "replica" &&
        entry.loadState === "live" &&
        entry.messages.some((message) => message.content === "authoritative catch-up")
      );
    });
  });

  it("rehydrates from /head when active heads came only from bootstrap cache", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-5-bootstrap";
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 0,
      state_rev: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);

    const store: WorkspaceActiveSnapshotEventSource & {
      getSessionHeadsSnapshot: () => Record<string, SessionHeadSnapshot>;
    } = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getSessionHeadsSnapshot: () => ({ [sessionId]: head }),
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => ({ ...mkWorkspaceSnapshotState(), liveSnapshotApplied: false }),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "bootstrap" && entry.lastEventSeq === 0;
    });
    expect(getSessionHead).toHaveBeenCalledTimes(1);

    resolveHead(head);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
  });

  it("forces /head on warm reopen after disconnect clears authority", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-disconnect-reopen";
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 3,
      state_rev: 3,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-disconnect-reopen"],
      tasksById: {
        "task-disconnect-reopen": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-disconnect-reopen",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: head,
        },
      },
      totalActive: 1,
    };
    let resolveFirstHead!: (value: SessionHeadSnapshot) => void;
    const firstHeadPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveFirstHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => firstHeadPromise);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    sup.setWorkspaceSnapshotState(activeState);
    const close = sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "bootstrap" && entry.lastEventSeq === 3;
    });
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);

    resolveFirstHead(head);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");

    sup.setWorkspaceSnapshotState({ ...activeState, connection: "disconnected" });
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.loadState === "recovering");

    close();
    let resolveSecondHead!: (value: SessionHeadSnapshot) => void;
    const secondHeadPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveSecondHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => secondHeadPromise);
    sup.openSession(sessionId, { mode: "active" });

    await waitForSessionHeadCalls(2);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "bootstrap" && entry.lastEventSeq === 3;
    });

    resolveSecondHead(head);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
  });

  it("keeps active bootstrap transcript pending until authoritative catch-up completes", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-active-bootstrap-pending";
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-bootstrap", status: "running", startSeq: 3 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-bootstrap",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-bootstrap",
          role: "assistant",
          content: "bootstrap",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:03.000Z",
        },
      ],
      last_event_seq: 3,
      state_rev: 3,
      activity: { is_working: true, last_turn_status: "running" },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-active-bootstrap-pending"],
      tasksById: {
        "task-active-bootstrap-pending": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-active-bootstrap-pending",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: head,
        },
      },
      totalActive: 1,
    };
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "bootstrap" && entry.messages.length === 1;
    });
    expect(sup.getSnapshot().sessions[sessionId]?.loadState).toBe("pending_hydration");

    resolveHead({
      ...head,
      turns: [mkTurn({ sessionId, turnId: "turn-authoritative", status: "completed", startSeq: 4 })],
      messages: [
        {
          id: "m-authoritative",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-authoritative",
          role: "assistant",
          content: "authoritative",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:04.000Z",
        },
      ],
      last_event_seq: 4,
      state_rev: 4,
      activity: { is_working: false, last_turn_status: "completed" },
    });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "replica" && entry.loadState === "live";
    });
  });

  it("does not let compact active-head seeds overwrite a replica-warm session", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-warm-replica";
    const olderTime = "2026-03-09T00:00:01.000Z";
    const newerTime = "2026-03-09T00:00:02.000Z";
    const compactHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: newerTime,
        },
      ],
      last_event_seq: 4,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };
    const fullHead: SessionHeadSnapshot = {
      ...compactHead,
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: olderTime,
        },
        ...compactHead.messages,
      ],
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256_000,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-warm-authoritative"],
      tasksById: {
        "task-warm-authoritative": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-warm-authoritative",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: compactHead,
        },
      },
      totalActive: 1,
    };
    getSessionHeadMock.mockResolvedValueOnce(fullHead);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSnapshotState(activeState);
    const close = sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 2);
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.turns.length === 2);

    sup.setWorkspaceSnapshotState(activeState);
    expect(sup.getSnapshot().sessions[sessionId]?.messages).toHaveLength(2);
    expect(sup.getSnapshot().sessions[sessionId]?.turns).toHaveLength(2);

    close();
    sup.openSession(sessionId, { mode: "active" });

    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(sup.getSnapshot().sessions[sessionId]?.messages).toHaveLength(2);
    expect(sup.getSnapshot().sessions[sessionId]?.turns).toHaveLength(2);
  });

  it("repair-replaces an already-open stale active transcript from workspace session heads", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-open-stale-repair";
    const staleTurn = mkTurn({ sessionId, turnId: "turn-stale", status: "running", startSeq: 1 });
    const staleMessage: Message = {
      id: "m-stale",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-stale",
      role: "assistant",
      content: "stale",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:01.000Z",
    };
    const freshTurns = [
      mkTurn({ sessionId, turnId: "turn-fresh-1", status: "completed", startSeq: 10 }),
      mkTurn({ sessionId, turnId: "turn-fresh-2", status: "completed", startSeq: 12 }),
    ];
    const freshMessages: Message[] = [
      {
        id: "m-fresh-1",
        session_id: sessionId,
        task_id: "task-1",
        turn_id: "turn-fresh-1",
        role: "assistant",
        content: "fresh-1",
        delivery: "immediate",
        created_at: "2026-03-09T00:00:10.000Z",
      },
      {
        id: "m-fresh-2",
        session_id: sessionId,
        task_id: "task-1",
        turn_id: "turn-fresh-2",
        role: "assistant",
        content: "fresh-2",
        delivery: "immediate",
        created_at: "2026-03-09T00:00:12.000Z",
      },
    ];
    const currentHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: freshTurns,
      events: [] as SessionEvent[],
      messages: freshMessages,
      activity: { is_working: false, last_turn_status: "completed" },
      last_event_seq: 20,
      projection_rev: 7,
      state_rev: 7,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256_000,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const pendingActiveHydrate = new Promise<SessionHeadSnapshot>(() => {});
    getSessionHeadMock.mockImplementation((id) => {
      if (id === sessionId) {
        return pendingActiveHydrate;
      }
      throw new Error(`unexpected getSessionHead call: ${id}`);
    });

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "active" });
    await waitForCondition(
      () => getSessionHeadMock.mock.calls.some(([id]) => id === sessionId),
      SUPERVISOR_ASYNC_WAIT_TIMEOUT_MS,
    );

    const internals = asSupervisorInternals(sup);
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [staleTurn],
          messages: [staleMessage],
          events: [] as SessionEvent[],
          activity: { is_working: false, last_turn_status: "completed" },
          lastEventSeq: 20,
          projectionRev: 7,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "replica" && entry.turns[0]?.turn_id === "turn-stale";
    });

    sup.setWorkspaceSessionHeads({ [sessionId]: currentHead });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.messages.map((message) => message.id).join(",") === "m-fresh-1,m-fresh-2";
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.freshness).toBe("replica");
    expect(entry?.messages.map((message) => message.id)).toEqual(["m-fresh-1", "m-fresh-2"]);
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-fresh-1", "turn-fresh-2"]);
    expect(entry?.lastEventSeq).toBe(20);
    expect(entry?.projectionRev).toBe(7);
  });

  it("promotes an overlapping partial authoritative head into an open active transcript", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-open-partial-tail-repair";
    const probe2Turn = mkTurn({ sessionId, turnId: "turn-probe-2", status: "completed", startSeq: 20 });
    const probe2User: Message = {
      id: "m-probe-2-user",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-probe-2",
      role: "user",
      content: "remote-ui-progress-2",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:20.000Z",
    };
    const probe2Assistant: Message = {
      ...probe2User,
      id: "m-probe-2-assistant",
      role: "assistant",
      content: "remote-ui-progress-2 complete",
      created_at: "2026-03-09T00:00:21.000Z",
    };
    const probe3Turn = mkTurn({ sessionId, turnId: "turn-probe-3", status: "running", startSeq: 30 });
    const probe3User: Message = {
      id: "m-probe-3-user",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-probe-3",
      role: "user",
      content: "remote-ui-progress-3",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:30.000Z",
    };
    const partialHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [probe2Turn, probe3Turn],
      events: [] as SessionEvent[],
      messages: [probe2User, probe2Assistant, probe3User],
      activity: { is_working: true, last_turn_status: "running" },
      last_event_seq: 31,
      projection_rev: 31,
      state_rev: 31,
      has_more_turns: true,
      has_more_history: true,
      history_cursor: 1,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256_000,
        turn_count: 60,
        message_count: 200,
        event_count: 200,
        bytes: 256_000,
        truncated: true,
      },
    };
    getSessionHeadMock.mockImplementationOnce(() => new Promise<SessionHeadSnapshot>(() => {}));

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "active" });
    await waitForSessionHeadCalls(1);

    const internals = asSupervisorInternals(sup);
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [probe2Turn],
          messages: [probe2User, probe2Assistant],
          events: [] as SessionEvent[],
          activity: { is_working: false, last_turn_status: "completed" },
          lastEventSeq: 22,
          projectionRev: 22,
          stateRev: 22,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.messages.some((message) => message.content === "remote-ui-progress-2 complete");
    });

    sup.setWorkspaceSessionHeads({ [sessionId]: partialHead });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return Boolean(entry?.messages.some((message) => message.content === "remote-ui-progress-3"));
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.freshness).toBe("replica");
    expect(entry?.messages.map((message) => message.id)).toEqual([
      "m-probe-2-user",
      "m-probe-2-assistant",
      "m-probe-3-user",
    ]);
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-probe-2", "turn-probe-3"]);
    expect(entry?.lastEventSeq).toBe(31);
    expect(entry?.projectionRev).toBe(31);
    expect(entry?.activity).toEqual({ is_working: true, last_turn_status: "running" });
  });

  it("repairs a newly promoted active session from a fresher workspace head", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-switch-fresh";
    const staleTurn = mkTurn({ sessionId, turnId: "turn-stale", status: "running", startSeq: 3 });
    const staleMessage: Message = {
      id: "m-stale",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-stale",
      role: "assistant",
      content: "stale",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:03.000Z",
    };
    const freshTurn = mkTurn({ sessionId, turnId: "turn-fresh", status: "completed", startSeq: 10 });
    const freshMessage: Message = {
      id: "m-fresh",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-fresh",
      role: "assistant",
      content: "fresh-marker",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:10.000Z",
    };
    const freshHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [freshTurn],
      events: [] as SessionEvent[],
      messages: [freshMessage],
      activity: { is_working: false, last_turn_status: "completed" },
      last_event_seq: 10,
      projection_rev: 10,
      state_rev: 10,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    sup.setWorkspaceSnapshotState({
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-1"],
      tasksById: {
        "task-1": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-1",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: freshHead,
        },
      },
      totalActive: 1,
    });
    sup.setWorkspaceSessionHeads({ [sessionId]: freshHead });

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [staleTurn],
          messages: [staleMessage],
          events: [] as SessionEvent[],
          activity: { is_working: true, last_turn_status: "running" },
          lastEventSeq: 3,
          projectionRev: 3,
          stateRev: 3,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.messages.map((message) => message.id)).toEqual([
      staleMessage.id,
    ]);

    sup.setActiveTaskSessionIds([sessionId]);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.freshness).not.toBe("bootstrap");
    expect(entry?.messages.map((message) => message.id)).toEqual([freshMessage.id]);
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual([freshTurn.turn_id]);
    expect(entry?.lastEventSeq).toBe(10);
    expect(entry?.projectionRev).toBe(10);
    expect(entry?.stateRev).toBe(10);
  });

  it("seeds freshly opened active sessions from compatible bounded active heads while /head hydrate is pending", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-bounded-active-open";
    const compactHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 4,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 5,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };
    const fullHead: SessionHeadSnapshot = {
      ...compactHead,
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        ...compactHead.messages,
      ],
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-bounded-active-open"],
      tasksById: {
        "task-bounded-active-open": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-bounded-active-open",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: compactHead,
        },
      },
      totalActive: 1,
    };
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForSessionHeadCalls(1);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "bootstrap" &&
        entry.loadState === "pending_hydration" &&
        entry.messages.map((message) => message.id).join(",") === "m-2" &&
        entry.turns.map((turn) => turn.turn_id).join(",") === "turn-2"
      );
    });

    resolveHead(fullHead);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "replica" &&
        entry.messages.length === 2 &&
        entry.turns.length === 2
      );
    });
  });

  it("does not reseed recovering active sessions from bounded active heads before /head hydrate", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-bounded-active-recovering";
    const compactHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 4,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 5,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };
    const fullHead: SessionHeadSnapshot = {
      ...compactHead,
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        ...compactHead.messages,
      ],
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-bounded-active-recovering"],
      tasksById: {
        "task-bounded-active-recovering": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-bounded-active-recovering",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: compactHead,
        },
      },
      totalActive: 1,
    };
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    let resolveReconnectHead!: (value: SessionHeadSnapshot) => void;
    const reconnectHeadPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveReconnectHead = resolve;
    });
    getSessionHeadMock
      .mockImplementationOnce(() => headPromise)
      .mockImplementationOnce(() => reconnectHeadPromise);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForSessionHeadCalls(1);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "bootstrap" &&
        entry.loadState === "pending_hydration" &&
        entry.messages.map((message) => message.id).join(",") === "m-2"
      );
    });

    sup.setWorkspaceSnapshotState({ ...activeState, connection: "disconnected" });
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "recovering" && entry.loadState === "recovering";
    });

    sup.setWorkspaceSnapshotState(activeState);
    await waitForSessionHeadCalls(2);
    expect(sup.getSnapshot().sessions[sessionId]?.freshness).toBe("recovering");
    expect(sup.getSnapshot().sessions[sessionId]?.loadState).toBe("recovering");
    expect(sup.getSnapshot().sessions[sessionId]?.messages.map((message) => message.id)).toEqual(["m-2"]);
    expect(sup.getSnapshot().sessions[sessionId]?.turns.map((turn) => turn.turn_id)).toEqual(["turn-2"]);

    resolveReconnectHead(fullHead);
    resolveHead(fullHead);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        entry?.freshness === "replica" &&
        entry.messages.length === 2 &&
        entry.turns.length === 2
      );
    });
  });

  it("clears recovering open sessions from reconnecting active-head hydration without dropping history", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-reconnect-active-head";
    const olderTime = "2026-03-09T00:00:01.000Z";
    const newerTime = "2026-03-09T00:00:02.000Z";
    const compactHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: newerTime,
        },
      ],
      last_event_seq: 4,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    const fullHead: SessionHeadSnapshot = {
      ...compactHead,
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: olderTime,
        },
        ...compactHead.messages,
      ],
    };
    const activeState: WorkspaceActiveSnapshotState = {
      ...mkWorkspaceSnapshotState(),
      activeIds: ["task-reconnect-active-head"],
      tasksById: {
        "task-reconnect-active-head": {
          ...mkWorkspaceTaskSummary({
            taskId: "task-reconnect-active-head",
            primarySessionId: sessionId,
            sessionIds: [sessionId],
          }),
          primarySessionHead: compactHead,
        },
      },
      totalActive: 1,
    };
    getSessionHeadMock.mockResolvedValueOnce(fullHead).mockResolvedValueOnce(fullHead);

    const sup = new SessionSupervisor();
    sup.setWorkspaceSnapshotState(activeState);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 2);

    sup.setWorkspaceSnapshotState({ ...activeState, connection: "disconnected" });
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.freshness === "recovering" && entry.loadState === "recovering";
    });

    sup.setWorkspaceSnapshotState(activeState);
    await waitForSessionHeadCalls(2);
    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.loadState === "live" && entry.freshness !== "recovering";
    });

    expect(sup.getSnapshot().sessions[sessionId]?.messages).toHaveLength(2);
    expect(sup.getSnapshot().sessions[sessionId]?.turns).toHaveLength(2);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
  });

  it("keeps in-flight assistant streaming when bounded workspace heads repeat the same covered transcript", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-bounded-streaming-overlay";
    const runningTurn = mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 });
    const userMessage: Message = {
      id: "m-user-1",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: "user",
      content: "hello",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:01.000Z",
    };
    const boundedHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [runningTurn],
      events: [] as SessionEvent[],
      messages: [userMessage],
      activity: { is_working: true, last_turn_status: "running" },
      last_event_seq: 4,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 5,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [runningTurn],
          messages: [userMessage],
          events: [] as SessionEvent[],
          activity: { is_working: true, last_turn_status: "running" },
          lastEventSeq: 4,
          projectionRev: 7,
          stateRev: 7,
          turnsHydrated: true,
          loading: false,
          assistantStreamingByTurnId: {
            "turn-1": {
              content: "Hi ",
              providerMessageId: "msg-1",
              orderSeq: 2,
            },
          },
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.assistantStreamingByTurnId?.["turn-1"]?.content).toBe(
      "Hi ",
    );

    sup.setWorkspaceSessionHeads({ [sessionId]: boundedHead });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.assistantStreamingByTurnId?.["turn-1"]?.content).toBe("Hi ");
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-1"]);
    expect(entry?.messages.map((message) => message.id)).toEqual(["m-user-1"]);
  });

  it("does not overwrite authoritative transcript state when workspace session heads refresh", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-workspace-head-summary-only";
    const authoritativeTurn = mkTurn({ sessionId, turnId: "turn-authoritative", status: "completed", startSeq: 4 });
    const authoritativeMessage: Message = {
      id: "m-authoritative",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-authoritative",
      role: "assistant",
      content: "authoritative transcript",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:04.000Z",
    };
    const workspaceHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [mkTurn({ sessionId, turnId: "turn-workspace", status: "completed", startSeq: 9 })],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-workspace",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-workspace",
          role: "assistant",
          content: "workspace summary copy",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:09.000Z",
        },
      ],
      last_event_seq: 9,
      projection_rev: 9,
      state_rev: 9,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 5,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [authoritativeTurn],
          messages: [authoritativeMessage],
          events: [] as SessionEvent[],
          activity: { is_working: false, last_turn_status: "completed" },
          lastEventSeq: 4,
          projectionRev: 4,
          stateRev: 4,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    sup.setWorkspaceSessionHeads({ [sessionId]: workspaceHead });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.freshness).toBe("replica");
    expect(entry?.lastEventSeq).toBe(4);
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-authoritative"]);
    expect(entry?.messages.map((message) => message.content)).toEqual(["authoritative transcript"]);
  });

  it("does not let workspace summary deltas overwrite authoritative session activity", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-summary-activity-interrupted";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 })],
          messages: [] as Message[],
          events: [] as SessionEvent[],
          activity: { is_working: true, last_turn_status: "running" },
          lastEventSeq: 1,
          projectionRev: 1,
          stateRev: 1,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    sup.handleWorkspaceEvent({
      type: "session_summary_delta",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      delta: {
        session_id: sessionId,
        task_id: "task-1",
        activity: { is_working: false, last_turn_status: "interrupted" },
        last_event_seq: 2,
        projection_rev: 2,
        state_rev: 2,
      },
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.activity).toEqual({ is_working: true, last_turn_status: "running" });
    expect(entry?.turns.at(-1)?.status).toBe("running");
    expect(entry?.lastEventSeq).toBe(1);
    expect(entry?.projectionRev).toBe(1);
    expect(entry?.stateRev).toBe(1);
  });

  it("normalizes running activity to completed when a terminal turn is applied", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-activity-normalizes-terminal";
    const sup = new SessionSupervisor();
    sup.setSession(mkSession(sessionId));
    sup.setTurns(sessionId, [mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 })], {
      replace: true,
    });
    sup.setSessionActivity(sessionId, { is_working: true, last_turn_status: "running" });
    sup.setTurns(sessionId, [mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 })], {
      replace: true,
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.activity).toEqual({ is_working: false, last_turn_status: "completed" });
    expect(entry?.turns.at(-1)?.status).toBe("completed");
  });

  it("keeps a newer running turn working when an older terminal turn arrives later", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-activity-keeps-newer-running";
    const sup = new SessionSupervisor();
    sup.setSession(mkSession(sessionId));
    sup.setTurns(
      sessionId,
      [
        mkTurn({ sessionId, turnId: "turn-2", status: "running", startSeq: 20 }),
      ],
      { replace: true },
    );
    sup.setSessionActivity(sessionId, { is_working: true, last_turn_status: "running" });
    sup.setTurns(
      sessionId,
      [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 10 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "running", startSeq: 20 }),
      ],
      { replace: true },
    );

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.activity).toEqual({ is_working: true, last_turn_status: "running" });
    expect(entry?.turns.map((turn) => turn.status)).toEqual(["completed", "running"]);
  });

  it("normalizes activity from the latest canonical turn order when start_seq ties", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-activity-order-tie";
    const sup = new SessionSupervisor();
    sup.setSession(mkSession(sessionId));
    sup.setTurns(
      sessionId,
      [
        mkTurn({
          sessionId,
          turnId: "turn-1",
          status: "completed",
          startSeq: 10,
          startedAt: "2026-03-09T00:00:10.000Z",
        }),
        mkTurn({
          sessionId,
          turnId: "turn-2",
          status: "running",
          startSeq: 10,
          startedAt: "2026-03-09T00:00:11.000Z",
        }),
      ],
      { replace: true },
    );

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.activity).toEqual({ is_working: true, last_turn_status: "running" });
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-1", "turn-2"]);
  });

  it("ignores older workspace summary deltas for authoritative session entries", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-summary-activity-stale";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          freshness: "authoritative",
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "interrupted", startSeq: 2 })],
          messages: [] as Message[],
          events: [] as SessionEvent[],
          activity: { is_working: false, last_turn_status: "interrupted" },
          lastEventSeq: 2,
          projectionRev: 2,
          stateRev: 2,
          turnsHydrated: true,
          loading: false,
        },
      },
    ]);

    sup.handleWorkspaceEvent({
      type: "session_summary_delta",
      workspace_id: "ws-1",
      snapshot_rev: 3,
      delta: {
        session_id: sessionId,
        task_id: "task-1",
        activity: { is_working: true, last_turn_status: "running" },
        last_event_seq: 1,
        projection_rev: 1,
        state_rev: 1,
      },
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.activity).toEqual({ is_working: false, last_turn_status: "interrupted" });
    expect(entry?.turns.at(-1)?.status).toBe("interrupted");
    expect(entry?.lastEventSeq).toBe(2);
    expect(entry?.projectionRev).toBe(2);
    expect(entry?.stateRev).toBe(2);
  });

  it("still allows workspace summary deltas to update bootstrap-only idle entries", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-summary-activity-sticky-interrupted";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.session = mkSession(sessionId);
    entry.loadState = "live";

    sup.handleWorkspaceEvent({
      type: "session_summary_delta",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      delta: {
        session_id: sessionId,
        task_id: "task-1",
        activity: { is_working: false, last_turn_status: "interrupted" },
        last_event_seq: 2,
        projection_rev: 2,
        state_rev: 2,
      },
    });

    const nextEntry = sup.getSnapshot().sessions[sessionId];
    expect(nextEntry?.activity).toEqual({ is_working: false, last_turn_status: "interrupted" });
    expect(nextEntry?.lastEventSeq).toBe(2);
    expect(nextEntry?.projectionRev).toBe(2);
    expect(nextEntry?.stateRev).toBe(2);
  });

  it("evicts omitted stale running turns from bounded active heads", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-stale-running";
    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        head: {
          session: mkSession(sessionId),
          turns: [mkTurn({ sessionId, turnId: "turn-stale", status: "running", startSeq: 1 })],
          events: [] as SessionEvent[],
          messages: [] as Message[],
          last_event_seq: 1,
          state_rev: 1,
          has_more_turns: false,
          has_more_history: false,
          history_cursor: null,
          head_window: {
            turn_limit: 5,
            message_limit: 200,
            event_limit: 0,
            byte_limit: 1500000,
            turn_count: 1,
            message_count: 0,
            event_count: 0,
            bytes: 0,
            truncated: false,
          },
        },
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.turns.length === 1);
    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("running");

    const freshTurns = Array.from({ length: 5 }, (_, index) =>
      mkTurn({
        sessionId,
        turnId: `turn-fresh-${index + 1}`,
        status: "completed",
        startSeq: index + 10,
      }),
    );

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: {
          session: mkSession(sessionId),
          turns: freshTurns,
          events: [] as SessionEvent[],
          messages: [] as Message[],
          last_event_seq: 20,
          state_rev: 2,
          has_more_turns: true,
          has_more_history: false,
          history_cursor: null,
          head_window: {
            turn_limit: 5,
            message_limit: 200,
            event_limit: 0,
            byte_limit: 1500000,
            turn_count: 5,
            message_count: 0,
            event_count: 0,
            bytes: 0,
            truncated: true,
          },
        },
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.lastEventSeq === 20);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(freshTurns.map((turn) => turn.turn_id));
    expect(entry?.turns.some((turn) => turn.status === "running" || turn.status === "queued")).toBe(
      false,
    );
  });

  it("preserves already loaded history when a bounded active head reseeds a covered tail window", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-bounded-covered-history";
    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };

    const compactHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [fullHead.turns[1]!],
      messages: [fullHead.messages[1]!],
      last_event_seq: 20,
      projection_rev: 9,
      state_rev: 9,
      head_window: {
        turn_limit: 0,
        message_limit: 50,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };

    getSessionHeadMock.mockResolvedValueOnce(fullHead);

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 2);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: compactHead,
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.lastEventSeq === 20);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.map((message) => message.id)).toEqual(fullHead.messages.map((message) => message.id));
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(fullHead.turns.map((turn) => turn.turn_id));
  });

  it("preserves already loaded history when a bounded active head window is disjoint", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-bounded-disjoint-history";
    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const oldTurn = mkTurn({ sessionId, turnId: "turn-old", status: "completed", startSeq: 1 });
    const oldMessage: Message = {
      id: "m-old",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-old",
      role: "assistant",
      content: "older loaded transcript",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:01.000Z",
    };
    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [oldTurn],
      events: [] as SessionEvent[],
      messages: [oldMessage],
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };
    const newTurn = mkTurn({ sessionId, turnId: "turn-new", status: "completed", startSeq: 20 });
    const newMessage: Message = {
      id: "m-new",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-new",
      role: "user",
      content: "new compact head message",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:20.000Z",
    };
    const compactHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [newTurn],
      messages: [newMessage],
      last_event_seq: 20,
      projection_rev: 9,
      state_rev: 9,
      head_window: {
        turn_limit: 1,
        message_limit: 1,
        event_limit: 800,
        byte_limit: 200_000,
        turn_count: 1,
        message_count: 1,
        event_count: 0,
        bytes: 256,
        truncated: true,
      },
    };

    getSessionHeadMock.mockResolvedValueOnce(fullHead);

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 1);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: compactHead,
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.lastEventSeq === 20);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.map((message) => message.id)).toEqual(["m-old", "m-new"]);
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(["turn-old", "turn-new"]);
  });

  it("preserves already loaded history when an unbounded active head reseeds a covered tail window", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-unbounded-covered-history";
    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const fullHead: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [
        mkTurn({ sessionId, turnId: "turn-1", status: "completed", startSeq: 1 }),
        mkTurn({ sessionId, turnId: "turn-2", status: "completed", startSeq: 3 }),
      ],
      events: [] as SessionEvent[],
      messages: [
        {
          id: "m-1",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "older",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
        {
          id: "m-2",
          session_id: sessionId,
          task_id: "task-1",
          turn_id: "turn-2",
          role: "assistant",
          content: "newer",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
        },
      ],
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
      head_window: {
        turn_limit: 0,
        message_limit: 0,
        event_limit: 0,
        byte_limit: 0,
        turn_count: 2,
        message_count: 2,
        event_count: 0,
        bytes: 512,
        truncated: false,
      },
    };

    const compactHead: SessionHeadSnapshot = {
      ...fullHead,
      turns: [fullHead.turns[1]!],
      messages: [fullHead.messages[1]!],
      last_event_seq: 20,
      projection_rev: 9,
    };

    getSessionHeadMock.mockResolvedValueOnce(fullHead);

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.freshness === "replica");
    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 2);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: compactHead,
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.lastEventSeq === 20);

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.messages.map((message) => message.id)).toEqual(fullHead.messages.map((message) => message.id));
    expect(entry?.turns.map((turn) => turn.turn_id)).toEqual(fullHead.turns.map((turn) => turn.turn_id));
  });

  it(
    "auto-loads support for open sessions once an authoritative head revision is known",
    async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-race";
    const createdAt = new Date().toISOString();
    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 3,
      state_rev: 7,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const activeState = mkWorkspaceSnapshotState();
    activeState.activeIds = ["task-1"];
    activeState.tasksById = {
      "task-1": mkWorkspaceTaskSummary({
        taskId: "task-1",
        primarySessionId: sessionId,
        sessionIds: [sessionId],
      }),
    };
    let workspaceHead: SessionHeadSnapshot | null = null;

    const store: WorkspaceActiveSnapshotEventSource & {
      getSessionHeadsSnapshot: () => Record<string, SessionHeadSnapshot>;
    } = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: (id: string) => (id === sessionId ? workspaceHead : null),
      getSessionHeadsSnapshot: () =>
        workspaceHead ? { [sessionId]: workspaceHead } : ({} as Record<string, SessionHeadSnapshot>),
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => activeState,
    };

    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-b",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/b",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);
    listSessionSubagentInvocationsMock
      .mockResolvedValueOnce([{ id: "subagent-a" }] as never)
      .mockResolvedValueOnce([{ id: "subagent-b" }] as never);

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    const internals = asSupervisorInternals(sup);
    let resolveHead!: (value: SessionHeadSnapshot) => void;
    const headPromise = new Promise<SessionHeadSnapshot>((resolve) => {
      resolveHead = resolve;
    });
    getSessionHeadMock.mockImplementationOnce(() => headPromise);
    sup.openSession(sessionId);

    await waitForCondition(() => getSessionHeadCallsFor(sessionId).length >= 1, SUPPORT_RACE_WAIT_TIMEOUT_MS);
    expect(getSessionState).toHaveBeenCalledTimes(1);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(1);

    await waitForCondition(() => {
      const entry = internals.entries.get(sessionId);
      return entry?.support.stateLoaded === true
        && entry?.support.stateAppliedRev === undefined
        && entry?.support.subagentInvocationsLoaded === true
        && entry?.support.subagentInvocationsAppliedRev === undefined
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-a";
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    workspaceHead = head;
    sup.setWorkspaceSessionHeads({ [sessionId]: head });
    resolveHead(head);
    await flushSupportRace();
    await waitForCondition(() => {
      const entry = internals.entries.get(sessionId);
      return entry?.stateRev === 7
        && entry?.support.stateAppliedRev === 7
        && entry?.support.subagentInvocationsAppliedRev === 7
        && entry?.support.stateLoaded === true
        && entry?.support.subagentInvocationsLoaded === true
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b"
        && getSessionStateMock.mock.calls.length === 2
        && listSessionSubagentInvocationsMock.mock.calls.length === 2;
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    expect(getSessionState).toHaveBeenCalledTimes(2);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(2);
    expect(getSessionHeadCallsFor(sessionId).length).toBeGreaterThanOrEqual(1);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "reloads support after reopen only after an authoritative revision is re-established",
    async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-unknown-revision-reopen";
    const createdAt = new Date().toISOString();
    let resolveSecondState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    let resolveSecondInvocations!: (value: Array<{ id: string }>) => void;
    const secondState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveSecondState = resolve;
    });
    const secondInvocations = new Promise<Array<{ id: string }>>((resolve) => {
      resolveSecondInvocations = resolve;
    });
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockImplementationOnce(() => secondState as never)
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-c",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/c",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);
    listSessionSubagentInvocationsMock
      .mockReset()
      .mockResolvedValueOnce([{ id: "subagent-a" }] as never)
      .mockImplementationOnce(() => secondInvocations as never)
      .mockResolvedValueOnce([{ id: "subagent-c" }] as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    internals.ensureEntry(sessionId).freshness = "authoritative";

    sup.openSession(sessionId, { mode: "active" });
    sup.loadSessionState(sessionId);
    sup.loadSubagentInvocations(sessionId);

    await waitForCondition(() => {
      const entry = internals.entries.get(sessionId);
      return Boolean(
        entry?.support.stateLoaded
          && entry?.support.subagentInvocationsLoaded
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a"
          && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-a",
      );
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    sup.closeSession(sessionId);
    sup.openSession(sessionId, { mode: "active" });

    const reopened = internals.entries.get(sessionId);
    expect(reopened?.stateRev).toBeUndefined();
    expect(reopened?.support.stateLoaded).toBe(false);
    expect(reopened?.support.subagentInvocationsLoaded).toBe(false);

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          freshness: "authoritative",
          stateRev: 5,
        },
      },
    ]);

    resolveSecondState({
      artifacts: [
        {
          id: "artifact-b",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/b",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });
    resolveSecondInvocations([{ id: "subagent-b" }]);
    await flushSupportRace();

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return entry?.stateLoaded === true
        && entry?.subagentInvocationsLoaded === true
        && entry?.artifacts[0]?.absolute_path === "/tmp/c"
        && entry?.subagentInvocations[0]?.id === "subagent-c";
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    expect(getSessionState).toHaveBeenCalledTimes(3);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(3);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "does not let pre-close support requests populate a reopened session without authority",
    async () => {
      const { SessionSupervisor } = await import("./sessionSupervisor");

      const sessionId = "session-support-reopen-invalidates-inflight";
      const createdAt = new Date().toISOString();
      let resolveFirstState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
      let resolveFirstInvocations!: (value: Array<{ id: string }>) => void;
      const firstState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
        resolveFirstState = resolve;
      });
      const firstInvocations = new Promise<Array<{ id: string }>>((resolve) => {
        resolveFirstInvocations = resolve;
      });
      getSessionStateMock
        .mockImplementationOnce(() => firstState as never)
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-b",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/b",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never);
      listSessionSubagentInvocationsMock
        .mockImplementationOnce(() => firstInvocations as never)
        .mockResolvedValueOnce([{ id: "subagent-b" }] as never);

      const sup = new SessionSupervisor();
      const internals = asSupervisorInternals(sup);
      internals.ensureEntry(sessionId).freshness = "authoritative";

      sup.openSession(sessionId, { mode: "active" });

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateLoading === true
          && current?.support.subagentInvocationsLoading === true
          && getSessionStateMock.mock.calls.length === 1
          && listSessionSubagentInvocationsMock.mock.calls.length === 1;
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      sup.closeSession(sessionId);
      sup.openSession(sessionId, { mode: "active" });

      await waitForCondition(() => {
        return getSessionStateMock.mock.calls.length === 2
          && listSessionSubagentInvocationsMock.mock.calls.length === 2;
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateLoaded === true
          && current?.support.subagentInvocationsLoaded === true
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
          && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      resolveFirstState({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      });
      resolveFirstInvocations([{ id: "subagent-a" }]);
      await flushSupportRace();

      const snapshot = sup.getSnapshot().sessions[sessionId];
      expect(snapshot?.artifacts[0]?.absolute_path).toBe("/tmp/b");
      expect(snapshot?.subagentInvocations[0]?.id).toBe("subagent-b");
      expect(snapshot?.loadErrors?.state).toBeUndefined();
      expect(snapshot?.loadErrors?.subagentInvocations).toBeUndefined();
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "ignores stale support load errors after stateRev changes mid-flight",
    async () => {
      const { SessionSupervisor } = await import("./sessionSupervisor");

      const sessionId = "session-support-stale-errors-ignored";
      const createdAt = new Date().toISOString();
      let rejectFirstState!: (reason?: unknown) => void;
      let rejectFirstInvocations!: (reason?: unknown) => void;
      const firstState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>(
        (_resolve, reject) => {
          rejectFirstState = reject;
        },
      );
      const firstInvocations = new Promise<Array<{ id: string }>>((_resolve, reject) => {
        rejectFirstInvocations = reject;
      });
      getSessionStateMock
        .mockImplementationOnce(() => firstState as never)
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-b",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/b",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never);
      listSessionSubagentInvocationsMock
        .mockImplementationOnce(() => firstInvocations as never)
        .mockResolvedValueOnce([{ id: "subagent-b" }] as never);

      const sup = new SessionSupervisor();
      const internals = asSupervisorInternals(sup);
      const entry = internals.ensureEntry(sessionId);
      entry.freshness = "authoritative";

      sup.openSession(sessionId, { mode: "active" });

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateLoading === true
          && current?.support.subagentInvocationsLoading === true
          && getSessionStateMock.mock.calls.length === 1
          && listSessionSubagentInvocationsMock.mock.calls.length === 1;
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      internals.handleReplicaPatches([
        {
          op: "append",
          sessionId,
          data: {
            appendMode: "metadata_update",
            lastEventSeq: 9,
            stateRev: 9,
          },
        },
      ]);

      rejectFirstState(new Error("stale state failure"));
      rejectFirstInvocations(new Error("stale subagent failure"));
      await flushSupportRace();

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateAppliedRev === 9
          && current?.support.subagentInvocationsAppliedRev === 9
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
          && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      const snapshot = sup.getSnapshot().sessions[sessionId];
      expect(snapshot?.loadErrors?.state).toBeUndefined();
      expect(snapshot?.loadErrors?.subagentInvocations).toBeUndefined();
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "refetches support when stateRev becomes known after a revisionless support load",
    async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-rev-adopt-after-load";
    const createdAt = new Date().toISOString();
    let resolveSecondState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    let resolveSecondInvocations!: (value: Array<{ id: string }>) => void;
    const secondState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveSecondState = resolve;
    });
    const secondInvocations = new Promise<Array<{ id: string }>>((resolve) => {
      resolveSecondInvocations = resolve;
    });
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockImplementationOnce(() => secondState as never);
    listSessionSubagentInvocationsMock
      .mockResolvedValueOnce([{ id: "subagent-a" }] as never)
      .mockImplementationOnce(() => secondInvocations as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateLoaded === true
        && current?.support.stateAppliedRev === undefined
        && current?.support.subagentInvocationsLoaded === true
        && current?.support.subagentInvocationsAppliedRev === undefined
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-a";
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 9,
          stateRev: 9,
        },
      },
    ]);

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.stateRev === 9
        && current?.support.stateAppliedRev === undefined
        && current?.support.subagentInvocationsAppliedRev === undefined
        && current?.support.stateLoading === true
        && current?.support.subagentInvocationsLoading === true
        && getSessionStateMock.mock.calls.length === 2
        && listSessionSubagentInvocationsMock.mock.calls.length === 2;
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    resolveSecondState({
      artifacts: [
        {
          id: "artifact-b",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/b",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });
    resolveSecondInvocations([{ id: "subagent-b" }]);
    await flushSupportRace();

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 9
        && current?.support.subagentInvocationsAppliedRev === 9
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "refetches revisionless support when workspace heads reveal stateRev mid-flight",
    async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-workspace-head-rev-race";
    const createdAt = new Date().toISOString();
    let resolveFirstState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    let resolveFirstInvocations!: (value: Array<{ id: string }>) => void;
    let resolveSecondState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    let resolveSecondInvocations!: (value: Array<{ id: string }>) => void;
    const firstState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveFirstState = resolve;
    });
    const firstInvocations = new Promise<Array<{ id: string }>>((resolve) => {
      resolveFirstInvocations = resolve;
    });
    const secondState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveSecondState = resolve;
    });
    const secondInvocations = new Promise<Array<{ id: string }>>((resolve) => {
      resolveSecondInvocations = resolve;
    });
    getSessionStateMock
      .mockImplementationOnce(() => firstState as never)
      .mockImplementationOnce(() => secondState as never);
    listSessionSubagentInvocationsMock
      .mockImplementationOnce(() => firstInvocations as never)
      .mockImplementationOnce(() => secondInvocations as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateLoading === true
        && current?.support.subagentInvocationsLoading === true
        && getSessionStateMock.mock.calls.length === 1
        && listSessionSubagentInvocationsMock.mock.calls.length === 1;
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    const head: SessionHeadSnapshot = {
      session: mkSession(sessionId),
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [] as Message[],
      last_event_seq: 9,
      state_rev: 9,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    sup.setWorkspaceSessionHeads({ [sessionId]: head });

    resolveFirstState({
      artifacts: [
        {
          id: "artifact-a",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/a",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });
    resolveFirstInvocations([{ id: "subagent-a" }]);
    await flushSupportRace();

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === undefined
        && current?.support.subagentInvocationsAppliedRev === undefined
        && current?.support.stateLoading === true
        && current?.support.subagentInvocationsLoading === true
        && getSessionStateMock.mock.calls.length === 2
        && listSessionSubagentInvocationsMock.mock.calls.length === 2
        && sup.getSnapshot().sessions[sessionId]?.artifacts.length === 0;
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

    resolveSecondState({
      artifacts: [
        {
          id: "artifact-b",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/b",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });
    resolveSecondInvocations([{ id: "subagent-b" }]);
    await flushSupportRace();

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 9
        && current?.support.subagentInvocationsAppliedRev === 9
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
    }, SUPPORT_RACE_WAIT_TIMEOUT_MS);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it("retries failed support auto-loads after reopen when the revision is unchanged", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-known-revision-reopen";
    const createdAt = new Date().toISOString();
    getSessionStateMock
      .mockRejectedValueOnce(new Error("daemon offline"))
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-b",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/b",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);
    listSessionSubagentInvocationsMock
      .mockRejectedValueOnce(new Error("subagent query failed"))
      .mockResolvedValueOnce([{ id: "subagent-b" }] as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";
    entry.stateRev = 7;

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = sup.getSnapshot().sessions[sessionId];
      return Boolean(current?.loadErrors?.state && current?.loadErrors?.subagentInvocations);
    });

    sup.closeSession(sessionId);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 7
        && current?.support.subagentInvocationsAppliedRev === 7
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
    });

    expect(getSessionState).toHaveBeenCalledTimes(2);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(2);
  });

  it("preserves support-load caches across replica replace patches", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-replace-support-cache";
    const now = new Date().toISOString();
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";
    entry.stateRev = 7;
    entry.support.stateAppliedRev = 7;

    sup.loadSessionState(sessionId);
    sup.loadSubagentInvocations(sessionId);

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return Boolean(current?.support.stateLoaded && current?.support.subagentInvocationsLoaded);
    });

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [] as SessionTurn[],
          events: [] as SessionEvent[],
          messages: [
            {
              id: "m-replace-support-cache",
              session_id: sessionId,
              task_id: "task-1",
              role: "assistant",
              content: "replacement head",
              delivery: "immediate",
              created_at: now,
            } as Message,
          ],
          lastEventSeq: 7,
          stateRev: 7,
          hasMoreTurns: false,
        },
      },
    ]);

    const replaced = internals.entries.get(sessionId);
    expect(replaced?.support.stateLoaded).toBe(true);
    expect(replaced?.support.stateAppliedRev).toBe(7);
    expect(replaced?.support.subagentInvocationsLoaded).toBe(true);

    sup.loadSessionState(sessionId);
    sup.loadSubagentInvocations(sessionId);

    await Promise.resolve();

    expect(getSessionState).toHaveBeenCalledTimes(1);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(1);
  });

  it("keeps hasMoreTurns enabled after explicit history extension when a replace patch reports false", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-replace-preserve";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.hasMoreTurns = true;
    entry.historyExtended = true;
    entry.oldestTurnSeq = 250;
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [
            mkTurn({
              sessionId,
              turnId: "turn-keep",
              status: "completed",
              startSeq: 250,
            }),
          ],
          events: [],
          messages: [],
          lastEventSeq: 250,
          hasMoreTurns: false,
        },
      },
    ]);

    const updated = internals.entries.get(sessionId);
    expect(updated?.hasMoreTurns).toBe(true);
    expect(updated?.historyExtended).toBe(true);
    expect(updated?.oldestTurnSeq).toBe(250);
  });

  it("keeps prepended history rows across bounded replace patches after history extension", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-prepend-replace-rows";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const now = new Date(Date.UTC(2026, 3, 9, 0, 0, 0)).toISOString();
    const turn10 = mkTurn({ sessionId, turnId: "turn-10", status: "completed", startSeq: 10 });
    const turn20 = mkTurn({ sessionId, turnId: "turn-20", status: "completed", startSeq: 20 });
    const turn30 = mkTurn({ sessionId, turnId: "turn-30", status: "completed", startSeq: 30 });
    const message10 = {
      id: "message-10",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-10",
      role: "user",
      content: "history 10",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 1,
    } as Message;
    const message20 = {
      id: "message-20",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-20",
      role: "user",
      content: "history 20",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 2,
    } as Message;
    const message30 = {
      id: "message-30",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-30",
      role: "assistant",
      content: "history 30",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 3,
    } as Message;

    sup.setSession(mkSession(sessionId));
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [turn20, turn30],
          events: [],
          messages: [message20, message30],
          lastEventSeq: 30,
          hasMoreTurns: true,
        },
      },
    ]);

    getSessionHistoryMock.mockResolvedValueOnce({
      turns: [turn10],
      messages: [message10],
      has_more: false,
      next_cursor: null,
    } as never);

    await sup.loadMoreTurns(sessionId);

    const extended = internals.entries.get(sessionId);
    expect(extended?.turns.map((turn) => turn.turn_id)).toEqual(["turn-10", "turn-20", "turn-30"]);
    expect(extended?.messages.map((message) => String(message.id))).toEqual([
      "message-10",
      "message-20",
      "message-30",
    ]);
    expect(extended?.historyExtended).toBe(true);
    expect(extended?.oldestTurnSeq).toBe(10);

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [turn20, turn30],
          events: [],
          messages: [message20, message30],
          lastEventSeq: 30,
          hasMoreTurns: false,
        },
      },
    ]);

    const updated = internals.entries.get(sessionId);
    expect(updated?.turns.map((turn) => turn.turn_id)).toEqual(["turn-10", "turn-20", "turn-30"]);
    expect(updated?.messages.map((message) => String(message.id))).toEqual([
      "message-10",
      "message-20",
      "message-30",
    ]);
    expect(updated?.historyExtended).toBe(true);
    expect(updated?.hasMoreTurns).toBe(true);
    expect(updated?.oldestTurnSeq).toBe(10);
  });

  it("keeps prepended history rows across shifted bounded replace patches after history extension", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-shifted-replace-rows";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const now = new Date(Date.UTC(2026, 3, 9, 0, 0, 0)).toISOString();
    const turn10 = mkTurn({ sessionId, turnId: "turn-10", status: "completed", startSeq: 10 });
    const turn20 = mkTurn({ sessionId, turnId: "turn-20", status: "completed", startSeq: 20 });
    const turn30 = mkTurn({ sessionId, turnId: "turn-30", status: "completed", startSeq: 30 });
    const turn40 = mkTurn({ sessionId, turnId: "turn-40", status: "completed", startSeq: 40 });
    const message10 = {
      id: "message-10",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-10",
      role: "user",
      content: "history 10",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 1,
    } as Message;
    const message20 = {
      id: "message-20",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-20",
      role: "user",
      content: "history 20",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 2,
    } as Message;
    const message30 = {
      id: "message-30",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-30",
      role: "assistant",
      content: "history 30",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 3,
    } as Message;
    const message40 = {
      id: "message-40",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-40",
      role: "assistant",
      content: "history 40",
      delivery: "immediate",
      created_at: now,
      turn_sequence: 4,
    } as Message;

    sup.setSession(mkSession(sessionId));
    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [turn20, turn30],
          events: [],
          messages: [message20, message30],
          lastEventSeq: 30,
          hasMoreTurns: true,
        },
      },
    ]);

    getSessionHistoryMock.mockResolvedValueOnce({
      turns: [turn10],
      messages: [message10],
      has_more: false,
      next_cursor: null,
    } as never);

    await sup.loadMoreTurns(sessionId);

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [turn20, turn30, turn40],
          events: [],
          messages: [message20, message30, message40],
          lastEventSeq: 40,
          hasMoreTurns: false,
          replaceMode: "repair_replace",
        },
      },
    ]);

    const updated = internals.entries.get(sessionId);
    expect(updated?.turns.map((turn) => turn.turn_id)).toEqual([
      "turn-10",
      "turn-20",
      "turn-30",
      "turn-40",
    ]);
    expect(updated?.messages.map((message) => String(message.id))).toEqual([
      "message-10",
      "message-20",
      "message-30",
      "message-40",
    ]);
    expect(updated?.historyExtended).toBe(true);
    expect(updated?.hasMoreTurns).toBe(true);
    expect(updated?.oldestTurnSeq).toBe(10);
  });

  it("does not let stale active-head seed regress an interrupted turn back to running", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-head-seed-interrupted-monotonic";
    const listeners = new Set<(evt: WorkspaceActiveSnapshotEvent) => void>();
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (listener: (evt: WorkspaceActiveSnapshotEvent) => void) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId] != null);

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 1,
        head: {
          session: mkSession(sessionId),
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 })],
          events: [] as SessionEvent[],
          messages: [] as Message[],
          last_event_seq: 1,
          state_rev: 1,
          has_more_turns: false,
          has_more_history: false,
          history_cursor: null,
          head_window: {
            turn_limit: 5,
            message_limit: 200,
            event_limit: 0,
            byte_limit: 1500000,
            turn_count: 1,
            message_count: 0,
            event_count: 0,
            bytes: 0,
            truncated: false,
          },
        },
      }),
    );

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.turns[0]?.status === "running");

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "interrupted", startSeq: 1 })],
          turnsRev: 2,
          events: [
            {
              seq: 2,
              id: "event-turn-interrupted",
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "turn_interrupted",
              payload_json: {},
              created_at: new Date(2).toISOString(),
            },
          ],
          lastEventSeq: 2,
        },
      },
    ]);

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.turns[0]?.status === "interrupted");

    listeners.forEach((listener) =>
      listener({
        type: "session_head_seed",
        workspace_id: "ws-1",
        snapshot_rev: 2,
        head: {
          session: mkSession(sessionId),
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 })],
          events: [] as SessionEvent[],
          messages: [] as Message[],
          last_event_seq: 1,
          state_rev: 1,
          has_more_turns: false,
          has_more_history: false,
          history_cursor: null,
          head_window: {
            turn_limit: 5,
            message_limit: 200,
            event_limit: 0,
            byte_limit: 1500000,
            turn_count: 1,
            message_count: 0,
            event_count: 0,
            bytes: 0,
            truncated: false,
          },
        },
      }),
    );

    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("interrupted");
  });

  it("keeps an interrupted turn interrupted when ordered cancel fallout later emits error", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-ordered-interrupt-error-monotonic";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 })];
    entry.turnsHydrated = true;
    entry.freshness = "authoritative";
    entry.lastEventSeq = 1;

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "interrupted", startSeq: 1 })],
          turnsRev: 2,
          events: [
            {
              seq: 2,
              id: "event-turn-interrupted",
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "turn_interrupted",
              payload_json: { reason: "user_interrupt" },
              created_at: new Date(2).toISOString(),
            },
            {
              seq: 3,
              id: "event-provider-cancel-failed",
              session_id: sessionId,
              run_id: "run-1",
              turn_id: "turn-1",
              event_type: "turn_finished",
              payload_json: { status: "failed", message: "cancelled" },
              created_at: new Date(3).toISOString(),
            },
          ],
          lastEventSeq: 3,
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("interrupted");
  });

  it("promotes a failed turn to interrupted when authoritative turn data corrects cancel fallout", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-failed-to-interrupted-turn-correction";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId: "turn-1", status: "failed", startSeq: 1 })];
    entry.turnsHydrated = true;
    entry.freshness = "authoritative";
    entry.lastEventSeq = 1;

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          turns: [mkTurn({ sessionId, turnId: "turn-1", status: "interrupted", startSeq: 1 })],
          turnsRev: 2,
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("interrupted");
  });

  it("promotes a failed turn to interrupted when authoritative activity corrects cancel fallout", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-failed-to-interrupted-activity-correction";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId: "turn-1", status: "failed", startSeq: 1 })];
    entry.turnsHydrated = true;
    entry.freshness = "authoritative";
    entry.lastEventSeq = 1;

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          activity: { is_working: false, last_turn_status: "interrupted" },
          lastEventSeq: 2,
          projectionRev: 2,
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.turns[0]?.status).toBe("interrupted");
  });

  it("keeps authoritative interrupted activity from regressing to later failed activity for the same turn", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-activity-interrupted-sticky";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);

    entry.session = mkSession(sessionId);
    entry.turns = [mkTurn({ sessionId, turnId: "turn-1", status: "interrupted", startSeq: 1 })];
    entry.turnsHydrated = true;
    entry.freshness = "authoritative";
    entry.lastEventSeq = 2;
    entry.activity = { is_working: false, last_turn_status: "interrupted" };

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          activity: { is_working: false, last_turn_status: "failed" },
          lastEventSeq: 3,
          projectionRev: 3,
        },
      },
    ]);

    expect(sup.getSnapshot().sessions[sessionId]?.activity?.last_turn_status).toBe("interrupted");
  });

  it("refetches session state instead of reusing cache when no revision is known", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-state-unknown-revision";
    const createdAt = new Date().toISOString();
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-b",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/b",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    sup.loadSessionState(sessionId);
    await waitForCondition(() => internals.entries.get(sessionId)?.support.stateLoaded === true);

    internals.entries.delete(sessionId);

    sup.loadSessionState(sessionId);
    await waitForCondition(() => {
      const entry = internals.entries.get(sessionId);
      return entry?.support.stateLoaded === true
        && entry.support.artifacts?.[0]?.absolute_path === "/tmp/b";
    });

    expect(getSessionState).toHaveBeenCalledTimes(2);
  });

  it("refetches session state when streamed head revisions advance after a warm load", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-state-rev-warm-cache";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    sup.loadSessionState(sessionId);

    await waitForCondition(() => internals.entries.get(sessionId)?.support.stateLoaded === true);

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 7,
          stateRev: 7,
        },
      },
    ]);

    await waitForCondition(() => internals.entries.get(sessionId)?.stateRev === 7);

    sup.loadSessionState(sessionId);
    await Promise.resolve();

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 9,
          stateRev: 9,
        },
      },
    ]);

    await waitForCondition(() => internals.entries.get(sessionId)?.stateRev === 9);

    sup.loadSessionState(sessionId);
    await Promise.resolve();

    await waitForCondition(() => {
      const entry = internals.entries.get(sessionId);
      return entry?.support.stateAppliedRev === 9;
    });

    expect(getSessionState).toHaveBeenCalledTimes(3);
  });

  it(
    "retries a closed warm session state load when stateRev changes mid-flight",
    async () => {
      const { SessionSupervisor } = await import("./sessionSupervisor");

      const sessionId = "session-state-warm-manual-race";
      const createdAt = new Date().toISOString();
      let resolveFirstState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
      const firstState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
        resolveFirstState = resolve;
      });
      getSessionStateMock
        .mockImplementationOnce(() => firstState as never)
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-b",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/b",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never);

      const sup = new SessionSupervisor();
      const internals = asSupervisorInternals(sup);
      const entry = internals.ensureEntry(sessionId);
      entry.freshness = "bootstrap";
      entry.stateRev = 7;

      sup.loadSessionState(sessionId);

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateLoading === true && getSessionStateMock.mock.calls.length === 1;
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      internals.handleReplicaPatches([
        {
          op: "append",
          sessionId,
          data: {
            appendMode: "metadata_update",
            lastEventSeq: 9,
            stateRev: 9,
          },
        },
      ]);

      await waitForCondition(() => internals.entries.get(sessionId)?.stateRev === 9, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      resolveFirstState({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      });
      await flushSupportRace();

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateAppliedRev === 9
          && getSessionStateMock.mock.calls.length === 2
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it(
    "does not reuse a revisionless /state cache as authoritative when stateRev returns",
    async () => {
      const { SessionSupervisor } = await import("./sessionSupervisor");

      const sessionId = "session-state-revisionless-cache-authority-return";
      const createdAt = new Date().toISOString();
      getSessionStateMock
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-a",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/a",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never)
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-b",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/b",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never)
        .mockResolvedValueOnce({
          artifacts: [
            {
              id: "artifact-c",
              session_id: sessionId,
              task_id: "task-1",
              worktree_id: "wt-1",
              absolute_path: "/tmp/c",
              mime_type: "text/plain",
              bytes: 1,
              created_at: createdAt,
            },
          ],
          git_status: null,
        } as never);

      const sup = new SessionSupervisor();
      const internals = asSupervisorInternals(sup);
      const entry = internals.ensureEntry(sessionId);
      entry.freshness = "authoritative";
      entry.stateRev = 7;

      sup.openSession(sessionId, { mode: "active" });

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateAppliedRev === 7
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      sup.closeSession(sessionId);

      const warmEntry = internals.entries.get(sessionId);
      expect(warmEntry).toBeDefined();
      if (!warmEntry) return;
      warmEntry.freshness = "bootstrap";
      warmEntry.stateRev = undefined;

      sup.loadSessionState(sessionId, { force: true });

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateLoaded === true
          && current.support.stateAppliedRev === undefined
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      warmEntry.freshness = "authoritative";
      internals.handleReplicaPatches([
        {
          op: "append",
          sessionId,
          data: {
            appendMode: "metadata_update",
            lastEventSeq: 7,
            stateRev: 7,
          },
        },
      ]);

      sup.loadSessionState(sessionId);

      await waitForCondition(() => {
        const current = internals.entries.get(sessionId);
        return current?.support.stateAppliedRev === 7
          && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/c";
      }, SUPPORT_RACE_WAIT_TIMEOUT_MS);

      expect(getSessionStateMock).toHaveBeenCalledTimes(3);
    },
    SUPPORT_RACE_TEST_TIMEOUT_MS,
  );

  it("auto-refreshes open-session support when streamed state revisions advance", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-rev-advance";
    const createdAt = new Date().toISOString();
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-b",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/b",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);
    listSessionSubagentInvocationsMock
      .mockResolvedValueOnce([{ id: "subagent-a" }] as never)
      .mockResolvedValueOnce([{ id: "subagent-b" }] as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";
    entry.stateRev = 7;

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 7
        && current?.support.subagentInvocationsAppliedRev === 7
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-a";
    });

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 9,
          stateRev: 9,
        },
      },
    ]);

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 9
        && current?.support.subagentInvocationsAppliedRev === 9
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b"
        && sup.getSnapshot().sessions[sessionId]?.subagentInvocations[0]?.id === "subagent-b";
    });

    expect(getSessionState).toHaveBeenCalledTimes(2);
    expect(listSessionSubagentInvocations).toHaveBeenCalledTimes(2);
  });

  it("drops a revisionless /state response when stateRev changes in flight and refetches", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-state-race-revisionless-response";
    const createdAt = new Date().toISOString();
    let resolveFirstState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    let resolveSecondState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    const firstState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveFirstState = resolve;
    });
    const secondState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>((resolve) => {
      resolveSecondState = resolve;
    });
    getSessionStateMock
      .mockImplementationOnce(() => firstState as never)
      .mockImplementationOnce(() => secondState as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateLoading === true && getSessionStateMock.mock.calls.length === 1;
    });

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 9,
          stateRev: 9,
        },
      },
    ]);

    await waitForCondition(() => internals.entries.get(sessionId)?.stateRev === 9);

    resolveFirstState({
      artifacts: [
        {
          id: "artifact-a",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/a",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });
    await flushSupportRace();

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current != null
        && current.support.stateAppliedRev === undefined
        && current.support.stateLoading === true
        && getSessionStateMock.mock.calls.length === 2
        && sup.getSnapshot().sessions[sessionId]?.artifacts.length === 0;
    });

    resolveSecondState({
      artifacts: [
        {
          id: "artifact-b",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/b",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 9
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b";
    });

    expect(getSessionStateMock).toHaveBeenCalledTimes(2);
  });

  it("keeps artifactsLoading false during background /state refreshes once artifacts are loaded", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-artifacts-background-refresh";
    const createdAt = new Date().toISOString();
    let resolveSecondState!: (value: { artifacts: Array<Record<string, unknown>>; git_status: null }) => void;
    const secondState = new Promise<{ artifacts: Array<Record<string, unknown>>; git_status: null }>(
      (resolve) => {
        resolveSecondState = resolve;
      },
    );
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-a",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/a",
            mime_type: "text/plain",
            bytes: 1,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never)
      .mockImplementationOnce(() => secondState as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";
    entry.stateRev = 7;

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 7
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/a";
    });

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          lastEventSeq: 9,
          stateRev: 9,
        },
      },
    ]);

    await waitForCondition(() => internals.entries.get(sessionId)?.support.stateLoading === true);

    const refreshingSnapshot = sup.getSnapshot().sessions[sessionId];
    expect(refreshingSnapshot?.stateLoading).toBe(true);
    expect(refreshingSnapshot?.artifactsLoading).toBe(false);

    resolveSecondState({
      artifacts: [
        {
          id: "artifact-b",
          session_id: sessionId,
          task_id: "task-1",
          worktree_id: "wt-1",
          absolute_path: "/tmp/b",
          mime_type: "text/plain",
          bytes: 1,
          created_at: createdAt,
        },
      ],
      git_status: null,
    });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 9
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/b";
    });
  });

  it("refreshes artifacts from /state after the artifacts pane has already loaded empty", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-artifacts-state-refresh";
    const createdAt = new Date().toISOString();
    getSessionStateMock
      .mockResolvedValueOnce({ artifacts: [], git_status: null } as never)
      .mockResolvedValueOnce({
        artifacts: [
          {
            id: "artifact-1",
            session_id: sessionId,
            task_id: "task-1",
            worktree_id: "wt-1",
            absolute_path: "/tmp/chart.png",
            mime_type: "image/png",
            bytes: 128,
            created_at: createdAt,
          },
        ],
        git_status: null,
      } as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.freshness = "authoritative";
    entry.stateRev = 0;

    sup.openSession(sessionId, { mode: "active" });

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 0
        && current.support.stateLoaded === true
        && sup.getSnapshot().sessions[sessionId]?.artifacts.length === 0;
    });

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          eventsRev: 1,
          lastEventSeq: 1,
          stateRev: 1,
        },
      },
    ]);

    await waitForCondition(() => {
      const current = internals.entries.get(sessionId);
      return current?.support.stateAppliedRev === 1
        && sup.getSnapshot().sessions[sessionId]?.artifacts[0]?.absolute_path === "/tmp/chart.png";
    });

    expect(getSessionStateMock).toHaveBeenCalledTimes(2);
  });

  it("skips /head hydrate for active sessions when snapshot store is bound", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-active-no-head";
    getSessionHeadMock.mockRejectedValue(new Error("Load failed"));
    getSessionSnapshotMock.mockRejectedValue(new Error("Load failed"));

    const now = new Date().toISOString();
    const activeState = mkWorkspaceSnapshotState();
    activeState.activeIds = ["task-active"];
    activeState.tasksById = {
      "task-active": {
        id: "task-active",
        task: {
          id: "task-active",
          workspace_id: "ws-1",
          title: "Active",
          status: "running",
          primary_session_id: sessionId,
          created_at: now,
          updated_at: now,
          archived_at: null,
        },
        sessions: [
          {
            session: mkSession(sessionId),
          },
        ],
        primarySessionHead: null,
        sortAtMs: Date.parse(now),
      },
    };
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => activeState,
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId);

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return Boolean(entry && !entry.loading);
    });

    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.error).toBeUndefined();
    expect(getSessionHead).not.toHaveBeenCalled();
    expect(getSessionSnapshot).not.toHaveBeenCalled();
  });

  it("hydrates /head for archived sessions", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-archived-head";
    const now = new Date().toISOString();
    const headMessage: Message = {
      id: "m-archived",
      session_id: sessionId,
      task_id: "task-archived",
      role: "assistant",
      content: "archived",
      delivery: "immediate",
      created_at: now,
    };
    getSessionHeadMock.mockResolvedValue({
      session: { ...mkSession(sessionId), task_id: "task-archived", status: "completed" },
      turns: [] as SessionTurn[],
      events: [] as SessionEvent[],
      messages: [headMessage],
      last_event_seq: 1,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    });

    const archivedState = mkWorkspaceSnapshotState();
    archivedState.archivedIds = ["task-archived"];
    archivedState.tasksById = {
      "task-archived": {
        id: "task-archived",
        task: {
          id: "task-archived",
          workspace_id: "ws-1",
          title: "Archived",
          status: "completed",
          primary_session_id: sessionId,
          created_at: now,
          updated_at: now,
          archived_at: now,
        },
        sessions: [
          {
            session: { ...mkSession(sessionId), task_id: "task-archived", status: "completed" },
          },
        ],
        primarySessionHead: null,
        sortAtMs: Date.parse(now),
      },
    };
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => archivedState,
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId);

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.messages.length === 1);

    expect(
      getSessionHeadMock.mock.calls.filter(([calledSessionId]) => calledSessionId === sessionId),
    ).toHaveLength(1);
  });

  it("marks archived hydrate failures as fatal", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-archived-fatal";
    const now = new Date().toISOString();
    getSessionHeadMock.mockRejectedValue(new Error("Load failed"));

    const archivedState = mkWorkspaceSnapshotState();
    archivedState.archivedIds = ["task-archived-fatal"];
    archivedState.tasksById = {
      "task-archived-fatal": {
        id: "task-archived-fatal",
        task: {
          id: "task-archived-fatal",
          workspace_id: "ws-1",
          title: "Archived fatal",
          status: "completed",
          primary_session_id: sessionId,
          created_at: now,
          updated_at: now,
          archived_at: now,
        },
        sessions: [
          {
            session: { ...mkSession(sessionId), task_id: "task-archived-fatal", status: "completed" },
          },
        ],
        primarySessionHead: null,
        sortAtMs: Date.parse(now),
      },
    };

    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => archivedState,
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId);

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.loadState === "fatal");
    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.error).toContain("Load failed");
    expect(
      getSessionHeadMock.mock.calls.filter(([calledSessionId]) => calledSessionId === sessionId),
    ).toHaveLength(1);
  });

  it("does not fallback to /head for unknown sessions and marks fatal after bounded resolution", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-unknown";
    const store: WorkspaceActiveSnapshotEventSource = {
      subscribe: () => () => {},
      subscribeEvents: (_listener: (evt: WorkspaceActiveSnapshotEvent) => void) => () => {},
      getSessionHeadSnapshot: () => null,
      getWorktreeRoot: () => null,
      setSubscribedSessions: () => {},
      getSnapshot: () => mkWorkspaceSnapshotState(),
    };

    const sup = new SessionSupervisor();
    attachWorkspaceStore(sup, store);
    sup.openSession(sessionId);

    await waitForCondition(() => sup.getSnapshot().sessions[sessionId]?.loadState === "fatal");
    const entry = sup.getSnapshot().sessions[sessionId];
    expect(entry?.error).toContain("Session not found in workspace snapshot");
    expect(getSessionHead).not.toHaveBeenCalled();
  });

  it("records support load failures and clears them after a successful retry", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-support-load-errors";
    getSessionStateMock
      .mockRejectedValueOnce(new Error("daemon offline"))
      .mockResolvedValueOnce({ artifacts: [], git_status: null });
    listSessionSubagentInvocationsMock
      .mockRejectedValueOnce(new Error("subagent query failed"))
      .mockResolvedValueOnce([]);

    const sup = new SessionSupervisor();
    sup.openSession(sessionId, { mode: "active" });

    sup.loadSessionState(sessionId);
    sup.loadSubagentInvocations(sessionId);

    await waitForCondition(() => {
      const loadErrors = sup.getSnapshot().sessions[sessionId]?.loadErrors;
      return Boolean(loadErrors?.state && loadErrors?.subagentInvocations);
    });

    const failedEntry = sup.getSnapshot().sessions[sessionId];
    expect(failedEntry?.stateLoading).toBe(false);
    expect(failedEntry?.artifactsLoading).toBe(false);
    expect(failedEntry?.subagentInvocationsLoading).toBe(false);
    expect(failedEntry?.loadErrors?.state).toBe("Failed to load session state: daemon offline");
    expect(failedEntry?.loadErrors?.subagentInvocations).toBe(
      "Failed to load subagent invocations: subagent query failed",
    );

    sup.loadSessionState(sessionId, { force: true });
    sup.loadSubagentInvocations(sessionId, { force: true });

    await waitForCondition(() => {
      const entry = sup.getSnapshot().sessions[sessionId];
      return (
        Boolean(entry?.stateLoaded) &&
        Boolean(entry?.loadErrors) &&
        !entry?.loadErrors?.state &&
        !entry?.loadErrors?.subagentInvocations &&
        entry?.artifactsLoading === false &&
        entry?.subagentInvocationsLoading === false
      );
    });

    const recoveredEntry = sup.getSnapshot().sessions[sessionId];
    expect(recoveredEntry?.artifacts).toEqual([]);
    expect(recoveredEntry?.subagentInvocations).toEqual([]);
  });

  it("loads and saves session history pages with workspace owner scope", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-scope";
    loadSessionHistoryPageV1Mock.mockResolvedValueOnce(null);
    getSessionHistoryMock.mockResolvedValueOnce({
      turns: [],
      messages: [],
      has_more: false,
      next_cursor: null,
    } as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    sup.setSession(mkSession(sessionId));
    const entry = internals.ensureEntry(sessionId);
    entry.hasMoreTurns = true;
    entry.oldestTurnSeq = 10;

    await sup.loadMoreTurns(sessionId);

    expect(loadSessionHistoryPageV1Mock).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "workspace",
        workspaceId: "ws-1",
        daemon: { kind: "browser", baseUrl: "http://daemon.test" },
      }),
      sessionId,
      10,
      60,
    );
    expect(saveSessionHistoryPageV1Mock).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "workspace",
        workspaceId: "ws-1",
        daemon: { kind: "browser", baseUrl: "http://daemon.test" },
      }),
      sessionId,
      10,
      60,
      expect.objectContaining({ has_more: false }),
    );
  });

  it("uses the oldest turn from replica hydration as the history cursor", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-cursor-from-replica";
    getSessionHistoryMock.mockResolvedValueOnce({
      turns: [],
      messages: [],
      has_more: false,
      next_cursor: null,
    } as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    sup.setSession(mkSession(sessionId));

    internals.handleReplicaPatches([
      {
        op: "replace",
        sessionId,
        data: {
          session: mkSession(sessionId),
          turns: [
            mkTurn({
              sessionId,
              turnId: "turn-oldest",
              status: "completed",
              startSeq: 50,
            }),
            mkTurn({
              sessionId,
              turnId: "turn-latest",
              status: "completed",
              startSeq: 70,
            }),
          ],
          events: [],
          messages: [],
          lastEventSeq: 70,
          hasMoreTurns: true,
        },
      },
    ]);

    const hydrated = internals.entries.get(sessionId);
    expect(hydrated?.hasMoreTurns).toBe(true);
    expect(hydrated?.oldestTurnSeq).toBe(50);

    await sup.loadMoreTurns(sessionId);

    expect(getSessionHistoryMock).toHaveBeenCalledWith(sessionId, 50, 60);
  });

  it("loads and saves task thought caches with workspace owner scope", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-thought-scope";
    const createdAt = new Date().toISOString();
    loadTaskThoughtsV1Mock.mockResolvedValueOnce({
      v: 1,
      taskId: "task-1",
      sessions: {},
      updatedAtMs: Date.now(),
    });

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    sup.setSession(mkSession(sessionId));

    internals.handleReplicaPatches([
      {
        op: "append",
        sessionId,
        data: {
          appendMode: "metadata_update",
          session: mkSession(sessionId),
          events: [
            {
              id: "evt-thought-1",
              session_id: sessionId,
              turn_id: "turn-1",
              event_type: "thought_chunk",
              payload_json: {
                item_id: "item-1",
                full_content: "final thought",
                is_final: true,
              },
              created_at: createdAt,
              seq: 1,
            },
          ],
          lastEventSeq: 1,
        },
      },
    ]);

    await waitForCondition(() => saveTaskThoughtsV1Mock.mock.calls.length > 0);

    expect(loadTaskThoughtsV1Mock).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "workspace",
        workspaceId: "ws-1",
        daemon: { kind: "browser", baseUrl: "http://daemon.test" },
      }),
      "task-1",
    );
    expect(saveTaskThoughtsV1Mock).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "workspace",
        workspaceId: "ws-1",
        daemon: { kind: "browser", baseUrl: "http://daemon.test" },
      }),
      "task-1",
      expect.objectContaining({
        sessions: expect.objectContaining({
          [sessionId]: expect.objectContaining({
            sessionId,
          }),
        }),
      }),
    );
  });

  it("does not collapse history pagination when oldest cursor is unavailable", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-history-no-cursor";
    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);
    const entry = internals.ensureEntry(sessionId);
    entry.hasMoreTurns = true;
    entry.oldestTurnSeq = undefined;
    entry.historyExtended = false;

    const result = await sup.loadMoreTurns(sessionId);

    expect(result).toBe(0);
    expect(entry.hasMoreTurns).toBe(true);
    expect(entry.historyExtended).toBe(false);
    expect(loadSessionHistoryPageV1Mock).not.toHaveBeenCalled();
    expect(getSessionHistoryMock).not.toHaveBeenCalled();
  });

  it("clears cached git status when a fresh state response omits it", async () => {
    const { SessionSupervisor } = await import("./sessionSupervisor");

    const sessionId = "session-state-clears-git-status";
    getSessionStateMock
      .mockResolvedValueOnce({
        artifacts: [],
        git_status: {
          summary_line: "main +1",
          branch: "main",
          upstream: "origin/main",
          ahead: 1,
          behind: 0,
          detached: false,
          staged: 0,
          unstaged: 1,
          untracked: 0,
        },
      } as never)
      .mockResolvedValueOnce({
        artifacts: [],
        git_status: null,
      } as never);

    const sup = new SessionSupervisor();
    const internals = asSupervisorInternals(sup);

    sup.loadSessionState(sessionId);
    await waitForCondition(() => internals.entries.get(sessionId)?.support.stateLoaded === true);

    sup.loadSessionState(sessionId, { force: true });
    await waitForCondition(() => internals.stateCacheBySessionId.get(sessionId)?.state.git_status === null);

    expect(sup.getSnapshot().sessions[sessionId]?.gitStatusSummary).toBeNull();
    expect(internals.stateCacheBySessionId.get(sessionId)?.state.git_status).toBeNull();
  });
});
