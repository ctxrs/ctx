import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionHeadSnapshot, SessionSnapshotSummary } from "@ctx/types";
import { SessionHeadBootstrapCache } from "../../state/sessionHeadBootstrapCache";
import type { WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import {
  buildWorkspaceSyncPrefetchVersionKey,
  collectAuthoritativePrefetchReadySessionIds,
  collectSessionHeadsForSupervisor,
  maybeCacheSessionHeadSeed,
  planSessionHeadPrefetchTargets,
  primeAuthoritativeSessionHeads,
  primePersistedSessionHeads,
  SESSION_HEAD_PREFETCH_CONCURRENCY,
  SESSION_HEAD_PREFETCH_TARGET_LIMIT,
} from "./sessionHeadPrefetch";

const loadSessionHeadV1Mock = vi.fn();
const getSessionHeadMock = vi.fn();

vi.mock("../../state/uiStateStore", () => ({
  loadSessionHeadV1: (...args: unknown[]) => loadSessionHeadV1Mock(...args),
}));

vi.mock("../../api/clientSessions", () => ({
  getSessionHead: (...args: unknown[]) => getSessionHeadMock(...args),
}));

const now = "2026-03-18T00:00:00.000Z";

const makeHead = (
  sessionId: string,
  opts?: {
    turnCount?: number;
    lastEventSeq?: number;
    turnStatus?: "queued" | "starting" | "running" | "completed" | "interrupted" | "failed";
  },
): SessionHeadSnapshot => {
  const turnCount = opts?.turnCount ?? 1;
  const lastEventSeq = opts?.lastEventSeq ?? turnCount;
  const turnStatus = opts?.turnStatus ?? "completed";
  return {
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
    turns: Array.from({ length: turnCount }, (_, index) => ({
      turn_id: `turn-${index + 1}`,
      session_id: sessionId,
      run_id: null,
      user_message_id: `msg-${index + 1}`,
      status: turnStatus,
      start_seq: index + 1,
      end_seq: index + 2,
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
    })),
    messages: Array.from({ length: turnCount }, (_, index) => ({
      id: `message-${index + 1}`,
      session_id: sessionId,
      task_id: "task-1",
      turn_id: `turn-${index + 1}`,
      role: index % 2 === 0 ? "user" : "assistant",
      content: `message-${index + 1}`,
      delivery: "immediate",
      created_at: now,
      updated_at: now,
    })),
    events: [],
    last_event_seq: lastEventSeq,
    has_more_turns: turnCount < 3,
    has_more_history: turnCount < 3,
    history_cursor: turnCount < 3 ? 1 : null,
  };
};

const makeSnapshot = (
  sessionId: string,
  opts?: {
    lastEventSeq?: number;
    projectionRev?: number;
    stateRev?: number;
    activity?: SessionSnapshotSummary["activity"];
  },
): WorkspaceActiveSnapshotState => ({
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
        primary_session_id: sessionId,
      },
      sessions: [
        {
          session: makeHead(sessionId).session,
          last_message_at: now,
          last_message_preview: "preview",
          last_event_seq: opts?.lastEventSeq ?? 1,
          projection_rev: opts?.projectionRev,
          state_rev: opts?.stateRev,
          activity: opts?.activity ?? { is_working: false, last_turn_status: null },
          unread: false,
        },
      ],
      primarySessionId: sessionId,
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

const makeSnapshotWithSessions = (
  sessionIds: readonly string[],
  opts?: { lastEventSeq?: number; projectionRev?: number; stateRev?: number },
): WorkspaceActiveSnapshotState => {
  const primarySessionId = sessionIds[0] ?? "session-1";
  return {
    ...makeSnapshot(primarySessionId, opts),
    tasksById: {
      "task-1": {
        ...makeSnapshot(primarySessionId, opts).tasksById["task-1"],
        sessions: sessionIds.map((sessionId) => ({
          session: makeHead(sessionId).session,
          last_message_at: now,
          last_message_preview: "preview",
          last_event_seq: opts?.lastEventSeq ?? 1,
          projection_rev: opts?.projectionRev,
          state_rev: opts?.stateRev,
          activity: { is_working: false, last_turn_status: null },
          unread: false,
        })),
      },
    },
  };
};

describe("sessionHeadPrefetch", () => {
  beforeEach(() => {
    loadSessionHeadV1Mock.mockReset();
    loadSessionHeadV1Mock.mockResolvedValue(null);
    getSessionHeadMock.mockReset();
    getSessionHeadMock.mockResolvedValue(null);
  });

  it("loads persisted heads into the bootstrap cache once for active primary sessions", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const persistedHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
    loadSessionHeadV1Mock.mockResolvedValue({
      v: 1,
      sessionId,
      head: {
        ...persistedHead,
        has_more_history: undefined,
        history_cursor: undefined,
      },
      updatedAtMs: Date.now(),
    });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const firstChanged = await primePersistedSessionHeads(snapshot, store, bootstrapCache);
    const secondChanged = await primePersistedSessionHeads(snapshot, store, bootstrapCache);

    expect(firstChanged).toBe(true);
    expect(secondChanged).toBe(false);
    expect(loadSessionHeadV1Mock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.snapshot()[sessionId]?.turns).toHaveLength(2);
  });

  it("retries persisted prefetch after a canceled generation keeps the session retained", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const persistedHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
    let resolvePersisted: ((value: unknown) => void) | null = null;
    loadSessionHeadV1Mock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolvePersisted = resolve;
        }),
    );
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };
    let shouldContinue = true;

    const first = primePersistedSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => shouldContinue,
    });
    shouldContinue = false;
    if (!resolvePersisted) {
      throw new Error("Expected persisted resolver");
    }
    const resolveFirstPersisted = resolvePersisted as (value: unknown) => void;
    resolveFirstPersisted({
      v: 1,
      sessionId,
      head: {
        ...persistedHead,
        has_more_history: undefined,
        history_cursor: undefined,
      },
      updatedAtMs: Date.now(),
    });
    await first;

    shouldContinue = true;
    loadSessionHeadV1Mock.mockResolvedValue({
      v: 1,
      sessionId,
      head: {
        ...persistedHead,
        has_more_history: undefined,
        history_cursor: undefined,
      },
      updatedAtMs: Date.now(),
    });

    const secondChanged = await primePersistedSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => shouldContinue,
    });

    expect(secondChanged).toBe(true);
    expect(loadSessionHeadV1Mock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.turns).toHaveLength(2);
  });

  it("retries persisted prefetch when a newer generation overlaps an older canceled load", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const persistedHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
    let resolveFirstPersisted: ((value: unknown) => void) | null = null;
    let loadCalls = 0;
    loadSessionHeadV1Mock.mockImplementation(() => {
      loadCalls += 1;
      if (loadCalls === 1) {
        return new Promise((resolve) => {
          resolveFirstPersisted = resolve;
        });
      }
      return Promise.resolve({
        v: 1,
        sessionId,
        head: {
          ...persistedHead,
          has_more_history: undefined,
          history_cursor: undefined,
        },
        updatedAtMs: Date.now(),
      });
    });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };
    let firstShouldContinue = true;

    const first = primePersistedSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => firstShouldContinue,
    });
    await vi.waitFor(() => {
      expect(loadSessionHeadV1Mock).toHaveBeenCalledTimes(1);
    });

    const second = primePersistedSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => true,
    });

    firstShouldContinue = false;
    if (!resolveFirstPersisted) {
      throw new Error("Expected first persisted resolver");
    }
    const finishFirstPersisted = resolveFirstPersisted as (value: unknown) => void;
    finishFirstPersisted({
      v: 1,
      sessionId,
      head: {
        ...persistedHead,
        has_more_history: undefined,
        history_cursor: undefined,
      },
      updatedAtMs: Date.now(),
    });

    const firstChanged = await first;
    await vi.waitFor(() => {
      expect(loadSessionHeadV1Mock).toHaveBeenCalledTimes(2);
    });
    const secondChanged = await second;

    expect(firstChanged).toBe(false);
    expect(secondChanged).toBe(true);
    expect(bootstrapCache.get(sessionId)?.turns).toHaveLength(2);
  });

  it("evicts bootstrap heads that fall out of the current target set", () => {
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(makeHead("session-1", { turnCount: 2, lastEventSeq: 2 }));
    bootstrapCache.upsert(makeHead("session-2", { turnCount: 2, lastEventSeq: 2 }));

    bootstrapCache.retain(["session-2"]);

    expect(Object.keys(bootstrapCache.snapshot())).toEqual(["session-2"]);
    expect(bootstrapCache.get("session-1")).toBeUndefined();
    expect(bootstrapCache.get("session-2")?.turns).toHaveLength(2);
  });

  it("prefers the richer bootstrap cached head over a narrower direct store head", () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const directHead = makeHead(sessionId, { turnCount: 1, lastEventSeq: 2 });
    const persistedHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(persistedHead);
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache);

    expect(heads[sessionId]?.turns).toHaveLength(2);
    expect(heads[sessionId]?.messages).toHaveLength(2);
  });

  it("compacts oversized bootstrap heads before merging them into supervisor state", () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const directHead = makeHead(sessionId, { turnCount: 1, lastEventSeq: 8 });
    const persistedHead = makeHead(sessionId, { turnCount: 8, lastEventSeq: 8 });
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(persistedHead);
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache);

    expect(heads[sessionId]?.turns).toHaveLength(5);
    expect(heads[sessionId]?.messages).toHaveLength(5);
  });

  it("collects only the requested session heads when an explicit target list is provided", () => {
    const sessionId = "session-1";
    const otherSessionId = "session-2";
    const snapshot = makeSnapshot(sessionId);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn((id: string) => {
        if (id === sessionId) return makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
        if (id === otherSessionId) return makeHead(otherSessionId, { turnCount: 3, lastEventSeq: 3 });
        return null;
      }),
      getSessionHeadsSnapshot: vi.fn(() => ({
        [sessionId]: makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 }),
        [otherSessionId]: makeHead(otherSessionId, { turnCount: 3, lastEventSeq: 3 }),
      })),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache, [sessionId]);

    expect(Object.keys(heads)).toEqual([sessionId]);
    expect(heads[sessionId]?.turns).toHaveLength(2);
  });

  it("does not keep bootstrap-only heads when the explicit target list is empty", () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId);
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 }));
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache, []);

    expect(heads).toEqual({});
  });

  it("keeps bootstrap heads scoped to the current snapshot sessions when no explicit target list is provided", () => {
    const sessionId = "session-1";
    const offSnapshotSessionId = "session-2";
    const snapshot = makeSnapshot(sessionId);
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 }));
    bootstrapCache.upsert(makeHead(offSnapshotSessionId, { turnCount: 2, lastEventSeq: 2 }));
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache);

    expect(Object.keys(heads)).toEqual([sessionId]);
  });

  it("rejects bootstrap heads that lag the current summary cursor", () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 5 });
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(makeHead(sessionId, { turnCount: 3, lastEventSeq: 3 }));
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const heads = collectSessionHeadsForSupervisor(snapshot, store, bootstrapCache);

    expect(heads).toEqual({});
  });

  it("prefetches an authoritative head when the summary is newer than known heads", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 5 });
    const authoritativeHead = makeHead(sessionId, { turnCount: 3, lastEventSeq: 5 });
    getSessionHeadMock.mockResolvedValue(authoritativeHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const firstChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId]);
    const secondChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId]);

    expect(firstChanged).toBe(true);
    expect(secondChanged).toBe(false);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.snapshot()[sessionId]?.last_event_seq).toBe(5);
  });

  it("rejects authoritative heads that become stale against the latest summary before completion", async () => {
    const sessionId = "session-1";
    const initialSnapshot = makeSnapshot(sessionId, { lastEventSeq: 1 });
    const latestSnapshot = makeSnapshot(sessionId, { lastEventSeq: 5 });
    getSessionHeadMock.mockResolvedValue(makeHead(sessionId, { turnCount: 3, lastEventSeq: 3 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const onHead = vi.fn();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const changed = await primeAuthoritativeSessionHeads(initialSnapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => latestSnapshot,
      onHead,
    });

    expect(changed).toBe(false);
    expect(onHead).not.toHaveBeenCalled();
    expect(bootstrapCache.get(sessionId)).toBeUndefined();
  });

  it("coalesces ctx-ui sized summary churn behind the in-flight authoritative session head", async () => {
    const sessionId = "session-1";
    const summaryVersionCount = 512;
    const latestSnapshot = makeSnapshot(sessionId, { lastEventSeq: summaryVersionCount });
    const authoritativeHead = makeHead(sessionId, {
      turnCount: 3,
      lastEventSeq: summaryVersionCount,
    });

    let resolveFetch: ((value: SessionHeadSnapshot) => void) | null = null;
    getSessionHeadMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveFetch = resolve as (value: SessionHeadSnapshot) => void;
        }),
    );
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const pending = Array.from({ length: summaryVersionCount }, (_, index) =>
      primeAuthoritativeSessionHeads(
        makeSnapshot(sessionId, { lastEventSeq: index + 1 }),
        store,
        bootstrapCache,
        [sessionId],
        { getSnapshot: () => latestSnapshot },
      ));
    await vi.waitFor(() => {
      expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    });
    await Promise.resolve();

    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    if (!resolveFetch) {
      throw new Error("Expected authoritative resolver");
    }
    const finishFetch = resolveFetch as (value: SessionHeadSnapshot) => void;
    finishFetch(authoritativeHead);

    const results = await Promise.all(pending);
    expect(results.filter(Boolean)).toHaveLength(1);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(summaryVersionCount);
  });

  it("does not turn queued ctx-ui sized summary churn into a sequential authoritative head fetch storm", async () => {
    const sessionId = "session-1";
    const summaryVersionCount = 512;
    const latestSnapshot = makeSnapshot(sessionId, { lastEventSeq: summaryVersionCount });
    let fetchCalls = 0;
    getSessionHeadMock.mockImplementation(async () => {
      fetchCalls += 1;
      await Promise.resolve();
      return makeHead(sessionId, {
        turnCount: 3,
        lastEventSeq: Math.min(fetchCalls, summaryVersionCount - 1),
      });
    });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const pending = Array.from({ length: summaryVersionCount }, (_, index) =>
      primeAuthoritativeSessionHeads(
        makeSnapshot(sessionId, { lastEventSeq: index + 1 }),
        store,
        bootstrapCache,
        [sessionId],
        { getSnapshot: () => latestSnapshot },
      ));

    const results = await Promise.all(pending);

    expect(results.some(Boolean)).toBe(false);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.get(sessionId)).toBeUndefined();
  });

  it("lets foreground force bypass a stale authoritative head cooldown", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    getSessionHeadMock
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 3, lastEventSeq: 3 }))
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const staleChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => snapshot,
      reason: "summary_repair",
    });
    const forceChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
      getSnapshot: () => snapshot,
      reason: "foreground_force",
    });

    expect(staleChanged).toBe(false);
    expect(forceChanged).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("retries summary repair after an authoritative fetch races with a stale head", async () => {
    const sessionId = "session-1";
    const staleSnapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    const completedSnapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    getSessionHeadMock
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 1, lastEventSeq: 3, turnStatus: "running" }))
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const staleChanged = await primeAuthoritativeSessionHeads(staleSnapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => staleSnapshot,
      reason: "summary_repair",
    });
    const repairChanged = await primeAuthoritativeSessionHeads(completedSnapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => completedSnapshot,
      reason: "summary_repair",
    });

    expect(staleChanged).toBe(false);
    expect(repairChanged).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("lets summary repair run after an early foreground force fetch fails", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    getSessionHeadMock
      .mockRejectedValueOnce(new Error("Session not found"))
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const forceChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
      getSnapshot: () => snapshot,
      reason: "foreground_force",
    });
    const repairChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => snapshot,
      reason: "summary_repair",
    });

    expect(forceChanged).toBe(false);
    expect(repairChanged).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("lets foreground force suppress cooldown on an in-flight non-forced fetch", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    let rejectFirstFetch: ((error: Error) => void) | null = null;
    getSessionHeadMock
      .mockImplementationOnce(
        () =>
          new Promise((_resolve, reject) => {
            rejectFirstFetch = reject as (error: Error) => void;
          }),
      )
      .mockResolvedValueOnce(makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const firstRepair = primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => snapshot,
      reason: "summary_repair",
    });
    await vi.waitFor(() => {
      expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    });

    const foregroundForce = primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
      getSnapshot: () => snapshot,
      reason: "foreground_force",
    });
    await Promise.resolve();
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);

    if (!rejectFirstFetch) {
      throw new Error("Expected first authoritative rejecter");
    }
    const rejectFetch = rejectFirstFetch as (error: Error) => void;
    rejectFetch(new Error("Session not found"));

    const [firstChanged, forceChanged] = await Promise.all([firstRepair, foregroundForce]);
    const repairChanged = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      getSnapshot: () => snapshot,
      reason: "summary_repair",
    });

    expect(firstChanged).toBe(false);
    expect(forceChanged).toBe(false);
    expect(repairChanged).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("retries same-version authoritative prefetch when a newer generation overlaps an older canceled load", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 5 });
    const authoritativeHead = makeHead(sessionId, { turnCount: 3, lastEventSeq: 5 });
    let resolveFirstFetch: ((value: SessionHeadSnapshot) => void) | null = null;
    let fetchCalls = 0;
    getSessionHeadMock.mockImplementation(() => {
      fetchCalls += 1;
      if (fetchCalls === 1) {
        return new Promise((resolve) => {
          resolveFirstFetch = resolve as (value: SessionHeadSnapshot) => void;
        });
      }
      return Promise.resolve(authoritativeHead);
    });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };
    let firstShouldContinue = true;

    const first = primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => firstShouldContinue,
    });
    await vi.waitFor(() => {
      expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    });

    const second = primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldContinue: () => true,
    });

    firstShouldContinue = false;
    if (!resolveFirstFetch) {
      throw new Error("Expected first authoritative resolver");
    }
    const finishFirstFetch = resolveFirstFetch as (value: SessionHeadSnapshot) => void;
    finishFirstFetch(authoritativeHead);

    const firstChanged = await first;
    await vi.waitFor(() => {
      expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    });
    const secondChanged = await second;

    expect(firstChanged).toBe(false);
    expect(secondChanged).toBe(true);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(5);
  });

  it("rejects authoritative heads when the retained target set drops before completion", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 1 });
    let resolveFetch: ((value: SessionHeadSnapshot) => void) | null = null;
    getSessionHeadMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveFetch = resolve as (value: SessionHeadSnapshot) => void;
        }),
    );
    const bootstrapCache = new SessionHeadBootstrapCache();
    const onHead = vi.fn();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };
    let shouldRetain = true;

    const pending = primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      shouldRetainSessionId: () => shouldRetain,
      onHead,
    });
    await vi.waitFor(() => {
      expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    });

    shouldRetain = false;
    if (!resolveFetch) {
      throw new Error("Expected authoritative resolver");
    }
    const finishFetch = resolveFetch as (value: SessionHeadSnapshot) => void;
    finishFetch(makeHead(sessionId, { turnCount: 3, lastEventSeq: 3 }));

    const changed = await pending;

    expect(changed).toBe(false);
    expect(onHead).not.toHaveBeenCalled();
    expect(bootstrapCache.get(sessionId)).toBeUndefined();
  });

  it("caches compacted heads but publishes the fetched authoritative head to downstream consumers", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    getSessionHeadMock.mockResolvedValue(makeHead(sessionId, { turnCount: 8, lastEventSeq: 8 }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const onHead = vi.fn();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      onHead,
    });

    expect(changed).toBe(true);
    expect(bootstrapCache.get(sessionId)?.turns).toHaveLength(5);
    const publishedHead = onHead.mock.calls[0]?.[1] as SessionHeadSnapshot | undefined;
    expect(publishedHead?.turns).toHaveLength(8);
    expect(publishedHead?.messages).toHaveLength(8);
  });

  it("publishes fetched authoritative heads even when the bootstrap cache is already current", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    const authoritativeHead = makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 });
    getSessionHeadMock.mockResolvedValue(authoritativeHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    bootstrapCache.upsert(authoritativeHead);
    const onHead = vi.fn();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
      onHead,
    });

    expect(changed).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(onHead).toHaveBeenCalledWith(
      sessionId,
      expect.objectContaining({ last_event_seq: 8 }),
    );
  });

  it("prefetches an authoritative head when the direct head cursor matches but activity is stale", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 8 });
    const directHead = {
      ...makeHead(sessionId, { turnCount: 1, lastEventSeq: 8, turnStatus: "running" }),
      activity: { is_working: true, last_turn_status: "running" },
    } satisfies SessionHeadSnapshot;
    const authoritativeHead = makeHead(sessionId, { turnCount: 1, lastEventSeq: 8 });
    getSessionHeadMock.mockResolvedValue(authoritativeHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId]);

    expect(changed).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("does not prefetch an authoritative head when the direct head already satisfies the summary", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 2 });
    const directHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 2 });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId]);

    expect(changed).toBe(false);
    expect(getSessionHeadMock).not.toHaveBeenCalled();
  });

  it("force-prefetches an authoritative head even when the direct head satisfies the summary", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 2 });
    const directHead = makeHead(sessionId, { turnCount: 1, lastEventSeq: 2 });
    const authoritativeHead = makeHead(sessionId, { turnCount: 2, lastEventSeq: 3 });
    getSessionHeadMock.mockResolvedValue(authoritativeHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
    });

    expect(changed).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(3);
  });

  it("does not throttle repair prefetch after a forced optimistic miss", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, { lastEventSeq: 3, projectionRev: 7 });
    const repairedHead = makeHead(sessionId, { turnCount: 3, lastEventSeq: 8 });
    getSessionHeadMock
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(repairedHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const forcedChanged = await primeAuthoritativeSessionHeads(
      snapshot,
      store,
      bootstrapCache,
      [sessionId],
      {
        force: true,
        reason: "foreground_force",
      },
    );
    const repairChanged = await primeAuthoritativeSessionHeads(
      snapshot,
      store,
      bootstrapCache,
      [sessionId],
      {
        reason: "summary_repair",
      },
    );

    expect(forcedChanged).toBe(false);
    expect(repairChanged).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(2);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
  });

  it("accepts a newer authoritative head when activity has advanced past a running summary", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, {
      lastEventSeq: 3,
      activity: { is_working: true, last_turn_status: "running" },
    });
    const directHead = {
      ...makeHead(sessionId, { turnCount: 1, lastEventSeq: 3, turnStatus: "running" }),
      activity: { is_working: true, last_turn_status: "running" },
    } satisfies SessionHeadSnapshot;
    const authoritativeHead = {
      ...makeHead(sessionId, { turnCount: 2, lastEventSeq: 8, turnStatus: "completed" }),
      activity: { is_working: false, last_turn_status: "completed" },
    } satisfies SessionHeadSnapshot;
    getSessionHeadMock.mockResolvedValue(authoritativeHead);
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => directHead),
      getSessionHeadsSnapshot: vi.fn(() => ({ [sessionId]: directHead })),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
    });

    expect(changed).toBe(true);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
    expect(bootstrapCache.get(sessionId)?.last_event_seq).toBe(8);
    expect(bootstrapCache.get(sessionId)?.activity?.last_turn_status).toBe("completed");
  });

  it("plans foreground targets before bounded warm targets", () => {
    const plan = planSessionHeadPrefetchTargets({
      foregroundSessionIds: ["foreground", "warm-1"],
      warmSessionIds: ["warm-1", "warm-2", "warm-3"],
      maxTargets: 3,
    });

    expect(plan.targetSessionIds).toEqual(["foreground", "warm-1", "warm-2"]);
    expect(plan.foregroundSessionIds).toEqual(["foreground", "warm-1"]);
    expect(plan.warmSessionIds).toEqual(["warm-2"]);
  });

  it("excludes working sessions from workspace-sync authoritative prefetch readiness", () => {
    const snapshot = makeSnapshot("session-1", {
      lastEventSeq: 8,
      activity: { is_working: true, last_turn_status: "running" },
    });

    expect(collectAuthoritativePrefetchReadySessionIds(snapshot, ["session-1"])).toEqual([]);
    expect(
      collectAuthoritativePrefetchReadySessionIds(
        makeSnapshot("session-1", {
          lastEventSeq: 8,
          activity: { is_working: false, last_turn_status: "completed" },
        }),
        ["session-1"],
      ),
    ).toEqual(["session-1"]);
  });

  it("does not fetch authoritative heads for working summaries unless forced", async () => {
    const sessionId = "session-1";
    const snapshot = makeSnapshot(sessionId, {
      lastEventSeq: 8,
      activity: { is_working: true, last_turn_status: "running" },
    });
    getSessionHeadMock.mockResolvedValue(makeHead(sessionId, { turnCount: 3, lastEventSeq: 8, turnStatus: "running" }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    const changed = await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      reason: "workspace_sync",
    });

    expect(changed).toBe(false);
    expect(getSessionHeadMock).not.toHaveBeenCalled();

    await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, [sessionId], {
      force: true,
      reason: "foreground_force",
    });

    expect(getSessionHeadMock).toHaveBeenCalledTimes(1);
  });

  it("caps authoritative head prefetch when no explicit target list is provided", async () => {
    const sessionIds = Array.from({ length: SESSION_HEAD_PREFETCH_TARGET_LIMIT + 25 }, (_, index) => `session-${index + 1}`);
    const snapshot = makeSnapshotWithSessions(sessionIds, { lastEventSeq: 5 });
    getSessionHeadMock.mockImplementation(async (sessionId: string) =>
      makeHead(sessionId, { turnCount: 3, lastEventSeq: 5 }),
    );
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache);

    expect(getSessionHeadMock).toHaveBeenCalledTimes(SESSION_HEAD_PREFETCH_TARGET_LIMIT);
    expect(getSessionHeadMock).toHaveBeenCalledWith("session-1", expect.any(Number), true);
    expect(getSessionHeadMock).not.toHaveBeenCalledWith(
      `session-${SESSION_HEAD_PREFETCH_TARGET_LIMIT + 1}`,
      expect.any(Number),
      true,
    );
  });

  it("caps persisted bootstrap head priming with the same target budget", async () => {
    const sessionIds = Array.from({ length: 20 }, (_, index) => `session-${index + 1}`);
    const snapshot = makeSnapshotWithSessions(sessionIds);
    loadSessionHeadV1Mock.mockImplementation(async (sessionId: string) => ({
      v: 1,
      sessionId,
      head: makeHead(sessionId),
      updatedAtMs: Date.now(),
    }));
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    await primePersistedSessionHeads(snapshot, store, bootstrapCache, undefined, { maxTargets: 5 });

    expect(loadSessionHeadV1Mock).toHaveBeenCalledTimes(5);
    expect(loadSessionHeadV1Mock).toHaveBeenCalledWith("session-1");
    expect(loadSessionHeadV1Mock).not.toHaveBeenCalledWith("session-6");
  });

  it("limits concurrent authoritative head requests", async () => {
    const sessionIds = Array.from({ length: 6 }, (_, index) => `session-${index + 1}`);
    const snapshot = makeSnapshotWithSessions(sessionIds, { lastEventSeq: 5 });
    let inFlight = 0;
    let maxInFlight = 0;
    getSessionHeadMock.mockImplementation(async (sessionId: string) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 0));
      inFlight -= 1;
      return makeHead(sessionId, { turnCount: 3, lastEventSeq: 5 });
    });
    const bootstrapCache = new SessionHeadBootstrapCache();
    const store = {
      getSessionHeadSnapshot: vi.fn(() => null),
      getSessionHeadsSnapshot: vi.fn(() => ({})),
    };

    await primeAuthoritativeSessionHeads(snapshot, store, bootstrapCache, sessionIds, {
      concurrency: SESSION_HEAD_PREFETCH_CONCURRENCY,
      maxTargets: sessionIds.length,
    });

    expect(maxInFlight).toBeLessThanOrEqual(SESSION_HEAD_PREFETCH_CONCURRENCY);
    expect(getSessionHeadMock).toHaveBeenCalledTimes(sessionIds.length);
  });

  it("ignores late seed events for sessions that are no longer retained", () => {
    const sessionId = "session-1";
    const bootstrapCache = new SessionHeadBootstrapCache();

    const changed = maybeCacheSessionHeadSeed(
      bootstrapCache,
      {
        type: "session_head_seed",
        workspace_id: "workspace-1",
        snapshot_rev: 1,
        head: makeHead(sessionId, { turnCount: 3, lastEventSeq: 3 }),
      },
      new Set(["session-2"]),
    );

    expect(changed).toBe(false);
    expect(bootstrapCache.get(sessionId)).toBeUndefined();
  });
});
